use anchor_lang::prelude::*;

#[cfg(test)]
use super::liquidity::prepare_concentrated_hlp_transition;
use super::liquidity::{
    consume_hlp_tracking_unrealized_interest, current_hlp_signed_navs_with_prices,
    hlp_curve_prices_from_base_price_nad, prepare_concentrated_hlp_transition_at_current_state,
    rebase_hlp_tracking_for_socialized_loss, ConcentratedHlpTransition, SwapCashPolicy,
};
use super::{AmmSwapQuote, HlpRebalanceReceipt, SwapFeeBreakdown};
use crate::{
    constants::{
        BPS_DENOMINATOR, LEVERAGE_INITIAL_MARGIN_BPS, LEVERAGE_MAINTENANCE_BUFFER_BPS, LEVERAGE_MAX_MULTIPLIER_BPS,
        LEVERAGE_MAX_UNWIND_IMPACT_BPS, LIQUIDATION_INCENTIVE_BPS, NAD,
    },
    errors::ErrorCode,
    math::{
        ceil_div, denormalize_from_nad_floor, mul_div_ceil_u128, mul_div_u128, normalize_to_nad,
        realized_interest_split,
    },
    state::{ConcentratedCurveCache, Debt, LeveragePosition, Market, MarketAsset, ProtocolAuctionSplit},
};

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
    liquidity::{reconstruct_hlp_endpoint, HlpYieldEligibility},
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedLeverageSwap {
    pub swap: LeverageSwapQuote,
    pub base_pre_rebalance: HlpRebalanceReceipt,
    pub quote_pre_rebalance: HlpRebalanceReceipt,
    pub fee_eligible_ylp_supply: u64,
    pub interest_eligibility: HlpYieldEligibility,
    pub(crate) cash_policy: SwapCashPolicy,
    pub(crate) post_fee_curve_cache: Option<Box<ConcentratedCurveCache>>,
    pub(crate) concentrated_transition: Option<Box<ConcentratedHlpTransition>>,
}

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
    base_borrow_index_nad: u128,
    quote_borrow_index_nad: u128,
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
            base_borrow_index_nad: market.debt.base_borrow_index_nad,
            quote_borrow_index_nad: market.debt.quote_borrow_index_nad,
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
            isolated_base_shares: self.isolated_base_shares,
            isolated_quote_shares: self.isolated_quote_shares,
            isolated_base_principal: self.isolated_base_principal,
            isolated_quote_principal: self.isolated_quote_principal,
            ..crate::state::Debt::default()
        }
    }

    fn set_debt(&mut self, debt: crate::state::Debt) {
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

    fn isolated_unrealized_interest(self, asset: MarketAsset) -> Result<u128> {
        let debt = self.debt();
        let debt_amount = debt.isolated_debt(asset)?;
        let principal = match asset {
            MarketAsset::Base => self.isolated_base_principal,
            MarketAsset::Quote => self.isolated_quote_principal,
        };
        debt_amount
            .checked_sub(u128::from(principal).min(debt_amount))
            .ok_or_else(|| ErrorCode::DebtMathOverflow.into())
    }

    /// Reconstruct the post-state curve reserve from an identity-bound start
    /// reserve. Fixed-debt interest is unchanged by this lifecycle, so only
    /// live reserve and isolated-interest deltas can move the coordinate.
    fn projected_curve_reserve_from(self, start: Self, asset: MarketAsset, start_curve_reserve: u64) -> Result<u64> {
        let start_isolated_interest = start.isolated_unrealized_interest(asset)?;
        let unchanged_fixed_interest = u128::from(start.live_reserve(asset))
            .checked_sub(u128::from(start_curve_reserve))
            .and_then(|total_interest| total_interest.checked_sub(start_isolated_interest))
            .ok_or(ErrorCode::BrokenInvariant)?;
        let post_isolated_interest = self.isolated_unrealized_interest(asset)?;
        let post_curve_reserve = u128::from(self.live_reserve(asset))
            .checked_sub(unchanged_fixed_interest)
            .and_then(|reserve| reserve.checked_sub(post_isolated_interest))
            .ok_or(ErrorCode::BrokenInvariant)?;
        u64::try_from(post_curve_reserve).map_err(|_| ErrorCode::MarketMathOverflow.into())
    }
}

