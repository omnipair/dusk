#[cfg(test)]
use crate::state::HlpVault;
use crate::{
    constants::NAD,
    errors::ErrorCode,
    math::{
        apply_hlp_recovery_bonus, denormalize_from_nad_ceil, denormalize_from_nad_floor, hlp_opposite_exposure_nad,
        mul_div_u128, normalize_to_nad, quote_hlp_recovery, ratio_lte_full_width, reconstruct_hlp_endpoint,
        reconstruct_hlp_ownership, ExplicitCurveDirection, ExplicitCurvePoint, HlpInventoryValuesNad,
        IntegratedCurveState,
    },
    state::{Debt, Market, MarketAsset},
};
use anchor_lang::prelude::*;

use super::{amm::ExplicitIntegratedAmmQuote, HlpRecoveryBreakdown};

/// Post-transition exposure is protocol dust only when it is no more than
/// 0.00001 target tokens and no more than one part per million of current hLP
/// NAV. Coarse assets and small vaults therefore fail closed rather than hide
/// a meaningful constrained gap.
const HLP_REBALANCE_DUST_MAX_NAD: u128 = 10_000;
const HLP_REBALANCE_DUST_NAV_DENOMINATOR: u128 = 1_000_000;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SwapCashFloors {
    base: u64,
    quote: u64,
}

impl SwapCashFloors {
    fn set(&mut self, asset: MarketAsset, amount: u64) {
        match asset {
            MarketAsset::Base => self.base = amount,
            MarketAsset::Quote => self.quote = amount,
        }
    }

    fn for_asset(self, asset: MarketAsset) -> u64 {
        match asset {
            MarketAsset::Base => self.base,
            MarketAsset::Quote => self.quote,
        }
    }

