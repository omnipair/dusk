mod engine;
pub(crate) mod integrated;
pub(crate) mod recovery;
pub(crate) mod solver;

pub use engine::*;
pub(crate) use integrated::*;
pub(crate) use recovery::*;
pub(crate) use solver::*;

use super::*;
use crate::transitions::{
    amm::{ConcentratedCurveDirection, ConcentratedCurvePoint, ConcentratedIntegratedAmmQuote},
    DebtClearance, DebtRepaymentQuote, HlpRecoveryBreakdown,
};
use crate::{constants::*, math::*, state::*};

/// Post-transition exposure is protocol dust only when it is no more than
/// 0.00001 target tokens and no more than one part per million of current hLP
/// NAV. Coarse assets and small vaults therefore fail closed rather than hide
/// a meaningful constrained gap.
const HLP_REBALANCE_DUST_MAX_NAD: u128 = 10_000;
const HLP_REBALANCE_DUST_NAV_DENOMINATOR: u128 = 1_000_000;

impl HlpYieldEligibility {
    /// yLP supply eligible for interest paid by an hLP funding position.
    ///
    /// Both hLP vault balances are excluded. The permanently burned
    /// `MIN_LIQUIDITY` shares remain in this denominator, so funding interest
    /// has a deterministic sink when no ordinary yLP holder exists and cannot
    /// be captured by a later deposit.
    pub fn non_hlp_ylp_supply(self) -> Result<u64> {
        let hlp_ylp_shares = self
            .base_hlp_ylp_shares
            .checked_add(self.quote_hlp_ylp_shares)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let supply = self
            .ylp_supply
            .checked_sub(hlp_ylp_shares)
            .ok_or(ErrorCode::BrokenInvariant)?;
        require_gte!(supply, MIN_LIQUIDITY, ErrorCode::BrokenInvariant);
        Ok(supply)
    }
}

impl HlpVault {
    pub fn initialize(&mut self, ylp_vault: Pubkey) {
        self.ylp_vault = ylp_vault;
    }

    pub fn mint_hlp(&mut self, amount: u64) -> Result<()> {
        require!(amount > 0, ErrorCode::AmountZero);
        self.hlp_supply = self.hlp_supply.checked_add(amount).ok_or(ErrorCode::SupplyOverflow)?;
        Ok(())
    }

    pub fn burn_hlp(&mut self, amount: u64) -> Result<()> {
        require!(amount > 0, ErrorCode::AmountZero);
        self.hlp_supply = self.hlp_supply.checked_sub(amount).ok_or(ErrorCode::SupplyUnderflow)?;
        if self.hlp_supply == 0 {
            require_eq!(self.ylp_shares, 0, ErrorCode::BrokenInvariant);
            require_eq!(self.base_hlp_live_reserve, 0, ErrorCode::BrokenInvariant);
            require_eq!(self.quote_hlp_live_reserve, 0, ErrorCode::BrokenInvariant);
            require_eq!(self.debt_shares, 0, ErrorCode::BrokenInvariant);
            require_eq!(self.debt_principal, 0, ErrorCode::BrokenInvariant);
            // A fully closed vault has no economic exposure. Do not leave a
            // stale fail-closed signal that would keep the next generation of
            // deposits gated after every share and debt claim is gone.
            self.residual_exposure = 0;
            self.funding_apr_ema_nad = 0;
            self.funding_apr_ema_last_slot = 0;
        }
        Ok(())
    }

    pub fn credit_ylp(&mut self, shares: u64) -> Result<()> {
        self.ylp_shares = self.ylp_shares.checked_add(shares).ok_or(ErrorCode::SupplyOverflow)?;
        Ok(())
    }

    pub fn debit_ylp(&mut self, shares: u64) -> Result<()> {
        self.ylp_shares = self.ylp_shares.checked_sub(shares).ok_or(ErrorCode::SupplyUnderflow)?;
        Ok(())
    }