fn commit_leverage_lifecycle_state(market: &mut Market, post: LeverageLifecycleState) {
    market.base_side.reserves.live_reserve = post.base_live_reserve;
    market.base_side.reserves.cash_reserve = post.base_cash_reserve;
    market.quote_side.reserves.live_reserve = post.quote_live_reserve;
    market.quote_side.reserves.cash_reserve = post.quote_cash_reserve;
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
        SwapCashPolicy::Liquidate { debt_asset, .. } => Some(market.curve_reserve(debt_asset)?),
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
            require_eq!(transition.clearance.cash_repaid, amount_out, ErrorCode::BrokenInvariant);
            transition.position_debt_shares = shares;
            transition.position_debt_principal = principal;
            transition.removed_unrealized_interest = transition.clearance.interest_paid;
            cash_debit_out = transition
                .clearance
                .interest_paid
                .checked_add(output_fee)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            extra_live_debit_out = transition.clearance.live_debit_for_cash_repay()?;
        }
        SwapCashPolicy::Close {
            debt_asset,
            debt_shares,
            debt_principal,
        } => {
            require!(debt_asset == asset_in.opposite(), ErrorCode::BrokenInvariant);
            let mut shares = debt_shares;
            let mut principal = debt_principal;
            transition.clearance = debt.clear_isolated_debt(debt_asset, &mut shares, &mut principal, u64::MAX)?;
            require_gte!(
                amount_out,
                transition.clearance.cash_repaid,
                ErrorCode::InsufficientAmount
            );
            require_eq!(transition.clearance.remaining_debt, 0, ErrorCode::BrokenInvariant);
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
        SwapCashPolicy::Liquidate {
            debt_asset,
            debt_shares,
            debt_principal,
        } => {
            require!(debt_asset == asset_in.opposite(), ErrorCode::BrokenInvariant);
            let full_repayment = debt.isolated_repayment_for_max(debt_asset, debt_shares, u64::MAX)?;
            let position_principal = u64::try_from(debt_principal).map_err(|_| ErrorCode::DebtMathOverflow)?;
            require_gte!(
                full_repayment.cash_repaid,
                position_principal,
                ErrorCode::DebtMathOverflow
            );
            let repay_credit = amount_out.min(full_repayment.cash_repaid);
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
            cash_debit_out = amount_out
                .saturating_sub(full_repayment.cash_repaid)
                .checked_add(interest_paid)
                .and_then(|value| value.checked_add(output_fee))
                .ok_or(ErrorCode::MarketMathOverflow)?;
            extra_live_debit_out = transition.clearance.live_debit_for_cash_repay()?;
        }
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
    let asset_out_live = post
        .live_reserve(asset_out)
        .checked_sub(
            gross_amount_out
                .checked_add(extra_live_debit_out)
                .ok_or(ErrorCode::ReserveUnderflow)?,
        )
        .ok_or(ErrorCode::ReserveUnderflow)?;
    *post.live_reserve_mut(asset_out) = asset_out_live;
    let asset_out_cash = post
        .cash_reserve_mut(asset_out)
        .checked_sub(cash_debit_out)
        .ok_or(ErrorCode::CashReserveUnderflow)?;
    *post.cash_reserve_mut(asset_out) = asset_out_cash;

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

    if let (SwapCashPolicy::Liquidate { debt_asset, .. }, Some(curve_before)) =
        (policy, debt_curve_reserve_before_share_removal)
    {
        let expected_curve_after = curve_before
            .checked_sub(gross_amount_out)
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

impl PreparedLeverageSwap {
    fn require_cash_policy(&self, expected: SwapCashPolicy) -> Result<()> {
        require!(self.cash_policy == expected, ErrorCode::BrokenInvariant);
        Ok(())
    }

    fn consume_tracking_unrealized_interest(&mut self, asset: MarketAsset, amount: u64) -> Result<()> {
        consume_hlp_tracking_unrealized_interest(&mut self.base_pre_rebalance, asset, amount)?;
        consume_hlp_tracking_unrealized_interest(&mut self.quote_pre_rebalance, asset, amount)
    }
}

impl LeverageSwapQuote {
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
        let fee = quote.fee_breakdown;
        require_gte!(fee.claimable_fee_debit, total_credit, ErrorCode::BrokenInvariant);
        if fee.claimable_fee_debit == 0 {
            require_eq!(total_credit, 0, ErrorCode::BrokenInvariant);
            return Ok(Self::default());
        }
        let claimable_base_fee = fee
            .base_fee_debit
            .checked_sub(fee.compounded_base_fee_debit)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        let base = u64::try_from(
            (total_credit as u128)
                .checked_mul(claimable_base_fee as u128)
                .and_then(|value| value.checked_div(fee.claimable_fee_debit as u128))
                .ok_or(ErrorCode::FeeMathOverflow)?,
        )
        .map_err(|_| ErrorCode::FeeMathOverflow)?;
        Ok(Self {
            base,
            distributed_surcharge: total_credit.checked_sub(base).ok_or(ErrorCode::FeeMathOverflow)?,
        })
    }

    fn validate_for_quote(self, quote: &LeverageSwapQuote) -> Result<()> {
        let total = self
            .base
            .checked_add(self.distributed_surcharge)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        require!(
            self == Self::from_total_actual_credit(quote, total)?,
            ErrorCode::BrokenInvariant
        );
        Ok(())
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

impl Market {
    pub fn leverage_close_slice(&self, position: &LeveragePosition, close_bps: u16) -> Result<LeverageCloseSlice> {
        position.require_open()?;
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
        require!(
            debt_shares > 0 && debt_shares < position.debt_shares,
            ErrorCode::InsufficientDebt
        );
        let debt_asset = position.debt_asset()?;
        let total_debt = Debt::shares_to_debt(position.debt_shares, self.debt.borrow_index(debt_asset))?;
        let slice_debt = u64::try_from(Debt::shares_to_debt(debt_shares, self.debt.borrow_index(debt_asset))?)
            .map_err(|_| ErrorCode::DebtMathOverflow)?;
        let (slice_principal, _) =
            realized_interest_split(slice_debt, total_debt, position.debt_principal.min(total_debt))?;
        Ok(LeverageCloseSlice {
            collateral_amount,
            debt_shares,
            debt_principal: slice_principal as u128,
        })
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
        let ordinary = self.integrated_curve_state_nad()?;
        let parameters = self.config.amm.concentrated_curve_parameters()?;
        self.amm.concentrated_curve_cache = prepare_concentrated_cache_at_point(
            ordinary.ordinary_base,
            ordinary.ordinary_quote,
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
        if loss == 0 {
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
        current_slot: u64,
    ) -> Result<LeverageSwapQuote> {
        let after_launch = self.config.start_time.saturating_add(
            self.config
                .amm
                .launch_fee_duration_seconds
                .max(self.config.amm.launch_rate_limit_duration_seconds)
                .min(i64::MAX as u64) as i64,
        );
        self.quote_leverage_swap_at_time(asset_in, amount_in, current_slot, after_launch)
    }

    pub fn quote_leverage_swap_at_time(
        &self,
        asset_in: MarketAsset,
        amount_in: u64,
        current_slot: u64,
        current_unix_timestamp: i64,
    ) -> Result<LeverageSwapQuote> {
        let pre_state = self.dynamic_fee_pre_state(current_slot)?;
        let preliminary = self.preliminary_swap_inputs_for_state_at_time(
            asset_in,
            amount_in,
            current_slot,
            current_unix_timestamp,
            pre_state,
        )?;
        let quote = self
            .quote_concentrated_integrated_with_fee(asset_in, amount_in, preliminary, 0)?
            .ok_or(ErrorCode::BrokenInvariant)?
            .as_swap_quote(asset_in);
        Ok(LeverageSwapQuote::from_amm(quote, current_slot))
    }

    fn validate_leverage_swap_quote(
        &self,
        quote: LeverageSwapQuote,
        asset_in: MarketAsset,
        current_slot: u64,
    ) -> Result<()> {
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

    /// Commits the AMM leg, performs the same exact inline hLP correction used
    /// by spot, then materializes the final curve/risk identity. The returned
    /// receipts are settled by the instruction as one net token change per
    /// side; no maintenance call can be required later.
    fn finalize_leverage_swap_hlp(
        &mut self,
        prepared_swap: &PreparedLeverageSwap,
        current_slot: u64,
        socialized_loss_applied: bool,
    ) -> Result<(HlpRebalanceReceipt, HlpRebalanceReceipt)> {
        if prepared_swap.swap.fee_breakdown.retained_surcharge > 0 {
            let asset_in = MarketAsset::try_from_code(prepared_swap.swap.asset_in)?;
            let fee_asset = MarketAsset::try_from_code(prepared_swap.swap.fee_breakdown.fee_asset)?;
            require!(
                fee_asset == asset_in || fee_asset == asset_in.opposite(),
                ErrorCode::BrokenInvariant
            );
            self.credit_protected_recenter_reserve(fee_asset, prepared_swap.swap.fee_breakdown.retained_surcharge)?;
        }
        let fresh_transition;
        let transition = if socialized_loss_applied {
            fresh_transition = prepare_concentrated_hlp_transition_at_current_state(self)?;
            &fresh_transition
        } else {
            prepared_swap
                .concentrated_transition
                .as_deref()
                .ok_or(ErrorCode::BrokenInvariant)?
        };
        let (base, quote) = transition.consume(self)?;
        if prepared_swap.swap.fee_breakdown.compounded_fee_debit > 0 {
            let prepared_cache = if socialized_loss_applied {
                None
            } else {
                Some(
                    *prepared_swap
                        .post_fee_curve_cache
                        .as_deref()
                        .ok_or(ErrorCode::BrokenInvariant)?,
                )
            };
            self.checkpoint_amm_neutral_inventory_raw(current_slot, prepared_cache)?;
        } else {
            require!(prepared_swap.post_fee_curve_cache.is_none(), ErrorCode::BrokenInvariant);
        }
        self.finalize_amm_trade_after_inventory_checkpoint(
            prepared_swap.swap.start_price_nad,
            prepared_swap.swap.end_price_nad,
            current_slot,
        )?;
        let curve_depth_nad = self
            .amm
            .concentrated_curve_cache
            .tail_liquidity
            .checked_add(self.amm.concentrated_curve_cache.concentrated_liquidity)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let final_price_nad = if socialized_loss_applied {
            self.current_concentrated_spot_price_nad()?
                .ok_or(ErrorCode::BrokenInvariant)?
        } else {
            prepared_swap.swap.reserve_end_price_nad
        };
        self.observe_risk_from_concentrated_curve(final_price_nad, curve_depth_nad, current_slot)?;
        require_eq!(
            self.risk.cached_spot_base_price_nad,
            final_price_nad,
            ErrorCode::BrokenInvariant
        );
        self.assert_market_invariants()?;
        Ok((base, quote))
    }

    pub fn open_leverage(
        &mut self,
        position: &mut LeveragePosition,
        owner: Pubkey,
        market: Pubkey,
        position_id: Pubkey,
        referral_partner: Pubkey,
        referral_interest_share_bps: u16,
        debt_asset: MarketAsset,
        margin_credit: u64,
        multiplier_bps: u64,
        collateral_credit: u64,
        prepared_swap: PreparedLeverageSwap,
        swap_fee_credit: LeverageSwapFeeCredit,
        opened_at: i64,
        opened_slot: u64,
        bump: u8,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
    ) -> Result<LeverageOpenReceipt> {
        let swap = prepared_swap.swap;
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
        self.ensure_amm_initialized(opened_slot)?;
        require_eq!(swap.amount_in, notional, ErrorCode::BrokenInvariant);
        self.validate_leverage_swap_quote(swap, debt_asset, opened_slot)?;
        swap_fee_credit.validate_for_quote(&swap)?;
        require_gte!(swap.amount_out, collateral_credit, ErrorCode::SlippageExceeded);
        require!(collateral_credit > 0, ErrorCode::InsufficientOutputAmount);

        let cash_policy = SwapCashPolicy::Borrow {
            asset: debt_asset,
            amount: borrowed_amount,
        };
        prepared_swap.require_cash_policy(cash_policy)?;
        let lifecycle = self.apply_leverage_lifecycle_transition(
            cash_policy,
            debt_asset,
            swap.amount_in_after_fee,
            swap.amount_out,
            swap.gross_amount_out,
        )?;
        let fees = self.apply_leverage_swap(
            debt_asset,
            swap,
            swap_fee_credit,
            protocol_fee_bps,
            protocol_auction_split,
            prepared_swap.fee_eligible_ylp_supply,
            opened_slot,
        )?;
        let debt_shares = lifecycle.added_debt_shares;
        require_gt!(debt_shares, 0, ErrorCode::BrokenInvariant);
        position.initialize(
            owner,
            market,
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
        let (base_hlp_rebalance, quote_hlp_rebalance) =
            self.finalize_leverage_swap_hlp(&prepared_swap, opened_slot, false)?;
        let closeout_value = self.require_position_initial_leverage_health(position, opened_slot, opened_at)?;
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
        collateral_credit: u64,
        prepared_swap: PreparedLeverageSwap,
        swap_fee_credit: LeverageSwapFeeCredit,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        current_slot: u64,
        current_unix_timestamp: i64,
    ) -> Result<LeverageUpdateReceipt> {
        let swap = prepared_swap.swap;
        position.require_open()?;
        require!(borrowed_amount > 0, ErrorCode::AmountZero);
        require!(collateral_credit > 0, ErrorCode::InsufficientOutputAmount);
        let debt_asset = position.debt_asset()?;
        self.ensure_amm_initialized(current_slot)?;
        require_eq!(swap.amount_in, borrowed_amount, ErrorCode::BrokenInvariant);
        self.validate_leverage_swap_quote(swap, debt_asset, current_slot)?;
        swap_fee_credit.validate_for_quote(&swap)?;
        require_gte!(swap.amount_out, collateral_credit, ErrorCode::SlippageExceeded);
        let cash_policy = SwapCashPolicy::Borrow {
            asset: debt_asset,
            amount: borrowed_amount,
        };
        prepared_swap.require_cash_policy(cash_policy)?;
        let lifecycle = self.apply_leverage_lifecycle_transition(
            cash_policy,
            debt_asset,
            swap.amount_in_after_fee,
            swap.amount_out,
            swap.gross_amount_out,
        )?;
        let fees = self.apply_leverage_swap(
            debt_asset,
            swap,
            swap_fee_credit,
            protocol_fee_bps,
            protocol_auction_split,
            prepared_swap.fee_eligible_ylp_supply,
            current_slot,
        )?;
        let added_shares = lifecycle.added_debt_shares;
        require_gt!(added_shares, 0, ErrorCode::BrokenInvariant);
        position.debt_shares = position
            .debt_shares
            .checked_add(added_shares)
            .ok_or(ErrorCode::DebtShareMathOverflow)?;
        position.debt_principal = position
            .debt_principal
            .checked_add(borrowed_amount as u128)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        position.credit_collateral(collateral_credit)?;
        let (base_hlp_rebalance, quote_hlp_rebalance) =
            self.finalize_leverage_swap_hlp(&prepared_swap, current_slot, false)?;
        let closeout_value =
            self.require_position_initial_leverage_health(position, current_slot, current_unix_timestamp)?;
        Ok(LeverageUpdateReceipt {
            borrowed_amount,
            debt_delta: i64::try_from(borrowed_amount).map_err(|_| ErrorCode::Overflow)?,
            collateral_delta: i64::try_from(collateral_credit).map_err(|_| ErrorCode::Overflow)?,
            debt_amount: position.debt_amount(&self.debt)?,
            debt_shares: position.debt_shares,
            collateral_amount: position.collateral_amount,
            closeout_value,
            interest_paid: 0,
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
        prepared_swap: &mut PreparedLeverageSwap,
        swap_fee_credit: LeverageSwapFeeCredit,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        current_slot: u64,
        current_unix_timestamp: i64,
    ) -> Result<LeverageUpdateReceipt> {
        let swap = prepared_swap.swap;
        position.require_open()?;
        require!(collateral_debit > 0, ErrorCode::AmountZero);
        require_gt!(
            position.collateral_amount,
            collateral_debit,
            ErrorCode::InsufficientAmount
        );
        let debt_asset = position.debt_asset()?;
        let collateral_asset = debt_asset.opposite();
        let debt_before = position.debt_amount(&self.debt)?;
        self.ensure_amm_initialized(current_slot)?;
        require!(
            swap.amount_in > 0 && swap.amount_in <= collateral_debit,
            ErrorCode::BrokenInvariant
        );
        self.validate_leverage_swap_quote(swap, collateral_asset, current_slot)?;
        swap_fee_credit.validate_for_quote(&swap)?;
        require_gte!(swap.amount_out, min_repay_out, ErrorCode::SlippageExceeded);
        require_gt!(debt_before, swap.amount_out, ErrorCode::InsufficientDebt);
        let repayment = self
            .debt
            .isolated_repayment_for_max(debt_asset, position.debt_shares, swap.amount_out)?;
        // This instruction has no debt-token refund account. Reject a quote in
        // a share-granularity gap instead of silently donating the unused output.
        require_eq!(
            repayment.cash_repaid,
            swap.amount_out,
            ErrorCode::DebtShareDivisionOverflow
        );
        let collateral_after = position
            .collateral_amount
            .checked_sub(collateral_debit)
            .ok_or(ErrorCode::InsufficientAmount)?;
        let debt_after = debt_before
            .checked_sub(swap.amount_out)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        let pre_finalize_closeout_value = self
            .post_swap_closeout_quote_with_quote(
                collateral_asset,
                swap,
                collateral_asset,
                collateral_after,
                current_slot,
                current_unix_timestamp,
            )?
            .amount_out;
        require_leverage_not_liquidatable(pre_finalize_closeout_value, debt_after)?;
        let cash_policy = SwapCashPolicy::Decrease {
            debt_asset,
            debt_shares: position.debt_shares,
            debt_principal: position.debt_principal,
        };
        prepared_swap.require_cash_policy(cash_policy)?;
        let lifecycle = self.apply_leverage_lifecycle_transition(
            cash_policy,
            collateral_asset,
            swap.amount_in_after_fee,
            swap.amount_out,
            swap.gross_amount_out,
        )?;
        let clearance = lifecycle.clearance;
        position.debt_shares = lifecycle.position_debt_shares;
        position.debt_principal = lifecycle.position_debt_principal;
        prepared_swap.consume_tracking_unrealized_interest(debt_asset, clearance.interest_paid)?;
        let fees = self.apply_leverage_swap(
            collateral_asset,
            swap,
            swap_fee_credit,
            protocol_fee_bps,
            protocol_auction_split,
            prepared_swap.fee_eligible_ylp_supply,
            current_slot,
        )?;
        position.debit_collateral(collateral_debit)?;
        let (base_hlp_rebalance, quote_hlp_rebalance) =
            self.finalize_leverage_swap_hlp(prepared_swap, current_slot, false)?;
        let closeout_value = self.leverage_closeout_value_at_time(position, current_slot, current_unix_timestamp)?;
        require_leverage_not_liquidatable(closeout_value, clearance.remaining_debt)?;
        Ok(LeverageUpdateReceipt {
            borrowed_amount: 0,
            debt_delta: -i64::try_from(clearance.debt_reduced).map_err(|_| ErrorCode::Overflow)?,
            collateral_delta: -i64::try_from(collateral_debit).map_err(|_| ErrorCode::Overflow)?,
            debt_amount: clearance.remaining_debt,
            debt_shares: position.debt_shares,
            collateral_amount: position.collateral_amount,
            closeout_value,
            interest_paid: clearance.interest_paid,
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
        current_slot: u64,
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
            current_slot,
            None,
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
        current_slot: u64,
        current_unix_timestamp: i64,
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
            current_slot,
            Some(current_unix_timestamp),
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
        current_slot: u64,
        partial_close_unix_timestamp: Option<i64>,
    ) -> Result<Box<LeverageCloseReceipt>> {
        let swap = prepared_swap.swap;
        position.require_open()?;
        let debt_asset = position.debt_asset()?;
        let collateral_asset = debt_asset.opposite();
        require!(
            slice.collateral_amount > 0
                && slice.collateral_amount <= position.collateral_amount
                && slice.debt_shares > 0
                && slice.debt_shares <= position.debt_shares
                && slice.debt_principal <= position.debt_principal,
            ErrorCode::BrokenInvariant
        );
        let is_full_close = slice.collateral_amount == position.collateral_amount;
        require!(
            is_full_close == partial_close_unix_timestamp.is_none(),
            ErrorCode::BrokenInvariant
        );
        let debt_amount = self
            .debt
            .isolated_repayment_for_max(debt_asset, slice.debt_shares, u64::MAX)?
            .cash_repaid;
        let collateral_sold = slice.collateral_amount;
        self.ensure_amm_initialized(current_slot)?;
        require!(
            swap.amount_in > 0 && swap.amount_in <= collateral_sold,
            ErrorCode::BrokenInvariant
        );
        self.validate_leverage_swap_quote(swap, collateral_asset, current_slot)?;
        swap_fee_credit.validate_for_quote(&swap)?;
        require_gte!(swap.amount_out, debt_amount, ErrorCode::InsufficientAmount);
        let residual = swap
            .amount_out
            .checked_sub(debt_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require_gte!(residual, min_residual_out, ErrorCode::SlippageExceeded);
        let cash_policy = SwapCashPolicy::Close {
            debt_asset,
            debt_shares: slice.debt_shares,
            debt_principal: slice.debt_principal,
        };
        prepared_swap.require_cash_policy(cash_policy)?;
        let lifecycle = self.apply_leverage_lifecycle_transition(
            cash_policy,
            collateral_asset,
            swap.amount_in_after_fee,
            swap.amount_out,
            swap.gross_amount_out,
        )?;
        let clearance = lifecycle.clearance;
        require_eq!(lifecycle.position_debt_shares, 0, ErrorCode::BrokenInvariant);
        require_eq!(lifecycle.position_debt_principal, 0, ErrorCode::BrokenInvariant);
        position.debt_shares = position
            .debt_shares
            .checked_sub(slice.debt_shares)
            .ok_or(ErrorCode::DebtShareMathOverflow)?;
        position.debt_principal = position
            .debt_principal
            .checked_sub(slice.debt_principal)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        prepared_swap.consume_tracking_unrealized_interest(debt_asset, clearance.interest_paid)?;
        require_eq!(clearance.cash_repaid, debt_amount, ErrorCode::BrokenInvariant);
        require_eq!(clearance.remaining_debt, 0, ErrorCode::BrokenInvariant);
        let fees = self.apply_leverage_swap(
            collateral_asset,
            swap,
            swap_fee_credit,
            protocol_fee_bps,
            protocol_auction_split,
            prepared_swap.fee_eligible_ylp_supply,
            current_slot,
        )?;
        position.collateral_amount = position
            .collateral_amount
            .checked_sub(collateral_sold)
            .ok_or(ErrorCode::InsufficientAmount)?;
        let (base_hlp_rebalance, quote_hlp_rebalance) =
            self.finalize_leverage_swap_hlp(&prepared_swap, current_slot, false)?;
        let (remaining_debt_amount, remaining_closeout_value) =
            if let Some(current_unix_timestamp) = partial_close_unix_timestamp {
                position.require_open()?;
                let remaining_debt = position.debt_amount(&self.debt)?;
                let remaining_closeout =
                    self.leverage_closeout_value_at_time(position, current_slot, current_unix_timestamp)?;
                require_leverage_not_liquidatable(remaining_closeout, remaining_debt)?;
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
        mut prepared_swap: PreparedLeverageSwap,
        swap_fee_credit: LeverageSwapFeeCredit,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        current_slot: u64,
    ) -> Result<LeverageLiquidationReceipt> {
        let swap = prepared_swap.swap;
        position.require_open()?;
        let debt_asset = position.debt_asset()?;
        let collateral_asset = debt_asset.opposite();
        let debt_amount = position.debt_amount(&self.debt)?;
        require_gt!(debt_amount, 0, ErrorCode::ZeroDebtAmount);
        let collateral_sold = position.collateral_amount;
        self.ensure_amm_initialized(current_slot)?;
        require!(
            swap.amount_in > 0 && swap.amount_in <= collateral_sold,
            ErrorCode::BrokenInvariant
        );
        self.validate_leverage_swap_quote(swap, collateral_asset, current_slot)?;
        swap_fee_credit.validate_for_quote(&swap)?;
        let margin_bps = equity_bps(swap.amount_out, debt_amount)?;
        require!(
            swap.amount_out <= debt_amount || margin_bps <= LEVERAGE_MAINTENANCE_BUFFER_BPS as u128,
            ErrorCode::LeveragePositionNotLiquidatable
        );

        let cash_policy = SwapCashPolicy::Liquidate {
            debt_asset,
            debt_shares: position.debt_shares,
            debt_principal: position.debt_principal,
        };
        prepared_swap.require_cash_policy(cash_policy)?;
        let lifecycle = self.apply_leverage_lifecycle_transition(
            cash_policy,
            collateral_asset,
            swap.amount_in_after_fee,
            swap.amount_out,
            swap.gross_amount_out,
        )?;
        let clearance = lifecycle.clearance;
        let writeoff = lifecycle.writeoff;
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
        let fees = self.apply_leverage_swap(
            collateral_asset,
            swap,
            swap_fee_credit,
            protocol_fee_bps,
            protocol_auction_split,
            prepared_swap.fee_eligible_ylp_supply,
            current_slot,
        )?;
        prepared_swap.consume_tracking_unrealized_interest(debt_asset, lifecycle.removed_unrealized_interest)?;
        let rebase = self.apply_leverage_socialized_loss(debt_asset, lifecycle, current_slot)?;
        self.side(debt_asset).assert_share_backing()?;
        if lifecycle.socialized_principal_loss > 0 {
            rebase_hlp_tracking_for_socialized_loss(
                &mut prepared_swap.base_pre_rebalance,
                0,
                rebase.base_nav_delta_nad,
            )?;
            rebase_hlp_tracking_for_socialized_loss(
                &mut prepared_swap.quote_pre_rebalance,
                0,
                rebase.quote_nav_delta_nad,
            )?;
        }
        position.collateral_amount = 0;
        let (base_hlp_rebalance, quote_hlp_rebalance) =
            self.finalize_leverage_swap_hlp(&prepared_swap, current_slot, lifecycle.socialized_principal_loss > 0)?;
        Ok(LeverageLiquidationReceipt {
            debt_repaid: clearance.cash_repaid,
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
        current_slot: u64,
    ) -> Result<LeverageUpdateReceipt> {
        position.require_open()?;
        require!(repay_credit > 0, ErrorCode::AmountZero);
        let debt_asset = position.debt_asset()?;
        let debt_before = position.debt_amount(&self.debt)?;
        require_gt!(debt_before, repay_credit, ErrorCode::InsufficientDebt);
        let pre_finalize_closeout_value = self.leverage_closeout_value(position, current_slot)?;
        let debt_after = debt_before
            .checked_sub(repay_credit)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        require_leverage_not_liquidatable(pre_finalize_closeout_value, debt_after)?;
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
        let live_debit = clearance.live_debit_for_cash_repay()?;
        let side = self.side_mut(debt_asset);
        side.reserves.live_reserve = side
            .reserves
            .live_reserve
            .checked_sub(live_debit)
            .ok_or(ErrorCode::ReserveUnderflow)?;
        side.reserves.cash_reserve = side
            .reserves
            .cash_reserve
            .checked_add(principal_paid)
            .ok_or(ErrorCode::ReserveOverflow)?;
        self.finalize_amm_transition_and_observe_risk(current_slot)?;
        // Adding margin only reduces debt, so it remains available as a rescue
        // path even when the position's final-curve health is poor.
        let closeout_value = self.leverage_closeout_value(position, current_slot)?;
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
            base_hlp_rebalance: HlpRebalanceReceipt::default(),
            quote_hlp_rebalance: HlpRebalanceReceipt {
                target_asset: MarketAsset::Quote,
                ..HlpRebalanceReceipt::default()
            },
        })
    }

    pub fn remove_leverage_margin(
        &mut self,
        position: &mut LeveragePosition,
        borrow_amount: u64,
        current_slot: u64,
        current_unix_timestamp: i64,
    ) -> Result<LeverageUpdateReceipt> {
        position.require_open()?;
        require!(borrow_amount > 0, ErrorCode::AmountZero);
        let debt_asset = position.debt_asset()?;
        let debt_before = position.debt_amount(&self.debt)?;
        let debt_after = debt_before
            .checked_add(borrow_amount)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        let collateral_asset = position.collateral_asset()?;
        let pre_finalize_closeout_quote = self.quote_leverage_swap_at_time(
            collateral_asset,
            position.collateral_amount,
            current_slot,
            current_unix_timestamp,
        )?;
        require_initial_leverage_health(
            self,
            collateral_asset,
            position.collateral_amount,
            pre_finalize_closeout_quote.start_price_nad,
            pre_finalize_closeout_quote.amount_out,
            debt_after,
        )?;
        self.debit_leverage_cash(debt_asset, borrow_amount)?;
        let shares = self.add_isolated_borrow_debt(debt_asset, borrow_amount)?;
        position.debt_shares = position
            .debt_shares
            .checked_add(shares)
            .ok_or(ErrorCode::DebtShareMathOverflow)?;
        position.debt_principal = position
            .debt_principal
            .checked_add(borrow_amount as u128)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        self.finalize_amm_transition_and_observe_risk(current_slot)?;
        let closeout_value =
            self.require_position_initial_leverage_health(position, current_slot, current_unix_timestamp)?;
        Ok(LeverageUpdateReceipt {
            borrowed_amount: borrow_amount,
            debt_delta: i64::try_from(borrow_amount).map_err(|_| ErrorCode::Overflow)?,
            collateral_delta: 0,
            debt_amount: position.debt_amount(&self.debt)?,
            debt_shares: position.debt_shares,
            collateral_amount: position.collateral_amount,
            closeout_value,
            interest_paid: 0,
            fees: FeesReceipt::default(),
            base_hlp_rebalance: HlpRebalanceReceipt::default(),
            quote_hlp_rebalance: HlpRebalanceReceipt {
                target_asset: MarketAsset::Quote,
                ..HlpRebalanceReceipt::default()
            },
        })
    }

    pub fn leverage_closeout_value(&self, position: &LeveragePosition, current_slot: u64) -> Result<u64> {
        let after_launch = self.config.start_time.saturating_add(
            self.config
                .amm
                .launch_fee_duration_seconds
                .max(self.config.amm.launch_rate_limit_duration_seconds)
                .min(i64::MAX as u64) as i64,
        );
        self.leverage_closeout_value_at_time(position, current_slot, after_launch)
    }

    pub fn leverage_closeout_value_at_time(
        &self,
        position: &LeveragePosition,
        current_slot: u64,
        current_unix_timestamp: i64,
    ) -> Result<u64> {
        let collateral_asset = position.collateral_asset()?;
        self.quote_leverage_swap_at_time(
            collateral_asset,
            position.collateral_amount,
            current_slot,
            current_unix_timestamp,
        )
        .map(|quote| quote.amount_out)
    }

    fn require_position_initial_leverage_health(
        &self,
        position: &LeveragePosition,
        current_slot: u64,
        current_unix_timestamp: i64,
    ) -> Result<u64> {
        let collateral_asset = position.collateral_asset()?;
        let closeout_quote = self.quote_leverage_swap_at_time(
            collateral_asset,
            position.collateral_amount,
            current_slot,
            current_unix_timestamp,
        )?;
        let closeout_value = closeout_quote.amount_out;
        let spot_price_nad = closeout_quote.start_price_nad;
        require_initial_leverage_health(
            self,
            collateral_asset,
            position.collateral_amount,
            spot_price_nad,
            closeout_value,
            position.debt_amount(&self.debt)?,
        )?;
        Ok(closeout_value)
    }

    fn post_swap_closeout_quote_with_quote(
        &self,
        asset_in: MarketAsset,
        swap: LeverageSwapQuote,
        collateral_asset: MarketAsset,
        collateral_amount: u64,
        current_slot: u64,
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
        let input_nad = normalize_to_nad(input_principal as u128, self.side(asset_in).asset_decimals)?;
        let output_nad = normalize_to_nad(output_principal as u128, self.side(asset_in.opposite()).asset_decimals)?;
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
            volatility_last_update_slot: current_slot,
        };
        let preliminary = self.preliminary_swap_inputs_for_state_at_time(
            collateral_asset,
            collateral_amount,
            current_slot,
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

    fn apply_leverage_swap(
        &mut self,
        asset_in: MarketAsset,
        swap: LeverageSwapQuote,
        swap_fee_credit: LeverageSwapFeeCredit,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        fee_eligible_ylp_supply: u64,
        _current_slot: u64,
    ) -> Result<FeesReceipt> {
        swap_fee_credit.validate_for_quote(&swap)?;
        let fee_asset = MarketAsset::try_from_code(swap.fee_breakdown.fee_asset)?;
        if fee_asset == asset_in {
            require_eq!(
                swap.reserve_input_credit,
                swap.amount_in_after_fee
                    .checked_add(swap.fee_breakdown.retained_surcharge)
                    .and_then(|value| value.checked_add(swap.fee_breakdown.compounded_fee_debit))
                    .ok_or(ErrorCode::ReserveOverflow)?,
                ErrorCode::BrokenInvariant
            );
        } else {
            require!(fee_asset == asset_in.opposite(), ErrorCode::BrokenInvariant);
            require_eq!(swap.reserve_input_credit, swap.amount_in, ErrorCode::BrokenInvariant);
        }
        // The shared lifecycle transition already committed executable
        // reserves and debt. Checkpoint that exact curve state, then route the
        // retained principal identically for execution and predictive scratch.
        if swap.fee_breakdown.compounded_fee_debit > 0 {
            self.side_mut(fee_asset)
                .credit_reserve(swap.fee_breakdown.compounded_fee_debit, true)?;
        }

        let actual_claimable_credit = swap_fee_credit
            .base
            .checked_add(swap_fee_credit.distributed_surcharge)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        require_eq!(
            actual_claimable_credit,
            swap.fee_breakdown.claimable_fee_debit,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            swap.fee_breakdown.protocol_fee_bps,
            protocol_fee_bps,
            ErrorCode::BrokenInvariant
        );
        let fees = self.side_mut(fee_asset).record_swap_fee_allocation(
            swap.fee_breakdown.base_fee_debit,
            swap.fee_breakdown.distributed_surcharge_debit,
            swap.fee_breakdown.compounded_base_fee_debit,
            swap.fee_breakdown.compounded_dynamic_surcharge_debit,
            protocol_fee_bps,
            protocol_auction_split,
            fee_eligible_ylp_supply,
        )?;
        self.base_side.assert_share_backing()?;
        self.quote_side.assert_share_backing()?;
        self.side(fee_asset).fees.assert_backed()?;
        Ok(fees)
    }

    fn debit_leverage_cash(&mut self, debt_asset: MarketAsset, gross_debt: u64) -> Result<()> {
        require_gte!(
            self.side(debt_asset).reserves.cash_reserve,
            gross_debt,
            ErrorCode::InsufficientBorrowHeadroom
        );
        let debt_side = self.side_mut(debt_asset);
        debt_side.reserves.cash_reserve = debt_side
            .reserves
            .cash_reserve
            .checked_sub(gross_debt)
            .ok_or(ErrorCode::CashReserveUnderflow)?;
        Ok(())
    }

    fn add_isolated_borrow_debt(&mut self, debt_asset: MarketAsset, cash_debit: u64) -> Result<u128> {
        let aggregate_debt_before = self.debt.isolated_debt(debt_asset)?;
        let shares = self.debt.add_isolated_debt(debt_asset, cash_debit)?;
        let aggregate_debt_after = self.debt.isolated_debt(debt_asset)?;
        let aggregate_debt_increase = u64::try_from(
            aggregate_debt_after
                .checked_sub(aggregate_debt_before)
                .ok_or(ErrorCode::DebtMathOverflow)?,
        )
        .map_err(|_| ErrorCode::DebtMathOverflow)?;
        let side = self.side_mut(debt_asset);
        if aggregate_debt_increase > cash_debit {
            side.reserves.live_reserve = side
                .reserves
                .live_reserve
                .checked_add(aggregate_debt_increase - cash_debit)
                .ok_or(ErrorCode::ReserveOverflow)?;
        } else if aggregate_debt_increase < cash_debit {
            side.reserves.live_reserve = side
                .reserves
                .live_reserve
                .checked_sub(cash_debit - aggregate_debt_increase)
                .ok_or(ErrorCode::ReserveUnderflow)?;
        }
        Ok(shares)
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
    if closeout_value == 0 {
        return Ok(0);
    }
    Ok((closeout_value.saturating_sub(debt_amount) as u128)
        .checked_mul(BPS_DENOMINATOR as u128)
        .and_then(|value| value.checked_div(closeout_value as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?)
}

fn require_initial_leverage_health(
    market: &Market,
    collateral_asset: MarketAsset,
    collateral_amount: u64,
    base_price_nad: u64,
    closeout_value: u64,
    debt_amount: u64,
) -> Result<()> {
    require_gt!(closeout_value, debt_amount, ErrorCode::LeverageInitialMarginTooLow);
    let margin_bps = equity_bps(closeout_value, debt_amount)?;
    require_gte!(
        margin_bps,
        LEVERAGE_INITIAL_MARGIN_BPS as u128,
        ErrorCode::LeverageInitialMarginTooLow
    );
    require!(base_price_nad > 0, ErrorCode::InsufficientLiquidity);
    let collateral_nad = normalize_to_nad(collateral_amount as u128, market.side(collateral_asset).asset_decimals)?;
    let spot_value_nad = match collateral_asset {
        MarketAsset::Base => collateral_nad
            .checked_mul(base_price_nad as u128)
            .and_then(|value| value.checked_div(crate::constants::NAD as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?,
        MarketAsset::Quote => collateral_nad
            .checked_mul(crate::constants::NAD as u128)
            .and_then(|value| value.checked_div(base_price_nad as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?,
    };
    let spot_value =
        denormalize_from_nad_floor(spot_value_nad, market.side(collateral_asset.opposite()).asset_decimals)?;
    require!(spot_value > 0, ErrorCode::InsufficientLiquidity);
    let unwind_bps = if closeout_value >= spot_value {
        0
    } else {
        (spot_value as u128)
            .checked_sub(closeout_value as u128)
            .and_then(|value| value.checked_mul(BPS_DENOMINATOR as u128))
            .and_then(|value| value.checked_div(spot_value as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?
    };
    require_gte!(
        LEVERAGE_MAX_UNWIND_IMPACT_BPS as u128,
        unwind_bps,
        ErrorCode::LeverageUnwindImpactTooHigh
    );
    Ok(())
}

fn require_leverage_not_liquidatable(closeout_value: u64, debt_amount: u64) -> Result<()> {
    let margin_bps = equity_bps(closeout_value, debt_amount)?;
    require!(
        closeout_value > debt_amount && margin_bps > LEVERAGE_MAINTENANCE_BUFFER_BPS as u128,
        ErrorCode::LeveragePositionNotLiquidatable
    );
    Ok(())
}

impl Market {
    /// Advances debt, controller clocks, and hLP accounting for leverage
    /// margin changes without eagerly rebuilding risk that the transition will
    /// immediately invalidate. The transition records its final exact risk
    /// observation after the reserve/debt mutation.
    pub(crate) fn prepare_leverage_margin_operation(&mut self, current_slot: u64) -> Result<()> {
        self.assert_current_version()?;
        self.accrue_interest_to_slot(current_slot)?;
        if self.base_side.reserves.live_reserve > 0 && self.quote_side.reserves.live_reserve > 0 {
            self.advance_amm_clock(current_slot)?;
            self.checkpoint_hlp_vaults()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    include!("../tests/transitions/leverage.rs");
}