    pub(crate) fn available(self, market: &Market) -> bool {
        market.base_side.reserves.cash_reserve >= self.base && market.quote_side.reserves.cash_reserve >= self.quote
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SwapCashPolicy {
    Spot,
    Borrow {
        asset: MarketAsset,
        amount: u64,
    },
    Decrease {
        debt_asset: MarketAsset,
        debt_shares: u128,
        debt_principal: u128,
    },
    Close {
        debt_asset: MarketAsset,
        debt_shares: u128,
        debt_principal: u128,
    },
    Liquidate {
        debt_asset: MarketAsset,
        debt_shares: u128,
        debt_principal: u128,
    },
}

impl SwapCashPolicy {
    pub(crate) fn floors(self, market: &Market, asset_in: MarketAsset, amount_out: u64) -> Result<SwapCashFloors> {
        let mut floors = SwapCashFloors::default();
        match self {
            Self::Spot => floors.set(asset_in.opposite(), amount_out),
            Self::Borrow { asset, amount } => {
                require!(asset == asset_in, ErrorCode::BrokenInvariant);
                floors.set(asset, amount);
                floors.set(asset_in.opposite(), amount_out);
            }
            Self::Decrease {
                debt_asset,
                debt_shares,
                debt_principal,
            } => {
                require!(debt_asset == asset_in.opposite(), ErrorCode::BrokenInvariant);
                let (_, interest) =
                    isolated_repayment_cash_and_interest(market, debt_asset, debt_shares, debt_principal, amount_out)?;
                floors.set(debt_asset, interest);
            }
            Self::Close {
                debt_asset,
                debt_shares,
                debt_principal,
            } => {
                require!(debt_asset == asset_in.opposite(), ErrorCode::BrokenInvariant);
                let (debt_cash, interest) =
                    isolated_repayment_cash_and_interest(market, debt_asset, debt_shares, debt_principal, u64::MAX)?;
                floors.set(
                    debt_asset,
                    interest
                        .checked_add(amount_out.saturating_sub(debt_cash))
                        .ok_or(ErrorCode::MarketMathOverflow)?,
                );
            }
            Self::Liquidate {
                debt_asset,
                debt_shares,
                debt_principal,
            } => {
                require!(debt_asset == asset_in.opposite(), ErrorCode::BrokenInvariant);
                if debt_shares == 0 {
                    require_eq!(debt_principal, 0, ErrorCode::BrokenInvariant);
                    return Ok(floors);
                }
                let full = market
                    .debt
                    .isolated_repayment_for_max(debt_asset, debt_shares, u64::MAX)?;
                let repay_credit = amount_out.min(full.cash_repaid);
                let repayment_basis = (full.cash_repaid as u128).max(debt_principal);
                let (_, interest) =
                    crate::math::realized_interest_split(repay_credit, repayment_basis, debt_principal)?;
                floors.set(
                    debt_asset,
                    interest
                        .checked_add(amount_out.saturating_sub(full.cash_repaid))
                        .ok_or(ErrorCode::MarketMathOverflow)?,
                );
            }
        }
        Ok(floors)
    }
}

fn isolated_repayment_cash_and_interest(
    market: &Market,
    debt_asset: MarketAsset,
    debt_shares: u128,
    debt_principal: u128,
    max_repay: u64,
) -> Result<(u64, u64)> {
    if max_repay == 0 {
        return Ok((0, 0));
    }
    let clearance = market
        .debt
        .isolated_clearance_for_max(debt_asset, debt_shares, debt_principal, max_repay)?;
    Ok((clearance.cash_repaid, clearance.interest_paid))
}

fn recognized_hlp_residual_exposure(actual_residual_nad: i128, nav_nad: u128) -> i128 {
    let tolerance_nad = HLP_REBALANCE_DUST_MAX_NAD.min(nav_nad / HLP_REBALANCE_DUST_NAV_DENOMINATOR);
    if actual_residual_nad.unsigned_abs() <= tolerance_nad {
        0
    } else {
        actual_residual_nad
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SingleSidedLiquidityReceipt {
    pub deposit_amount: u64,
    pub borrowed_amount: u64,
    pub ylp_amount: u64,
    pub hlp_amount: u64,
    pub hlp_supply: u64,
    pub target_amount_out: u64,
    pub debt_repaid: u64,
    pub interest_paid: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HlpRebalanceReceipt {
    pub target_asset: MarketAsset,
    pub ideal_delta: i128,
    pub executed_delta: i128,
    pub residual_exposure: i128,
    pub current_swap_fee_eligible_ylp_shares: u64,
    pub ylp_mint_amount: u64,
    pub ylp_burn_amount: u64,
    pub debt_delta: i128,
    pub interest_paid: u64,
    pub nav_nad: u128,
    pub(crate) tracking_start_nav_nad: i128,
    pub(crate) tracking_loss_budget_nad: u128,
    /// Frozen public-borrow interest only; hLP funding interest is ineligible.
    pub(crate) tracking_base_unrealized_interest: u64,
    /// Frozen public-borrow interest only; hLP funding interest is ineligible.
    pub(crate) tracking_quote_unrealized_interest: u64,
    pub(crate) tracking_start_ylp_shares: u64,
    pub(crate) tracking_start_ylp_supply: u64,
    pub(crate) tracking_retained_contribution_nad: i128,
    /// Internal controller signal: an explicit cash/collateral/debt/share cap
    /// clipped the requested preposition. Such a point is authoritative when
    /// all lifecycle guards pass, but cannot define a predictive derivative.
    pub(crate) preposition_capacity_bound: bool,
}

impl Default for HlpRebalanceReceipt {
    fn default() -> Self {
        Self {
            target_asset: MarketAsset::Base,
            ideal_delta: 0,
            executed_delta: 0,
            residual_exposure: 0,
            current_swap_fee_eligible_ylp_shares: 0,
            ylp_mint_amount: 0,
            ylp_burn_amount: 0,
            debt_delta: 0,
            interest_paid: 0,
            nav_nad: 0,
            tracking_start_nav_nad: 0,
            tracking_loss_budget_nad: 0,
            tracking_base_unrealized_interest: 0,
            tracking_quote_unrealized_interest: 0,
            tracking_start_ylp_shares: 0,
            tracking_start_ylp_supply: 0,
            tracking_retained_contribution_nad: 0,
            preposition_capacity_bound: false,
        }
    }
}

/// Identity-bound O(1) hLP ownership/debt reconstruction for the explicit
/// curve. The quote fixes the ordinary tranche; this plan only realizes
/// accrued hLP funding interest and refinances both vaults to the quoted
/// zero-opposite-exposure endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ExplicitHlpTransition {
    expected_curve_revision: u64,
    expected_ylp_supply: u64,
    expected_base_ylp_shares: u64,
    expected_quote_ylp_shares: u64,
    expected_base_debt_shares: u128,
    expected_quote_debt_shares: u128,
    expected_base_debt_principal: u64,
    expected_quote_debt_principal: u64,
    final_ylp_supply: u64,
    final_base_ylp_shares: u64,
    final_quote_ylp_shares: u64,
    final_base_debt_shares: u128,
    final_quote_debt_shares: u128,
    final_base_debt: u64,
    final_quote_debt: u64,
    final_base_live_reserve: u64,
    final_quote_live_reserve: u64,
    base_interest_paid: u64,
    quote_interest_paid: u64,
    base_receipt: HlpRebalanceReceipt,
    quote_receipt: HlpRebalanceReceipt,
}

fn signed_u64_delta(end: u64, start: u64) -> Result<i128> {
    if end >= start {
        i128::try_from(end - start).map_err(|_| ErrorCode::MarketMathOverflow.into())
    } else {
        i128::try_from(start - end)
            .map(|value| -value)
            .map_err(|_| ErrorCode::MarketMathOverflow.into())
    }
}

fn explicit_hlp_receipt(
    target_asset: MarketAsset,
    start_ylp_shares: u64,
    end_ylp_shares: u64,
    start_debt: u64,
    end_debt: u64,
    interest_paid: u64,
    nav_nad: u128,
    start_supply: u64,
) -> Result<HlpRebalanceReceipt> {
    Ok(HlpRebalanceReceipt {
        target_asset,
        ideal_delta: signed_u64_delta(end_debt, start_debt)?,
        executed_delta: signed_u64_delta(end_debt, start_debt)?,
        residual_exposure: 0,
        current_swap_fee_eligible_ylp_shares: start_ylp_shares,
        ylp_mint_amount: end_ylp_shares.saturating_sub(start_ylp_shares),
        ylp_burn_amount: start_ylp_shares.saturating_sub(end_ylp_shares),
        debt_delta: signed_u64_delta(end_debt, start_debt)?,
        interest_paid,
        nav_nad,
        tracking_start_nav_nad: i128::try_from(nav_nad).map_err(|_| ErrorCode::MarketMathOverflow)?,
        tracking_loss_budget_nad: 0,
        tracking_base_unrealized_interest: 0,
        tracking_quote_unrealized_interest: 0,
        tracking_start_ylp_shares: start_ylp_shares,
        tracking_start_ylp_supply: start_supply,
        tracking_retained_contribution_nad: 0,
        preposition_capacity_bound: false,
    })
}

/// Chooses debt shares whose raw debt equals the vault's proportional
/// opposite-asset yLP claim at the final reserve point. The continuous fixed
/// point is closed form; five adjacent raw atoms only certify integer rounding.
fn canonical_debt_for_proportional_claim(
    non_debt_reserve: u64,
    hlp_ylp_shares: u64,
    total_ylp_supply: u64,
    borrow_index_nad: u128,
) -> Result<(u128, u64)> {
    if hlp_ylp_shares == 0 {
        return Ok((0, 0));
    }
    let ordinary_and_other_shares = total_ylp_supply
        .checked_sub(hlp_ylp_shares)
        .ok_or(ErrorCode::SupplyUnderflow)?;
    require!(ordinary_and_other_shares > 0, ErrorCode::SupplyUnderflow);
    let continuous = u64::try_from(
        (non_debt_reserve as u128)
            .checked_mul(hlp_ylp_shares as u128)
            .ok_or(ErrorCode::MarketMathOverflow)?
            / ordinary_and_other_shares as u128,
    )
    .map_err(|_| ErrorCode::DebtMathOverflow)?;

    let mut best: Option<(u128, u64, u64)> = None;
    for desired in [
        Some(continuous),
        continuous.checked_sub(1),
        continuous.checked_add(1),
        continuous.checked_sub(2),
        continuous.checked_add(2),
    ]
    .into_iter()
    .flatten()
    {
        let debt_shares = if desired == 0 {
            0
        } else {
            Debt::debt_to_shares(desired, borrow_index_nad)?
        };
        let debt = u64::try_from(Debt::shares_to_debt(debt_shares, borrow_index_nad)?)
            .map_err(|_| ErrorCode::DebtMathOverflow)?;
        let total_reserve = non_debt_reserve.checked_add(debt).ok_or(ErrorCode::ReserveOverflow)?;
        let claim = u64::try_from(
            (total_reserve as u128)
                .checked_mul(hlp_ylp_shares as u128)
                .ok_or(ErrorCode::MarketMathOverflow)?
                / total_ylp_supply as u128,
        )
        .map_err(|_| ErrorCode::MarketMathOverflow)?;
        let error = debt.abs_diff(claim);
        if error <= 1 {
            return Ok((debt_shares, debt));
        }
        if best.is_none_or(|(_, _, best_error)| error < best_error) {
            best = Some((debt_shares, debt, error));
        }
    }
    let (debt_shares, debt, error) = best.ok_or(ErrorCode::BrokenInvariant)?;
    require!(error <= 1, ErrorCode::BrokenInvariant);
    Ok((debt_shares, debt))
}

pub(crate) fn prepare_explicit_hlp_transition(
    market: &Market,
    quote: ExplicitIntegratedAmmQuote,
    asset_in: MarketAsset,
) -> Result<ExplicitHlpTransition> {
    let _ = asset_in;
    prepare_explicit_hlp_transition_from_end(
        market,
        quote.integrated.executable.end,
        false,
        quote.recovery.bonus_output > 0,
    )
}

/// Adds the Yield-Basis-like recovery tranche to a Spot quote. The complete
/// input remains on the ordinary curve; only the incremental price improvement
/// is paid by the hLP whose borrowed asset matches `asset_in`.
pub(crate) fn apply_explicit_hlp_recovery(
    market: &Market,
    asset_in: MarketAsset,
    start: IntegratedCurveState,
    quote: &mut ExplicitIntegratedAmmQuote,
) -> Result<()> {
    let target_asset = asset_in.opposite();
    let vault = match target_asset {
        MarketAsset::Base => &market.base_hlp_vault,
        MarketAsset::Quote => &market.quote_hlp_vault,
    };
    if vault.hlp_supply == 0 || vault.ylp_shares == 0 || vault.debt_shares == 0 {
        return Ok(());
    }
    let supply = market.base_side.shares.ylp_supply;
    require_eq!(supply, market.quote_side.shares.ylp_supply, ErrorCode::BrokenInvariant);
    require!(supply > 0, ErrorCode::SupplyUnderflow);

    let opposite_reserve = market.curve_reserve(asset_in)?;
    let canonical_opposite_claim = u64::try_from(
        (opposite_reserve as u128)
            .checked_mul(vault.ylp_shares as u128)
            .ok_or(ErrorCode::MarketMathOverflow)?
            / supply as u128,
    )
    .map_err(|_| ErrorCode::MarketMathOverflow)?;
    let actual_debt = u64::try_from(Debt::shares_to_debt(
        vault.debt_shares,
        match target_asset {
            MarketAsset::Base => market.debt.quote_borrow_index_nad,
            MarketAsset::Quote => market.debt.base_borrow_index_nad,
        },
    )?)
    .map_err(|_| ErrorCode::DebtMathOverflow)?;
    let (target_equity_nad, target_reserve_nad, opposite_reserve_nad) = match target_asset {
        MarketAsset::Base => (start.base_hlp_equity, start.ordinary_base, start.ordinary_quote),
        MarketAsset::Quote => (start.quote_hlp_equity, start.ordinary_quote, start.ordinary_base),
    };
    let target_equity = denormalize_from_nad_floor(target_equity_nad, market.side(target_asset).asset_decimals)?;
    let target_reserve = denormalize_from_nad_floor(target_reserve_nad, market.side(target_asset).asset_decimals)?;
    let ordinary_opposite_reserve =
        denormalize_from_nad_floor(opposite_reserve_nad, market.side(asset_in).asset_decimals)?;

    // Gross hLP yield is already a holder liability in the current ledger.
    // Until the net-yield checkpoint is introduced, none of it may be spent
    // here; the recovery bonus is therefore funded solely by hLP equity.
    let recovery = quote_hlp_recovery(
        actual_debt,
        canonical_opposite_claim,
        0,
        quote.fee.amount_in_for_quote,
        target_equity,
        target_reserve,
        ordinary_opposite_reserve,
    )?;
    let bonus_output = recovery.bonus_output;
    quote.recovery = HlpRecoveryBreakdown {
        target_asset: target_asset.code(),
        funding_gap: recovery.funding_gap,
        matched_input: recovery.matched_input,
        bonus_output,
        discount_bps: recovery.discount_bps,
        critical: recovery.critical,
    };
    if bonus_output == 0 {
        return Ok(());
    }
    let effective_bonus_nad = normalize_to_nad(bonus_output as u128, market.side(target_asset).asset_decimals)?;
    apply_hlp_recovery_bonus(
        start,
        &mut quote.integrated,
        target_asset == MarketAsset::Base,
        effective_bonus_nad,
    )?;
    quote.amount_out = quote
        .amount_out
        .checked_add(bonus_output)
        .ok_or(ErrorCode::ReserveOverflow)?;
    Ok(())
}

/// Identity-bound terminal settlement for an hLP whose indexed funding debt
/// has consumed all of its marked collateral. The vault's yLP ownership is
/// retired first; insurance then replaces only the borrowed-asset shortfall,
/// and any remaining unpaid funding interest is explicitly socialized.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HlpTerminalWaterfallPlan {
    target_asset: MarketAsset,
    expected_curve_revision: u64,
    expected_ylp_supply: u64,
    expected_ylp_shares: u64,
    expected_debt_shares: u128,
    expected_debt_principal: u64,
    expected_target_hlp_live: u64,
    expected_borrowed_hlp_live: u64,
    debt: u64,
    interest_due: u64,
    collateral_value_in_borrowed: u64,
    insurance_request: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HlpTerminalWaterfallReceipt {
    pub target_asset: MarketAsset,
    pub ylp_burn_amount: u64,
    pub debt_closed: u64,
    pub interest_paid: u64,
    pub insurance_drawn: u64,
    pub socialized_loss: u64,
    pub remaining_hlp_supply: u64,
}

impl HlpTerminalWaterfallPlan {
    pub(crate) fn insurance_request(self) -> u64 {
        self.insurance_request
    }

    pub(crate) fn target_asset(self) -> MarketAsset {
        self.target_asset
    }

    pub(crate) fn consume(
        self,
        market: &mut Market,
        insurance_spent: u64,
        insurance_credit: u64,
        max_socialized_loss: u64,
    ) -> Result<HlpTerminalWaterfallReceipt> {
        require_eq!(
            market.curve_revision,
            self.expected_curve_revision,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            market.base_side.shares.ylp_supply,
            self.expected_ylp_supply,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            market.quote_side.shares.ylp_supply,
            self.expected_ylp_supply,
            ErrorCode::BrokenInvariant
        );
        let vault = match self.target_asset {
            MarketAsset::Base => &market.base_hlp_vault,
            MarketAsset::Quote => &market.quote_hlp_vault,
        };
        require_eq!(vault.ylp_shares, self.expected_ylp_shares, ErrorCode::BrokenInvariant);
        require_eq!(vault.debt_shares, self.expected_debt_shares, ErrorCode::BrokenInvariant);
        require_eq!(
            vault.debt_principal,
            self.expected_debt_principal,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            vault.hlp_live_reserve(self.target_asset),
            self.expected_target_hlp_live,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            vault.hlp_live_reserve(self.target_asset.opposite()),
            self.expected_borrowed_hlp_live,
            ErrorCode::BrokenInvariant
        );
        let remaining_hlp_supply = vault.hlp_supply;
        require_eq!(insurance_spent, self.insurance_request, ErrorCode::BrokenInvariant);
        require_gte!(insurance_spent, insurance_credit, ErrorCode::BrokenInvariant);

        let shortfall = self.debt.saturating_sub(self.collateral_value_in_borrowed);
        let socialized_loss = shortfall.saturating_sub(insurance_credit);
        require_gte!(
            max_socialized_loss,
            socialized_loss,
            ErrorCode::LiquidationSocializationExceeded
        );
        // This waterfall is intentionally scoped to passive funding accrual.
        // Principal insolvency indicates a broken hedge/accounting invariant,
        // not a loss which this permissionless path may silently socialize.
        require!(socialized_loss <= self.interest_due, ErrorCode::BrokenInvariant);
        let interest_paid = self
            .interest_due
            .checked_sub(socialized_loss)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let borrowed_asset = self.target_asset.opposite();

        if insurance_spent > 0 {
            let available = match borrowed_asset {
                MarketAsset::Base => &mut market.insurance.base_available,
                MarketAsset::Quote => &mut market.insurance.quote_available,
            };
            *available = available
                .checked_sub(insurance_spent)
                .ok_or(ErrorCode::InsufficientInsurance)?;
        }
        if insurance_credit > 0 {
            market.side_mut(borrowed_asset).credit_reserve(insurance_credit, true)?;
        }

        market.base_side.shares.burn(self.expected_ylp_shares)?;
        market.quote_side.shares.burn(self.expected_ylp_shares)?;
        if self.expected_target_hlp_live > 0 {
            market.side_mut(self.target_asset).reserves.live_reserve = market
                .side(self.target_asset)
                .reserves
                .live_reserve
                .checked_sub(self.expected_target_hlp_live)
                .ok_or(ErrorCode::ReserveUnderflow)?;
        }
        if self.expected_borrowed_hlp_live > 0 {
            market.side_mut(borrowed_asset).reserves.live_reserve = market
                .side(borrowed_asset)
                .reserves
                .live_reserve
                .checked_sub(self.expected_borrowed_hlp_live)
                .ok_or(ErrorCode::ReserveUnderflow)?;
        }
        debit_cash_for_hlp_interest(market.side_mut(borrowed_asset), interest_paid)?;

        let vault = match self.target_asset {
            MarketAsset::Base => &mut market.base_hlp_vault,
            MarketAsset::Quote => &mut market.quote_hlp_vault,
        };
        vault.ylp_shares = 0;
        vault.base_hlp_live_reserve = 0;
        vault.quote_hlp_live_reserve = 0;
        vault.debt_shares = 0;
        vault.debt_principal = 0;
        vault.residual_exposure = 0;
        vault.last_nav_nad = 0;
        vault.cached_settlement_price_nad = 0;

        market.rebase_explicit_curve_after_terminal_hlp_loss()?;
        market.assert_virtual_reserve_invariant(MarketAsset::Base)?;
        market.assert_virtual_reserve_invariant(MarketAsset::Quote)?;
        Ok(HlpTerminalWaterfallReceipt {
            target_asset: self.target_asset,
            ylp_burn_amount: self.expected_ylp_shares,
            debt_closed: self.debt,
            interest_paid,
            insurance_drawn: insurance_credit,
            socialized_loss,
            remaining_hlp_supply,
        })
    }
}

impl Market {
    pub(crate) fn hlp_terminally_closed(&self, target_asset: MarketAsset) -> bool {
        let vault = match target_asset {
            MarketAsset::Base => &self.base_hlp_vault,
            MarketAsset::Quote => &self.quote_hlp_vault,
        };
        vault.ylp_shares == 0
            && vault.debt_shares == 0
            && vault.debt_principal == 0
            && vault.base_hlp_live_reserve == 0
            && vault.quote_hlp_live_reserve == 0
    }

    pub(crate) fn prepare_terminal_hlp_waterfall(
        &mut self,
        target_asset: MarketAsset,
        max_insurance_draw: u64,
    ) -> Result<HlpTerminalWaterfallPlan> {
        checkpoint_hlp_yield_from_ylp(self, target_asset)?;
        let vault = match target_asset {
            MarketAsset::Base => &self.base_hlp_vault,
            MarketAsset::Quote => &self.quote_hlp_vault,
        };
        require!(
            vault.hlp_supply > 0 && vault.ylp_shares > 0 && vault.debt_shares > 0,
            ErrorCode::HlpNotLiquidatable
        );
        let supply = self.base_side.shares.ylp_supply;
        require_eq!(supply, self.quote_side.shares.ylp_supply, ErrorCode::BrokenInvariant);
        require!(supply > 0, ErrorCode::SupplyUnderflow);
        let target_claim = ylp_live_underlying_amount(self, target_asset, vault.ylp_shares)?;
        let borrowed_asset = target_asset.opposite();
        let borrowed_claim = ylp_live_underlying_amount(self, borrowed_asset, vault.ylp_shares)?;
        let target_curve_reserve = self.curve_reserve(target_asset)?;
        let borrowed_curve_reserve = self.curve_reserve(borrowed_asset)?;
        require!(
            target_curve_reserve > 0 && borrowed_curve_reserve > 0,
            ErrorCode::InsufficientLiquidity
        );
        let target_value_in_borrowed = u64::try_from(mul_div_u128(
            target_claim as u128,
            borrowed_curve_reserve as u128,
            target_curve_reserve as u128,
        )?)
        .map_err(|_| ErrorCode::MarketMathOverflow)?;
        let collateral_value_in_borrowed = borrowed_claim
            .checked_add(target_value_in_borrowed)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let debt = u64::try_from(Debt::shares_to_debt(
            vault.debt_shares,
            self.debt.borrow_index(borrowed_asset),
        )?)
        .map_err(|_| ErrorCode::DebtMathOverflow)?;
        let shortfall = debt.saturating_sub(collateral_value_in_borrowed);
        require!(shortfall > 0, ErrorCode::HlpNotLiquidatable);
        let interest_due = debt.saturating_sub(vault.debt_principal);
        require!(shortfall <= interest_due, ErrorCode::BrokenInvariant);
        let insurance_available = match borrowed_asset {
            MarketAsset::Base => self.insurance.base_available,
            MarketAsset::Quote => self.insurance.quote_available,
        };
        // Insurance is senior to socialization. The caller cap can make the
        // transaction fail under state drift, but it cannot elect to preserve
        // available insurance while pushing the same loss onto yLP.
        let insurance_request = shortfall.min(insurance_available);
        require_gte!(max_insurance_draw, insurance_request, ErrorCode::InsuranceDrawExceeded);
        Ok(HlpTerminalWaterfallPlan {
            target_asset,
            expected_curve_revision: self.curve_revision,
            expected_ylp_supply: supply,
            expected_ylp_shares: vault.ylp_shares,
            expected_debt_shares: vault.debt_shares,
            expected_debt_principal: vault.debt_principal,
            expected_target_hlp_live: vault.hlp_live_reserve(target_asset),
            expected_borrowed_hlp_live: vault.hlp_live_reserve(borrowed_asset),
            debt,
            interest_due,
            collateral_value_in_borrowed,
            insurance_request,
        })
    }
}

/// Rebuilds the exact one-sided hLP endpoint around an already materialized
/// ordinary reserve point. Liquidation uses this after a socialized reserve
/// haircut so the loss cannot be overwritten by the pre-loss swap plan.
pub(crate) fn prepare_explicit_hlp_transition_at_current_state(market: &Market) -> Result<ExplicitHlpTransition> {
    prepare_explicit_hlp_transition_from_end(market, market.integrated_curve_state_nad()?, true, false)
}

fn prepare_explicit_hlp_transition_from_end(
    market: &Market,
    mut end: IntegratedCurveState,
    preserve_current_ordinary_reserves: bool,
    certify_proportional_claim: bool,
) -> Result<ExplicitHlpTransition> {
    require_eq!(
        market.base_side.shares.ylp_supply,
        market.quote_side.shares.ylp_supply,
        ErrorCode::BrokenInvariant
    );
    require_eq!(
        market.base_side.reserves.total_hlp_backing_inventory()?,
        0,
        ErrorCode::BrokenInvariant
    );
    require_eq!(
        market.quote_side.reserves.total_hlp_backing_inventory()?,
        0,
        ErrorCode::BrokenInvariant
    );
    let start_supply = market.base_side.shares.ylp_supply;
    let ordinary_supply = start_supply
        .checked_sub(market.base_hlp_vault.ylp_shares)
        .and_then(|value| value.checked_sub(market.quote_hlp_vault.ylp_shares))
        .ok_or(ErrorCode::BrokenInvariant)?;
    let endpoint = reconstruct_hlp_endpoint(end)?;
    end.base_hlp_quote_debt = endpoint.base_hlp_quote_debt;
    end.quote_hlp_base_debt = endpoint.quote_hlp_base_debt;
    let ownership = reconstruct_hlp_ownership(
        ordinary_supply,
        end.ordinary_base,
        end.ordinary_quote,
        end.base_hlp_equity,
        end.quote_hlp_equity,
    )?;

    let current_base_debt = u64::try_from(Debt::shares_to_debt(
        market.base_hlp_vault.debt_shares,
        market.debt.quote_borrow_index_nad,
    )?)
    .map_err(|_| ErrorCode::DebtMathOverflow)?;
    let current_quote_debt = u64::try_from(Debt::shares_to_debt(
        market.quote_hlp_vault.debt_shares,
        market.debt.base_borrow_index_nad,
    )?)
    .map_err(|_| ErrorCode::DebtMathOverflow)?;
    let base_interest_paid = current_base_debt.saturating_sub(market.base_hlp_vault.debt_principal);
    let quote_interest_paid = current_quote_debt.saturating_sub(market.quote_hlp_vault.debt_principal);

    let (base_non_debt_reserve, quote_non_debt_reserve) = if preserve_current_ordinary_reserves {
        // Changing hLP ownership/debt around an already materialized reserve
        // point must not create or destroy ordinary curve reserves. Preserve
        // the exact raw identity and change only hLP funding debt.
        let old_base_hlp_live =
            u64::try_from(market.hlp_live_reserve(MarketAsset::Base)?).map_err(|_| ErrorCode::ReserveOverflow)?;
        let old_quote_hlp_live =
            u64::try_from(market.hlp_live_reserve(MarketAsset::Quote)?).map_err(|_| ErrorCode::ReserveOverflow)?;
        (
            market
                .base_side
                .reserves
                .live_reserve
                .checked_sub(old_base_hlp_live)
                .and_then(|value| value.checked_sub(quote_interest_paid))
                .and_then(|value| value.checked_sub(quote_interest_paid))
                .ok_or(ErrorCode::ReserveUnderflow)?,
            market
                .quote_side
                .reserves
                .live_reserve
                .checked_sub(old_quote_hlp_live)
                .and_then(|value| value.checked_sub(base_interest_paid))
                .and_then(|value| value.checked_sub(base_interest_paid))
                .ok_or(ErrorCode::ReserveUnderflow)?,
        )
    } else {
        // A quoted swap has not materialized its endpoint yet, so reconstruct
        // the exact post-swap live reserves from the quoted ordinary point.
        let ordinary_base = denormalize_from_nad_floor(end.ordinary_base, market.base_side.asset_decimals)?;
        let ordinary_quote = denormalize_from_nad_floor(end.ordinary_quote, market.quote_side.asset_decimals)?;
        let base_equity = denormalize_from_nad_floor(end.base_hlp_equity, market.base_side.asset_decimals)?;
        let quote_equity = denormalize_from_nad_floor(end.quote_hlp_equity, market.quote_side.asset_decimals)?;
        (
            ordinary_base
                .checked_add(base_equity)
                .and_then(|value| value.checked_sub(quote_interest_paid))
                .and_then(|value| value.checked_sub(quote_interest_paid))
                .ok_or(ErrorCode::ReserveUnderflow)?,
            ordinary_quote
                .checked_add(quote_equity)
                .and_then(|value| value.checked_sub(base_interest_paid))
                .and_then(|value| value.checked_sub(base_interest_paid))
                .ok_or(ErrorCode::ReserveUnderflow)?,
        )
    };
    let ((final_base_debt_shares, final_base_debt), (final_quote_debt_shares, final_quote_debt)) =
        if !certify_proportional_claim {
            // The ordinary path starts on the canonical hedge and the direct
            // algebraic endpoint is already atom-tight. Avoid the additional
            // proportional-claim certificate on every healthy swap.
            let base_debt = denormalize_from_nad_floor(endpoint.base_hlp_quote_debt, market.quote_side.asset_decimals)?;
            let quote_debt = denormalize_from_nad_floor(endpoint.quote_hlp_base_debt, market.base_side.asset_decimals)?;
            let base_shares = if base_debt == 0 {
                0
            } else {
                Debt::debt_to_shares(base_debt, market.debt.quote_borrow_index_nad)?
            };
            let quote_shares = if quote_debt == 0 {
                0
            } else {
                Debt::debt_to_shares(quote_debt, market.debt.base_borrow_index_nad)?
            };
            (
                (
                    base_shares,
                    u64::try_from(Debt::shares_to_debt(base_shares, market.debt.quote_borrow_index_nad)?)
                        .map_err(|_| ErrorCode::DebtMathOverflow)?,
                ),
                (
                    quote_shares,
                    u64::try_from(Debt::shares_to_debt(quote_shares, market.debt.base_borrow_index_nad)?)
                        .map_err(|_| ErrorCode::DebtMathOverflow)?,
                ),
            )
        } else {
            (
                canonical_debt_for_proportional_claim(
                    quote_non_debt_reserve,
                    ownership.base_hlp_ylp_shares,
                    ownership.total_ylp_supply,
                    market.debt.quote_borrow_index_nad,
                )?,
                canonical_debt_for_proportional_claim(
                    base_non_debt_reserve,
                    ownership.quote_hlp_ylp_shares,
                    ownership.total_ylp_supply,
                    market.debt.base_borrow_index_nad,
                )?,
            )
        };
    let final_base_live_reserve = base_non_debt_reserve
        .checked_add(final_quote_debt)
        .ok_or(ErrorCode::ReserveOverflow)?;
    let final_quote_live_reserve = quote_non_debt_reserve
        .checked_add(final_base_debt)
        .ok_or(ErrorCode::ReserveOverflow)?;

    let base_receipt = explicit_hlp_receipt(
        MarketAsset::Base,
        market.base_hlp_vault.ylp_shares,
        ownership.base_hlp_ylp_shares,
        current_base_debt,
        final_base_debt,
        base_interest_paid,
        end.base_hlp_equity,
        start_supply,
    )?;
    let quote_receipt = explicit_hlp_receipt(
        MarketAsset::Quote,
        market.quote_hlp_vault.ylp_shares,
        ownership.quote_hlp_ylp_shares,
        current_quote_debt,
        final_quote_debt,
        quote_interest_paid,
        end.quote_hlp_equity,
        start_supply,
    )?;

    Ok(ExplicitHlpTransition {
        expected_curve_revision: market.curve_revision,
        expected_ylp_supply: start_supply,
        expected_base_ylp_shares: market.base_hlp_vault.ylp_shares,
        expected_quote_ylp_shares: market.quote_hlp_vault.ylp_shares,
        expected_base_debt_shares: market.base_hlp_vault.debt_shares,
        expected_quote_debt_shares: market.quote_hlp_vault.debt_shares,
        expected_base_debt_principal: market.base_hlp_vault.debt_principal,
        expected_quote_debt_principal: market.quote_hlp_vault.debt_principal,
        final_ylp_supply: ownership.total_ylp_supply,
        final_base_ylp_shares: ownership.base_hlp_ylp_shares,
        final_quote_ylp_shares: ownership.quote_hlp_ylp_shares,
        final_base_debt_shares,
        final_quote_debt_shares,
        final_base_debt,
        final_quote_debt,
        final_base_live_reserve,
        final_quote_live_reserve,
        base_interest_paid,
        quote_interest_paid,
        base_receipt,
        quote_receipt,
    })
}

impl ExplicitHlpTransition {
    pub(crate) fn debt_deltas(&self) -> (i128, i128) {
        (self.base_receipt.debt_delta, self.quote_receipt.debt_delta)
    }

    pub(crate) fn interest_cash_floors(&self, asset_in: MarketAsset, amount_out: u64) -> SwapCashFloors {
        let mut floors = SwapCashFloors::default();
        floors.set(
            MarketAsset::Base,
            self.quote_interest_paid
                .saturating_add(if asset_in == MarketAsset::Quote { amount_out } else { 0 }),
        );
        floors.set(
            MarketAsset::Quote,
            self.base_interest_paid
                .saturating_add(if asset_in == MarketAsset::Base { amount_out } else { 0 }),
        );
        floors
    }

    pub(crate) fn consume(&self, market: &mut Market) -> Result<(HlpRebalanceReceipt, HlpRebalanceReceipt)> {
        require_eq!(
            market.curve_revision,
            self.expected_curve_revision,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            market.base_side.shares.ylp_supply,
            self.expected_ylp_supply,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            market.quote_side.shares.ylp_supply,
            self.expected_ylp_supply,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            market.base_hlp_vault.ylp_shares,
            self.expected_base_ylp_shares,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            market.quote_hlp_vault.ylp_shares,
            self.expected_quote_ylp_shares,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            market.base_hlp_vault.debt_shares,
            self.expected_base_debt_shares,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            market.quote_hlp_vault.debt_shares,
            self.expected_quote_debt_shares,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            market.base_hlp_vault.debt_principal,
            self.expected_base_debt_principal,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            market.quote_hlp_vault.debt_principal,
            self.expected_quote_debt_principal,
            ErrorCode::BrokenInvariant
        );

        if self.quote_interest_paid > 0 {
            debit_cash_for_hlp_interest(&mut market.base_side, self.quote_interest_paid)?;
        }
        if self.base_interest_paid > 0 {
            debit_cash_for_hlp_interest(&mut market.quote_side, self.base_interest_paid)?;
        }
        let old_base_hlp_live = market.hlp_live_reserve(MarketAsset::Base)?;
        let old_quote_hlp_live = market.hlp_live_reserve(MarketAsset::Quote)?;
        let identity_base_live = (market.base_side.reserves.live_reserve as u128)
            .checked_sub(old_base_hlp_live)
            .and_then(|value| value.checked_add(self.final_quote_debt as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let identity_quote_live = (market.quote_side.reserves.live_reserve as u128)
            .checked_sub(old_quote_hlp_live)
            .and_then(|value| value.checked_add(self.final_base_debt as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        // The quoted ordinary output, each target equity, and the reconstructed
        // opposite debt are independently floored to raw atoms. Their summed
        // reserve identity can therefore differ from the raw cash transition
        // by at most three atoms, without leaving any debt/claim mismatch.
        const MAX_EXPLICIT_HLP_LIVE_DUST_ATOMS: u128 = 3;
        require!(
            identity_base_live.abs_diff(self.final_base_live_reserve as u128) <= MAX_EXPLICIT_HLP_LIVE_DUST_ATOMS
                && identity_quote_live.abs_diff(self.final_quote_live_reserve as u128)
                    <= MAX_EXPLICIT_HLP_LIVE_DUST_ATOMS,
            ErrorCode::BrokenInvariant
        );
        market.base_side.shares.ylp_supply = self.final_ylp_supply;
        market.quote_side.shares.ylp_supply = self.final_ylp_supply;
        market.base_hlp_vault.ylp_shares = self.final_base_ylp_shares;
        market.quote_hlp_vault.ylp_shares = self.final_quote_ylp_shares;
        market.base_hlp_vault.debt_shares = self.final_base_debt_shares;
        market.quote_hlp_vault.debt_shares = self.final_quote_debt_shares;
        market.base_hlp_vault.debt_principal = self.final_base_debt;
        market.quote_hlp_vault.debt_principal = self.final_quote_debt;
        market.base_hlp_vault.base_hlp_live_reserve = 0;
        market.base_hlp_vault.quote_hlp_live_reserve = self.final_base_debt;
        market.quote_hlp_vault.base_hlp_live_reserve = self.final_quote_debt;
        market.quote_hlp_vault.quote_hlp_live_reserve = 0;
        market.base_side.reserves.live_reserve =
            u64::try_from(identity_base_live).map_err(|_| ErrorCode::ReserveOverflow)?;
        market.quote_side.reserves.live_reserve =
            u64::try_from(identity_quote_live).map_err(|_| ErrorCode::ReserveOverflow)?;
        market.base_hlp_vault.last_nav_nad = self.base_receipt.nav_nad;
        market.quote_hlp_vault.last_nav_nad = self.quote_receipt.nav_nad;
        market.base_hlp_vault.residual_exposure = 0;
        market.quote_hlp_vault.residual_exposure = 0;
        market.assert_virtual_reserve_invariant(MarketAsset::Base)?;
        market.assert_virtual_reserve_invariant(MarketAsset::Quote)?;
        Ok((self.base_receipt, self.quote_receipt))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HlpTrackingReference {
    pub(crate) principal_nav_nad: i128,
    pub(crate) loss_budget_nad: u128,
    /// Frozen public-borrow interest only; never an hLP-funding claim.
    pub(crate) base_unrealized_interest: u64,
    /// Frozen public-borrow interest only; never an hLP-funding claim.
    pub(crate) quote_unrealized_interest: u64,
    pub(crate) start_ylp_shares: u64,
    pub(crate) start_ylp_supply: u64,
}

impl Market {
    pub fn deposit_single_sided(
        &mut self,
        target_asset: MarketAsset,
        deposit_amount: u64,
        min_hlp_amount: u64,
    ) -> Result<SingleSidedLiquidityReceipt> {
        let market = self;
        require!(deposit_amount > 0, ErrorCode::AmountZero);
        require_hlp_settlement_available(market, target_asset)?;
        let pre_prices = current_hlp_curve_prices(market)?;
        let pre_entry = current_hlp_entry_state_with_prices(market, target_asset, pre_prices)?;
        let (hlp_supply_before, settlement_reference_before) = match target_asset {
            MarketAsset::Base => (
                market.base_hlp_vault.hlp_supply,
                market.base_hlp_vault.cached_settlement_price_nad,
            ),
            MarketAsset::Quote => (
                market.quote_hlp_vault.hlp_supply,
                market.quote_hlp_vault.cached_settlement_price_nad,
            ),
        };
        require!(
            hlp_supply_before == 0 || pre_entry.disposition.admits_entry(),
            ErrorCode::HlpSettlementUnavailable
        );
        // An hLP owns ordinary yLP shares. Its two deposit legs must therefore
        // follow the executable reserve claims, not a 50/50 marginal-value
        // split (the two coincide only for CPMM).
        let target_reserve = market.curve_reserve(target_asset)?;
        let opposite_reserve = market.curve_reserve(target_asset.opposite())?;
        require!(
            target_reserve > 0 && opposite_reserve > 0,
            ErrorCode::InsufficientLiquidity
        );
        let borrowed_amount = u64::try_from(
            (deposit_amount as u128)
                .checked_mul(opposite_reserve as u128)
                .and_then(|value| value.checked_div(target_reserve as u128))
                .ok_or(ErrorCode::MarketMathOverflow)?,
        )
        .map_err(|_| ErrorCode::MarketMathOverflow)?;
        require!(borrowed_amount > 0, ErrorCode::InsufficientLiquidity);
        let debt_shares = require_hlp_borrow_headroom(market, target_asset.opposite(), borrowed_amount)?;
        checkpoint_hlp_yield_from_ylp(market, target_asset)?;

        let (ylp_amount, hlp_amount, hlp_supply, post_prices) = match target_asset {
            MarketAsset::Base => {
                let hlp_supply_before = market.base_hlp_vault.hlp_supply;
                let nav_before_nad = if hlp_supply_before == 0 {
                    0
                } else if market.base_hlp_vault.last_nav_nad > 0 {
                    market.base_hlp_vault.last_nav_nad
                } else {
                    hlp_nav_nad(market, MarketAsset::Base)?
                };
                let ylp_amount = ylp_for_live_reserve_deposit(market, deposit_amount, borrowed_amount)?;
                require!(ylp_amount > 0, ErrorCode::SlippageExceeded);
                market.base_side.credit_reserve(deposit_amount, true)?;
                market.quote_side.credit_reserve(borrowed_amount, false)?;
                market
                    .base_hlp_vault
                    .credit_hlp_live_reserve(MarketAsset::Quote, borrowed_amount)?;
                market.base_side.shares.mint(ylp_amount)?;
                market.quote_side.shares.mint(ylp_amount)?;
                market.base_hlp_vault.add_debt_shares(debt_shares)?;
                market.base_hlp_vault.add_debt_principal(borrowed_amount)?;
                market.base_hlp_vault.credit_ylp(ylp_amount)?;
                let current_prices = current_hlp_curve_prices(market)?;
                let current_nav_nad = hlp_nav_nad_with_prices(market, MarketAsset::Base, current_prices)?;
                let hlp_amount = if hlp_supply_before == 0 {
                    deposit_amount
                } else {
                    let delta_nav_nad = current_nav_nad
                        .checked_sub(nav_before_nad)
                        .ok_or(ErrorCode::MarketMathOverflow)?;
                    hlp_shares_for_delta_nav(
                        delta_nav_nad,
                        nav_before_nad.max(market.base_hlp_vault.last_nav_nad),
                        hlp_supply_before,
                    )?
                };
                market.base_hlp_vault.mint_hlp(hlp_amount)?;
                market.base_hlp_vault.last_nav_nad = current_nav_nad;
                (ylp_amount, hlp_amount, market.base_hlp_vault.hlp_supply, current_prices)
            }
            MarketAsset::Quote => {
                let hlp_supply_before = market.quote_hlp_vault.hlp_supply;
                let nav_before_nad = if hlp_supply_before == 0 {
                    0
                } else if market.quote_hlp_vault.last_nav_nad > 0 {
                    market.quote_hlp_vault.last_nav_nad
                } else {
                    hlp_nav_nad(market, MarketAsset::Quote)?
                };
                let ylp_amount = ylp_for_live_reserve_deposit(market, borrowed_amount, deposit_amount)?;
                require!(ylp_amount > 0, ErrorCode::SlippageExceeded);
                market.base_side.credit_reserve(borrowed_amount, false)?;
                market.quote_side.credit_reserve(deposit_amount, true)?;
                market
                    .quote_hlp_vault
                    .credit_hlp_live_reserve(MarketAsset::Base, borrowed_amount)?;
                market.base_side.shares.mint(ylp_amount)?;
                market.quote_side.shares.mint(ylp_amount)?;
                market.quote_hlp_vault.add_debt_shares(debt_shares)?;
                market.quote_hlp_vault.add_debt_principal(borrowed_amount)?;
                market.quote_hlp_vault.credit_ylp(ylp_amount)?;
                let current_prices = current_hlp_curve_prices(market)?;
                let current_nav_nad = hlp_nav_nad_with_prices(market, MarketAsset::Quote, current_prices)?;
                let hlp_amount = if hlp_supply_before == 0 {
                    deposit_amount
                } else {
                    let delta_nav_nad = current_nav_nad
                        .checked_sub(nav_before_nad)
                        .ok_or(ErrorCode::MarketMathOverflow)?;
                    hlp_shares_for_delta_nav(
                        delta_nav_nad,
                        nav_before_nad.max(market.quote_hlp_vault.last_nav_nad),
                        hlp_supply_before,
                    )?
                };
                market.quote_hlp_vault.mint_hlp(hlp_amount)?;
                market.quote_hlp_vault.last_nav_nad = current_nav_nad;
                (
                    ylp_amount,
                    hlp_amount,
                    market.quote_hlp_vault.hlp_supply,
                    current_prices,
                )
            }
        };
        require_gte!(hlp_amount, min_hlp_amount, ErrorCode::SlippageExceeded);
        let post_entry = current_hlp_entry_state_with_prices(market, target_asset, post_prices)?;
        require!(
            post_entry.disposition.admits_entry(),
            ErrorCode::HlpSettlementUnavailable
        );
        if hlp_supply_before > 0 {
            match pre_entry.disposition {
                HlpEntryDisposition::Settled => require!(
                    post_entry.disposition == HlpEntryDisposition::Settled,
                    ErrorCode::HlpSettlementUnavailable
                ),
                HlpEntryDisposition::ControllerGranularityLimited => {
                    let pre_residual = pre_entry.residual_exposure.unsigned_abs();
                    let post_residual = post_entry.residual_exposure.unsigned_abs();
                    require!(
                        post_entry.nav_nad > pre_entry.nav_nad && hlp_supply > hlp_supply_before,
                        ErrorCode::HlpSettlementUnavailable
                    );
                    require!(post_residual <= pre_residual, ErrorCode::HlpSettlementUnavailable);
                    require!(
                        post_entry.residual_exposure == 0
                            || post_entry.residual_exposure.is_negative() == pre_entry.residual_exposure.is_negative(),
                        ErrorCode::HlpSettlementUnavailable
                    );
                    require!(
                        ratio_lte_full_width(post_residual, post_entry.nav_nad, pre_residual, pre_entry.nav_nad,)?
                            && ratio_lte_full_width(
                                post_residual,
                                hlp_supply as u128,
                                pre_residual,
                                hlp_supply_before as u128,
                            )?
                            && ratio_lte_full_width(
                                pre_entry.nav_nad,
                                hlp_supply_before as u128,
                                post_entry.nav_nad,
                                hlp_supply as u128,
                            )?,
                        ErrorCode::HlpSettlementUnavailable
                    );
                }
                _ => return err!(ErrorCode::HlpSettlementUnavailable),
            }
        }
        let vault = match target_asset {
            MarketAsset::Base => &mut market.base_hlp_vault,
            MarketAsset::Quote => &mut market.quote_hlp_vault,
        };
        vault.last_nav_nad = post_entry.nav_nad;
        vault.residual_exposure = post_entry.residual_exposure;
        vault.cached_settlement_price_nad = if hlp_supply_before == 0 || post_entry.residual_exposure == 0 {
            post_prices.for_asset(target_asset)
        } else {
            settlement_reference_before
        };
        let health = market.market_health()?;
        market.assert_market_health_snapshot(&health)?;
        market.assert_virtual_reserve_invariant(MarketAsset::Base)?;
        market.assert_virtual_reserve_invariant(MarketAsset::Quote)?;
        Ok(SingleSidedLiquidityReceipt {
            deposit_amount,
            borrowed_amount,
            ylp_amount,
            hlp_amount,
            hlp_supply,
            target_amount_out: 0,
            debt_repaid: 0,
            interest_paid: 0,
        })
    }
    pub fn withdraw_single_sided(
        &mut self,
        target_asset: MarketAsset,
        hlp_amount: u64,
    ) -> Result<SingleSidedLiquidityReceipt> {
        let market = self;
        require!(hlp_amount > 0, ErrorCode::AmountZero);
        if market.hlp_terminally_closed(target_asset) {
            // Terminal hLP tokens represent no remaining principal. Burning
            // them must not depend on a later AMM/controller checkpoint;
            // already-accrued fee claims live in the separate yield account.
            let vault = match target_asset {
                MarketAsset::Base => &mut market.base_hlp_vault,
                MarketAsset::Quote => &mut market.quote_hlp_vault,
            };
            vault.burn_hlp(hlp_amount)?;
            if vault.hlp_supply == 0 {
                vault.last_nav_nad = 0;
                vault.cached_settlement_price_nad = 0;
            }
            return Ok(SingleSidedLiquidityReceipt {
                hlp_amount,
                hlp_supply: vault.hlp_supply,
                ..SingleSidedLiquidityReceipt::default()
            });
        }
        let residual_exposure = match target_asset {
            MarketAsset::Base => market.base_hlp_vault.residual_exposure,
            MarketAsset::Quote => market.quote_hlp_vault.residual_exposure,
        };
        let settlement_reference_before = match target_asset {
            MarketAsset::Base => market.base_hlp_vault.cached_settlement_price_nad,
            MarketAsset::Quote => market.quote_hlp_vault.cached_settlement_price_nad,
        };
        // Preserve the ordinary stale-price guard. An explicitly recorded
        // partial controller residual is different: an exit reduces or fully
        // retires that hedge, so trapping it behind the prior reference would
        // create a liveness failure.
        if residual_exposure == 0 {
            require_hlp_settlement_available(market, target_asset)?;
        }
        checkpoint_hlp_yield_from_ylp(market, target_asset)?;
        let receipt = match target_asset {
            MarketAsset::Base => {
                let supply = market.base_hlp_vault.hlp_supply;
                require_gte!(supply, hlp_amount, ErrorCode::InsufficientBalance);
                let ylp_amount = proportional(market.base_hlp_vault.ylp_shares, hlp_amount, supply)?;
                let quote_debt_shares = proportional_u128(market.base_hlp_vault.debt_shares, hlp_amount, supply)?;
                let base_out = ylp_live_underlying_amount(market, MarketAsset::Base, ylp_amount)?;
                let quote_redeemed = ylp_live_underlying_amount(market, MarketAsset::Quote, ylp_amount)?;
                let debt_repaid = Debt::aggregate_debt_reduction_for_shares(
                    market.base_hlp_vault.debt_shares,
                    quote_debt_shares,
                    market.debt.quote_borrow_index_nad,
                )?;
                let base_hlp_live_debit =
                    proportional(market.base_hlp_vault.base_hlp_live_reserve, hlp_amount, supply)?;
                let quote_hlp_live_debit =
                    proportional(market.base_hlp_vault.quote_hlp_live_reserve, hlp_amount, supply)?;
                let base_out =
                    settled_close_target_amount(market, MarketAsset::Base, base_out, quote_redeemed, debt_repaid)?;
                release_hlp_backing_inventory(market, MarketAsset::Base, hlp_amount, supply)?;
                let debt_clearance = market
                    .base_hlp_vault
                    .clear_debt_repay(quote_debt_shares, market.debt.quote_borrow_index_nad)?;
                let interest_paid = debt_clearance.interest_paid;
                market.base_side.debit_reserve(base_out, true)?;
                debit_hlp_live_reserve(market, MarketAsset::Base, MarketAsset::Base, base_hlp_live_debit)?;
                debit_hlp_live_reserve(market, MarketAsset::Base, MarketAsset::Quote, quote_hlp_live_debit)?;
                market.base_side.shares.burn(ylp_amount)?;
                market.quote_side.shares.burn(ylp_amount)?;
                market.base_side.assert_share_backing()?;
                market.quote_side.assert_share_backing()?;
                market.base_hlp_vault.debit_ylp(ylp_amount)?;
                debit_cash_for_hlp_interest(&mut market.quote_side, interest_paid)?;
                market.base_hlp_vault.burn_hlp(hlp_amount)?;
                if market.base_hlp_vault.hlp_supply == 0 {
                    require_eq!(
                        market.base_side.reserves.base_hlp_backing_inventory,
                        0,
                        ErrorCode::BrokenInvariant
                    );
                    require_eq!(
                        market.quote_side.reserves.base_hlp_backing_inventory,
                        0,
                        ErrorCode::BrokenInvariant
                    );
                    market.base_hlp_vault.last_nav_nad = 0;
                    market.base_hlp_vault.cached_settlement_price_nad = 0;
                } else {
                    let current_prices = current_hlp_curve_prices(market)?;
                    market.base_hlp_vault.last_nav_nad =
                        hlp_nav_nad_with_prices(market, MarketAsset::Base, current_prices)?;
                    if residual_exposure == 0 {
                        market.base_hlp_vault.cached_settlement_price_nad = current_prices.for_asset(MarketAsset::Base);
                    } else {
                        market.base_hlp_vault.cached_settlement_price_nad = settlement_reference_before;
                    }
                }
                SingleSidedLiquidityReceipt {
                    hlp_amount,
                    ylp_amount,
                    hlp_supply: market.base_hlp_vault.hlp_supply,
                    target_amount_out: base_out,
                    debt_repaid: debt_clearance.debt_reduced,
                    interest_paid,
                    ..SingleSidedLiquidityReceipt::default()
                }
            }
            MarketAsset::Quote => {
                let supply = market.quote_hlp_vault.hlp_supply;
                require_gte!(supply, hlp_amount, ErrorCode::InsufficientBalance);
                let ylp_amount = proportional(market.quote_hlp_vault.ylp_shares, hlp_amount, supply)?;
                let base_debt_shares = proportional_u128(market.quote_hlp_vault.debt_shares, hlp_amount, supply)?;
                let quote_out = ylp_live_underlying_amount(market, MarketAsset::Quote, ylp_amount)?;
                let base_redeemed = ylp_live_underlying_amount(market, MarketAsset::Base, ylp_amount)?;
                let debt_repaid = Debt::aggregate_debt_reduction_for_shares(
                    market.quote_hlp_vault.debt_shares,
                    base_debt_shares,
                    market.debt.base_borrow_index_nad,
                )?;
                let base_hlp_live_debit =
                    proportional(market.quote_hlp_vault.base_hlp_live_reserve, hlp_amount, supply)?;
                let quote_hlp_live_debit =
                    proportional(market.quote_hlp_vault.quote_hlp_live_reserve, hlp_amount, supply)?;
                let quote_out =
                    settled_close_target_amount(market, MarketAsset::Quote, quote_out, base_redeemed, debt_repaid)?;
                release_hlp_backing_inventory(market, MarketAsset::Quote, hlp_amount, supply)?;
                let debt_clearance = market
                    .quote_hlp_vault
                    .clear_debt_repay(base_debt_shares, market.debt.base_borrow_index_nad)?;
                let interest_paid = debt_clearance.interest_paid;
                market.quote_side.debit_reserve(quote_out, true)?;
                debit_hlp_live_reserve(market, MarketAsset::Quote, MarketAsset::Quote, quote_hlp_live_debit)?;
                debit_hlp_live_reserve(market, MarketAsset::Quote, MarketAsset::Base, base_hlp_live_debit)?;
                market.base_side.shares.burn(ylp_amount)?;
                market.quote_side.shares.burn(ylp_amount)?;
                market.base_side.assert_share_backing()?;
                market.quote_side.assert_share_backing()?;
                market.quote_hlp_vault.debit_ylp(ylp_amount)?;
                debit_cash_for_hlp_interest(&mut market.base_side, interest_paid)?;
                market.quote_hlp_vault.burn_hlp(hlp_amount)?;
                if market.quote_hlp_vault.hlp_supply == 0 {
                    require_eq!(
                        market.base_side.reserves.quote_hlp_backing_inventory,
                        0,
                        ErrorCode::BrokenInvariant
                    );
                    require_eq!(
                        market.quote_side.reserves.quote_hlp_backing_inventory,
                        0,
                        ErrorCode::BrokenInvariant
                    );
                    market.quote_hlp_vault.last_nav_nad = 0;
                    market.quote_hlp_vault.cached_settlement_price_nad = 0;
                } else {
                    let current_prices = current_hlp_curve_prices(market)?;
                    market.quote_hlp_vault.last_nav_nad =
                        hlp_nav_nad_with_prices(market, MarketAsset::Quote, current_prices)?;
                    if residual_exposure == 0 {
                        market.quote_hlp_vault.cached_settlement_price_nad =
                            current_prices.for_asset(MarketAsset::Quote);
                    } else {
                        market.quote_hlp_vault.cached_settlement_price_nad = settlement_reference_before;
                    }
                }
                SingleSidedLiquidityReceipt {
                    hlp_amount,
                    ylp_amount,
                    hlp_supply: market.quote_hlp_vault.hlp_supply,
                    target_amount_out: quote_out,
                    debt_repaid: debt_clearance.debt_reduced,
                    interest_paid,
                    ..SingleSidedLiquidityReceipt::default()
                }
            }
        };
        market.refresh_risk()?;
        let health = market.market_health()?;
        market.assert_market_health_snapshot(&health)?;
        market.assert_virtual_reserve_invariant(MarketAsset::Base)?;
        market.assert_virtual_reserve_invariant(MarketAsset::Quote)?;
        Ok(receipt)
    }
}

pub(crate) fn require_residual_hlp_swap_safe(
    market: &Market,
    target_asset: MarketAsset,
    start_prices: HlpCurvePrices,
    end_prices: HlpCurvePrices,
    residual_on_entry: bool,
) -> Result<()> {
    let vault = match target_asset {
        MarketAsset::Base => &market.base_hlp_vault,
        MarketAsset::Quote => &market.quote_hlp_vault,
    };
    // The settlement band is a recovery guard for an already-actionable
    // residual, not a pre-emptive trade-size limit on a settled hLP. A settled
    // vault is corrected from the actual post-trade state below. If that
    // maximum-safe correction leaves a residual, this unchanged settlement
    // reference then prevents later outward flow from compounding it.
    if vault.hlp_supply == 0
        || vault.cached_settlement_price_nad == 0
        || !residual_on_entry
        || vault.residual_exposure == 0
    {
        return Ok(());
    }
    let reference = vault.cached_settlement_price_nad;
    let start_divergence = absolute_difference(start_prices.for_asset(target_asset), reference);
    let end_divergence = absolute_difference(end_prices.for_asset(target_asset), reference);
    let max_divergence = reference
        .checked_mul(market.config.settlement_divergence_bps as u128)
        .and_then(|value| value.checked_div(crate::constants::BPS_DENOMINATOR as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    // Once outside the normal band, only a strictly restoring trade remains
    // executable. This avoids bricking recovery while preventing repeated
    // same-direction flow from accumulating unbounded stale hLP exposure.
    require!(
        end_divergence <= max_divergence || end_divergence < start_divergence,
        ErrorCode::HlpSettlementUnavailable
    );
    Ok(())
}

fn absolute_difference(first: u128, second: u128) -> u128 {
    first.max(second) - first.min(second)
}

pub(crate) fn checkpoint_pre_solve_fee_eligibility(market: &mut Market, receipt: &HlpRebalanceReceipt) -> Result<()> {
    if receipt.ylp_mint_amount == 0 && receipt.ylp_burn_amount == 0 {
        return Ok(());
    }
    checkpoint_hlp_yield_from_ylp_shares(
        market,
        receipt.target_asset,
        receipt.current_swap_fee_eligible_ylp_shares,
    )
}

pub(crate) fn combine_hlp_rebalance_receipts(
    pre: HlpRebalanceReceipt,
    post: HlpRebalanceReceipt,
) -> Result<HlpRebalanceReceipt> {
    require!(pre.target_asset == post.target_asset, ErrorCode::BrokenInvariant);
    let total_mint = pre
        .ylp_mint_amount
        .checked_add(post.ylp_mint_amount)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let total_burn = pre
        .ylp_burn_amount
        .checked_add(post.ylp_burn_amount)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let (ylp_mint_amount, ylp_burn_amount) = if total_mint >= total_burn {
        (total_mint - total_burn, 0)
    } else {
        (0, total_burn - total_mint)
    };
    Ok(HlpRebalanceReceipt {
        target_asset: pre.target_asset,
        ideal_delta: pre
            .ideal_delta
            .checked_add(post.ideal_delta)
            .ok_or(ErrorCode::MarketMathOverflow)?,
        executed_delta: pre
            .executed_delta
            .checked_add(post.executed_delta)
            .ok_or(ErrorCode::MarketMathOverflow)?,
        residual_exposure: post.residual_exposure,
        current_swap_fee_eligible_ylp_shares: 0,
        // Pre- and post-positioning have already changed state. Settle their
        // net token delta once so a direction reversal cannot issue both a
        // mint and a burn CPI for the same hLP side.
        ylp_mint_amount,
        ylp_burn_amount,
        debt_delta: pre
            .debt_delta
            .checked_add(post.debt_delta)
            .ok_or(ErrorCode::MarketMathOverflow)?,
        interest_paid: pre
            .interest_paid
            .checked_add(post.interest_paid)
            .ok_or(ErrorCode::MarketMathOverflow)?,
        nav_nad: post.nav_nad,
        tracking_start_nav_nad: pre.tracking_start_nav_nad,
        tracking_loss_budget_nad: pre.tracking_loss_budget_nad,
        tracking_base_unrealized_interest: pre.tracking_base_unrealized_interest,
        tracking_quote_unrealized_interest: pre.tracking_quote_unrealized_interest,
        tracking_start_ylp_shares: pre.tracking_start_ylp_shares,
        tracking_start_ylp_supply: pre.tracking_start_ylp_supply,
        tracking_retained_contribution_nad: pre.tracking_retained_contribution_nad,
        preposition_capacity_bound: pre.preposition_capacity_bound || post.preposition_capacity_bound,
    })
}

pub(crate) fn empty_hlp_rebalance_receipt(target_asset: MarketAsset) -> HlpRebalanceReceipt {
    HlpRebalanceReceipt {
        target_asset,
        ..HlpRebalanceReceipt::default()
    }
}

#[cfg(test)]
fn deposit_base_hlp(
    market: &mut Market,
    base_deposit: u64,
    quote_borrow: u64,
) -> Result<(u64, u64, u64, HlpCurvePrices)> {
    let debt_shares = require_hlp_borrow_headroom(market, MarketAsset::Quote, quote_borrow)?;
    let hlp_supply_before = market.base_hlp_vault.hlp_supply;
    let nav_before_nad = if hlp_supply_before == 0 {
        0
    } else if market.base_hlp_vault.last_nav_nad > 0 {
        market.base_hlp_vault.last_nav_nad
    } else {
        hlp_nav_nad(market, MarketAsset::Base)?
    };
    let ylp_amount = ylp_for_live_reserve_deposit(market, base_deposit, quote_borrow)?;
    require!(ylp_amount > 0, ErrorCode::SlippageExceeded);
    market.base_side.credit_reserve(base_deposit, true)?;
    market.quote_side.credit_reserve(quote_borrow, false)?;
    market
        .base_hlp_vault
        .credit_hlp_live_reserve(MarketAsset::Quote, quote_borrow)?;
    market.base_side.shares.mint(ylp_amount)?;
    market.quote_side.shares.mint(ylp_amount)?;
    market.base_hlp_vault.add_debt_shares(debt_shares)?;
    market.base_hlp_vault.add_debt_principal(quote_borrow)?;
    market.base_hlp_vault.credit_ylp(ylp_amount)?;
    let current_prices = current_hlp_curve_prices(market)?;
    let current_nav_nad = hlp_nav_nad_with_prices(market, MarketAsset::Base, current_prices)?;
    let hlp_amount = if hlp_supply_before == 0 {
        base_deposit
    } else {
        let delta_nav_nad = current_nav_nad
            .checked_sub(nav_before_nad)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        hlp_shares_for_delta_nav(
            delta_nav_nad,
            nav_before_nad.max(market.base_hlp_vault.last_nav_nad),
            hlp_supply_before,
        )?
    };
    market.base_hlp_vault.mint_hlp(hlp_amount)?;
    market.base_hlp_vault.last_nav_nad = current_nav_nad;
    Ok((ylp_amount, hlp_amount, market.base_hlp_vault.hlp_supply, current_prices))
}

#[cfg(test)]
fn deposit_quote_hlp(
    market: &mut Market,
    quote_deposit: u64,
    base_borrow: u64,
) -> Result<(u64, u64, u64, HlpCurvePrices)> {
    let debt_shares = require_hlp_borrow_headroom(market, MarketAsset::Base, base_borrow)?;
    let hlp_supply_before = market.quote_hlp_vault.hlp_supply;
    let nav_before_nad = if hlp_supply_before == 0 {
        0
    } else if market.quote_hlp_vault.last_nav_nad > 0 {
        market.quote_hlp_vault.last_nav_nad
    } else {
        hlp_nav_nad(market, MarketAsset::Quote)?
    };
    let ylp_amount = ylp_for_live_reserve_deposit(market, base_borrow, quote_deposit)?;
    require!(ylp_amount > 0, ErrorCode::SlippageExceeded);
    market.base_side.credit_reserve(base_borrow, false)?;
    market.quote_side.credit_reserve(quote_deposit, true)?;
    market
        .quote_hlp_vault
        .credit_hlp_live_reserve(MarketAsset::Base, base_borrow)?;
    market.base_side.shares.mint(ylp_amount)?;
    market.quote_side.shares.mint(ylp_amount)?;
    market.quote_hlp_vault.add_debt_shares(debt_shares)?;
    market.quote_hlp_vault.add_debt_principal(base_borrow)?;
    market.quote_hlp_vault.credit_ylp(ylp_amount)?;
    let current_prices = current_hlp_curve_prices(market)?;
    let current_nav_nad = hlp_nav_nad_with_prices(market, MarketAsset::Quote, current_prices)?;
    let hlp_amount = if hlp_supply_before == 0 {
        quote_deposit
    } else {
        let delta_nav_nad = current_nav_nad
            .checked_sub(nav_before_nad)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        hlp_shares_for_delta_nav(
            delta_nav_nad,
            nav_before_nad.max(market.quote_hlp_vault.last_nav_nad),
            hlp_supply_before,
        )?
    };
    market.quote_hlp_vault.mint_hlp(hlp_amount)?;
    market.quote_hlp_vault.last_nav_nad = current_nav_nad;
    Ok((
        ylp_amount,
        hlp_amount,
        market.quote_hlp_vault.hlp_supply,
        current_prices,
    ))
}

fn debit_cash_for_hlp_interest(borrowed_side: &mut crate::state::MarketSide, interest_paid: u64) -> Result<()> {
    if interest_paid == 0 {
        return Ok(());
    }
    borrowed_side.reserves.live_reserve = borrowed_side
        .reserves
        .live_reserve
        .checked_sub(interest_paid)
        .ok_or(ErrorCode::ReserveUnderflow)?;
    borrowed_side.reserves.cash_reserve = borrowed_side
        .reserves
        .cash_reserve
        .checked_sub(interest_paid)
        .ok_or(ErrorCode::CashReserveUnderflow)?;
    Ok(())
}

fn debit_hlp_live_reserve(
    market: &mut Market,
    target_asset: MarketAsset,
    reserve_asset: MarketAsset,
    amount: u64,
) -> Result<()> {
    if amount == 0 {
        return Ok(());
    }
    market.side_mut(reserve_asset).debit_reserve(amount, false)?;
    match target_asset {
        MarketAsset::Base => market.base_hlp_vault.debit_hlp_live_reserve(reserve_asset, amount),
        MarketAsset::Quote => market.quote_hlp_vault.debit_hlp_live_reserve(reserve_asset, amount),
    }
}

fn release_hlp_backing_inventory(
    market: &mut Market,
    target_asset: MarketAsset,
    hlp_amount: u64,
    hlp_supply: u64,
) -> Result<()> {
    for reserve_asset in [MarketAsset::Base, MarketAsset::Quote] {
        let inventory = market.side(reserve_asset).reserves.hlp_backing_inventory(target_asset);
        let release = if hlp_amount == hlp_supply {
            inventory
        } else {
            proportional(inventory, hlp_amount, hlp_supply)?
        };
        if release > 0 {
            let side = market.side_mut(reserve_asset);
            side.reserves.debit_hlp_backing_inventory(target_asset, release)?;
            side.credit_reserve(release, true)?;
        }
    }
    Ok(())
}

fn settled_close_target_amount(
    market: &Market,
    target_asset: MarketAsset,
    target_redeemed: u64,
    borrowed_redeemed: u64,
    debt_repaid: u64,
) -> Result<u64> {
    if borrowed_redeemed == debt_repaid {
        return Ok(target_redeemed);
    }

    let state = market.integrated_curve_state_nad()?;
    let geometry = market
        .current_explicit_curve_geometry()?
        .ok_or(ErrorCode::BrokenInvariant)?;
    let start = ExplicitCurvePoint {
        base_reserve: state.ordinary_base,
        quote_reserve: state.ordinary_quote,
    };
    let borrowed_asset = target_asset.opposite();
    if borrowed_redeemed > debt_repaid {
        let surplus_borrowed = borrowed_redeemed
            .checked_sub(debt_repaid)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let surplus_nad = normalize_to_nad(surplus_borrowed as u128, market.side(borrowed_asset).asset_decimals)?;
        let direction = match borrowed_asset {
            MarketAsset::Base => ExplicitCurveDirection::BaseToQuote,
            MarketAsset::Quote => ExplicitCurveDirection::QuoteToBase,
        };
        let quote = geometry.quote_exact_in(start, surplus_nad, direction)?;
        let target_from_surplus =
            denormalize_from_nad_floor(quote.amount_out, market.side(target_asset).asset_decimals)?;
        return target_redeemed
            .checked_add(target_from_surplus)
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into());
    }

    let borrowed_shortfall = debt_repaid
        .checked_sub(borrowed_redeemed)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let shortfall_nad = normalize_to_nad(borrowed_shortfall as u128, market.side(borrowed_asset).asset_decimals)?;
    let direction = match target_asset {
        MarketAsset::Base => ExplicitCurveDirection::BaseToQuote,
        MarketAsset::Quote => ExplicitCurveDirection::QuoteToBase,
    };
    let quote = geometry.quote_exact_out(start, shortfall_nad, direction)?;
    let target_retained = denormalize_from_nad_ceil(quote.amount_in, market.side(target_asset).asset_decimals)?;
    require_gte!(target_redeemed, target_retained, ErrorCode::HlpSettlementUnavailable);
    target_redeemed
        .checked_sub(target_retained)
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HlpCurvePrices {
    base_in_quote_nad: u128,
    quote_in_base_nad: u128,
}

impl HlpCurvePrices {
    const fn for_asset(self, asset: MarketAsset) -> u128 {
        match asset {
            MarketAsset::Base => self.base_in_quote_nad,
            MarketAsset::Quote => self.quote_in_base_nad,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ProportionalRebalanceAmounts {
    target_leg_amount: u64,
    borrowed_leg_amount: u64,
    debt_amount: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct HlpValuation {
    ideal_delta: i128,
    nav_nad: u128,
    values: HlpInventoryValuesNad,
    prices: HlpCurvePrices,
    proportional_hedge_available: bool,
}

fn current_hlp_valuation_with_prices(
    market: &Market,
    target_asset: MarketAsset,
    prices: HlpCurvePrices,
) -> Result<HlpValuation> {
    let values = current_hlp_inventory_values_nad_with_prices(market, target_asset, prices)?;
    hlp_valuation_from_values(values, prices)
}

fn hlp_valuation_from_values(values: HlpInventoryValuesNad, prices: HlpCurvePrices) -> Result<HlpValuation> {
    let collateral = values
        .target_inventory_value_nad
        .checked_add(values.opposite_inventory_value_nad)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let debt = values.debt_value_nad;
    let nav_nad = collateral.saturating_sub(debt);
    let ideal_delta = if values.target_inventory_value_nad == 0 {
        hlp_opposite_exposure_nad(values)?
    } else {
        let exposure = hlp_opposite_exposure_nad(values)?;
        if exposure == 0 {
            0
        } else {
            let opposite_magnitude = mul_div_u128(
                exposure.unsigned_abs(),
                values.opposite_inventory_value_nad,
                values.target_inventory_value_nad,
            )?;
            let total_magnitude = exposure
                .unsigned_abs()
                .checked_add(opposite_magnitude)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            if exposure < 0 {
                if total_magnitude == 1_u128 << 127 {
                    i128::MIN
                } else {
                    i128::try_from(total_magnitude)
                        .map_err(|_| ErrorCode::MarketMathOverflow)?
                        .checked_neg()
                        .ok_or(ErrorCode::MarketMathOverflow)?
                }
            } else {
                i128::try_from(total_magnitude).map_err(|_| ErrorCode::MarketMathOverflow)?
            }
        }
    };
    Ok(HlpValuation {
        ideal_delta,
        nav_nad,
        values,
        prices,
        proportional_hedge_available: values.target_inventory_value_nad > 0,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HlpEntryDisposition {
    Settled,
    ControllerGranularityLimited,
    Actionable,
    CashConstrained,
    Unhedgeable,
}

impl HlpEntryDisposition {
    pub(crate) const fn admits_entry(self) -> bool {
        matches!(self, Self::Settled | Self::ControllerGranularityLimited)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HlpEntryState {
    pub(crate) disposition: HlpEntryDisposition,
    residual_exposure: i128,
    nav_nad: u128,
}

pub(crate) fn current_hlp_entry_state_with_prices(
    market: &Market,
    target_asset: MarketAsset,
    prices: HlpCurvePrices,
) -> Result<HlpEntryState> {
    let valuation = current_hlp_valuation_with_prices(market, target_asset, prices)?;
    let residual_exposure = recognized_hlp_residual_exposure(valuation.ideal_delta, valuation.nav_nad);
    let disposition = if residual_exposure == 0 {
        HlpEntryDisposition::Settled
    } else if !valuation.proportional_hedge_available || valuation.nav_nad == 0 {
        HlpEntryDisposition::Unhedgeable
    } else if residual_exposure > 0 {
        'entry: {
            let amounts = proportional_rebalance_amounts(market, target_asset, residual_exposure, valuation)?;
            if !complete_rebalance_amounts(amounts) {
                break 'entry HlpEntryDisposition::ControllerGranularityLimited;
            }
            let (base_leg, quote_leg) = match target_asset {
                MarketAsset::Base => (amounts.target_leg_amount, amounts.borrowed_leg_amount),
                MarketAsset::Quote => (amounts.borrowed_leg_amount, amounts.target_leg_amount),
            };
            if ylp_for_live_reserve_deposit(market, base_leg, quote_leg)? == 0 {
                break 'entry HlpEntryDisposition::ControllerGranularityLimited;
            }
            if market.hlp_funding_headroom(target_asset.opposite())? < amounts.debt_amount {
                break 'entry HlpEntryDisposition::CashConstrained;
            }
            HlpEntryDisposition::Actionable
        }
    } else {
        'entry: {
            let (borrow_index, debt_shares, debt_principal, vault_ylp) = match target_asset {
                MarketAsset::Base => (
                    market.debt.quote_borrow_index_nad,
                    market.base_hlp_vault.debt_shares,
                    market.base_hlp_vault.debt_principal,
                    market.base_hlp_vault.ylp_shares,
                ),
                MarketAsset::Quote => (
                    market.debt.base_borrow_index_nad,
                    market.quote_hlp_vault.debt_shares,
                    market.quote_hlp_vault.debt_principal,
                    market.quote_hlp_vault.ylp_shares,
                ),
            };
            if debt_shares == 0 || vault_ylp == 0 || valuation.values.debt_value_nad == 0 {
                break 'entry HlpEntryDisposition::Unhedgeable;
            }
            let collateral_value_nad = valuation
                .values
                .target_inventory_value_nad
                .checked_add(valuation.values.opposite_inventory_value_nad)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            let requested_delta_nad = residual_exposure.unsigned_abs();
            if collateral_value_nad.min(valuation.values.debt_value_nad) < requested_delta_nad
                || requested_delta_nad == 0
            {
                break 'entry HlpEntryDisposition::Unhedgeable;
            }
            let feasible_delta = -i128::try_from(requested_delta_nad).map_err(|_| ErrorCode::MarketMathOverflow)?;
            let amounts = proportional_rebalance_amounts(market, target_asset, feasible_delta, valuation)?;
            if !complete_rebalance_amounts(amounts) {
                break 'entry HlpEntryDisposition::ControllerGranularityLimited;
            }
            let (base_leg, quote_leg) = match target_asset {
                MarketAsset::Base => (amounts.target_leg_amount, amounts.borrowed_leg_amount),
                MarketAsset::Quote => (amounts.borrowed_leg_amount, amounts.target_leg_amount),
            };
            let base_burn = ylp_shares_for_live_reserve_amount(market, MarketAsset::Base, base_leg)?;
            let quote_burn = ylp_shares_for_live_reserve_amount(market, MarketAsset::Quote, quote_leg)?;
            let ylp_burn = base_burn.min(quote_burn).min(vault_ylp);
            if ylp_burn == 0 {
                break 'entry HlpEntryDisposition::ControllerGranularityLimited;
            }
            let base_out = ylp_live_underlying_amount(market, MarketAsset::Base, ylp_burn)?;
            let quote_out = ylp_live_underlying_amount(market, MarketAsset::Quote, ylp_burn)?;
            if base_out == 0 || quote_out == 0 {
                break 'entry HlpEntryDisposition::ControllerGranularityLimited;
            }
            let (target_out, borrowed_out) = match target_asset {
                MarketAsset::Base => (base_out, quote_out),
                MarketAsset::Quote => (quote_out, base_out),
            };
            let borrowed_asset = target_asset.opposite();
            let removed_value_nad = asset_value_in_target_nad_with_prices(
                market,
                valuation.prices,
                target_asset,
                target_out,
                target_asset,
            )?
            .checked_add(asset_value_in_target_nad_with_prices(
                market,
                valuation.prices,
                borrowed_asset,
                borrowed_out,
                target_asset,
            )?)
            .ok_or(ErrorCode::MarketMathOverflow)?;
            let current_debt_nadless = Debt::shares_to_debt(debt_shares, borrow_index)?;
            let current_debt = u64::try_from(current_debt_nadless).unwrap_or(u64::MAX);
            let repay = raw_amount_from_target_value_nad_with_prices(
                market,
                valuation.prices,
                borrowed_asset,
                target_asset,
                removed_value_nad,
            )?
            .min(current_debt);
            if repay == 0 {
                break 'entry HlpEntryDisposition::ControllerGranularityLimited;
            }
            let (_, interest_paid) =
                crate::math::realized_interest_split(repay, current_debt_nadless, u128::from(debt_principal))?;
            if market.side(borrowed_asset).reserves.cash_reserve < interest_paid {
                break 'entry HlpEntryDisposition::CashConstrained;
            }
            HlpEntryDisposition::Actionable
        }
    };
    Ok(HlpEntryState {
        disposition,
        residual_exposure,
        nav_nad: valuation.nav_nad,
    })
}

const fn complete_rebalance_amounts(amounts: ProportionalRebalanceAmounts) -> bool {
    amounts.target_leg_amount > 0 && amounts.borrowed_leg_amount > 0 && amounts.debt_amount > 0
}

#[cfg(test)]
pub(crate) fn hlp_end_to_end_tracking_delta(
    market: &Market,
    receipt: HlpRebalanceReceipt,
    final_prices: HlpCurvePrices,
) -> Result<i128> {
    let tracking = HlpTrackingReference {
        principal_nav_nad: receipt.tracking_start_nav_nad,
        loss_budget_nad: receipt.tracking_loss_budget_nad,
        base_unrealized_interest: receipt.tracking_base_unrealized_interest,
        quote_unrealized_interest: receipt.tracking_quote_unrealized_interest,
        start_ylp_shares: receipt.tracking_start_ylp_shares,
        start_ylp_supply: receipt.tracking_start_ylp_supply,
    };
    let values = current_hlp_inventory_values_nad_with_prices(market, receipt.target_asset, final_prices)?;
    let collateral = values
        .target_inventory_value_nad
        .checked_add(values.opposite_inventory_value_nad)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let final_principal_nav_nad = signed_value_difference(collateral, values.debt_value_nad)?;
    let (_, _, tracking_delta_nad) = hlp_tracking_deltas_nad(
        market,
        receipt.target_asset,
        final_prices,
        final_principal_nav_nad,
        tracking,
    )?;
    tracking_delta_nad
        .checked_sub(receipt.tracking_retained_contribution_nad)
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

#[cfg(test)]
pub(crate) fn current_hlp_signed_navs(market: &Market) -> Result<(i128, i128)> {
    let prices = current_hlp_curve_prices(market)?;
    current_hlp_signed_navs_with_prices(market, prices)
}

/// Values both active hLP principals from a caller-proved executable price.
/// This is the same accounting path as `current_hlp_signed_navs`; it merely
/// avoids solving the identical curve again when an identity-bound endpoint
/// already carries the marginal price.
pub(crate) fn current_hlp_signed_navs_with_prices(market: &Market, prices: HlpCurvePrices) -> Result<(i128, i128)> {
    let base = current_hlp_inventory_values_nad_with_prices(market, MarketAsset::Base, prices)?;
    let quote = current_hlp_inventory_values_nad_with_prices(market, MarketAsset::Quote, prices)?;
    let principal_nav = |values: HlpInventoryValuesNad| -> Result<i128> {
        let collateral = values
            .target_inventory_value_nad
            .checked_add(values.opposite_inventory_value_nad)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        signed_value_difference(collateral, values.debt_value_nad)
    };
    Ok((principal_nav(base)?, principal_nav(quote)?))
}

fn signed_value_difference(collateral_nad: u128, debt_nad: u128) -> Result<i128> {
    if collateral_nad >= debt_nad {
        i128::try_from(collateral_nad - debt_nad).map_err(|_| ErrorCode::MarketMathOverflow.into())
    } else {
        i128::try_from(debt_nad - collateral_nad)
            .map_err(|_| ErrorCode::MarketMathOverflow)?
            .checked_neg()
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    }
}

pub(crate) fn rebase_hlp_tracking_for_socialized_loss(
    receipt: &mut HlpRebalanceReceipt,
    nav_before_nad: i128,
    nav_after_nad: i128,
) -> Result<()> {
    if receipt.tracking_loss_budget_nad == 0 {
        return Ok(());
    }
    let authorized_delta = nav_after_nad
        .checked_sub(nav_before_nad)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    receipt.tracking_start_nav_nad = receipt
        .tracking_start_nav_nad
        .checked_add(authorized_delta)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    Ok(())
}

pub(crate) fn checkpoint_one_hlp_with_prices(
    market: &mut Market,
    target_asset: MarketAsset,
    prices: HlpCurvePrices,
) -> Result<i128> {
    let valuation = current_hlp_valuation_with_prices(market, target_asset, prices)?;
    let nav = valuation.nav_nad;
    let vault = match target_asset {
        MarketAsset::Base => &mut market.base_hlp_vault,
        MarketAsset::Quote => &mut market.quote_hlp_vault,
    };
    let ideal_delta = recognized_hlp_residual_exposure(valuation.ideal_delta, nav);
    vault.last_nav_nad = nav;
    vault.residual_exposure = ideal_delta;
    // This reference belongs to the last actual hLP settlement/rebalance.
    // Updating it during a generic market checkpoint would make the later
    // settlement-divergence guard compare the current price with itself.
    Ok(ideal_delta)
}

pub(crate) fn checkpoint_hlp_yield_from_ylp(market: &mut Market, target_asset: MarketAsset) -> Result<()> {
    let ylp_shares = match target_asset {
        MarketAsset::Base => market.base_hlp_vault.ylp_shares,
        MarketAsset::Quote => market.quote_hlp_vault.ylp_shares,
    };
    checkpoint_hlp_yield_from_ylp_shares(market, target_asset, ylp_shares)
}

/// Checkpoints both hLP vaults from one carried fee snapshot. The legacy
/// base-then-quote sequence carried the same four side accumulators twice;
/// the first vault checkpoint cannot mutate those side indexes, so the second
/// carry/snapshot was observationally redundant.
fn checkpoint_hlp_yield_from_ylp_pair(
    market: &mut Market,
    checkpoint_base: bool,
    checkpoint_quote: bool,
) -> Result<()> {
    if !checkpoint_base && !checkpoint_quote {
        return Ok(());
    }
    market.base_side.carry_forward_swap_fees()?;
    market.base_side.carry_forward_interest()?;
    market.quote_side.carry_forward_swap_fees()?;
    market.quote_side.carry_forward_interest()?;
    let base_side = market.base_side;
    let quote_side = market.quote_side;
    let base_shares = market.base_hlp_vault.ylp_shares;
    let quote_shares = market.quote_hlp_vault.ylp_shares;
    if checkpoint_base {
        market
            .base_hlp_vault
            .checkpoint_yield_from_ylp_shares(&base_side, &quote_side, base_shares)?;
    }
    if checkpoint_quote {
        market
            .quote_hlp_vault
            .checkpoint_yield_from_ylp_shares(&base_side, &quote_side, quote_shares)?;
    }
    Ok(())
}

pub(crate) fn checkpoint_hlp_yield_from_ylp_shares(
    market: &mut Market,
    target_asset: MarketAsset,
    eligible_ylp_shares: u64,
) -> Result<()> {
    market.base_side.carry_forward_swap_fees()?;
    market.base_side.carry_forward_interest()?;
    market.quote_side.carry_forward_swap_fees()?;
    market.quote_side.carry_forward_interest()?;
    let base_side = market.base_side;
    let quote_side = market.quote_side;
    match target_asset {
        MarketAsset::Base => {
            market
                .base_hlp_vault
                .checkpoint_yield_from_ylp_shares(&base_side, &quote_side, eligible_ylp_shares)
        }
        MarketAsset::Quote => {
            market
                .quote_hlp_vault
                .checkpoint_yield_from_ylp_shares(&base_side, &quote_side, eligible_ylp_shares)
        }
    }
}

fn require_hlp_settlement_available(market: &Market, target_asset: MarketAsset) -> Result<()> {
    let prices = current_hlp_curve_prices(market)?;
    let vault = match target_asset {
        MarketAsset::Base => &market.base_hlp_vault,
        MarketAsset::Quote => &market.quote_hlp_vault,
    };
    if vault.hlp_supply == 0 || vault.cached_settlement_price_nad == 0 {
        return Ok(());
    }
    let current_price = prices.for_asset(target_asset);
    let reference_price = vault.cached_settlement_price_nad;
    let divergence = if current_price >= reference_price {
        current_price
            .checked_sub(reference_price)
            .ok_or(ErrorCode::MarketMathOverflow)?
    } else {
        reference_price
            .checked_sub(current_price)
            .ok_or(ErrorCode::MarketMathOverflow)?
    };
    let max_divergence = reference_price
        .checked_mul(market.config.settlement_divergence_bps as u128)
        .and_then(|value| value.checked_div(crate::constants::BPS_DENOMINATOR as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    require!(divergence <= max_divergence, ErrorCode::HlpSettlementUnavailable);
    Ok(())
}

/// One executable marginal-price evaluation supplies both reciprocal
/// numeraires used by a single hLP accounting snapshot. Re-evaluating the
/// identical curve for every inventory/debt leg is semantically redundant and
/// exhausts Solana's non-freeing 32 KiB program heap on composite swaps.
pub(crate) fn current_hlp_curve_prices(market: &Market) -> Result<HlpCurvePrices> {
    let price_nad = market
        .current_explicit_spot_price_nad()?
        .ok_or(ErrorCode::BrokenInvariant)?;
    hlp_curve_prices_from_base_price_nad(price_nad as u128)
}

pub(crate) fn hlp_curve_prices_from_base_price_nad(base_in_quote_nad: u128) -> Result<HlpCurvePrices> {
    require!(base_in_quote_nad > 0, ErrorCode::InvalidSettlementPrice);
    let base_in_quote_nad = u64::try_from(base_in_quote_nad).map_err(|_| ErrorCode::MarketMathOverflow)? as u128;
    let quote_in_base_nad = (NAD as u128)
        .checked_mul(NAD as u128)
        .and_then(|value| value.checked_div(base_in_quote_nad))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let quote_in_base_nad = u64::try_from(quote_in_base_nad).map_err(|_| ErrorCode::MarketMathOverflow)? as u128;
    require!(quote_in_base_nad > 0, ErrorCode::InvalidSettlementPrice);
    Ok(HlpCurvePrices {
        base_in_quote_nad,
        quote_in_base_nad,
    })
}

#[cfg(test)]
fn current_settlement_price_nad(market: &Market, target_asset: MarketAsset) -> Result<u128> {
    Ok(current_hlp_curve_prices(market)?.for_asset(target_asset))
}

fn hlp_nav_nad(market: &Market, target_asset: MarketAsset) -> Result<u128> {
    let vault = match target_asset {
        MarketAsset::Base => &market.base_hlp_vault,
        MarketAsset::Quote => &market.quote_hlp_vault,
    };
    if vault.ylp_shares == 0 && vault.debt_shares == 0 {
        return Ok(0);
    }
    hlp_nav_nad_with_prices(market, target_asset, current_hlp_curve_prices(market)?)
}

fn hlp_nav_nad_with_prices(market: &Market, target_asset: MarketAsset, prices: HlpCurvePrices) -> Result<u128> {
    let values = current_hlp_inventory_values_nad_with_prices(market, target_asset, prices)?;
    let collateral = values
        .target_inventory_value_nad
        .checked_add(values.opposite_inventory_value_nad)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let debt = values.debt_value_nad;
    collateral
        .checked_sub(debt)
        .ok_or_else(|| ErrorCode::Undercollateralized.into())
}

#[cfg(test)]
fn hlp_collateral_value_nad(market: &Market, target_asset: MarketAsset, vault: &HlpVault) -> Result<u128> {
    let base_underlying = ylp_curve_underlying_amount(market, MarketAsset::Base, vault.ylp_shares)?;
    let quote_underlying = ylp_curve_underlying_amount(market, MarketAsset::Quote, vault.ylp_shares)?;
    let base_value = asset_value_in_target_nad(market, MarketAsset::Base, base_underlying, target_asset)?;
    let quote_value = asset_value_in_target_nad(market, MarketAsset::Quote, quote_underlying, target_asset)?;
    base_value
        .checked_add(quote_value)
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

#[cfg(test)]
fn hlp_debt_value_nad(market: &Market, target_asset: MarketAsset) -> Result<u128> {
    let borrowed_asset = target_asset.opposite();
    let debt_amount = hlp_debt_amount(market, target_asset)?;
    asset_value_in_target_nad(market, borrowed_asset, debt_amount, target_asset)
}

fn hlp_debt_amount(market: &Market, target_asset: MarketAsset) -> Result<u64> {
    let debt_amount = match target_asset {
        MarketAsset::Base => {
            Debt::shares_to_debt(market.base_hlp_vault.debt_shares, market.debt.quote_borrow_index_nad)?
        }
        MarketAsset::Quote => {
            Debt::shares_to_debt(market.quote_hlp_vault.debt_shares, market.debt.base_borrow_index_nad)?
        }
    };
    u64::try_from(debt_amount).map_err(|_| ErrorCode::DebtMathOverflow.into())
}

fn ylp_curve_underlying_amount(market: &Market, asset: MarketAsset, ylp_amount: u64) -> Result<u64> {
    let side = market.side(asset);
    if ylp_amount == 0 || side.shares.ylp_supply == 0 {
        return Ok(0);
    }
    let reserve_amount = (ylp_amount as u128)
        .checked_mul(market.curve_reserve(asset)? as u128)
        .and_then(|value| value.checked_div(side.shares.ylp_supply as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    u64::try_from(reserve_amount).map_err(|_| ErrorCode::MarketMathOverflow.into())
}

fn ylp_live_underlying_amount(market: &Market, asset: MarketAsset, ylp_amount: u64) -> Result<u64> {
    let side = market.side(asset);
    ylp_live_underlying_amount_from_values(side.reserves.live_reserve, side.shares.ylp_supply, ylp_amount)
}

fn ylp_live_underlying_amount_from_values(live_reserve: u64, supply: u64, ylp_amount: u64) -> Result<u64> {
    require!(ylp_amount > 0, ErrorCode::AmountZero);
    require_gte!(supply, ylp_amount, ErrorCode::InsufficientBalance);
    let amount = mul_div_u128(ylp_amount as u128, live_reserve as u128, supply as u128)?;
    u64::try_from(amount).map_err(|_| ErrorCode::MarketMathOverflow.into())
}

#[cfg(test)]
fn asset_value_in_target_nad(
    market: &Market,
    asset: MarketAsset,
    amount: u64,
    target_asset: MarketAsset,
) -> Result<u128> {
    if amount == 0 {
        return Ok(0);
    }
    if asset == target_asset {
        return normalize_to_nad(amount as u128, market.side(asset).asset_decimals);
    }
    asset_value_in_target_nad_with_prices(market, current_hlp_curve_prices(market)?, asset, amount, target_asset)
}

fn asset_value_in_target_nad_with_prices(
    market: &Market,
    prices: HlpCurvePrices,
    asset: MarketAsset,
    amount: u64,
    target_asset: MarketAsset,
) -> Result<u128> {
    if amount == 0 {
        return Ok(0);
    }
    let amount_nad = normalize_to_nad(amount as u128, market.side(asset).asset_decimals)?;
    if asset == target_asset {
        return Ok(amount_nad);
    }
    let price_nad = prices.for_asset(asset);
    mul_div_u128(amount_nad, price_nad, NAD as u128)
}

fn current_hlp_inventory_values_nad_with_prices(
    market: &Market,
    target_asset: MarketAsset,
    prices: HlpCurvePrices,
) -> Result<HlpInventoryValuesNad> {
    let vault = match target_asset {
        MarketAsset::Base => &market.base_hlp_vault,
        MarketAsset::Quote => &market.quote_hlp_vault,
    };
    let opposite_asset = target_asset.opposite();
    let target_underlying = ylp_curve_underlying_amount(market, target_asset, vault.ylp_shares)?;
    let opposite_underlying = ylp_curve_underlying_amount(market, opposite_asset, vault.ylp_shares)?;
    let debt_amount = u64::try_from(match target_asset {
        MarketAsset::Base => {
            Debt::shares_to_debt(market.base_hlp_vault.debt_shares, market.debt.quote_borrow_index_nad)?
        }
        MarketAsset::Quote => {
            Debt::shares_to_debt(market.quote_hlp_vault.debt_shares, market.debt.base_borrow_index_nad)?
        }
    })
    .map_err(|_| ErrorCode::DebtMathOverflow)?;
    Ok(HlpInventoryValuesNad {
        target_inventory_value_nad: asset_value_in_target_nad_with_prices(
            market,
            prices,
            target_asset,
            target_underlying,
            target_asset,
        )?,
        opposite_inventory_value_nad: asset_value_in_target_nad_with_prices(
            market,
            prices,
            opposite_asset,
            opposite_underlying,
            target_asset,
        )?,
        debt_value_nad: asset_value_in_target_nad_with_prices(
            market,
            prices,
            opposite_asset,
            debt_amount,
            target_asset,
        )?,
    })
}

/// Values both vaults from one immutable reserve/supply snapshot. This keeps
/// the two numeraires on the same curve state while avoiding four repeated
/// curve-reserve derivations in the joint lifecycle hot path.
fn current_hlp_inventory_values_pair_nad_with_prices(
    market: &Market,
    prices: HlpCurvePrices,
    base_active: bool,
    quote_active: bool,
) -> Result<(HlpInventoryValuesNad, HlpInventoryValuesNad)> {
    let base_curve = market.curve_reserve(MarketAsset::Base)?;
    let quote_curve = market.curve_reserve(MarketAsset::Quote)?;
    let base_supply = market.base_side.shares.ylp_supply;
    let quote_supply = market.quote_side.shares.ylp_supply;

    let claim = |reserve: u64, shares: u64, supply: u64| -> Result<u64> {
        if shares == 0 || supply == 0 {
            return Ok(0);
        }
        u64::try_from(mul_div_u128(reserve as u128, shares as u128, supply as u128)?)
            .map_err(|_| ErrorCode::MarketMathOverflow.into())
    };

    let base_values = if base_active {
        let shares = market.base_hlp_vault.ylp_shares;
        let base_claim = claim(base_curve, shares, base_supply)?;
        let quote_claim = claim(quote_curve, shares, quote_supply)?;
        let quote_debt = u64::try_from(Debt::shares_to_debt(
            market.base_hlp_vault.debt_shares,
            market.debt.quote_borrow_index_nad,
        )?)
        .map_err(|_| ErrorCode::DebtMathOverflow)?;
        HlpInventoryValuesNad {
            target_inventory_value_nad: asset_value_in_target_nad_with_prices(
                market,
                prices,
                MarketAsset::Base,
                base_claim,
                MarketAsset::Base,
            )?,
            opposite_inventory_value_nad: asset_value_in_target_nad_with_prices(
                market,
                prices,
                MarketAsset::Quote,
                quote_claim,
                MarketAsset::Base,
            )?,
            debt_value_nad: asset_value_in_target_nad_with_prices(
                market,
                prices,
                MarketAsset::Quote,
                quote_debt,
                MarketAsset::Base,
            )?,
        }
    } else {
        HlpInventoryValuesNad::default()
    };

    let quote_values = if quote_active {
        let shares = market.quote_hlp_vault.ylp_shares;
        let base_claim = claim(base_curve, shares, base_supply)?;
        let quote_claim = claim(quote_curve, shares, quote_supply)?;
        let base_debt = u64::try_from(Debt::shares_to_debt(
            market.quote_hlp_vault.debt_shares,
            market.debt.base_borrow_index_nad,
        )?)
        .map_err(|_| ErrorCode::DebtMathOverflow)?;
        HlpInventoryValuesNad {
            target_inventory_value_nad: asset_value_in_target_nad_with_prices(
                market,
                prices,
                MarketAsset::Quote,
                quote_claim,
                MarketAsset::Quote,
            )?,
            opposite_inventory_value_nad: asset_value_in_target_nad_with_prices(
                market,
                prices,
                MarketAsset::Base,
                base_claim,
                MarketAsset::Quote,
            )?,
            debt_value_nad: asset_value_in_target_nad_with_prices(
                market,
                prices,
                MarketAsset::Base,
                base_debt,
                MarketAsset::Quote,
            )?,
        }
    } else {
        HlpInventoryValuesNad::default()
    };
    Ok((base_values, quote_values))
}

fn hlp_interest_claims_for_shares(
    base_unrealized_interest: u64,
    quote_unrealized_interest: u64,
    ylp_shares: u64,
    ylp_supply: u64,
) -> Result<(u64, u64)> {
    if ylp_shares == 0 {
        return Ok((0, 0));
    }
    require!(ylp_supply > 0, ErrorCode::BrokenInvariant);
    if base_unrealized_interest == 0 && quote_unrealized_interest == 0 {
        return Ok((0, 0));
    }
    let base_claim = u64::try_from(mul_div_u128(
        base_unrealized_interest as u128,
        ylp_shares as u128,
        ylp_supply as u128,
    )?)
    .map_err(|_| ErrorCode::MarketMathOverflow)?;
    let quote_claim = u64::try_from(mul_div_u128(
        quote_unrealized_interest as u128,
        ylp_shares as u128,
        ylp_supply as u128,
    )?)
    .map_err(|_| ErrorCode::MarketMathOverflow)?;
    Ok((base_claim, quote_claim))
}

fn signed_asset_value_in_target_nad_with_prices(
    market: &Market,
    prices: HlpCurvePrices,
    asset: MarketAsset,
    amount: i128,
    target_asset: MarketAsset,
) -> Result<i128> {
    let magnitude = u64::try_from(amount.unsigned_abs()).map_err(|_| ErrorCode::MarketMathOverflow)?;
    let value = i128::try_from(asset_value_in_target_nad_with_prices(
        market,
        prices,
        asset,
        magnitude,
        target_asset,
    )?)
    .map_err(|_| ErrorCode::MarketMathOverflow)?;
    if amount < 0 {
        value.checked_neg().ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    } else {
        Ok(value)
    }
}

fn hlp_frozen_interest_claim_delta_value_nad(
    market: &Market,
    target_asset: MarketAsset,
    prices: HlpCurvePrices,
    tracking: HlpTrackingReference,
) -> Result<i128> {
    let vault = match target_asset {
        MarketAsset::Base => &market.base_hlp_vault,
        MarketAsset::Quote => &market.quote_hlp_vault,
    };
    let final_ylp_supply = market.base_side.shares.ylp_supply;
    require_eq!(
        final_ylp_supply,
        market.quote_side.shares.ylp_supply,
        ErrorCode::BrokenInvariant
    );
    let (base_public_claim, quote_public_claim) = hlp_interest_claims_for_shares(
        tracking.base_unrealized_interest,
        tracking.quote_unrealized_interest,
        vault.ylp_shares,
        final_ylp_supply,
    )?;
    let (start_base_public_claim, start_quote_public_claim) = hlp_interest_claims_for_shares(
        tracking.base_unrealized_interest,
        tracking.quote_unrealized_interest,
        tracking.start_ylp_shares,
        tracking.start_ylp_supply,
    )?;
    let base_delta = i128::from(base_public_claim)
        .checked_sub(i128::from(start_base_public_claim))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let quote_delta = i128::from(quote_public_claim)
        .checked_sub(i128::from(start_quote_public_claim))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    signed_asset_value_in_target_nad_with_prices(market, prices, MarketAsset::Base, base_delta, target_asset)?
        .checked_add(signed_asset_value_in_target_nad_with_prices(
            market,
            prices,
            MarketAsset::Quote,
            quote_delta,
            target_asset,
        )?)
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

fn hlp_tracking_deltas_nad(
    market: &Market,
    target_asset: MarketAsset,
    prices: HlpCurvePrices,
    final_principal_nav_nad: i128,
    tracking: HlpTrackingReference,
) -> Result<(i128, i128, i128)> {
    let principal_delta = final_principal_nav_nad
        .checked_sub(tracking.principal_nav_nad)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let claim_delta = hlp_frozen_interest_claim_delta_value_nad(market, target_asset, prices, tracking)?;
    let combined_delta = principal_delta
        .checked_add(claim_delta)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    Ok((principal_delta, claim_delta, combined_delta))
}

pub(crate) fn stamp_hlp_tracking_reference(receipt: &mut HlpRebalanceReceipt, tracking: HlpTrackingReference) {
    receipt.tracking_start_nav_nad = tracking.principal_nav_nad;
    receipt.tracking_loss_budget_nad = tracking.loss_budget_nad;
    receipt.tracking_base_unrealized_interest = tracking.base_unrealized_interest;
    receipt.tracking_quote_unrealized_interest = tracking.quote_unrealized_interest;
    receipt.tracking_start_ylp_shares = tracking.start_ylp_shares;
    receipt.tracking_start_ylp_supply = tracking.start_ylp_supply;
}

pub(crate) fn consume_hlp_tracking_unrealized_interest(
    receipt: &mut HlpRebalanceReceipt,
    asset: MarketAsset,
    amount: u64,
) -> Result<()> {
    if receipt.tracking_loss_budget_nad == 0 {
        return Ok(());
    }
    let tracked = match asset {
        MarketAsset::Base => &mut receipt.tracking_base_unrealized_interest,
        MarketAsset::Quote => &mut receipt.tracking_quote_unrealized_interest,
    };
    require_gte!(*tracked, amount, ErrorCode::BrokenInvariant);
    *tracked = tracked.checked_sub(amount).ok_or(ErrorCode::MarketMathOverflow)?;
    Ok(())
}

pub(crate) fn cap_hlp_tracking_unrealized_interest(
    receipt: &mut HlpRebalanceReceipt,
    asset: MarketAsset,
    surviving_amount: u64,
) {
    let tracked = match asset {
        MarketAsset::Base => &mut receipt.tracking_base_unrealized_interest,
        MarketAsset::Quote => &mut receipt.tracking_quote_unrealized_interest,
    };
    *tracked = (*tracked).min(surviving_amount);
}

#[cfg(test)]
fn current_hlp_inventory_values_nad(market: &Market, target_asset: MarketAsset) -> Result<HlpInventoryValuesNad> {
    current_hlp_inventory_values_nad_with_prices(market, target_asset, current_hlp_curve_prices(market)?)
}

fn raw_amount_from_target_value_nad_with_prices(
    market: &Market,
    prices: HlpCurvePrices,
    asset: MarketAsset,
    target_asset: MarketAsset,
    value_nad: u128,
) -> Result<u64> {
    if value_nad == 0 {
        return Ok(0);
    }
    let amount_nad = if asset == target_asset {
        value_nad
    } else {
        let price_nad = prices.for_asset(asset);
        require!(price_nad > 0, ErrorCode::InvalidSettlementPrice);
        mul_div_u128(value_nad, NAD as u128, price_nad)?
    };
    denormalize_from_nad_floor(amount_nad, market.side(asset).asset_decimals)
}

fn ylp_for_live_reserve_deposit(market: &Market, base_amount: u64, quote_amount: u64) -> Result<u64> {
    require!(market.base_side.shares.ylp_supply > 0, ErrorCode::SupplyUnderflow);
    let base_reserve = market.base_side.reserves.live_reserve;
    let quote_reserve = market.quote_side.reserves.live_reserve;
    require!(base_reserve > 0 && quote_reserve > 0, ErrorCode::InsufficientLiquidity);
    market.ylp_for_deposit(base_reserve, quote_reserve, base_amount, quote_amount)
}

fn proportional_rebalance_amounts(
    market: &Market,
    target_asset: MarketAsset,
    total_value_delta_nad: i128,
    valuation: HlpValuation,
) -> Result<ProportionalRebalanceAmounts> {
    if total_value_delta_nad == 0 {
        return Ok(ProportionalRebalanceAmounts::default());
    }
    let collateral_value = valuation
        .values
        .target_inventory_value_nad
        .checked_add(valuation.values.opposite_inventory_value_nad)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    require!(collateral_value > 0, ErrorCode::DenominatorOverflow);
    let total_value_delta = total_value_delta_nad.unsigned_abs();
    let target_value_delta = mul_div_u128(
        total_value_delta,
        valuation.values.target_inventory_value_nad,
        collateral_value,
    )?;
    let borrowed_value_delta = total_value_delta
        .checked_sub(target_value_delta)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let target_leg_amount = raw_amount_from_target_value_nad_with_prices(
        market,
        valuation.prices,
        target_asset,
        target_asset,
        target_value_delta,
    )?;
    let borrowed_asset = target_asset.opposite();
    let borrowed_leg_amount = raw_amount_from_target_value_nad_with_prices(
        market,
        valuation.prices,
        borrowed_asset,
        target_asset,
        borrowed_value_delta,
    )?;
    let debt_amount = raw_amount_from_target_value_nad_with_prices(
        market,
        valuation.prices,
        borrowed_asset,
        target_asset,
        total_value_delta,
    )?;
    Ok(ProportionalRebalanceAmounts {
        target_leg_amount,
        borrowed_leg_amount,
        debt_amount,
    })
}

fn ylp_shares_for_live_reserve_amount(market: &Market, asset: MarketAsset, reserve_amount: u64) -> Result<u64> {
    if reserve_amount == 0 {
        return Ok(0);
    }
    let side = market.side(asset);
    require!(
        side.reserves.live_reserve > 0 && side.shares.ylp_supply > 0,
        ErrorCode::InsufficientLiquidity
    );
    side.shares
        .shares_for_deposit(side.reserves.live_reserve, reserve_amount)
}

fn require_hlp_borrow_headroom(market: &Market, borrowed_asset: MarketAsset, amount: u64) -> Result<u128> {
    let (current_shares, borrow_index_nad) = match borrowed_asset {
        MarketAsset::Base => (market.quote_hlp_vault.debt_shares, market.debt.base_borrow_index_nad),
        MarketAsset::Quote => (market.base_hlp_vault.debt_shares, market.debt.quote_borrow_index_nad),
    };
    let added_shares = Debt::debt_to_shares(amount, borrow_index_nad)?;
    let projected_shares = current_shares
        .checked_add(added_shares)
        .ok_or(ErrorCode::DebtShareMathOverflow)?;
    let projected_debt = Debt::shares_to_debt(projected_shares, borrow_index_nad)?;
    require_gte!(
        market.side(borrowed_asset).reserves.cash_reserve as u128,
        projected_debt,
        ErrorCode::InsufficientBorrowHeadroom
    );
    Ok(added_shares)
}

fn curve_slot(market: &Market) -> u64 {
    // Curve parameters are explicitly admitted into `applied_curve_parameters`
    // by the instruction update path; merely observing wall-clock time never
    // advances a ramp. Avoid repeated Clock deserialization inside the bounded
    // hLP solver because Solana's bump allocator cannot reclaim that memory.
    market.amm.last_observation_slot.max(market.last_update_slot)
}

fn proportional(amount: u64, numerator: u64, denominator: u64) -> Result<u64> {
    let value = (amount as u128)
        .checked_mul(numerator as u128)
        .and_then(|value| value.checked_div(denominator as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    u64::try_from(value).map_err(|_| ErrorCode::MarketMathOverflow.into())
}

fn proportional_u128(amount: u128, numerator: u64, denominator: u64) -> Result<u128> {
    amount
        .checked_mul(numerator as u128)
        .and_then(|value| value.checked_div(denominator as u128))
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

fn hlp_shares_for_delta_nav(delta_nav_nad: u128, nav_basis_nad: u128, hlp_supply: u64) -> Result<u64> {
    require!(delta_nav_nad > 0, ErrorCode::AmountZero);
    require!(nav_basis_nad > 0, ErrorCode::MarketMathOverflow);
    let shares = delta_nav_nad
        .checked_mul(hlp_supply as u128)
        .and_then(|value| value.checked_div(nav_basis_nad))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let shares = u64::try_from(shares).map_err(|_| ErrorCode::MarketMathOverflow)?;
    require!(shares > 0, ErrorCode::AmountZero);
    Ok(shares)
}