    pub fn hlp_live_reserve(&self, asset: MarketAsset) -> u64 {
        match asset {
            MarketAsset::Base => self.base_hlp_live_reserve,
            MarketAsset::Quote => self.quote_hlp_live_reserve,
        }
    }

    pub fn credit_hlp_live_reserve(&mut self, asset: MarketAsset, amount: u64) -> Result<()> {
        let reserve = match asset {
            MarketAsset::Base => &mut self.base_hlp_live_reserve,
            MarketAsset::Quote => &mut self.quote_hlp_live_reserve,
        };
        *reserve = reserve.checked_add(amount).ok_or(ErrorCode::ReserveOverflow)?;
        Ok(())
    }

    pub fn debit_hlp_live_reserve(&mut self, asset: MarketAsset, amount: u64) -> Result<()> {
        let reserve = match asset {
            MarketAsset::Base => &mut self.base_hlp_live_reserve,
            MarketAsset::Quote => &mut self.quote_hlp_live_reserve,
        };
        *reserve = reserve.checked_sub(amount).ok_or(ErrorCode::ReserveUnderflow)?;
        Ok(())
    }

    pub fn add_debt_shares(&mut self, shares: u128) -> Result<()> {
        self.debt_shares = self
            .debt_shares
            .checked_add(shares)
            .ok_or(ErrorCode::DebtShareMathOverflow)?;
        Ok(())
    }

    pub fn add_debt_principal(&mut self, amount: u64) -> Result<()> {
        self.debt_principal = self
            .debt_principal
            .checked_add(amount)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        Ok(())
    }

    pub fn clear_debt_repay(&mut self, shares_burned: u128, borrow_index_nad: u128) -> Result<DebtClearance> {
        require!(shares_burned > 0, ErrorCode::DebtShareDivisionOverflow);
        require_gte!(self.debt_shares, shares_burned, ErrorCode::DebtShareMathOverflow);
        let total_debt = Debt::shares_to_debt(self.debt_shares, borrow_index_nad)?;
        let remaining_shares = self
            .debt_shares
            .checked_sub(shares_burned)
            .ok_or(ErrorCode::DebtShareMathOverflow)?;
        let remaining_debt = Debt::shares_to_debt(remaining_shares, borrow_index_nad)?;
        let debt_reduced_u128 = total_debt
            .checked_sub(remaining_debt)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        let debt_reduced = u64::try_from(debt_reduced_u128).map_err(|_| ErrorCode::DebtMathOverflow)?;

        let principal = u128::from(self.debt_principal).min(total_debt);
        let (principal_paid, interest_paid) =
            crate::math::realized_interest_split(debt_reduced, total_debt, principal)?;
        let remaining_debt = u64::try_from(remaining_debt).map_err(|_| ErrorCode::DebtMathOverflow)?;
        self.debt_shares = remaining_shares;
        self.debt_principal = self.debt_principal.saturating_sub(principal_paid);
        if self.debt_shares == 0 {
            self.debt_principal = 0;
        }

        Ok(DebtClearance {
            shares_burned,
            cash_repaid: debt_reduced,
            debt_reduced,
            aggregate_debt_reduced: debt_reduced,
            principal_paid,
            interest_paid,
            remaining_debt,
            position_principal_reduced: principal_paid,
        })
    }

    pub fn repayment_for_max(&self, max_repay_amount: u64, borrow_index_nad: u128) -> Result<DebtRepaymentQuote> {
        Debt::repayment_for_max(self.debt_shares, self.debt_shares, borrow_index_nad, max_repay_amount)
    }

    pub fn checkpoint_yield_from_ylp(&mut self, base_side: &MarketSide, quote_side: &MarketSide) -> Result<()> {
        self.checkpoint_yield_from_ylp_shares(base_side, quote_side, self.ylp_shares)
    }

