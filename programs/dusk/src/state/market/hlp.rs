use anchor_lang::prelude::*;

use super::{
    accrue_fee_liability_with_remainder, distribute_growth_q64, Debt, DebtClearance, DebtRepaymentQuote, Market,
    MarketAsset, MarketSide,
};
use crate::{errors::ErrorCode, state::YieldAccount};

pub(crate) use crate::state::market::transitions::hedge::HlpRebalanceReceipt;
use crate::state::market::transitions::hedge::{
    checkpoint_hlp_yield_from_ylp, checkpoint_one_hlp_with_prices, checkpoint_pre_solve_fee_eligibility,
    combine_hlp_rebalance_receipts, current_hlp_curve_prices, empty_hlp_rebalance_receipt, rebalance_one_hlp,
};

/// LP ownership frozen before an operation may mint or burn vault-owned yLP.
/// Any hLP debt interest realized by that operation was accrued by debt that
/// existed at this snapshot; newly borrowed principal at the same debt index
/// cannot create interest. Inline settlement therefore uses these balances,
/// never post-rebalance supply, when publishing the eventual vault credit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HlpYieldEligibility {
    pub ylp_supply: u64,
    pub base_hlp_ylp_shares: u64,
    pub quote_hlp_ylp_shares: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace)]
pub struct HlpVault {
    pub ylp_vault: Pubkey,
    pub ylp_shares: u64,
    /// hLP-owned live reserve depth that is not backed by reserve cash or
    /// normal cash-backed debt. This is the explicit synthetic live component
    /// in `r_virtual = r_cash + r_cash_backed_debt + r_hlp_live`.
    pub base_hlp_live_reserve: u64,
    pub quote_hlp_live_reserve: u64,
    /// Funding debt used by the hLP vault. It accrues interest and counts
    /// toward utilization, but is not same-side cash-backed reserve debt.
    pub debt_shares: u128,
    /// Raw borrowed token atoms; products and indexed shares stay `u128`.
    pub debt_principal: u64,
    pub hlp_supply: u64,
    pub residual_exposure: i128,
    pub base_swap_fee_growth_index_q64: u128,
    pub base_interest_growth_index_q64: u128,
    pub quote_swap_fee_growth_index_q64: u128,
    pub quote_interest_growth_index_q64: u128,
    pub base_swap_fee_checkpoint_q64: u128,
    pub base_interest_checkpoint_q64: u128,
    pub quote_swap_fee_checkpoint_q64: u128,
    pub quote_interest_checkpoint_q64: u128,
    /// Aggregate sub-atom yLP entitlement carried across hLP checkpoints.
    /// These are distinct from each holder YieldAccount remainder: this layer
    /// converts vault-owned yLP growth into hLP growth without double-flooring.
    pub base_swap_fee_remainder_q64: u64,
    pub base_interest_remainder_q64: u64,
    pub quote_swap_fee_remainder_q64: u64,
    pub quote_interest_remainder_q64: u64,
    /// Sub-index distribution carry for the second, yLP-to-hLP allocation
    /// layer. Whole-token backing represented here has already left the
    /// corresponding `unallocated_*` bucket.
    pub base_swap_fee_growth_remainder_scaled: u64,
    pub base_interest_growth_remainder_scaled: u64,
    pub quote_swap_fee_growth_remainder_scaled: u64,
    pub quote_interest_growth_remainder_scaled: u64,
    pub unallocated_base_swap_fee_amount: u64,
    pub unallocated_base_interest_amount: u64,
    pub unallocated_quote_swap_fee_amount: u64,
    pub unallocated_quote_interest_amount: u64,
    pub last_nav_nad: u128,
    pub cached_settlement_price_nad: u128,
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

/// Publish the yLP-owned revenue of one hLP vault into its holder index. This
/// is the same exact distributor used at the outer yLP tier:
/// `new_amount * 2^64 + old_carry = delta * hlp_supply + new_carry`.
/// Once supply is positive, every whole atom leaves `unallocated_amount`; the
/// carry is its backed, not-yet-indexable residue and cannot be allocated a
/// second time. A zero-supply vault instead retains the whole amount for its
/// final-holder drain.
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

    pub fn deposit_single_sided(
        &mut self,
        target_asset: MarketAsset,
        deposit_amount: u64,
        min_hlp_amount: u64,
    ) -> Result<crate::state::market::transitions::hedge::HedgeReceipt> {
        crate::state::market::transitions::hedge::DepositSingleSided::new(target_asset, deposit_amount, min_hlp_amount)
            .apply(self)
    }

    pub fn withdraw_single_sided(
        &mut self,
        target_asset: MarketAsset,
        hlp_amount: u64,
    ) -> Result<crate::state::market::transitions::hedge::HedgeReceipt> {
        crate::state::market::transitions::hedge::WithdrawSingleSided::new(target_asset, hlp_amount).apply(self)
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

    pub fn finalize_hlp_vaults_for_swap(
        &mut self,
        base_pre_rebalance: HlpRebalanceReceipt,
        quote_pre_rebalance: HlpRebalanceReceipt,
    ) -> Result<(HlpRebalanceReceipt, HlpRebalanceReceipt)> {
        checkpoint_pre_solve_fee_eligibility(self, &base_pre_rebalance)?;
        checkpoint_pre_solve_fee_eligibility(self, &quote_pre_rebalance)?;
        // A swap moves both numeraires. Correct each active side explicitly so
        // neither vault carries avoidable delta into the next user operation.
        let base_post_rebalance = if self.base_hlp_vault.hlp_supply > 0 || self.base_hlp_vault.residual_exposure != 0 {
            rebalance_one_hlp(self, MarketAsset::Base)?
        } else {
            empty_hlp_rebalance_receipt(MarketAsset::Base)
        };
        let quote_post_rebalance = if self.quote_hlp_vault.hlp_supply > 0 || self.quote_hlp_vault.residual_exposure != 0
        {
            rebalance_one_hlp(self, MarketAsset::Quote)?
        } else {
            empty_hlp_rebalance_receipt(MarketAsset::Quote)
        };
        Ok((
            combine_hlp_rebalance_receipts(base_pre_rebalance, base_post_rebalance)?,
            combine_hlp_rebalance_receipts(quote_pre_rebalance, quote_post_rebalance)?,
        ))
    }

    pub fn checkpoint_hlp_yield_from_ylp(&mut self, target_asset: MarketAsset) -> Result<()> {
        crate::state::market::transitions::hedge::checkpoint_hlp_yield_from_ylp(self, target_asset)
    }

    pub fn checkpoint_hlp_yield_from_ylp_shares(
        &mut self,
        target_asset: MarketAsset,
        eligible_ylp_shares: u64,
    ) -> Result<()> {
        crate::state::market::transitions::hedge::checkpoint_hlp_yield_from_ylp_shares(
            self,
            target_asset,
            eligible_ylp_shares,
        )
    }
}

#[cfg(test)]
mod tests {
    include!("../../tests/state/hlp.rs");
}
