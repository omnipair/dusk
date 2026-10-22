use anchor_lang::prelude::*;
use spl_token_2022::extension::transfer_fee::TransferFee;

#[cfg(test)]
use super::liquidity::prepare_concentrated_hlp_transition;
use super::liquidity::{
    current_hlp_signed_navs_with_prices, hlp_curve_prices_from_base_price_nad,
    prepare_concentrated_hlp_transition_at_current_state, IntegratedCurveState, SwapCashPolicy,
};
use super::{AmmSwapQuote, HlpRebalanceReceipt, SwapFeeBreakdown};
use crate::{
    constants::{BPS_DENOMINATOR, LEVERAGE_MAX_MULTIPLIER_BPS, LIQUIDATION_INCENTIVE_BPS, NAD},
    errors::ErrorCode,
    math::{ceil_div, mul_div_ceil_u128, mul_div_u128, realized_interest_split},
    state::{Debt, LeveragePosition, Market, MarketAsset, ProtocolAuctionSplit},
};

/// The net amount that a collateral-vault transfer can return to the AMM.
/// A position stores the vault's gross balance; risk checks use this credit.
///
/// `effective` is the fee a transfer pays now. `pending` is a fee change the
/// mint has already scheduled for a later epoch. Transfers use the effective
/// fee. Admission checks also carry all announced fees; liquidation health
/// carries a pending fee during its final notice epoch. Existing positions can
/// therefore become liquidatable before activation. The issuer can reschedule
/// or cancel a change, so execution must read the mint and recheck health.
#[derive(Clone, Copy, Debug, Default)]
pub struct LeverageCollateralFee {
    effective: Option<TransferFee>,
    pending: Option<TransferFee>,
}

impl LeverageCollateralFee {
    pub fn new(effective: Option<TransferFee>) -> Self {
        Self {
            effective,
            pending: None,
        }
    }

    pub fn with_pending(self, pending: Option<TransferFee>) -> Self {
        Self { pending, ..self }
    }

    /// Net credit under the worst carried fee schedule.
    pub fn unwind_credit(self, gross_collateral: u64) -> Result<u64> {
        let mut credit = gross_collateral;
        for fee in [self.effective, self.pending].into_iter().flatten() {
            credit = credit.min(
                fee.calculate_post_fee_amount(gross_collateral)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
            );
        }
        Ok(credit)
    }

    /// Gross vault debit needed to deliver at least `net_credit` under the
    /// currently effective fee. Auction fills use this for each transfer;
    /// maximum-fee caps make a whole-position fee ratio inaccurate.
    pub fn effective_gross_for_credit(self, net_credit: u64) -> Result<u64> {
        let gross = match self.effective {
            Some(fee) => fee
                .calculate_pre_fee_amount(net_credit)
                .ok_or(ErrorCode::MarketMathOverflow)?,
            None => net_credit,
        };
        require_gte!(
            self.effective
                .map(|fee| fee
                    .calculate_post_fee_amount(gross)
                    .ok_or(ErrorCode::MarketMathOverflow))
                .transpose()?
                .unwrap_or(gross),
            net_credit,
            ErrorCode::BrokenInvariant
        );
        Ok(gross)
    }

    /// Largest carried fee rate. Market-wide health contributions are stored
    /// in gross collateral units and discounted by this rate when read, so a
    /// fee change applies to every position, not only the ones it touches.
    /// A per-transfer maximum fee can only lower the real fee, so the rate
    /// alone is conservative.
    pub fn haircut_bps(self) -> u16 {
        [self.effective, self.pending]
            .into_iter()
            .flatten()
            .map(|fee| u16::from(fee.transfer_fee_basis_points))
            .max()
            .unwrap_or(0)
    }
}

/// Conservative all-in Quote-per-Base execution price after swap fees, price
/// impact, and output transfer effects.
pub(crate) fn leverage_entry_price_nad(
    market: &Market,
    debt_asset: MarketAsset,
    notional_amount: u64,
    collateral_amount: u64,
) -> Result<u64> {
    require!(notional_amount > 0 && collateral_amount > 0, ErrorCode::AmountZero);
    let (base_amount, quote_amount) = match debt_asset {
        MarketAsset::Base => (notional_amount, collateral_amount),
        MarketAsset::Quote => (collateral_amount, notional_amount),
    };
    let base_decimals = market.base_side.asset_decimals;
    let quote_decimals = market.quote_side.asset_decimals;
    let (numerator_scale, denominator) = if base_decimals >= quote_decimals {
        let decimal_scale = 10_u128
            .checked_pow((base_decimals - quote_decimals) as u32)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        (
            (NAD as u128)
                .checked_mul(decimal_scale)
                .ok_or(ErrorCode::MarketMathOverflow)?,
            base_amount as u128,
        )
    } else {
        let decimal_scale = 10_u128
            .checked_pow((quote_decimals - base_decimals) as u32)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        (
            NAD as u128,
            (base_amount as u128)
                .checked_mul(decimal_scale)
                .ok_or(ErrorCode::MarketMathOverflow)?,
        )
    };
    let price = match debt_asset {
        // Base is sold for Quote. Floor so an order cannot pass unless the
        // received Quote-per-Base is at least its lower limit.
        MarketAsset::Base => mul_div_u128(quote_amount as u128, numerator_scale, denominator)?,
        // Quote is sold for Base. Ceil so an order cannot pass when its true
        // Quote-per-Base cost is even one atom above its upper limit.
        MarketAsset::Quote => mul_div_ceil_u128(quote_amount as u128, numerator_scale, denominator)?,
    };
    u64::try_from(price).map_err(|_| ErrorCode::MarketMathOverflow.into())
}

pub(crate) fn require_leverage_entry_limit(
    market: &Market,
    debt_asset: MarketAsset,
    notional_amount: u64,
    collateral_amount: u64,
    limit_price_nad: u64,
) -> Result<()> {
    if limit_price_nad == 0 {
        return Ok(());
    }
    let execution_price_nad = leverage_entry_price_nad(market, debt_asset, notional_amount, collateral_amount)?;
    require!(
        leverage_entry_limit_satisfied(debt_asset, execution_price_nad, limit_price_nad),
        ErrorCode::SlippageExceeded
    );
    Ok(())
}

pub(crate) const fn leverage_entry_limit_satisfied(
    debt_asset: MarketAsset,
    execution_price_nad: u64,
    limit_price_nad: u64,
) -> bool {
    match debt_asset {
        // Selling Base to obtain Quote: require at least the requested Quote
        // per Base.
        MarketAsset::Base => execution_price_nad >= limit_price_nad,
        // Selling Quote to obtain Base: pay no more Quote per Base than the
        // requested ceiling.
        MarketAsset::Quote => execution_price_nad <= limit_price_nad,
    }
}