    pub fn checkpoint_yield_from_ylp_shares(
        &mut self,
        base_side: &MarketSide,
        quote_side: &MarketSide,
        eligible_ylp_shares: u64,
    ) -> Result<()> {
        let (base_swap_fee_amount, base_swap_fee_remainder_q64) = accrue_fee_liability_with_remainder(
            eligible_ylp_shares,
            base_side.fees.swap_fee_growth_index_q64,
            self.base_swap_fee_checkpoint_q64,
            self.base_swap_fee_remainder_q64,
        )?;
        let (base_interest_amount, base_interest_remainder_q64) = accrue_fee_liability_with_remainder(
            eligible_ylp_shares,
            base_side.fees.interest_growth_index_q64,
            self.base_interest_checkpoint_q64,
            self.base_interest_remainder_q64,
        )?;
        let (quote_swap_fee_amount, quote_swap_fee_remainder_q64) = accrue_fee_liability_with_remainder(
            eligible_ylp_shares,
            quote_side.fees.swap_fee_growth_index_q64,
            self.quote_swap_fee_checkpoint_q64,
            self.quote_swap_fee_remainder_q64,
        )?;
        let (quote_interest_amount, quote_interest_remainder_q64) = accrue_fee_liability_with_remainder(
            eligible_ylp_shares,
            quote_side.fees.interest_growth_index_q64,
            self.quote_interest_checkpoint_q64,
            self.quote_interest_remainder_q64,
        )?;

        credit_hlp_growth(
            self.hlp_supply,
            &mut self.unallocated_base_swap_fee_amount,
            &mut self.base_swap_fee_growth_index_q64,
            &mut self.base_swap_fee_growth_remainder_scaled,
            base_swap_fee_amount,
        )?;
        credit_hlp_growth(
            self.hlp_supply,
            &mut self.unallocated_base_interest_amount,
            &mut self.base_interest_growth_index_q64,
            &mut self.base_interest_growth_remainder_scaled,
            base_interest_amount,
        )?;
        credit_hlp_growth(
            self.hlp_supply,
            &mut self.unallocated_quote_swap_fee_amount,
            &mut self.quote_swap_fee_growth_index_q64,
            &mut self.quote_swap_fee_growth_remainder_scaled,
            quote_swap_fee_amount,
        )?;
        credit_hlp_growth(
            self.hlp_supply,
            &mut self.unallocated_quote_interest_amount,
            &mut self.quote_interest_growth_index_q64,
            &mut self.quote_interest_growth_remainder_scaled,
            quote_interest_amount,
        )?;

        self.base_swap_fee_checkpoint_q64 = base_side.fees.swap_fee_growth_index_q64;
        self.base_interest_checkpoint_q64 = base_side.fees.interest_growth_index_q64;
        self.quote_swap_fee_checkpoint_q64 = quote_side.fees.swap_fee_growth_index_q64;
        self.quote_interest_checkpoint_q64 = quote_side.fees.interest_growth_index_q64;
        self.base_swap_fee_remainder_q64 = base_swap_fee_remainder_q64;
        self.base_interest_remainder_q64 = base_interest_remainder_q64;
        self.quote_swap_fee_remainder_q64 = quote_swap_fee_remainder_q64;
        self.quote_interest_remainder_q64 = quote_interest_remainder_q64;
        Ok(())
    }

    pub fn yield_growth_indexes(&self, revenue_asset: MarketAsset) -> (u128, u128) {
        match revenue_asset {
            MarketAsset::Base => (self.base_swap_fee_growth_index_q64, self.base_interest_growth_index_q64),
            MarketAsset::Quote => (
                self.quote_swap_fee_growth_index_q64,
                self.quote_interest_growth_index_q64,
            ),
        }
    }
}

/// Publishes one hLP vault's yLP-owned revenue through the same exact Q64
/// distributor used by the outer yLP tier, retaining whole atoms only while
/// the hLP supply is zero.
fn credit_hlp_growth(
    hlp_supply: u64,
    unallocated_amount: &mut u64,
    growth_index_q64: &mut u128,
    growth_remainder_scaled: &mut u64,
    new_amount: u64,
) -> Result<()> {
    *unallocated_amount = unallocated_amount
        .checked_add(new_amount)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    if hlp_supply == 0 || (*unallocated_amount == 0 && *growth_remainder_scaled == 0) {
        return Ok(());
    }
    let allocated = *unallocated_amount;
    let (growth_delta, remainder_scaled) = distribute_growth_q64(allocated, hlp_supply, *growth_remainder_scaled)?;
    *growth_index_q64 = growth_index_q64
        .checked_add(growth_delta)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    *unallocated_amount = unallocated_amount
        .checked_sub(allocated)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    *growth_remainder_scaled = remainder_scaled;
    Ok(())
}

impl Market {
    pub fn has_active_hlp(&self) -> bool {
        self.base_hlp_vault.hlp_supply > 0
            || self.base_hlp_vault.residual_exposure != 0
            || self.quote_hlp_vault.hlp_supply > 0
            || self.quote_hlp_vault.residual_exposure != 0
    }

    pub fn hlp_yield_growth_indexes(&self, hlp_asset: MarketAsset, revenue_asset: MarketAsset) -> (u128, u128) {
        match hlp_asset {
            MarketAsset::Base => self.base_hlp_vault.yield_growth_indexes(revenue_asset),
            MarketAsset::Quote => self.quote_hlp_vault.yield_growth_indexes(revenue_asset),
        }
    }

    pub fn drain_hlp_unallocated_yield(
        &mut self,
        hlp_asset: MarketAsset,
        base_yield_account: &mut YieldAccount,
        quote_yield_account: &mut YieldAccount,
    ) -> Result<()> {
        let vault = match hlp_asset {
            MarketAsset::Base => &mut self.base_hlp_vault,
            MarketAsset::Quote => &mut self.quote_hlp_vault,
        };
        require_eq!(vault.hlp_supply, 0, ErrorCode::BrokenInvariant);
        base_yield_account.credit_unallocated(
            vault.unallocated_base_swap_fee_amount,
            vault.unallocated_base_interest_amount,
            (vault.base_swap_fee_remainder_q64 as u128)
                .checked_add(vault.base_swap_fee_growth_remainder_scaled as u128)
                .ok_or(ErrorCode::MarketMathOverflow)?,
            (vault.base_interest_remainder_q64 as u128)
                .checked_add(vault.base_interest_growth_remainder_scaled as u128)
                .ok_or(ErrorCode::MarketMathOverflow)?,
        )?;
        quote_yield_account.credit_unallocated(
            vault.unallocated_quote_swap_fee_amount,
            vault.unallocated_quote_interest_amount,
            (vault.quote_swap_fee_remainder_q64 as u128)
                .checked_add(vault.quote_swap_fee_growth_remainder_scaled as u128)
                .ok_or(ErrorCode::MarketMathOverflow)?,
            (vault.quote_interest_remainder_q64 as u128)
                .checked_add(vault.quote_interest_growth_remainder_scaled as u128)
                .ok_or(ErrorCode::MarketMathOverflow)?,
        )?;
        vault.unallocated_base_swap_fee_amount = 0;
        vault.unallocated_base_interest_amount = 0;
        vault.unallocated_quote_swap_fee_amount = 0;
        vault.unallocated_quote_interest_amount = 0;
        vault.base_swap_fee_remainder_q64 = 0;
        vault.base_interest_remainder_q64 = 0;
        vault.quote_swap_fee_remainder_q64 = 0;
        vault.quote_interest_remainder_q64 = 0;
        vault.base_swap_fee_growth_remainder_scaled = 0;
        vault.base_interest_growth_remainder_scaled = 0;
        vault.quote_swap_fee_growth_remainder_scaled = 0;
        vault.quote_interest_growth_remainder_scaled = 0;
        Ok(())
    }