use super::{
    amm::{prepare_concentrated_cache_at_point, DynamicFeePreState},
    liquidity::reconstruct_hlp_endpoint,
    DebtClearance, DebtWriteoff, FeesReceipt,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LeverageSwapQuote {
    pub asset_in: u8,
    pub quoted_slot: u64,
    pub amount_in: u64,
    pub amount_in_after_fee: u64,
    pub reserve_input_credit: u64,
    /// Net output credited to collateral, repayment, or the trader.
    pub amount_out: u64,
    /// Curve reserve debit before an output-denominated fee is withheld.
    pub gross_amount_out: u64,
    pub start_price_nad: u64,
    /// Invariant-preserving trade endpoint; retained principal is excluded.
    pub end_price_nad: u64,
    /// Final executable-reserve marginal price after retained principal.
    pub reserve_end_price_nad: u64,
    pub decayed_volatility_nad: u64,
    pub post_success_volatility_nad: u64,
    /// Nominal claimable fee debit held in the reserve vault but excluded from
    /// executable reserves. Actual Token-2022 credit is recorded separately
    /// through `LeverageSwapFeeCredit`.
    pub fee_credit: u64,
    pub fee_breakdown: SwapFeeBreakdown,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LeverageSwapFeeCredit {
    pub base: u64,
    pub distributed_surcharge: u64,
}

pub type PreparedLeverageSwap = Box<super::amm::PreparedSwap>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LeverageLifecycleTransition {
    pub(crate) added_debt_shares: u128,
    pub(crate) position_debt_shares: u128,
    pub(crate) position_debt_principal: u128,
    pub(crate) clearance: DebtClearance,
    pub(crate) writeoff: DebtWriteoff,
    pub(crate) removed_unrealized_interest: u64,
    pub(crate) phantom_unpaid_interest: u64,
    pub(crate) socialized_principal_loss: u64,
    pub(crate) socialized_advance_loss: u64,
}

/// Exact mutable footprint of the reserve/debt lifecycle transition. The
/// surrounding AMM checkpoint, fee distribution, hLP rebalance, and position
/// writes remain explicit sequencing barriers outside this kernel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct LeverageLifecycleState {
    base_live_reserve: u64,
    base_cash_reserve: u64,
    quote_live_reserve: u64,
    quote_cash_reserve: u64,
    base_interest_advance: crate::state::PublicInterestAdvance,
    quote_interest_advance: crate::state::PublicInterestAdvance,
    base_borrow_index_nad: u128,
    quote_borrow_index_nad: u128,
    fixed_base_shares: u128,
    fixed_quote_shares: u128,
    fixed_base_principal: u64,
    fixed_quote_principal: u64,
    isolated_base_shares: u128,
    isolated_quote_shares: u128,
    isolated_base_principal: u64,
    isolated_quote_principal: u64,
}

/// Fixed-size identity-bound transition. `post` and `transition` are derived
/// from the semantic inputs, then independently re-derived during preflight;
/// apply performs only infallible field commits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LeverageLifecyclePlan {
    start: LeverageLifecycleState,
    post: LeverageLifecycleState,
    policy: SwapCashPolicy,
    asset_in: MarketAsset,
    amount_in_after_fee: u64,
    amount_out: u64,
    gross_amount_out: u64,
    debt_curve_reserve_before_share_removal: Option<u64>,
    debt_curve_reserve_after_transition: Option<u64>,
    transition: LeverageLifecycleTransition,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HlpSocializedLossRebase {
    pub(crate) base_nav_delta_nad: i128,
    pub(crate) quote_nav_delta_nad: i128,
}

impl LeverageLifecycleState {
    fn capture(market: &Market) -> Self {
        Self {
            base_live_reserve: market.base_side.reserves.live_reserve,
            base_cash_reserve: market.base_side.reserves.cash_reserve,
            quote_live_reserve: market.quote_side.reserves.live_reserve,
            quote_cash_reserve: market.quote_side.reserves.cash_reserve,
            base_interest_advance: market.base_side.fees.public_interest_advance,
            quote_interest_advance: market.quote_side.fees.public_interest_advance,
            base_borrow_index_nad: market.debt.base_borrow_index_nad,
            quote_borrow_index_nad: market.debt.quote_borrow_index_nad,
            fixed_base_shares: market.debt.fixed_base_shares,
            fixed_quote_shares: market.debt.fixed_quote_shares,
            fixed_base_principal: market.debt.fixed_base_principal,
            fixed_quote_principal: market.debt.fixed_quote_principal,
            isolated_base_shares: market.debt.isolated_base_shares,
            isolated_quote_shares: market.debt.isolated_quote_shares,
            isolated_base_principal: market.debt.isolated_base_principal,
            isolated_quote_principal: market.debt.isolated_quote_principal,
        }
    }

    fn debt(self) -> crate::state::Debt {
        crate::state::Debt {
            base_borrow_index_nad: self.base_borrow_index_nad,
            quote_borrow_index_nad: self.quote_borrow_index_nad,
            fixed_base_shares: self.fixed_base_shares,
            fixed_quote_shares: self.fixed_quote_shares,
            fixed_base_principal: self.fixed_base_principal,
            fixed_quote_principal: self.fixed_quote_principal,
            isolated_base_shares: self.isolated_base_shares,
            isolated_quote_shares: self.isolated_quote_shares,
            isolated_base_principal: self.isolated_base_principal,
            isolated_quote_principal: self.isolated_quote_principal,
            ..crate::state::Debt::default()
        }
    }

    fn set_debt(&mut self, debt: crate::state::Debt) {
        self.fixed_base_shares = debt.fixed_base_shares;
        self.fixed_quote_shares = debt.fixed_quote_shares;
        self.fixed_base_principal = debt.fixed_base_principal;
        self.fixed_quote_principal = debt.fixed_quote_principal;
        self.isolated_base_shares = debt.isolated_base_shares;
        self.isolated_quote_shares = debt.isolated_quote_shares;
        self.isolated_base_principal = debt.isolated_base_principal;
        self.isolated_quote_principal = debt.isolated_quote_principal;
    }

    const fn live_reserve(self, asset: MarketAsset) -> u64 {
        match asset {
            MarketAsset::Base => self.base_live_reserve,
            MarketAsset::Quote => self.quote_live_reserve,
        }
    }

    fn live_reserve_mut(&mut self, asset: MarketAsset) -> &mut u64 {
        match asset {
            MarketAsset::Base => &mut self.base_live_reserve,
            MarketAsset::Quote => &mut self.quote_live_reserve,
        }
    }

    fn cash_reserve_mut(&mut self, asset: MarketAsset) -> &mut u64 {
        match asset {
            MarketAsset::Base => &mut self.base_cash_reserve,
            MarketAsset::Quote => &mut self.quote_cash_reserve,
        }
    }

    fn unrealized_interest(self, asset: MarketAsset) -> Result<u128> {
        let debt = self.debt();
        let debt_amount = debt.isolated_debt(asset)?;
        let principal = match asset {
            MarketAsset::Base => self.isolated_base_principal,
            MarketAsset::Quote => self.isolated_quote_principal,
        };
        let isolated_interest = debt_amount
            .checked_sub(u128::from(principal).min(debt_amount))
            .ok_or(ErrorCode::DebtMathOverflow)?;
        let (fixed_debt, fixed_principal) = match asset {
            MarketAsset::Base => (debt.fixed_base_debt()?, self.fixed_base_principal),
            MarketAsset::Quote => (debt.fixed_quote_debt()?, self.fixed_quote_principal),
        };
        isolated_interest
            .checked_add(fixed_debt.saturating_sub(u128::from(fixed_principal)))
            .ok_or_else(|| ErrorCode::DebtMathOverflow.into())
    }

    /// Reconstruct the post-state curve reserve from an identity-bound start
    /// reserve. Retain interest outside fixed/isolated borrower debt while
    /// accounting for either borrower ledger's collected or canceled interest.
    fn projected_curve_reserve_from(self, start: Self, asset: MarketAsset, start_curve_reserve: u64) -> Result<u64> {
        let delta = i128::from(self.live_reserve(asset)) - i128::from(start.live_reserve(asset));
        let before = i128::try_from(start.unrealized_interest(asset)?).map_err(|_| ErrorCode::MarketMathOverflow)?;
        let after = i128::try_from(self.unrealized_interest(asset)?).map_err(|_| ErrorCode::MarketMathOverflow)?;
        let advance_delta = match asset {
            MarketAsset::Base => {
                i128::from(self.base_interest_advance.outstanding_gross)
                    - i128::from(start.base_interest_advance.outstanding_gross)
            }
            MarketAsset::Quote => {
                i128::from(self.quote_interest_advance.outstanding_gross)
                    - i128::from(start.quote_interest_advance.outstanding_gross)
            }
        };
        let projected = i128::from(start_curve_reserve)
            .checked_add(advance_delta)
            .and_then(|v| v.checked_add(delta))
            .and_then(|v| v.checked_add(before))
            .and_then(|v| v.checked_sub(after))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        u64::try_from(projected).map_err(|_| ErrorCode::BrokenInvariant.into())
    }
}

fn commit_leverage_lifecycle_state(market: &mut Market, post: LeverageLifecycleState) {
    market.debt.fixed_base_shares = post.fixed_base_shares;
    market.debt.fixed_quote_shares = post.fixed_quote_shares;
    market.debt.fixed_base_principal = post.fixed_base_principal;
    market.debt.fixed_quote_principal = post.fixed_quote_principal;
    market.base_side.reserves.live_reserve = post.base_live_reserve;
    market.base_side.reserves.cash_reserve = post.base_cash_reserve;
    market.quote_side.reserves.live_reserve = post.quote_live_reserve;
    market.quote_side.reserves.cash_reserve = post.quote_cash_reserve;
    market.base_side.fees.public_interest_advance = post.base_interest_advance;
    market.quote_side.fees.public_interest_advance = post.quote_interest_advance;
    market.debt.isolated_base_shares = post.isolated_base_shares;
    market.debt.isolated_quote_shares = post.isolated_quote_shares;
    market.debt.isolated_base_principal = post.isolated_base_principal;
    market.debt.isolated_quote_principal = post.isolated_quote_principal;
}

fn derive_leverage_lifecycle_plan(
    market: &Market,
    policy: SwapCashPolicy,
    asset_in: MarketAsset,
    amount_in_after_fee: u64,
    amount_out: u64,
    gross_amount_out: u64,
) -> Result<LeverageLifecyclePlan> {
    let start = LeverageLifecycleState::capture(market);
    let debt_curve_reserve_before_share_removal = match policy {
        SwapCashPolicy::Liquidate { debt_asset, .. }
        | SwapCashPolicy::SettleLiquidation {
            debt_asset, full: true, ..
        } => Some(market.curve_reserve(debt_asset)?),
        _ => None,
    };
    derive_leverage_lifecycle_plan_from_state(
        start,
        policy,
        asset_in,
        amount_in_after_fee,
        amount_out,
        gross_amount_out,
        debt_curve_reserve_before_share_removal,
    )
}

/// Pure reserve/debt lifecycle kernel. Canonical Market execution and the
/// compact hLP planner supply the same fixed checkpoint plus any liquidation
/// curve-reserve identity, then commit the returned post-state through their
/// own adapters.
fn derive_leverage_lifecycle_plan_from_state(
    start: LeverageLifecycleState,
    policy: SwapCashPolicy,
    asset_in: MarketAsset,
    amount_in_after_fee: u64,
    amount_out: u64,
    gross_amount_out: u64,
    supplied_debt_curve_reserve_before_share_removal: Option<u64>,
) -> Result<LeverageLifecyclePlan> {
    let mut post = start;
    let mut debt = start.debt();
    let mut transition = LeverageLifecycleTransition::default();
    require_gte!(gross_amount_out, amount_out, ErrorCode::BrokenInvariant);
    let output_fee = gross_amount_out
        .checked_sub(amount_out)
        .ok_or(ErrorCode::BrokenInvariant)?;
    let mut cash_debit_out = gross_amount_out;
    let mut extra_live_debit_out = 0_u64;
    let mut debt_curve_reserve_before_share_removal = None;
    let mut debt_curve_reserve_after_transition = None;

    match policy {
        SwapCashPolicy::Spot => {}
        SwapCashPolicy::Borrow { asset, amount } => {
            require!(asset == asset_in, ErrorCode::BrokenInvariant);
            let cash_reserve = post.cash_reserve_mut(asset);
            require_gte!(*cash_reserve, amount, ErrorCode::InsufficientBorrowHeadroom);
            *cash_reserve = cash_reserve
                .checked_sub(amount)
                .ok_or(ErrorCode::CashReserveUnderflow)?;
        }
        SwapCashPolicy::Decrease {
            debt_asset,
            debt_shares,
            debt_principal,
        } => {
            require!(debt_asset == asset_in.opposite(), ErrorCode::BrokenInvariant);
            let mut shares = debt_shares;
            let mut principal = debt_principal;
            transition.clearance = debt.clear_isolated_debt(debt_asset, &mut shares, &mut principal, amount_out)?;
            transition.position_debt_shares = shares;
            transition.position_debt_principal = principal;
            transition.removed_unrealized_interest = transition.clearance.interest_paid;
            cash_debit_out = transition
                .clearance
                .interest_paid
                .checked_add(output_fee)
                .and_then(|value| value.checked_add(amount_out - transition.clearance.cash_repaid))
                .ok_or(ErrorCode::MarketMathOverflow)?;
            extra_live_debit_out = transition.clearance.live_debit_for_cash_repay()?;
        }
        SwapCashPolicy::Close {
            debt_asset,
            debt_shares,
            debt_principal,
            shares_to_burn,
        } => {
            require!(debt_asset == asset_in.opposite(), ErrorCode::BrokenInvariant);
            require_gte!(debt_shares, shares_to_burn, ErrorCode::DebtShareMathOverflow);
            let mut shares = debt_shares;
            let mut principal = debt_principal;
            if shares_to_burn > 0 {
                let aggregate_shares = match debt_asset {
                    MarketAsset::Base => debt.isolated_base_shares,
                    MarketAsset::Quote => debt.isolated_quote_shares,
                };
                let cash_repaid = Debt::aggregate_debt_reduction_for_shares(
                    aggregate_shares,
                    shares_to_burn,
                    debt.borrow_index(debt_asset),
                )?;
                transition.clearance =
                    debt.clear_isolated_debt(debt_asset, &mut shares, &mut principal, cash_repaid)?;
                require_eq!(
                    transition.clearance.shares_burned,
                    shares_to_burn,
                    ErrorCode::BrokenInvariant
                );
            } else {
                require_eq!(shares, 0, ErrorCode::BrokenInvariant);
                require_eq!(principal, 0, ErrorCode::BrokenInvariant);
            }
            require_gte!(
                amount_out,
                transition.clearance.cash_repaid,
                ErrorCode::InsufficientAmount
            );
            if shares_to_burn == debt_shares {
                require_eq!(transition.clearance.remaining_debt, 0, ErrorCode::BrokenInvariant);
            }
            transition.position_debt_shares = shares;
            transition.position_debt_principal = principal;
            transition.removed_unrealized_interest = transition.clearance.interest_paid;
            cash_debit_out = amount_out
                .checked_sub(transition.clearance.cash_repaid)
                .and_then(|residual| residual.checked_add(transition.clearance.interest_paid))
                .and_then(|value| value.checked_add(output_fee))
                .ok_or(ErrorCode::MarketMathOverflow)?;
            extra_live_debit_out = transition.clearance.live_debit_for_cash_repay()?;
        }
        SwapCashPolicy::LiquidationQuote => return err!(ErrorCode::InvalidLiquidationSession),
        SwapCashPolicy::SettleLiquidation {
            debt_asset,
            isolated,
            full,
            shares,
            principal_removed,
            debt_reduced,
            repayment,
            insurance_credit,
        } => {
            require_gt!(shares, 0, ErrorCode::InsufficientDebt);
            require!(debt_asset == asset_in.opposite(), ErrorCode::BrokenInvariant);
            require_gte!(amount_out, repayment, ErrorCode::InsufficientAmount);
            let index = debt.borrow_index(debt_asset);
            let (aggregate_shares, aggregate_principal) = match (isolated, debt_asset) {
                (true, MarketAsset::Base) => (&mut debt.isolated_base_shares, &mut debt.isolated_base_principal),
                (true, MarketAsset::Quote) => (&mut debt.isolated_quote_shares, &mut debt.isolated_quote_principal),
                (false, MarketAsset::Base) => (&mut debt.fixed_base_shares, &mut debt.fixed_base_principal),
                (false, MarketAsset::Quote) => (&mut debt.fixed_quote_shares, &mut debt.fixed_quote_principal),
            };
            require_eq!(
                Debt::aggregate_debt_reduction_for_shares(*aggregate_shares, shares, index)?,
                debt_reduced,
                ErrorCode::BrokenInvariant
            );
            let cash_repaid = repayment
                .checked_add(insurance_credit)
                .ok_or(ErrorCode::DebtMathOverflow)?;
            require_gte!(debt_reduced, cash_repaid, ErrorCode::DebtMathOverflow);
            require_gte!(
                principal_removed.saturating_sub(repayment),
                insurance_credit,
                ErrorCode::BrokenInvariant
            );
            if !full {
                require_eq!(cash_repaid, debt_reduced, ErrorCode::BrokenInvariant);
            }
            let principal_paid = repayment
                .min(principal_removed)
                .checked_add(insurance_credit)
                .ok_or(ErrorCode::DebtMathOverflow)?;
            let interest_paid = repayment.saturating_sub(principal_removed);
            *aggregate_shares = aggregate_shares
                .checked_sub(shares)
                .ok_or(ErrorCode::DebtShareMathOverflow)?;
            *aggregate_principal = aggregate_principal
                .checked_sub(principal_removed)
                .ok_or(ErrorCode::DebtMathOverflow)?;
            transition.clearance = DebtClearance {
                shares_burned: shares,
                cash_repaid,
                debt_reduced,
                aggregate_debt_reduced: cash_repaid,
                principal_paid,
                interest_paid,
                position_principal_reduced: principal_removed,
                remaining_debt: 0,
            };
            transition.writeoff = DebtWriteoff {
                debt_written_off: debt_reduced - cash_repaid,
                aggregate_debt_written_off: debt_reduced - cash_repaid,
                principal_written_off: principal_removed.saturating_sub(principal_paid),
                ..DebtWriteoff::default()
            };
            transition.removed_unrealized_interest = interest_paid;
            if full {
                debt_curve_reserve_before_share_removal =
                    Some(supplied_debt_curve_reserve_before_share_removal.ok_or(ErrorCode::BrokenInvariant)?);
            }
            let cash = post.cash_reserve_mut(debt_asset);
            *cash = cash.checked_add(insurance_credit).ok_or(ErrorCode::ReserveOverflow)?;
            cash_debit_out = gross_amount_out
                .checked_sub(repayment)
                .and_then(|v| v.checked_add(interest_paid))
                .ok_or(ErrorCode::CashReserveUnderflow)?;
            extra_live_debit_out = interest_paid;
        }
        SwapCashPolicy::Liquidate {
            debt_asset,
            debt_shares,
            debt_principal,
            insurance_credit,
        } => {
            require!(debt_asset == asset_in.opposite(), ErrorCode::BrokenInvariant);
            let full_repayment = debt.isolated_repayment_for_max(debt_asset, debt_shares, u64::MAX)?;
            let position_principal = u64::try_from(debt_principal).map_err(|_| ErrorCode::DebtMathOverflow)?;
            require_gte!(
                full_repayment.cash_repaid,
                position_principal,
                ErrorCode::DebtMathOverflow
            );
            require_gte!(
                full_repayment.cash_repaid.saturating_sub(amount_out),
                insurance_credit,
                ErrorCode::BrokenInvariant
            );
            let repay_credit = amount_out
                .min(full_repayment.cash_repaid)
                .checked_add(insurance_credit)
                .ok_or(ErrorCode::DebtMathOverflow)?;
            let (principal_paid, interest_paid) =
                crate::math::realized_interest_split(repay_credit, full_repayment.cash_repaid as u128, debt_principal)?;
            transition.clearance = DebtClearance {
                shares_burned: debt_shares,
                cash_repaid: repay_credit,
                debt_reduced: full_repayment.position_debt_reduced,
                aggregate_debt_reduced: repay_credit,
                principal_paid,
                interest_paid,
                remaining_debt: 0,
                position_principal_reduced: position_principal,
            };
            transition.writeoff = DebtWriteoff {
                shares_written_off: 0,
                debt_written_off: full_repayment.position_debt_reduced.saturating_sub(repay_credit),
                aggregate_debt_written_off: full_repayment
                    .cash_repaid
                    .checked_sub(repay_credit)
                    .ok_or(ErrorCode::DebtMathOverflow)?,
                principal_written_off: position_principal.saturating_sub(principal_paid),
            };
            debt_curve_reserve_before_share_removal =
                Some(supplied_debt_curve_reserve_before_share_removal.ok_or(ErrorCode::BrokenInvariant)?);
            let (aggregate_shares, aggregate_principal) = match debt_asset {
                MarketAsset::Base => (&mut debt.isolated_base_shares, &mut debt.isolated_base_principal),
                MarketAsset::Quote => (&mut debt.isolated_quote_shares, &mut debt.isolated_quote_principal),
            };
            *aggregate_shares = aggregate_shares
                .checked_sub(debt_shares)
                .ok_or(ErrorCode::DebtShareMathOverflow)?;
            *aggregate_principal = aggregate_principal
                .checked_sub(position_principal)
                .ok_or(ErrorCode::DebtMathOverflow)?;
            let cash_reserve = post.cash_reserve_mut(debt_asset);
            *cash_reserve = cash_reserve
                .checked_add(insurance_credit)
                .ok_or(ErrorCode::ReserveOverflow)?;
            cash_debit_out = amount_out
                .saturating_sub(full_repayment.cash_repaid)
                .checked_add(interest_paid)
                .and_then(|value| value.checked_add(output_fee))
                .ok_or(ErrorCode::MarketMathOverflow)?;
            extra_live_debit_out = transition.clearance.live_debit_for_cash_repay()?;
        }
    }

    // Recycle funded interest in the same reserve calculation as the debit.
    // It does not need to exist as cash a second time before debt is cleared.
    let mut restored_advance = 0_u64;
    let mut advanced_removed = 0_u64;
    if !matches!(policy, SwapCashPolicy::Spot | SwapCashPolicy::Borrow { .. }) {
        let asset = asset_in.opposite();
        post.set_debt(debt);
        let unpaid_before = start.unrealized_interest(asset)?;
        let unpaid_after = post.unrealized_interest(asset)?;
        let advance = match asset {
            MarketAsset::Base => &mut post.base_interest_advance,
            MarketAsset::Quote => &mut post.quote_interest_advance,
        };
        let cleared = advance.clear(unpaid_before, unpaid_after, transition.clearance.interest_paid)?;
        restored_advance = cleared
            .repaid
            .checked_add(cleared.protocol_returned)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        advanced_removed = cleared
            .repaid
            .checked_add(cleared.canceled)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        transition.socialized_advance_loss = cleared.canceled - cleared.protocol_returned;
    }

    let asset_in_live = post
        .live_reserve(asset_in)
        .checked_add(amount_in_after_fee)
        .ok_or(ErrorCode::ReserveOverflow)?;
    *post.live_reserve_mut(asset_in) = asset_in_live;
    let asset_in_cash = post
        .cash_reserve_mut(asset_in)
        .checked_add(amount_in_after_fee)
        .ok_or(ErrorCode::ReserveOverflow)?;
    *post.cash_reserve_mut(asset_in) = asset_in_cash;
    let asset_out = asset_in.opposite();
    let asset_out_live = u128::from(post.live_reserve(asset_out))
        .checked_add(u128::from(restored_advance))
        .and_then(|v| v.checked_sub(u128::from(gross_amount_out) + u128::from(extra_live_debit_out)))
        .ok_or(ErrorCode::ReserveUnderflow)?;
    *post.live_reserve_mut(asset_out) = u64::try_from(asset_out_live).map_err(|_| ErrorCode::ReserveOverflow)?;
    let asset_out_cash = u128::from(*post.cash_reserve_mut(asset_out))
        .checked_add(u128::from(restored_advance))
        .and_then(|v| v.checked_sub(u128::from(cash_debit_out)))
        .ok_or(ErrorCode::CashReserveUnderflow)?;
    *post.cash_reserve_mut(asset_out) = u64::try_from(asset_out_cash).map_err(|_| ErrorCode::ReserveOverflow)?;

    if let SwapCashPolicy::Borrow { asset, amount } = policy {
        let aggregate_debt_before = debt.isolated_debt(asset)?;
        transition.added_debt_shares = debt.add_isolated_debt(asset, amount)?;
        let aggregate_debt_after = debt.isolated_debt(asset)?;
        let aggregate_debt_increase = u64::try_from(
            aggregate_debt_after
                .checked_sub(aggregate_debt_before)
                .ok_or(ErrorCode::DebtMathOverflow)?,
        )
        .map_err(|_| ErrorCode::DebtMathOverflow)?;
        if aggregate_debt_increase > amount {
            let adjusted_live = post
                .live_reserve(asset)
                .checked_add(aggregate_debt_increase - amount)
                .ok_or(ErrorCode::ReserveOverflow)?;
            *post.live_reserve_mut(asset) = adjusted_live;
        } else if aggregate_debt_increase < amount {
            let adjusted_live = post
                .live_reserve(asset)
                .checked_sub(amount - aggregate_debt_increase)
                .ok_or(ErrorCode::ReserveUnderflow)?;
            *post.live_reserve_mut(asset) = adjusted_live;
        }
    }
    post.set_debt(debt);

    if let Some(curve_before) = debt_curve_reserve_before_share_removal {
        let debt_asset = match policy {
            SwapCashPolicy::Liquidate { debt_asset, .. }
            | SwapCashPolicy::SettleLiquidation {
                debt_asset, full: true, ..
            } => debt_asset,
            _ => return err!(ErrorCode::BrokenInvariant),
        };
        let expected_curve_after = curve_before
            .checked_sub(gross_amount_out)
            .and_then(|v| v.checked_sub(transition.socialized_advance_loss))
            .ok_or(ErrorCode::ReserveUnderflow)?;
        let curve_after = post.projected_curve_reserve_from(start, debt_asset, curve_before)?;
        require_gte!(curve_after, expected_curve_after, ErrorCode::BrokenInvariant);
        transition.phantom_unpaid_interest = curve_after
            .checked_sub(expected_curve_after)
            .ok_or(ErrorCode::BrokenInvariant)?;
        require_gte!(
            transition.writeoff.aggregate_debt_written_off,
            transition.phantom_unpaid_interest,
            ErrorCode::BrokenInvariant
        );
        transition.removed_unrealized_interest = transition
            .clearance
            .interest_paid
            .checked_add(transition.phantom_unpaid_interest)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        transition.socialized_principal_loss = transition
            .writeoff
            .aggregate_debt_written_off
            .checked_sub(transition.phantom_unpaid_interest)
            .ok_or(ErrorCode::BrokenInvariant)?;
        if transition.phantom_unpaid_interest > 0 {
            let adjusted_live = post
                .live_reserve(debt_asset)
                .checked_sub(transition.phantom_unpaid_interest)
                .ok_or(ErrorCode::ReserveUnderflow)?;
            *post.live_reserve_mut(debt_asset) = adjusted_live;
        }
        let curve_after = post.projected_curve_reserve_from(start, debt_asset, curve_before)?;
        require_eq!(curve_after, expected_curve_after, ErrorCode::BrokenInvariant);
        debt_curve_reserve_after_transition = Some(curve_after);
    }

    transition.removed_unrealized_interest = transition
        .removed_unrealized_interest
        .checked_sub(advanced_removed)
        .ok_or(ErrorCode::BrokenInvariant)?;

    require!(
        debt_curve_reserve_before_share_removal == supplied_debt_curve_reserve_before_share_removal,
        ErrorCode::BrokenInvariant
    );

    Ok(LeverageLifecyclePlan {
        start,
        post,
        policy,
        asset_in,
        amount_in_after_fee,
        amount_out,
        gross_amount_out,
        debt_curve_reserve_before_share_removal,
        debt_curve_reserve_after_transition,
        transition,
    })
}

fn apply_leverage_lifecycle_plan(
    market: &mut Market,
    plan: LeverageLifecyclePlan,
) -> Result<LeverageLifecycleTransition> {
    require!(
        LeverageLifecycleState::capture(market) == plan.start,
        ErrorCode::BrokenInvariant
    );
    let expected = derive_leverage_lifecycle_plan(
        market,
        plan.policy,
        plan.asset_in,
        plan.amount_in_after_fee,
        plan.amount_out,
        plan.gross_amount_out,
    )?;
    require!(expected == plan, ErrorCode::BrokenInvariant);
    commit_leverage_lifecycle_state(market, plan.post);
    Ok(plan.transition)
}

impl LeverageSwapQuote {
    #[inline(always)]
    pub(crate) fn from_amm(quote: AmmSwapQuote, current_slot: u64) -> Self {
        Self {
            asset_in: quote.asset_in.code(),
            quoted_slot: current_slot,
            amount_in: quote.fee.reserve_credit,
            amount_in_after_fee: quote.fee.amount_in_for_quote,
            reserve_input_credit: quote.fee.reserve_input_credit,
            amount_out: quote.amount_out,
            gross_amount_out: quote.gross_amount_out,
            start_price_nad: quote.start_price_nad,
            end_price_nad: quote.end_price_nad,
            reserve_end_price_nad: quote.reserve_end_price_nad,
            decayed_volatility_nad: quote.decayed_volatility_nad,
            post_success_volatility_nad: quote.post_success_volatility_nad,
            fee_credit: quote.fee.claimable_fee_debit,
            fee_breakdown: quote.fee,
        }
    }
}

impl LeverageSwapFeeCredit {
    pub fn from_total_actual_credit(quote: &LeverageSwapQuote, total_credit: u64) -> Result<Self> {
        let (base, distributed_surcharge) = super::amm::split_claimable_fee_credit(&quote.fee_breakdown, total_credit)?;
        Ok(Self {
            base,
            distributed_surcharge,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LeverageOpenReceipt {
    pub borrowed_amount: u64,
    pub debt_amount: u64,
    pub debt_shares: u128,
    pub notional: u64,
    pub collateral_amount: u64,
    pub closeout_value: u64,
    pub equity: u64,
    pub swap: LeverageSwapQuote,
    pub fees: FeesReceipt,
    pub base_hlp_rebalance: HlpRebalanceReceipt,
    pub quote_hlp_rebalance: HlpRebalanceReceipt,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LeverageUpdateReceipt {
    pub borrowed_amount: u64,
    pub debt_delta: i64,
    pub collateral_delta: i64,
    pub debt_amount: u64,
    pub debt_shares: u128,
    pub collateral_amount: u64,
    pub closeout_value: u64,
    pub interest_paid: u64,
    pub owner_residual: u64,
    pub fees: FeesReceipt,
    pub base_hlp_rebalance: HlpRebalanceReceipt,
    pub quote_hlp_rebalance: HlpRebalanceReceipt,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LeverageCloseReceipt {
    pub debt_repaid: u64,
    pub debt_reduced: u64,
    pub interest_paid: u64,
    pub collateral_sold: u64,
    pub closeout_value: u64,
    pub residual: u64,
    pub swap: LeverageSwapQuote,
    pub fees: FeesReceipt,
    pub base_hlp_rebalance: HlpRebalanceReceipt,
    pub quote_hlp_rebalance: HlpRebalanceReceipt,
    pub remaining_debt_amount: u64,
    pub remaining_debt_shares: u128,
    pub remaining_collateral_amount: u64,
    pub remaining_closeout_value: u64,
}

/// Canonical proportional slice closed by a delegated TP/SL order. Debt shares
/// round up so a partial close cannot leave the remaining position more
/// leveraged solely because of share granularity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LeverageCloseSlice {
    pub collateral_amount: u64,
    pub debt_shares: u128,
    pub debt_principal: u128,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LeverageLiquidationReceipt {
    pub debt_repaid: u64,
    pub insurance_drawn: u64,
    pub socialized_loss: u64,
    pub interest_paid: u64,
    pub principal_written_off: u64,
    pub collateral_sold: u64,
    pub closeout_value: u64,
    pub liquidator_amount: u64,
    pub owner_residual: u64,
    pub swap: LeverageSwapQuote,
    pub fees: FeesReceipt,
    pub base_hlp_rebalance: HlpRebalanceReceipt,
    pub quote_hlp_rebalance: HlpRebalanceReceipt,
}

/// Source debit and measured reserve credit from a real insurance-vault
/// transfer. The instruction supplies both after the token CPI completes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LeverageInsuranceDraw {
    pub spent: u64,
    pub credit: u64,
}

impl Market {
    pub fn leverage_close_slice(&self, position: &LeveragePosition, close_bps: u16) -> Result<LeverageCloseSlice> {
        position.require_collateral()?;
        require!(
            close_bps > 0 && close_bps <= BPS_DENOMINATOR,
            ErrorCode::InvalidArgument
        );
        if close_bps == BPS_DENOMINATOR {
            return Ok(LeverageCloseSlice {
                collateral_amount: position.collateral_amount,
                debt_shares: position.debt_shares,
                debt_principal: position.debt_principal,
            });
        }

        let collateral_amount = u64::try_from(
            (position.collateral_amount as u128)
                .checked_mul(close_bps as u128)
                .ok_or(ErrorCode::MarketMathOverflow)?
                / BPS_DENOMINATOR as u128,
        )
        .map_err(|_| ErrorCode::MarketMathOverflow)?;
        require!(
            collateral_amount > 0 && collateral_amount < position.collateral_amount,
            ErrorCode::InsufficientAmount
        );
        let debt_shares = ceil_div(
            position
                .debt_shares
                .checked_mul(collateral_amount as u128)
                .ok_or(ErrorCode::DebtShareMathOverflow)?,
            position.collateral_amount as u128,
        )
        .ok_or(ErrorCode::DebtShareMathOverflow)?;
        if position.debt_shares == 0 {
            return Ok(LeverageCloseSlice {
                collateral_amount,
                debt_shares: 0,
                debt_principal: 0,
            });
        }
        require!(
            debt_shares > 0 && debt_shares < position.debt_shares,
            ErrorCode::InsufficientDebt
        );
        let debt_asset = position.debt_asset()?;
        let total_debt = Debt::shares_to_debt(position.debt_shares, self.debt.borrow_index(debt_asset))?;
        let remaining_debt =
            Debt::shares_to_debt(position.debt_shares - debt_shares, self.debt.borrow_index(debt_asset))?;
        let slice_debt = u64::try_from(total_debt - remaining_debt).map_err(|_| ErrorCode::DebtMathOverflow)?;
        let (slice_principal, _) =
            realized_interest_split(slice_debt, total_debt, position.debt_principal.min(total_debt))?;
        Ok(LeverageCloseSlice {
            collateral_amount,
            debt_shares,
            debt_principal: slice_principal as u128,
        })
    }

    /// Cash available after the same debt/advance kernel used at settlement.
    /// Quote guards must recycle prepaid interest before reserving hLP funding.
    pub(crate) fn preview_leverage_settlement_cash(
        &self,
        policy: SwapCashPolicy,
        asset_in: MarketAsset,
        amount_in_after_fee: u64,
        amount_out: u64,
        gross_amount_out: u64,
    ) -> Result<(u64, u64)> {
        let plan = derive_leverage_lifecycle_plan(
            self,
            policy,
            asset_in,
            amount_in_after_fee,
            amount_out,
            gross_amount_out,
        )?;
        Ok((plan.post.base_cash_reserve, plan.post.quote_cash_reserve))
    }

    pub(crate) fn apply_leverage_lifecycle_transition(
        &mut self,
        policy: SwapCashPolicy,
        asset_in: MarketAsset,
        amount_in_after_fee: u64,
        amount_out: u64,
        gross_amount_out: u64,
    ) -> Result<LeverageLifecycleTransition> {
        let plan = derive_leverage_lifecycle_plan(
            self,
            policy,
            asset_in,
            amount_in_after_fee,
            amount_out,
            gross_amount_out,
        )?;
        apply_leverage_lifecycle_plan(self, plan)
    }

    fn rebase_concentrated_curve_at_current_ordinary_point(&mut self) -> Result<u128> {
        let ordinary = self.ordinary_curve_point_nad()?;
        let parameters = self.config.amm.concentrated_curve_parameters()?;
        self.amm.concentrated_curve_cache = prepare_concentrated_cache_at_point(
            ordinary.base_reserve,
            ordinary.quote_reserve,
            self.current_curve_center_price_nad()?,
            parameters,
        )?;
        let curve_depth_nad = self
            .amm
            .concentrated_curve_cache
            .tail_liquidity
            .checked_add(self.amm.concentrated_curve_cache.concentrated_liquidity)
            .ok_or(ErrorCode::InvariantOverflow)?;
        self.curve_depth_per_share_nad(curve_depth_nad)
    }

    pub(crate) fn rebase_concentrated_curve_after_terminal_hlp_loss(&mut self) -> Result<()> {
        let curve_depth_per_share_nad = self.rebase_concentrated_curve_at_current_ordinary_point()?;
        self.amm.checkpoint_recenter_or_loss(curve_depth_per_share_nad);
        self.defer_amm_retention_target()?;
        self.advance_curve_revision()
    }

    pub(crate) fn apply_leverage_socialized_loss(
        &mut self,
        debt_asset: MarketAsset,
        transition: LeverageLifecycleTransition,
        _current_slot: u64,
    ) -> Result<HlpSocializedLossRebase> {
        let loss = transition.socialized_principal_loss;
        if loss == 0 && transition.socialized_advance_loss == 0 {
            return Ok(HlpSocializedLossRebase::default());
        }
        require!(self.amm.initialized, ErrorCode::BrokenInvariant);
        let start_price = self
            .current_concentrated_spot_price_nad()?
            .ok_or(ErrorCode::BrokenInvariant)?;
        let start_prices = hlp_curve_prices_from_base_price_nad(start_price as u128)?;
        let (base_before, quote_before) = current_hlp_signed_navs_with_prices(self, start_prices)?;

        let post_loss_live_reserve = self
            .side(debt_asset)
            .reserves
            .live_reserve
            .checked_sub(loss)
            .ok_or(ErrorCode::ReserveUnderflow)?;
        self.side_mut(debt_asset).reserves.live_reserve = post_loss_live_reserve;

        // A socialized loss changes the reserve point, not the sticky center
        // or concentration policy. Reconstruct the unique positive concentrated
        // curve through that point in O(1); the following algebraic hLP
        // transition then restores zero opposite-asset exposure.
        let curve_depth_per_share_nad = self.rebase_concentrated_curve_at_current_ordinary_point()?;
        self.amm.checkpoint_recenter_or_loss(curve_depth_per_share_nad);
        self.defer_amm_retention_target()?;
        self.advance_curve_revision()?;

        let end_price = self
            .current_concentrated_spot_price_nad()?
            .ok_or(ErrorCode::BrokenInvariant)?;
        let end_prices = hlp_curve_prices_from_base_price_nad(end_price as u128)?;
        let (base_after, quote_after) = current_hlp_signed_navs_with_prices(self, end_prices)?;
        Ok(HlpSocializedLossRebase {
            base_nav_delta_nad: base_after
                .checked_sub(base_before)
                .ok_or(ErrorCode::MarketMathOverflow)?,
            quote_nav_delta_nad: quote_after
                .checked_sub(quote_before)
                .ok_or(ErrorCode::MarketMathOverflow)?,
        })
    }

    pub fn quote_leverage_swap(
        &self,
        asset_in: MarketAsset,
        amount_in: u64,
        clock: crate::clock::MarketClock,
    ) -> Result<LeverageSwapQuote> {
        let after_launch = self.config.start_time.saturating_add(
            self.config
                .amm
                .launch_fee_duration_seconds
                .max(self.config.amm.launch_rate_limit_duration_seconds)
                .min(i64::MAX as u64) as i64,
        );
        self.quote_leverage_swap_at_time(asset_in, amount_in, clock, after_launch)
    }

    pub fn quote_leverage_swap_at_time(
        &self,
        asset_in: MarketAsset,
        amount_in: u64,
        clock: crate::clock::MarketClock,
        current_unix_timestamp: i64,
    ) -> Result<LeverageSwapQuote> {
        let current_slot = clock.slot;
        let pre_state = self.dynamic_fee_pre_state(clock)?;
        let preliminary = self.preliminary_swap_inputs_for_state_at_time(
            asset_in,
            amount_in,
            clock,
            current_unix_timestamp,
            pre_state,
        )?;
        let quote = self
            .quote_concentrated_integrated_with_fee(asset_in, amount_in, preliminary, 0)?
            .ok_or(ErrorCode::BrokenInvariant)?
            .as_swap_quote(asset_in);
        Ok(LeverageSwapQuote::from_amm(quote, current_slot))
    }

    pub(super) fn validate_leverage_swap_quote(
        &self,
        quote: LeverageSwapQuote,
        asset_in: MarketAsset,
        clock: crate::clock::MarketClock,
    ) -> Result<()> {
        let current_slot = clock.slot;
        let fee = quote.fee_breakdown;
        require!(quote.asset_in == asset_in.code(), ErrorCode::BrokenInvariant);
        require_eq!(quote.quoted_slot, current_slot, ErrorCode::BrokenInvariant);
        require!(quote.amount_in > 0 && quote.amount_out > 0, ErrorCode::BrokenInvariant);
        require_eq!(fee.reserve_credit, quote.amount_in, ErrorCode::BrokenInvariant);
        require_eq!(
            fee.amount_in_for_quote,
            quote.amount_in_after_fee,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            fee.reserve_input_credit,
            quote.reserve_input_credit,
            ErrorCode::BrokenInvariant
        );
        require_eq!(fee.claimable_fee_debit, quote.fee_credit, ErrorCode::BrokenInvariant);
        require_eq!(fee.gross_amount_out, quote.gross_amount_out, ErrorCode::BrokenInvariant);
        require_gte!(BPS_DENOMINATOR, fee.protocol_fee_bps, ErrorCode::BrokenInvariant);
        require_eq!(
            fee.compounded_base_fee_debit
                .checked_add(fee.compounded_dynamic_surcharge_debit)
                .ok_or(ErrorCode::FeeMathOverflow)?,
            fee.compounded_fee_debit,
            ErrorCode::BrokenInvariant
        );
        require_gte!(
            fee.base_fee_debit,
            fee.compounded_base_fee_debit,
            ErrorCode::BrokenInvariant
        );
        require_gte!(
            fee.distributed_surcharge_debit,
            fee.compounded_dynamic_surcharge_debit,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            fee.base_fee_debit
                .checked_sub(fee.compounded_base_fee_debit)
                .and_then(|value| {
                    fee.distributed_surcharge_debit
                        .checked_sub(fee.compounded_dynamic_surcharge_debit)
                        .and_then(|dynamic| value.checked_add(dynamic))
                })
                .ok_or(ErrorCode::FeeMathOverflow)?,
            fee.claimable_fee_debit,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            fee.base_fee_debit
                .checked_add(fee.dynamic_surcharge_debit)
                .ok_or(ErrorCode::FeeMathOverflow)?,
            fee.total_fee_debit,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            fee.retained_surcharge
                .checked_add(fee.distributed_surcharge_debit)
                .ok_or(ErrorCode::FeeMathOverflow)?,
            fee.dynamic_surcharge_debit,
            ErrorCode::BrokenInvariant
        );
        if self.amm.retain_dynamic_surcharge {
            require_eq!(
                fee.retained_surcharge,
                fee.dynamic_surcharge_debit,
                ErrorCode::BrokenInvariant
            );
            require_eq!(fee.distributed_surcharge_debit, 0, ErrorCode::BrokenInvariant);
        } else {
            require_eq!(fee.retained_surcharge, 0, ErrorCode::BrokenInvariant);
            require_eq!(
                fee.distributed_surcharge_debit,
                fee.dynamic_surcharge_debit,
                ErrorCode::BrokenInvariant
            );
        }
        let fee_asset = MarketAsset::try_from_code(fee.fee_asset)?;
        if fee_asset == asset_in {
            require_eq!(quote.amount_out, quote.gross_amount_out, ErrorCode::BrokenInvariant);
            require_eq!(
                fee.amount_in_for_quote
                    .checked_add(fee.total_fee_debit)
                    .ok_or(ErrorCode::FeeMathOverflow)?,
                fee.reserve_credit,
                ErrorCode::BrokenInvariant
            );
            require_eq!(
                fee.reserve_input_credit
                    .checked_add(fee.claimable_fee_debit)
                    .ok_or(ErrorCode::FeeMathOverflow)?,
                fee.reserve_credit,
                ErrorCode::BrokenInvariant
            );
        } else {
            require!(fee_asset == asset_in.opposite(), ErrorCode::BrokenInvariant);
            require_eq!(fee.amount_in_for_quote, fee.reserve_credit, ErrorCode::BrokenInvariant);
            require_eq!(fee.reserve_input_credit, fee.reserve_credit, ErrorCode::BrokenInvariant);
            require_eq!(
                quote
                    .amount_out
                    .checked_add(fee.claimable_fee_debit)
                    .and_then(|value| value.checked_add(fee.retained_surcharge))
                    .and_then(|value| value.checked_add(fee.compounded_fee_debit))
                    .ok_or(ErrorCode::FeeMathOverflow)?,
                quote.gross_amount_out,
                ErrorCode::BrokenInvariant
            );
        }
        Ok(())
    }

    pub fn open_leverage(
        &mut self,
        position: &mut LeveragePosition,
        owner: Pubkey,
        market: Pubkey,
        namespace_authority: Pubkey,
        position_id: Pubkey,
        referral_partner: Pubkey,
        referral_interest_share_bps: u16,
        debt_asset: MarketAsset,
        margin_credit: u64,
        multiplier_bps: u64,
        collateral_credit: u64,
        mut prepared_swap: PreparedLeverageSwap,
        swap_fee_credit: LeverageSwapFeeCredit,
        opened_at: i64,
        opened_slot: u64,
        bump: u8,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        collateral_fee: LeverageCollateralFee,
    ) -> Result<LeverageOpenReceipt> {
        let swap = prepared_swap.leverage_quote();
        require!(margin_credit > 0, ErrorCode::AmountZero);
        require!(multiplier_bps > BPS_DENOMINATOR as u64, ErrorCode::InvalidArgument);
        require!(
            multiplier_bps <= LEVERAGE_MAX_MULTIPLIER_BPS,
            ErrorCode::LeverageMultiplierTooHigh
        );
        let borrowed_amount = leverage_debt_from_margin(margin_credit, multiplier_bps)?;
        let notional = margin_credit
            .checked_add(borrowed_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.ensure_amm_initialized(crate::clock::MarketClock::from_timestamp(opened_slot, opened_at)?)?;
        require_eq!(swap.amount_in, notional, ErrorCode::BrokenInvariant);
        require_gte!(swap.amount_out, collateral_credit, ErrorCode::SlippageExceeded);
        require!(collateral_credit > 0, ErrorCode::InsufficientOutputAmount);

        let margin_potential_before = self.leverage_admission_potential(debt_asset.opposite())?;
        let cash_policy = SwapCashPolicy::Borrow {
            asset: debt_asset,
            amount: borrowed_amount,
        };
        let finalized = prepared_swap.apply(
            self,
            cash_policy,
            swap_fee_credit,
            crate::clock::MarketClock::from_timestamp(opened_slot, opened_at)?,
            protocol_fee_bps,
            protocol_auction_split,
            None,
        )?;
        let lifecycle = finalized.lifecycle;
        let fees = finalized.fees;
        let (base_hlp_rebalance, quote_hlp_rebalance) = (finalized.base_rebalance, finalized.quote_rebalance);
        let debt_shares = lifecycle.added_debt_shares;
        require_gt!(debt_shares, 0, ErrorCode::BrokenInvariant);
        position.initialize(
            owner,
            market,
            namespace_authority,
            position_id,
            referral_partner,
            referral_interest_share_bps,
            debt_asset,
            collateral_credit,
            margin_credit,
            notional,
            borrowed_amount,
            debt_shares,
            multiplier_bps,
            opened_at,
            opened_slot,
            bump,
        );
        position.open_curve_revision = self.curve_revision;
        self.retain_leverage_admission(position, collateral_credit, margin_potential_before, true)?;
        let closeout_value = self.require_position_initial_leverage_health(
            position,
            crate::clock::MarketClock::from_timestamp(opened_slot, opened_at)?,
            opened_at,
            collateral_fee,
        )?;
        let equity = closeout_value
            .checked_sub(borrowed_amount)
            .ok_or(ErrorCode::LeverageInitialMarginTooLow)?;
        Ok(LeverageOpenReceipt {
            borrowed_amount,
            debt_amount: borrowed_amount,
            debt_shares,
            notional,
            collateral_amount: collateral_credit,
            closeout_value,
            equity,
            swap,
            fees,
            base_hlp_rebalance,
            quote_hlp_rebalance,
        })
    }

    pub fn increase_leverage(
        &mut self,
        position: &mut LeveragePosition,
        borrowed_amount: u64,
        margin_credit: u64,
        collateral_credit: u64,
        mut prepared_swap: PreparedLeverageSwap,
        swap_fee_credit: LeverageSwapFeeCredit,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        clock: crate::clock::MarketClock,
        current_unix_timestamp: i64,
        collateral_fee: LeverageCollateralFee,
    ) -> Result<LeverageUpdateReceipt> {
        let swap = prepared_swap.leverage_quote();
        position.require_collateral()?;
        let was_repaid = position.debt_shares == 0;
        let notional = borrowed_amount
            .checked_add(margin_credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require_gt!(notional, 0, ErrorCode::AmountZero);
        require!(collateral_credit > 0, ErrorCode::InsufficientOutputAmount);
        let debt_asset = position.debt_asset()?;
        self.ensure_amm_initialized(clock)?;
        require_eq!(swap.amount_in, notional, ErrorCode::BrokenInvariant);
        require_gte!(swap.amount_out, collateral_credit, ErrorCode::SlippageExceeded);
        let margin_potential_before = self.leverage_admission_potential(debt_asset.opposite())?;
        let cash_policy = if borrowed_amount == 0 {
            SwapCashPolicy::Spot
        } else {
            SwapCashPolicy::Borrow {
                asset: debt_asset,
                amount: borrowed_amount,
            }
        };
        let finalized = prepared_swap.apply(
            self,
            cash_policy,
            swap_fee_credit,
            clock,
            protocol_fee_bps,
            protocol_auction_split,
            None,
        )?;
        let lifecycle = finalized.lifecycle;
        let fees = finalized.fees;
        let (base_hlp_rebalance, quote_hlp_rebalance) = (finalized.base_rebalance, finalized.quote_rebalance);
        let added_shares = lifecycle.added_debt_shares;
        require!(borrowed_amount == 0 || added_shares > 0, ErrorCode::BrokenInvariant);
        position.debt_shares = position
            .debt_shares
            .checked_add(added_shares)
            .ok_or(ErrorCode::DebtShareMathOverflow)?;
        position.debt_principal = position
            .debt_principal
            .checked_add(borrowed_amount as u128)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        position.credit_collateral(collateral_credit)?;
        if position.debt_shares > 0 {
            // Fully repaid collateral was removed from aggregate exposure. A new
            // borrow must admit the entire position at current depth/crowding.
            let admitted = if was_repaid {
                position.collateral_amount
            } else {
                collateral_credit
            };
            self.retain_leverage_admission(position, admitted, margin_potential_before, was_repaid)?;
        }
        let closeout_value =
            self.require_position_initial_leverage_health(position, clock, current_unix_timestamp, collateral_fee)?;
        Ok(LeverageUpdateReceipt {
            borrowed_amount,
            debt_delta: i64::try_from(borrowed_amount).map_err(|_| ErrorCode::Overflow)?,
            collateral_delta: i64::try_from(collateral_credit).map_err(|_| ErrorCode::Overflow)?,
            debt_amount: position.debt_amount(&self.debt)?,
            debt_shares: position.debt_shares,
            collateral_amount: position.collateral_amount,
            closeout_value,
            interest_paid: 0,
            owner_residual: 0,
            fees,
            base_hlp_rebalance,
            quote_hlp_rebalance,
        })
    }

    pub fn decrease_leverage(
        &mut self,
        position: &mut LeveragePosition,
        collateral_debit: u64,
        min_repay_out: u64,
        max_surplus_out: u64,
        prepared_swap: &mut PreparedLeverageSwap,
        swap_fee_credit: LeverageSwapFeeCredit,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        clock: crate::clock::MarketClock,
        current_unix_timestamp: i64,
        collateral_fee: LeverageCollateralFee,
    ) -> Result<LeverageUpdateReceipt> {
        let swap = prepared_swap.leverage_quote();
        position.require_open()?;
        require!(collateral_debit > 0, ErrorCode::AmountZero);
        require_gt!(
            position.collateral_amount,
            collateral_debit,
            ErrorCode::InsufficientAmount
        );
        let debt_asset = position.debt_asset()?;
        let collateral_asset = debt_asset.opposite();
        self.ensure_amm_initialized(clock)?;
        require!(
            swap.amount_in > 0 && swap.amount_in <= collateral_debit,
            ErrorCode::BrokenInvariant
        );
        let repayment = self
            .debt
            .isolated_repayment_for_max(debt_asset, position.debt_shares, swap.amount_out)?;
        require_gte!(repayment.cash_repaid, min_repay_out, ErrorCode::SlippageExceeded);
        let owner_residual = swap
            .amount_out
            .checked_sub(repayment.cash_repaid)
            .ok_or(ErrorCode::BrokenInvariant)?;
        // Explicitly bound any unneeded swap output. In particular, a 1x
        // request must not silently become a large equity withdrawal.
        require_gte!(max_surplus_out, owner_residual, ErrorCode::SlippageExceeded);
        let collateral_after = position
            .collateral_amount
            .checked_sub(collateral_debit)
            .ok_or(ErrorCode::InsufficientAmount)?;
        let debt_after = repayment.remaining_position_debt;
        let pre_finalize_closeout_value = if debt_after == 0 {
            0
        } else {
            self.post_swap_closeout_quote_with_quote(
                collateral_asset,
                swap,
                collateral_asset,
                collateral_fee.unwind_credit(collateral_after)?,
                clock,
                current_unix_timestamp,
            )?
            .amount_out
        };
        let maintenance_bps = position.margin_terms.maintenance_bps(collateral_after)?;
        if debt_after > 0 {
            require_leverage_not_liquidatable(pre_finalize_closeout_value, debt_after, maintenance_bps)?;
            self.require_ema_leverage_not_liquidatable(
                collateral_asset,
                collateral_after,
                debt_after,
                collateral_fee,
                maintenance_bps,
            )?;
        }
        let cash_policy = SwapCashPolicy::Decrease {
            debt_asset,
            debt_shares: position.debt_shares,
            debt_principal: position.debt_principal,
        };
        let finalized = prepared_swap.apply(
            self,
            cash_policy,
            swap_fee_credit,
            clock,
            protocol_fee_bps,
            protocol_auction_split,
            None,
        )?;
        let lifecycle = finalized.lifecycle;
        let fees = finalized.fees;
        let (base_hlp_rebalance, quote_hlp_rebalance) = (finalized.base_rebalance, finalized.quote_rebalance);
        let clearance = lifecycle.clearance;
        position.debt_shares = lifecycle.position_debt_shares;
        position.debt_principal = lifecycle.position_debt_principal;
        self.reduce_leverage_collateral(position, collateral_debit)?;
        if position.debt_shares == 0 {
            self.remove_leverage_exposure(collateral_asset, position.collateral_amount)?;
            position.margin_terms.admission_equity_collateral_nad = 0;
        }
        let closeout_value = if clearance.remaining_debt == 0 {
            0
        } else {
            self.leverage_closeout_value_at_time_with_fee(position, clock, current_unix_timestamp, collateral_fee)?
        };
        if clearance.remaining_debt > 0 {
            require_leverage_not_liquidatable(closeout_value, clearance.remaining_debt, maintenance_bps)?;
            self.require_ema_leverage_not_liquidatable(
                collateral_asset,
                position.collateral_amount,
                clearance.remaining_debt,
                collateral_fee,
                maintenance_bps,
            )?;
        }
        Ok(LeverageUpdateReceipt {
            borrowed_amount: 0,
            debt_delta: -i64::try_from(clearance.debt_reduced).map_err(|_| ErrorCode::Overflow)?,
            collateral_delta: -i64::try_from(collateral_debit).map_err(|_| ErrorCode::Overflow)?,
            debt_amount: clearance.remaining_debt,
            debt_shares: position.debt_shares,
            collateral_amount: position.collateral_amount,
            closeout_value,
            interest_paid: clearance.interest_paid,
            owner_residual,
            fees,
            base_hlp_rebalance,
            quote_hlp_rebalance,
        })
    }

    pub fn close_leverage(
        &mut self,
        position: &mut LeveragePosition,
        min_residual_out: u64,
        prepared_swap: PreparedLeverageSwap,
        swap_fee_credit: LeverageSwapFeeCredit,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        clock: crate::clock::MarketClock,
        collateral_fee: LeverageCollateralFee,
    ) -> Result<Box<LeverageCloseReceipt>> {
        let slice = self.leverage_close_slice(position, BPS_DENOMINATOR)?;
        self.close_leverage_slice(
            position,
            slice,
            min_residual_out,
            prepared_swap,
            swap_fee_credit,
            protocol_fee_bps,
            protocol_auction_split,
            clock,
            None,
            collateral_fee,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn partial_close_leverage(
        &mut self,
        position: &mut LeveragePosition,
        close_bps: u16,
        min_residual_out: u64,
        prepared_swap: PreparedLeverageSwap,
        swap_fee_credit: LeverageSwapFeeCredit,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        clock: crate::clock::MarketClock,
        current_unix_timestamp: i64,
        collateral_fee: LeverageCollateralFee,
    ) -> Result<Box<LeverageCloseReceipt>> {
        require!(close_bps < BPS_DENOMINATOR, ErrorCode::InvalidArgument);
        let slice = self.leverage_close_slice(position, close_bps)?;
        self.close_leverage_slice(
            position,
            slice,
            min_residual_out,
            prepared_swap,
            swap_fee_credit,
            protocol_fee_bps,
            protocol_auction_split,
            clock,
            Some(current_unix_timestamp),
            collateral_fee,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn close_leverage_slice(
        &mut self,
        position: &mut LeveragePosition,
        slice: LeverageCloseSlice,
        min_residual_out: u64,
        mut prepared_swap: PreparedLeverageSwap,
        swap_fee_credit: LeverageSwapFeeCredit,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        clock: crate::clock::MarketClock,
        partial_close_unix_timestamp: Option<i64>,
        collateral_fee: LeverageCollateralFee,
    ) -> Result<Box<LeverageCloseReceipt>> {
        let swap = prepared_swap.leverage_quote();
        position.require_collateral()?;
        let had_debt = position.debt_shares > 0;
        let debt_asset = position.debt_asset()?;
        require!(
            slice.collateral_amount > 0
                && slice.collateral_amount <= position.collateral_amount
                && (!had_debt || slice.debt_shares > 0)
                && slice.debt_shares <= position.debt_shares
                && slice.debt_principal <= position.debt_principal,
            ErrorCode::BrokenInvariant
        );
        let is_full_close = slice.collateral_amount == position.collateral_amount;
        require!(
            is_full_close == partial_close_unix_timestamp.is_none(),
            ErrorCode::BrokenInvariant
        );
        let debt_amount = if had_debt {
            self.debt
                .isolated_repayment_for_max(debt_asset, slice.debt_shares, u64::MAX)?
                .cash_repaid
        } else {
            0
        };
        let collateral_sold = slice.collateral_amount;
        self.ensure_amm_initialized(clock)?;
        require!(
            swap.amount_in > 0 && swap.amount_in <= collateral_sold,
            ErrorCode::BrokenInvariant
        );
        require_gte!(swap.amount_out, debt_amount, ErrorCode::InsufficientAmount);
        let residual = swap
            .amount_out
            .checked_sub(debt_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require_gte!(residual, min_residual_out, ErrorCode::SlippageExceeded);
        let cash_policy = SwapCashPolicy::Close {
            debt_asset,
            debt_shares: position.debt_shares,
            debt_principal: position.debt_principal,
            shares_to_burn: slice.debt_shares,
        };
        let finalized = prepared_swap.apply(
            self,
            cash_policy,
            swap_fee_credit,
            clock,
            protocol_fee_bps,
            protocol_auction_split,
            None,
        )?;
        let lifecycle = finalized.lifecycle;
        let fees = finalized.fees;
        let (base_hlp_rebalance, quote_hlp_rebalance) = (finalized.base_rebalance, finalized.quote_rebalance);
        let clearance = lifecycle.clearance;
        require_eq!(
            clearance.position_principal_reduced as u128,
            slice.debt_principal,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            lifecycle.position_debt_shares,
            position.debt_shares - slice.debt_shares,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            lifecycle.position_debt_principal,
            position.debt_principal - slice.debt_principal,
            ErrorCode::BrokenInvariant
        );
        position.debt_shares = lifecycle.position_debt_shares;
        position.debt_principal = lifecycle.position_debt_principal;
        require_eq!(clearance.cash_repaid, debt_amount, ErrorCode::BrokenInvariant);
        require_eq!(
            clearance.remaining_debt,
            position.debt_amount(&self.debt)?,
            ErrorCode::BrokenInvariant
        );
        if had_debt {
            self.reduce_leverage_collateral(position, collateral_sold)?;
        } else {
            position.debit_collateral(collateral_sold)?;
        }
        let (remaining_debt_amount, remaining_closeout_value) = if let Some(current_unix_timestamp) =
            partial_close_unix_timestamp
        {
            position.require_collateral()?;
            let remaining_debt = position.debt_amount(&self.debt)?;
            let remaining_closeout = if !had_debt {
                0
            } else {
                self.leverage_closeout_value_at_time_with_fee(position, clock, current_unix_timestamp, collateral_fee)?
            };
            if had_debt {
                let maintenance_bps = position.margin_terms.maintenance_bps(position.collateral_amount)?;
                require_leverage_not_liquidatable(remaining_closeout, remaining_debt, maintenance_bps)?;
                self.require_ema_leverage_not_liquidatable(
                    position.collateral_asset()?,
                    position.collateral_amount,
                    remaining_debt,
                    collateral_fee,
                    maintenance_bps,
                )?;
            }
            (remaining_debt, remaining_closeout)
        } else {
            require_eq!(position.debt_shares, 0, ErrorCode::BrokenInvariant);
            require_eq!(position.debt_principal, 0, ErrorCode::BrokenInvariant);
            require_eq!(position.collateral_amount, 0, ErrorCode::BrokenInvariant);
            (0, 0)
        };
        Ok(Box::new(LeverageCloseReceipt {
            debt_repaid: clearance.cash_repaid,
            debt_reduced: clearance.debt_reduced,
            interest_paid: clearance.interest_paid,
            collateral_sold,
            closeout_value: swap.amount_out,
            residual,
            swap,
            fees,
            base_hlp_rebalance,
            quote_hlp_rebalance,
            remaining_debt_amount,
            remaining_debt_shares: position.debt_shares,
            remaining_collateral_amount: position.collateral_amount,
            remaining_closeout_value,
        }))
    }

    pub fn liquidate_leverage_position(
        &mut self,
        position: &mut LeveragePosition,
        prepared_swap: Option<PreparedLeverageSwap>,
        measured_unwind_credit: u64,
        swap_fee_credit: LeverageSwapFeeCredit,
        insurance: LeverageInsuranceDraw,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        clock: crate::clock::MarketClock,
    ) -> Result<LeverageLiquidationReceipt> {
        self.liquidate_leverage_position_with_pending_credit(
            position,
            prepared_swap,
            measured_unwind_credit,
            None,
            swap_fee_credit,
            insurance,
            protocol_fee_bps,
            protocol_auction_split,
            clock,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn liquidate_leverage_position_with_pending_credit(
        &mut self,
        position: &mut LeveragePosition,
        prepared_swap: Option<PreparedLeverageSwap>,
        measured_unwind_credit: u64,
        pending_unwind_credit: Option<u64>,
        swap_fee_credit: LeverageSwapFeeCredit,
        insurance: LeverageInsuranceDraw,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        clock: crate::clock::MarketClock,
    ) -> Result<LeverageLiquidationReceipt> {
        let current_slot = clock.slot;
        require_gte!(insurance.spent, insurance.credit, ErrorCode::BrokenInvariant);
        require!(insurance.spent > 0 || insurance.credit == 0, ErrorCode::BrokenInvariant);
        if prepared_swap.is_none() {
            require_eq!(measured_unwind_credit, 0, ErrorCode::BrokenInvariant);
            require!(
                swap_fee_credit == LeverageSwapFeeCredit::default(),
                ErrorCode::BrokenInvariant
            );
            position.require_open()?;
            let debt_asset = position.debt_asset()?;
            let collateral_asset = debt_asset.opposite();
            let debt_amount = position.debt_amount(&self.debt)?;
            require_gt!(debt_amount, 0, ErrorCode::ZeroDebtAmount);
            let collateral_sold = position.collateral_amount;
            self.ensure_amm_initialized(clock)?;

            let lifecycle = self.apply_leverage_lifecycle_transition(
                SwapCashPolicy::Liquidate {
                    debt_asset,
                    debt_shares: position.debt_shares,
                    debt_principal: position.debt_principal,
                    insurance_credit: insurance.credit,
                },
                collateral_asset,
                0,
                0,
                0,
            )?;
            require_eq!(
                lifecycle.clearance.cash_repaid,
                insurance.credit,
                ErrorCode::BrokenInvariant
            );
            if insurance.spent > 0 {
                self.insurance.consume_draw(debt_asset, insurance.spent, clock)?;
            }
            self.apply_leverage_socialized_loss(debt_asset, lifecycle, current_slot)?;
            let transition = prepare_concentrated_hlp_transition_at_current_state(self)?;
            let (base_hlp_rebalance, quote_hlp_rebalance) = transition.consume(self)?;
            self.finalize_amm_socialized_loss_and_observe_risk(clock)?;
            self.assert_market_invariants()?;

            position.debt_shares = lifecycle.position_debt_shares;
            position.debt_principal = lifecycle.position_debt_principal;
            self.reduce_leverage_collateral(position, collateral_sold)?;
            return Ok(LeverageLiquidationReceipt {
                debt_repaid: lifecycle.clearance.cash_repaid,
                insurance_drawn: insurance.credit,
                socialized_loss: lifecycle.socialized_principal_loss,
                interest_paid: lifecycle.clearance.interest_paid,
                principal_written_off: lifecycle.writeoff.principal_written_off,
                collateral_sold,
                closeout_value: 0,
                liquidator_amount: 0,
                owner_residual: 0,
                swap: LeverageSwapQuote {
                    asset_in: collateral_asset.code(),
                    quoted_slot: current_slot,
                    ..LeverageSwapQuote::default()
                },
                fees: FeesReceipt::default(),
                base_hlp_rebalance,
                quote_hlp_rebalance,
            });
        }
        let mut prepared_swap = prepared_swap.ok_or(ErrorCode::BrokenInvariant)?;
        let swap = prepared_swap.leverage_quote();
        position.require_open()?;
        let debt_asset = position.debt_asset()?;
        let debt_amount = position.debt_amount(&self.debt)?;
        require_gt!(debt_amount, 0, ErrorCode::ZeroDebtAmount);
        let collateral_sold = position.collateral_amount;
        self.ensure_amm_initialized(clock)?;
        require!(
            swap.amount_in > 0 && swap.amount_in <= collateral_sold,
            ErrorCode::BrokenInvariant
        );
        require_eq!(swap.amount_in, measured_unwind_credit, ErrorCode::BrokenInvariant);
        let maintenance_bps = position.margin_terms.maintenance_bps(position.collateral_amount)?;
        let collateral_asset = position.collateral_asset()?;
        let pending_unhealthy = pending_unwind_credit
            .map(|credit| {
                require_gte!(position.collateral_amount, credit, ErrorCode::BrokenInvariant);
                Ok(
                    self.ema_leverage_margin_bps_for_credit(collateral_asset, credit, debt_amount)?
                        <= u128::from(maintenance_bps),
                )
            })
            .transpose()?
            .unwrap_or(false);
        if !pending_unhealthy {
            require!(
                self.ema_leverage_margin_bps_for_credit(collateral_asset, measured_unwind_credit, debt_amount,)?
                    <= u128::from(maintenance_bps),
                ErrorCode::LeveragePositionNotLiquidatable
            );
        }

        let cash_policy = SwapCashPolicy::Liquidate {
            debt_asset,
            debt_shares: position.debt_shares,
            debt_principal: position.debt_principal,
            insurance_credit: insurance.credit,
        };
        require!(
            prepared_swap.cash_policy
                == SwapCashPolicy::Liquidate {
                    debt_asset,
                    debt_shares: position.debt_shares,
                    debt_principal: position.debt_principal,
                    insurance_credit: 0,
                },
            ErrorCode::BrokenInvariant
        );
        prepared_swap.cash_policy = cash_policy;
        let finalized = prepared_swap.apply(
            self,
            cash_policy,
            swap_fee_credit,
            clock,
            protocol_fee_bps,
            protocol_auction_split,
            None,
        )?;
        let lifecycle = finalized.lifecycle;
        let fees = finalized.fees;
        let (base_hlp_rebalance, quote_hlp_rebalance) = (finalized.base_rebalance, finalized.quote_rebalance);
        let clearance = lifecycle.clearance;
        let writeoff = lifecycle.writeoff;
        if insurance.spent > 0 {
            self.insurance.consume_draw(debt_asset, insurance.spent, clock)?;
        }
        position.debt_shares = lifecycle.position_debt_shares;
        position.debt_principal = lifecycle.position_debt_principal;
        let full_cash_repayment = clearance
            .cash_repaid
            .checked_add(writeoff.aggregate_debt_written_off)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        let residual = swap.amount_out.saturating_sub(full_cash_repayment);
        let max_incentive = (debt_amount as u128)
            .checked_mul(LIQUIDATION_INCENTIVE_BPS as u128)
            .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
            .ok_or(ErrorCode::MarketMathOverflow)? as u64;
        let liquidator_amount = residual.min(max_incentive);
        let owner_residual = residual
            .checked_sub(liquidator_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.reduce_leverage_collateral(position, collateral_sold)?;
        Ok(LeverageLiquidationReceipt {
            debt_repaid: clearance.cash_repaid,
            insurance_drawn: insurance.credit,
            socialized_loss: lifecycle.socialized_principal_loss,
            interest_paid: clearance.interest_paid,
            principal_written_off: writeoff.principal_written_off,
            collateral_sold,
            closeout_value: swap.amount_out,
            liquidator_amount,
            owner_residual,
            swap,
            fees,
            base_hlp_rebalance,
            quote_hlp_rebalance,
        })
    }

    pub fn add_leverage_margin(
        &mut self,
        position: &mut LeveragePosition,
        repay_credit: u64,
        clock: crate::clock::MarketClock,
        collateral_fee: LeverageCollateralFee,
    ) -> Result<LeverageUpdateReceipt> {
        let mut receipt = self.repay_leverage_debt(position, repay_credit, clock)?;
        if position.debt_shares > 0 {
            receipt.closeout_value = self.leverage_closeout_value_with_fee(position, clock, collateral_fee)?;
        }
        Ok(receipt)
    }

    /// Repayment without a collateral-sale quote. Zero closeout_value means
    /// unquoted; no collateral is sold and no withdrawal rights are granted.
    pub fn repay_leverage_debt(
        &mut self,
        position: &mut LeveragePosition,
        repay_credit: u64,
        clock: crate::clock::MarketClock,
    ) -> Result<LeverageUpdateReceipt> {
        self.repay_leverage_debt_from_curve(position, repay_credit, clock, None)
    }

    /// The new repayment instruction captures this before interest accrual.
    /// Identical integrated reserves prove the published curve itself did not
    /// move during the operation. Fresh risk is still evaluated after repayment.
    pub(crate) fn repay_leverage_debt_from_curve(
        &mut self,
        position: &mut LeveragePosition,
        repay_credit: u64,
        clock: crate::clock::MarketClock,
        before_accrual: Option<IntegratedCurveState>,
    ) -> Result<LeverageUpdateReceipt> {
        let curve_reserves_before = self.curve_reserves_nad()?;
        position.require_open()?;
        require!(repay_credit > 0, ErrorCode::AmountZero);
        let debt_asset = position.debt_asset()?;
        let unpaid_before = self.public_unpaid_interest(debt_asset)?;
        let repayment = self
            .debt
            .isolated_repayment_for_max(debt_asset, position.debt_shares, repay_credit)?;
        require_eq!(repayment.cash_repaid, repay_credit, ErrorCode::BrokenInvariant);
        let clearance = self.debt.clear_isolated_debt(
            debt_asset,
            &mut position.debt_shares,
            &mut position.debt_principal,
            repay_credit,
        )?;
        let principal_paid = clearance.principal_paid;
        if position.debt_shares == 0 {
            self.remove_leverage_exposure(position.collateral_asset()?, position.collateral_amount)?;
            position.margin_terms.admission_equity_collateral_nad = 0;
        }
        let live_debit = clearance.live_debit_for_cash_repay()?;
        self.settle_public_debt_reserves(
            debt_asset,
            unpaid_before,
            clearance.interest_paid,
            live_debit,
            principal_paid,
        )?;
        if self.amm.initialized
            && before_accrual.is_some()
            && before_accrual == Some(self.integrated_curve_state_nad()?)
        {
            // No LP supply, parameter, or center changes occur in repayment.
            // Keep the exact published geometry instead of reconstructing a
            // rounded equivalent at the identical point. Depth/floor, retention,
            // quote invalidation and a fresh risk observation still advance.
            let depth = self
                .amm
                .concentrated_curve_cache
                .tail_liquidity
                .checked_add(self.amm.concentrated_curve_cache.concentrated_liquidity)
                .ok_or(ErrorCode::InvariantOverflow)?;
            self.amm
                .checkpoint_neutral_liquidity(self.curve_depth_per_share_nad(depth)?);
            self.defer_amm_retention_target()?;
            self.advance_curve_revision()?;
            self.refresh_risk_at_clock(clock)?;
        } else {
            self.finalize_debt_repayment(clock, curve_reserves_before)?;
        }
        // Repayment remains available even when a collateral-sale quote would
        // fail. It only reduces debt; the caller can preview closeout separately.
        let closeout_value = 0;
        Ok(LeverageUpdateReceipt {
            borrowed_amount: 0,
            debt_delta: -i64::try_from(clearance.debt_reduced).map_err(|_| ErrorCode::Overflow)?,
            collateral_delta: 0,
            debt_amount: clearance.remaining_debt,
            debt_shares: position.debt_shares,
            collateral_amount: position.collateral_amount,
            closeout_value,
            interest_paid: clearance.interest_paid,
            fees: FeesReceipt::default(),
            owner_residual: 0,
            base_hlp_rebalance: HlpRebalanceReceipt::default(),
            quote_hlp_rebalance: HlpRebalanceReceipt {
                target_asset: MarketAsset::Quote,
                ..HlpRebalanceReceipt::default()
            },
        })
    }

    pub fn leverage_closeout_value(
        &self,
        position: &LeveragePosition,
        clock: crate::clock::MarketClock,
    ) -> Result<u64> {
        self.leverage_closeout_value_with_fee(position, clock, LeverageCollateralFee::default())
    }

    pub fn leverage_closeout_value_with_fee(
        &self,
        position: &LeveragePosition,
        clock: crate::clock::MarketClock,
        collateral_fee: LeverageCollateralFee,
    ) -> Result<u64> {
        let after_launch = self.config.start_time.saturating_add(
            self.config
                .amm
                .launch_fee_duration_seconds
                .max(self.config.amm.launch_rate_limit_duration_seconds)
                .min(i64::MAX as u64) as i64,
        );
        self.leverage_closeout_value_at_time_with_fee(position, clock, after_launch, collateral_fee)
    }

    pub fn leverage_closeout_value_at_time(
        &self,
        position: &LeveragePosition,
        clock: crate::clock::MarketClock,
        current_unix_timestamp: i64,
    ) -> Result<u64> {
        self.leverage_closeout_value_at_time_with_fee(
            position,
            clock,
            current_unix_timestamp,
            LeverageCollateralFee::default(),
        )
    }

    pub fn leverage_closeout_value_at_time_with_fee(
        &self,
        position: &LeveragePosition,
        clock: crate::clock::MarketClock,
        current_unix_timestamp: i64,
        collateral_fee: LeverageCollateralFee,
    ) -> Result<u64> {
        let collateral_asset = position.collateral_asset()?;
        self.quote_leverage_swap_at_time(
            collateral_asset,
            collateral_fee.unwind_credit(position.collateral_amount)?,
            clock,
            current_unix_timestamp,
        )
        .map(|quote| quote.amount_out)
    }

    /// Exact executable output used by protection health. Only valuation is
    /// returned; the execution path still constructs and validates a full swap.
    pub fn leverage_protection_closeout_value(
        &self,
        position: &LeveragePosition,
        clock: crate::clock::MarketClock,
        current_unix_timestamp: i64,
    ) -> Result<u64> {
        self.leverage_protection_closeout_value_with_fee(
            position,
            clock,
            current_unix_timestamp,
            LeverageCollateralFee::default(),
        )
    }

    pub fn leverage_protection_closeout_value_with_fee(
        &self,
        position: &LeveragePosition,
        clock: crate::clock::MarketClock,
        current_unix_timestamp: i64,
        collateral_fee: LeverageCollateralFee,
    ) -> Result<u64> {
        let asset = position.collateral_asset()?;
        let unwind_credit = collateral_fee.unwind_credit(position.collateral_amount)?;
        let preliminary = self.preliminary_swap_inputs_for_state_at_time(
            asset,
            unwind_credit,
            clock,
            current_unix_timestamp,
            self.dynamic_fee_pre_state(clock)?,
        )?;
        let (amount_out, _) = self.evaluate_concentrated_swap(
            asset,
            unwind_credit,
            preliminary,
            self.integrated_curve_state_nad()?,
            0,
            false,
        )?;
        require!(amount_out > 0, ErrorCode::InsufficientOutputAmount);
        Ok(amount_out)
    }

    fn require_position_initial_leverage_health(
        &self,
        position: &LeveragePosition,
        clock: crate::clock::MarketClock,
        current_unix_timestamp: i64,
        collateral_fee: LeverageCollateralFee,
    ) -> Result<u64> {
        let collateral_asset = position.collateral_asset()?;
        let unwind_credit = collateral_fee.unwind_credit(position.collateral_amount)?;
        let closeout_quote =
            self.quote_leverage_swap_at_time(collateral_asset, unwind_credit, clock, current_unix_timestamp)?;
        let closeout_value = closeout_quote.amount_out;
        let required_bps = position.margin_terms.initial_bps(position.collateral_amount)?;
        let debt_amount = position.debt_amount(&self.debt)?;
        require_gt!(closeout_value, debt_amount, ErrorCode::LeverageInitialMarginTooLow);
        require_gte!(
            equity_bps(closeout_value, debt_amount)?,
            u128::from(required_bps),
            ErrorCode::LeverageInitialMarginTooLow
        );
        let asset = collateral_asset;
        let price = match asset {
            MarketAsset::Base => self
                .risk
                .directional_base_price_ema_nad
                .min(self.risk.base_price_ema_nad),
            MarketAsset::Quote => self
                .risk
                .directional_quote_price_ema_nad
                .min(self.risk.quote_price_ema_nad),
        };
        require_gt!(price, 0, ErrorCode::InvalidSettlementPrice);
        let amount = self.normalize_amount(u128::from(unwind_credit), self.side(asset).asset_decimals)?;
        let value = mul_div_u128(amount, u128::from(price), u128::from(NAD))?;
        let debt = self.normalize_amount(
            u128::from(position.debt_amount(&self.debt)?),
            self.side(asset.opposite()).asset_decimals,
        )?;
        let required = mul_div_ceil_u128(value, u128::from(required_bps), 10_000)?;
        require!(
            value.saturating_sub(debt) >= required && value > debt,
            ErrorCode::LeverageInitialMarginTooLow
        );
        let debt_amount = position.debt_amount(&self.debt)?;
        require_gte!(
            self.ema_leverage_margin_bps_for_credit(collateral_asset, unwind_credit, debt_amount)?,
            u128::from(required_bps),
            ErrorCode::LeverageInitialMarginTooLow
        );
        Ok(closeout_value)
    }

    fn ema_leverage_margin_bps_for_credit(
        &self,
        collateral_asset: MarketAsset,
        unwind_credit: u64,
        debt_amount: u64,
    ) -> Result<u128> {
        let collateral_value_nad =
            self.linear_liquidation_collateral_value_nad(collateral_asset, unwind_credit, &self.risk)?;
        let debt_value_nad = self.normalize_amount(
            debt_amount as u128,
            self.side(collateral_asset.opposite()).asset_decimals,
        )?;
        equity_bps_u128(collateral_value_nad, debt_value_nad)
    }

    fn require_ema_leverage_not_liquidatable(
        &self,
        collateral_asset: MarketAsset,
        collateral_amount: u64,
        debt_amount: u64,
        collateral_fee: LeverageCollateralFee,
        maintenance_bps: u16,
    ) -> Result<()> {
        require_gt!(
            self.ema_leverage_margin_bps_for_credit(
                collateral_asset,
                collateral_fee.unwind_credit(collateral_amount)?,
                debt_amount,
            )?,
            u128::from(maintenance_bps),
            ErrorCode::LeveragePositionNotLiquidatable
        );
        Ok(())
    }

    fn post_swap_closeout_quote_with_quote(
        &self,
        asset_in: MarketAsset,
        swap: LeverageSwapQuote,
        collateral_asset: MarketAsset,
        collateral_amount: u64,
        clock: crate::clock::MarketClock,
        current_unix_timestamp: i64,
    ) -> Result<AmmSwapQuote> {
        let mut state = self.integrated_curve_state_nad()?;
        let fee_asset = MarketAsset::try_from_code(swap.fee_breakdown.fee_asset)?;
        if fee_asset == asset_in {
            require_eq!(
                swap.fee_breakdown.reserve_input_credit,
                swap.fee_breakdown
                    .amount_in_for_quote
                    .checked_add(swap.fee_breakdown.retained_surcharge)
                    .and_then(|value| value.checked_add(swap.fee_breakdown.compounded_fee_debit))
                    .ok_or(ErrorCode::ReserveOverflow)?,
                ErrorCode::BrokenInvariant
            );
        } else {
            require_eq!(
                swap.fee_breakdown.reserve_input_credit,
                swap.fee_breakdown.amount_in_for_quote,
                ErrorCode::BrokenInvariant
            );
        }
        let input_principal = swap
            .amount_in_after_fee
            .checked_add(if fee_asset == asset_in {
                swap.fee_breakdown.compounded_fee_debit
            } else {
                0
            })
            .ok_or(ErrorCode::ReserveOverflow)?;
        let output_principal = swap
            .gross_amount_out
            .checked_sub(if fee_asset == asset_in.opposite() {
                swap.fee_breakdown.compounded_fee_debit
            } else {
                0
            })
            .ok_or(ErrorCode::ReserveUnderflow)?;
        let input_nad = self.normalize_amount(input_principal as u128, self.side(asset_in).asset_decimals)?;
        let output_nad =
            self.normalize_amount(output_principal as u128, self.side(asset_in.opposite()).asset_decimals)?;
        match asset_in {
            MarketAsset::Base => {
                state.ordinary_base = state
                    .ordinary_base
                    .checked_add(input_nad)
                    .ok_or(ErrorCode::ReserveOverflow)?;
                state.ordinary_quote = state
                    .ordinary_quote
                    .checked_sub(output_nad)
                    .ok_or(ErrorCode::ReserveUnderflow)?;
            }
            MarketAsset::Quote => {
                state.ordinary_quote = state
                    .ordinary_quote
                    .checked_add(input_nad)
                    .ok_or(ErrorCode::ReserveOverflow)?;
                state.ordinary_base = state
                    .ordinary_base
                    .checked_sub(output_nad)
                    .ok_or(ErrorCode::ReserveUnderflow)?;
            }
        }
        let endpoint = reconstruct_hlp_endpoint(state)?;
        state.base_hlp_quote_debt = endpoint.base_hlp_quote_debt;
        state.quote_hlp_base_debt = endpoint.quote_hlp_base_debt;
        let pre_state = DynamicFeePreState {
            center_price_nad: self.current_curve_center_price_nad()?,
            volatility_accumulator_nad: swap.post_success_volatility_nad,
            volatility_last_update_timestamp_ms: clock.timestamp_ms,
        };
        let preliminary = self.preliminary_swap_inputs_for_state_at_time(
            collateral_asset,
            collateral_amount,
            clock,
            current_unix_timestamp,
            pre_state,
        )?;
        let quote = self
            .quote_concentrated_integrated_with_fee_from_state(
                collateral_asset,
                collateral_amount,
                preliminary,
                state,
                swap.fee_breakdown.protocol_fee_bps,
            )?
            .ok_or(ErrorCode::BrokenInvariant)?;
        Ok(quote.as_swap_quote(collateral_asset))
    }
}

pub(crate) fn leverage_debt_from_margin(margin_amount: u64, multiplier_bps: u64) -> Result<u64> {
    let notional = (margin_amount as u128)
        .checked_mul(multiplier_bps as u128)
        .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let debt = notional
        .checked_sub(margin_amount as u128)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    require!(debt > 0, ErrorCode::AmountZero);
    u64::try_from(debt).map_err(|_| ErrorCode::MarketMathOverflow.into())
}

fn equity_bps(closeout_value: u64, debt_amount: u64) -> Result<u128> {
    equity_bps_u128(closeout_value as u128, debt_amount as u128)
}

fn equity_bps_u128(closeout_value: u128, debt_amount: u128) -> Result<u128> {
    if closeout_value == 0 {
        return Ok(0);
    }
    Ok(closeout_value
        .saturating_sub(debt_amount)
        .checked_mul(BPS_DENOMINATOR as u128)
        .and_then(|value| value.checked_div(closeout_value))
        .ok_or(ErrorCode::MarketMathOverflow)?)
}

fn require_leverage_not_liquidatable(closeout_value: u64, debt_amount: u64, maintenance_bps: u16) -> Result<()> {
    require!(
        closeout_value > debt_amount && equity_bps(closeout_value, debt_amount)? > u128::from(maintenance_bps),
        ErrorCode::LeveragePositionNotLiquidatable
    );
    Ok(())
}

impl Market {
    /// Advances debt, controller clocks, and hLP accounting for leverage
    /// repayment. The hLP checkpoint's exact price also initializes risk;
    /// repayment can reuse it when the final curve remains identical.
    pub(crate) fn prepare_leverage_margin_operation(&mut self, clock: crate::clock::MarketClock) -> Result<()> {
        self.assert_current_version()?;
        self.accrue_interest_to_clock(clock)?;
        if self.base_side.reserves.live_reserve > 0 && self.quote_side.reserves.live_reserve > 0 {
            self.advance_amm_clock(clock)?;
            let (_, _, price_nad) = self.checkpoint_hlp_vaults_with_price()?;
            let curve_depth_nad = self
                .amm
                .concentrated_curve_cache
                .tail_liquidity
                .checked_add(self.amm.concentrated_curve_cache.concentrated_liquidity)
                .ok_or(ErrorCode::InvariantOverflow)?;
            self.observe_risk_from_concentrated_curve(price_nad, curve_depth_nad, clock)?;
            self.risk_revision = self.curve_revision;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    include!("../tests/transitions/leverage.rs");
}