    /// Validate that the current hLP hedge state admits new capital without
    /// exposing rebalance-engine intermediates outside the hLP domain.
    pub fn assert_hlp_entry_available(&self, target_asset: MarketAsset) -> Result<()> {
        let prices = current_hlp_curve_prices(self)?;
        let entry = current_hlp_entry_state_with_prices(self, target_asset, prices)?;
        require!(entry.disposition.admits_entry(), ErrorCode::HlpSettlementUnavailable);
        Ok(())
    }

    pub fn require_residual_hlp_swap_safety(
        &self,
        start_base_price_nad: u128,
        end_base_price_nad: u128,
        base_residual_on_entry: bool,
        quote_residual_on_entry: bool,
    ) -> Result<()> {
        let start_prices = hlp_curve_prices_from_base_price_nad(start_base_price_nad)?;
        let end_prices = hlp_curve_prices_from_base_price_nad(end_base_price_nad)?;
        require_residual_hlp_swap_safe(
            self,
            MarketAsset::Base,
            start_prices,
            end_prices,
            base_residual_on_entry,
        )?;
        require_residual_hlp_swap_safe(
            self,
            MarketAsset::Quote,
            start_prices,
            end_prices,
            quote_residual_on_entry,
        )
    }

    pub fn checkpoint_hlp_vaults(&mut self) -> Result<(i128, i128)> {
        let prices = current_hlp_curve_prices(self)?;
        checkpoint_hlp_yield_from_ylp(self, MarketAsset::Base)?;
        checkpoint_hlp_yield_from_ylp(self, MarketAsset::Quote)?;
        let base_active = self.base_hlp_vault.hlp_supply > 0 || self.base_hlp_vault.residual_exposure != 0;
        let quote_active = self.quote_hlp_vault.hlp_supply > 0 || self.quote_hlp_vault.residual_exposure != 0;
        let base_delta = if base_active {
            checkpoint_one_hlp_with_prices(self, MarketAsset::Base, prices)?
        } else {
            0
        };
        let quote_delta = if quote_active {
            checkpoint_one_hlp_with_prices(self, MarketAsset::Quote, prices)?
        } else {
            0
        };
        Ok((base_delta, quote_delta))
    }

    pub fn checkpoint_hlp_yield_from_ylp(&mut self, target_asset: MarketAsset) -> Result<()> {
        checkpoint_hlp_yield_from_ylp(self, target_asset)
    }

    pub fn checkpoint_hlp_yield_from_ylp_shares(
        &mut self,
        target_asset: MarketAsset,
        eligible_ylp_shares: u64,
    ) -> Result<()> {
        checkpoint_hlp_yield_from_ylp_shares(self, target_asset, eligible_ylp_shares)
    }
}

impl Market {
    /// Current opposite-asset funding APR for one target-asset hLP vault.
    pub fn current_hlp_funding_apr_nad(&self, target_asset: MarketAsset) -> Result<u128> {
        let borrowed_asset = target_asset.opposite();
        let side = self.side(borrowed_asset);
        let fixed_debt = match borrowed_asset {
            MarketAsset::Base => self.debt.fixed_base_debt()?,
            MarketAsset::Quote => self.debt.fixed_quote_debt()?,
        };
        let isolated_debt = self.debt.isolated_debt(borrowed_asset)?;
        let vault = match target_asset {
            MarketAsset::Base => &self.base_hlp_vault,
            MarketAsset::Quote => &self.quote_hlp_vault,
        };
        let hlp_debt = Debt::shares_to_debt(vault.debt_shares, self.debt.borrow_index(borrowed_asset))?;
        let total_debt = fixed_debt
            .checked_add(isolated_debt)
            .and_then(|value| value.checked_add(hlp_debt))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let utilization = utilization_bps(total_debt, side.reserves.cash_reserve as u128)?;
        let error = utilization_error_nad(utilization, self.config.irm.target_utilization_bps as u64)?;
        let rate_at_target = match borrowed_asset {
            MarketAsset::Base => self.debt.base_rate_at_target_nad,
            MarketAsset::Quote => self.debt.quote_rate_at_target_nad,
        };
        instantaneous_rate_apr_nad(rate_at_target, error, self.config.irm.curve_steepness_nad as u128)
    }

    /// Twelve-hour funding APR signal used by hLP Stop Rate orders.
    pub fn hlp_funding_apr_ema_nad(&self, target_asset: MarketAsset) -> Result<u128> {
        let vault = match target_asset {
            MarketAsset::Base => &self.base_hlp_vault,
            MarketAsset::Quote => &self.quote_hlp_vault,
        };
        if vault.funding_apr_ema_last_slot == 0 {
            self.current_hlp_funding_apr_nad(target_asset)
        } else {
            Ok(vault.funding_apr_ema_nad)
        }
    }
}

impl Market {
    pub fn hlp_live_reserve(&self, asset: MarketAsset) -> Result<u128> {
        (self.base_hlp_vault.hlp_live_reserve(asset) as u128)
            .checked_add(self.quote_hlp_vault.hlp_live_reserve(asset) as u128)
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    }

    /// Indexed aggregate hLP funding debt denominated in `asset`. Base debt is
    /// carried by the quote hLP vault and quote debt by the base hLP vault.
    pub fn hlp_funding_debt(&self, asset: MarketAsset) -> Result<u128> {
        match asset {
            MarketAsset::Base => {
                Debt::shares_to_debt(self.quote_hlp_vault.debt_shares, self.debt.base_borrow_index_nad)
            }
            MarketAsset::Quote => {
                Debt::shares_to_debt(self.base_hlp_vault.debt_shares, self.debt.quote_borrow_index_nad)
            }
        }
    }

    /// Admission-only capacity for new hLP funding. Existing hLP debt does not
    /// reserve cash from ordinary withdrawals, borrows, or liquidations.
    pub fn hlp_funding_headroom(&self, asset: MarketAsset) -> Result<u64> {
        let (debt_shares, borrow_index_nad) = match asset {
            MarketAsset::Base => (self.quote_hlp_vault.debt_shares, self.debt.base_borrow_index_nad),
            MarketAsset::Quote => (self.base_hlp_vault.debt_shares, self.debt.quote_borrow_index_nad),
        };
        let cash = self.side(asset).reserves.cash_reserve as u128;
        // Debt conversion floors total shares. Solve the largest total share
        // count whose converted debt remains <= cash; subtracting floored raw
        // debt would overstate capacity when the next borrow rounds shares up.
        let max_total_shares = mul_div_ceil_u128(
            cash.checked_add(1).ok_or(ErrorCode::MarketMathOverflow)?,
            NAD as u128,
            borrow_index_nad,
        )?
        .checked_sub(1)
        .ok_or(ErrorCode::MarketMathOverflow)?;
        let available_shares = max_total_shares.saturating_sub(debt_shares);
        let headroom = mul_div_u128(available_shares, borrow_index_nad, NAD as u128)?;
        u64::try_from(headroom).map_err(|_| ErrorCode::MarketMathOverflow.into())
    }
}

/// LP ownership frozen before an operation may mint or burn vault-owned yLP.
/// Funding entitlement is deliberately payment-time: ordinary yLP present at
/// this snapshot participates even if the debt accrued earlier, while a holder
/// that exited before the snapshot does not. Newly borrowed principal at the
/// same debt index cannot create interest. Inline settlement uses this
/// partition to prove that ordinary-plus-burned-MIN supply was conserved
/// across hLP rebalancing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HlpYieldEligibility {
    pub ylp_supply: u64,
    pub base_hlp_ylp_shares: u64,
    pub quote_hlp_ylp_shares: u64,
}

#[cfg(test)]
mod tests {
    include!("../../../tests/transitions/liquidity_hlp.rs");
}
