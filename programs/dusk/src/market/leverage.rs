use anchor_lang::prelude::*;

use super::amm::CurveCheckpoint;
#[cfg(test)]
use super::liquidity::prepare_explicit_hlp_transition;
use super::liquidity::{
    cap_hlp_tracking_unrealized_interest, consume_hlp_tracking_unrealized_interest,
    current_hlp_signed_navs_with_prices, hlp_curve_prices_from_base_price_nad,
    prepare_explicit_hlp_transition_at_current_state, rebase_hlp_tracking_for_socialized_loss, ExplicitHlpTransition,
    HlpPlannerState, HlpPlannerStatic, SwapCashPolicy,
};
use super::{AmmSwapQuote, HlpRebalanceReceipt, SwapFeeBreakdown};
use crate::{
    constants::{
        BPS_DENOMINATOR, LEVERAGE_INITIAL_MARGIN_BPS, LEVERAGE_MAINTENANCE_BUFFER_BPS, LEVERAGE_MAX_MULTIPLIER_BPS,
        LEVERAGE_MAX_UNWIND_IMPACT_BPS, LIQUIDATION_INCENTIVE_BPS,
    },
    errors::ErrorCode,
    math::{
        denormalize_from_nad_floor, normalize_to_nad, prepare_explicit_cache_at_point, reconstruct_hlp_endpoint,
        DynamicFeePreState,
    },
    state::{
        AmmState, DebtClearance, DebtWriteoff, FeesReceipt, HlpYieldEligibility, LeveragePosition, Market, MarketAsset,
        ProtocolAuctionSplit,
    },
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LeverageSwapQuote {
    pub explicit_curve: bool,
    pub asset_in: u8,
    pub quoted_slot: u64,
    pub amount_in: u64,
    pub amount_in_after_fee: u64,
    pub reserve_input_credit: u64,
    pub amount_out: u64,
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
    pub(crate) explicit_transition: Option<Box<ExplicitHlpTransition>>,
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
    debt_curve_reserve_before_share_removal: Option<u64>,
    debt_curve_reserve_after_transition: Option<u64>,
    transition: LeverageLifecycleTransition,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HlpSocializedLossRebase {
    pub(crate) base_nav_delta_nad: i128,
    pub(crate) quote_nav_delta_nad: i128,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LeverageSocializedLossPlan {
    Noop {
        debt_asset: MarketAsset,
        transition: LeverageLifecycleTransition,
        current_slot: u64,
        start_live_reserve: u64,
        start_amm: AmmState,
    },
    Apply {
        debt_asset: MarketAsset,
        socialized_principal_loss: u64,
        current_slot: u64,
        start_live_reserve: u64,
        post_live_reserve: u64,
        start_amm: AmmState,
        post_amm: AmmState,
        start_checkpoint: CurveCheckpoint,
        successor_checkpoint: CurveCheckpoint,
        base_nav_before_nad: i128,
        quote_nav_before_nad: i128,
        base_nav_after_nad: i128,
        quote_nav_after_nad: i128,
        rebase: HlpSocializedLossRebase,
    },
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

    fn capture_planner(fixed: HlpPlannerStatic, state: HlpPlannerState) -> Self {
        Self {
            base_live_reserve: state.base_side.live_reserve,
            base_cash_reserve: state.base_side.cash_reserve,
            quote_live_reserve: state.quote_side.live_reserve,
            quote_cash_reserve: state.quote_side.cash_reserve,
            base_borrow_index_nad: fixed.base_borrow_index_nad,
            quote_borrow_index_nad: fixed.quote_borrow_index_nad,
            isolated_base_shares: state.debt.isolated_base_shares,
            isolated_quote_shares: state.debt.isolated_quote_shares,
            isolated_base_principal: state.debt.isolated_base_principal,
            isolated_quote_principal: state.debt.isolated_quote_principal,
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

fn commit_leverage_lifecycle_planner_state(state: &mut HlpPlannerState, post: LeverageLifecycleState) {
    state.base_side.live_reserve = post.base_live_reserve;
    state.base_side.cash_reserve = post.base_cash_reserve;
    state.quote_side.live_reserve = post.quote_live_reserve;
    state.quote_side.cash_reserve = post.quote_cash_reserve;
    state.debt.isolated_base_shares = post.isolated_base_shares;
    state.debt.isolated_quote_shares = post.isolated_quote_shares;
    state.debt.isolated_base_principal = post.isolated_base_principal;
    state.debt.isolated_quote_principal = post.isolated_quote_principal;
}

pub(super) fn apply_leverage_lifecycle_to_planner_state(
    fixed: HlpPlannerStatic,
    state: &mut HlpPlannerState,
    policy: SwapCashPolicy,
    asset_in: MarketAsset,
    amount_in_after_fee: u64,
    amount_out: u64,
) -> Result<LeverageLifecycleTransition> {
    let start = LeverageLifecycleState::capture_planner(fixed, *state);
    let curve_before = match policy {
        SwapCashPolicy::Liquidate { debt_asset, .. } => Some(state.curve_reserve(fixed, debt_asset)?),
        _ => None,
    };
    let plan = derive_leverage_lifecycle_plan_from_state(
        start,
        policy,
        asset_in,
        amount_in_after_fee,
        amount_out,
        curve_before,
    )?;
    commit_leverage_lifecycle_planner_state(state, plan.post);
    Ok(plan.transition)
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
    supplied_debt_curve_reserve_before_share_removal: Option<u64>,
) -> Result<LeverageLifecyclePlan> {
    let mut post = start;
    let mut debt = start.debt();
    let mut transition = LeverageLifecycleTransition::default();
    let mut cash_debit_out = amount_out;
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
            cash_debit_out = transition.clearance.interest_paid;
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
            amount_out
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
            .checked_sub(amount_out)
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
    )?;
    require!(expected == plan, ErrorCode::BrokenInvariant);
    #[cfg(test)]
    let planner_expected = {
        let fixed = HlpPlannerStatic::capture(market)?;
        let mut state = HlpPlannerState::capture(market);
        let transition = apply_leverage_lifecycle_to_planner_state(
            fixed,
            &mut state,
            plan.policy,
            plan.asset_in,
            plan.amount_in_after_fee,
            plan.amount_out,
        )?;
        (state, transition)
    };
    commit_leverage_lifecycle_state(market, plan.post);
    #[cfg(test)]
    {
        require!(
            planner_expected.0 == HlpPlannerState::capture(market),
            ErrorCode::BrokenInvariant
        );
        require!(planner_expected.1 == plan.transition, ErrorCode::BrokenInvariant);
    }
    Ok(plan.transition)
}

fn socialized_loss_checkpoint_and_navs(
    market: &mut Market,
    debt_asset: MarketAsset,
    expected_live_reserve: u64,
    current_slot: u64,
    checkpoint: CurveCheckpoint,
) -> Result<(crate::math::ConcentratedEvaluation, i128, i128)> {
    let original_live_reserve = market.side(debt_asset).reserves.live_reserve;
    market.side_mut(debt_asset).reserves.live_reserve = expected_live_reserve;
    let projected = (|| {
        let evaluation = checkpoint.validated_evaluation(market, current_slot)?;
        let prices = hlp_curve_prices_from_base_price_nad(evaluation.marginal_price_nad)?;
        let (base_nav_nad, quote_nav_nad) = current_hlp_signed_navs_with_prices(market, prices)?;
        Ok((evaluation, base_nav_nad, quote_nav_nad))
    })();
    market.side_mut(debt_asset).reserves.live_reserve = original_live_reserve;
    projected
}

#[inline(never)]
fn derive_leverage_socialized_loss_plan(
    market: &mut Market,
    debt_asset: MarketAsset,
    transition: LeverageLifecycleTransition,
    current_slot: u64,
) -> Result<LeverageSocializedLossPlan> {
    let socialized_principal_loss = transition.socialized_principal_loss;
    if socialized_principal_loss == 0 {
        return Ok(LeverageSocializedLossPlan::Noop {
            debt_asset,
            transition,
            current_slot,
            start_live_reserve: market.side(debt_asset).reserves.live_reserve,
            start_amm: market.amm,
        });
    }
    require!(market.amm.initialized, ErrorCode::BrokenInvariant);

    let start_live_reserve = market.side(debt_asset).reserves.live_reserve;
    let post_live_reserve = start_live_reserve
        .checked_sub(socialized_principal_loss)
        .ok_or(ErrorCode::ReserveUnderflow)?;
    let center_price_nad = market.current_curve_center_price_nad()?;
    let start_prepared =
        market.prepare_curve_for_reserves_nad(market.curve_reserves_nad()?, center_price_nad, current_slot)?;
    let start_checkpoint = market.checkpoint_for_prepared_curve(start_prepared, current_slot)?;
    let start_evaluation = start_checkpoint.validated_evaluation(market, current_slot)?;
    let start_prices = hlp_curve_prices_from_base_price_nad(start_evaluation.marginal_price_nad)?;
    let (base_nav_before_nad, quote_nav_before_nad) = current_hlp_signed_navs_with_prices(market, start_prices)?;

    let loss_nad = normalize_to_nad(
        socialized_principal_loss as u128,
        market.side(debt_asset).asset_decimals,
    )?;
    let mut successor_reserves = start_checkpoint.reserves;
    match debt_asset {
        MarketAsset::Base => {
            successor_reserves.base = successor_reserves
                .base
                .checked_sub(loss_nad)
                .ok_or(ErrorCode::ReserveUnderflow)?;
        }
        MarketAsset::Quote => {
            successor_reserves.quote = successor_reserves
                .quote
                .checked_sub(loss_nad)
                .ok_or(ErrorCode::ReserveUnderflow)?;
        }
    }
    let successor_prepared =
        market.prepare_curve_for_reserves_nad(successor_reserves, center_price_nad, current_slot)?;
    let successor_checkpoint = market.checkpoint_for_prepared_curve(successor_prepared, current_slot)?;
    let (successor_evaluation, base_nav_after_nad, quote_nav_after_nad) = socialized_loss_checkpoint_and_navs(
        market,
        debt_asset,
        post_live_reserve,
        current_slot,
        successor_checkpoint,
    )?;

    let mut post_amm = market.amm;
    let q_per_share_nad = market.curve_q_per_share_nad(successor_evaluation.balanced_equivalent_q)?;
    post_amm.commit_invariant(successor_evaluation.invariant_d)?;
    post_amm.checkpoint_recenter_or_loss(q_per_share_nad);
    let rebase = HlpSocializedLossRebase {
        base_nav_delta_nad: base_nav_after_nad
            .checked_sub(base_nav_before_nad)
            .ok_or(ErrorCode::MarketMathOverflow)?,
        quote_nav_delta_nad: quote_nav_after_nad
            .checked_sub(quote_nav_before_nad)
            .ok_or(ErrorCode::MarketMathOverflow)?,
    };
    Ok(LeverageSocializedLossPlan::Apply {
        debt_asset,
        socialized_principal_loss,
        current_slot,
        start_live_reserve,
        post_live_reserve,
        start_amm: market.amm,
        post_amm,
        start_checkpoint,
        successor_checkpoint,
        base_nav_before_nad,
        quote_nav_before_nad,
        base_nav_after_nad,
        quote_nav_after_nad,
        rebase,
    })
}

#[inline(never)]
fn apply_leverage_socialized_loss_plan(
    market: &mut Market,
    plan: &LeverageSocializedLossPlan,
) -> Result<HlpSocializedLossRebase> {
    if let LeverageSocializedLossPlan::Noop {
        debt_asset,
        transition,
        current_slot,
        start_live_reserve,
        start_amm,
    } = plan
    {
        require_eq!(transition.socialized_principal_loss, 0, ErrorCode::BrokenInvariant);
        require_eq!(
            market.side(*debt_asset).reserves.live_reserve,
            *start_live_reserve,
            ErrorCode::BrokenInvariant
        );
        require!(market.amm == *start_amm, ErrorCode::BrokenInvariant);
        let expected = derive_leverage_socialized_loss_plan(market, *debt_asset, *transition, *current_slot)?;
        require!(&expected == plan, ErrorCode::BrokenInvariant);
        return Ok(HlpSocializedLossRebase::default());
    }

    let LeverageSocializedLossPlan::Apply {
        debt_asset,
        socialized_principal_loss,
        current_slot,
        start_live_reserve,
        post_live_reserve,
        start_amm,
        post_amm,
        start_checkpoint,
        successor_checkpoint,
        base_nav_before_nad,
        quote_nav_before_nad,
        base_nav_after_nad,
        quote_nav_after_nad,
        rebase,
    } = plan
    else {
        unreachable!("no-op socialized-loss plans return above")
    };

    require_eq!(
        market.side(*debt_asset).reserves.live_reserve,
        *start_live_reserve,
        ErrorCode::BrokenInvariant
    );
    require!(market.amm == *start_amm, ErrorCode::BrokenInvariant);
    require_eq!(
        *post_live_reserve,
        start_live_reserve
            .checked_sub(*socialized_principal_loss)
            .ok_or(ErrorCode::ReserveUnderflow)?,
        ErrorCode::BrokenInvariant
    );

    let start_evaluation = start_checkpoint.validated_evaluation(market, *current_slot)?;
    let start_prices = hlp_curve_prices_from_base_price_nad(start_evaluation.marginal_price_nad)?;
    let current_before = current_hlp_signed_navs_with_prices(market, start_prices)?;
    require_eq!(current_before.0, *base_nav_before_nad, ErrorCode::BrokenInvariant);
    require_eq!(current_before.1, *quote_nav_before_nad, ErrorCode::BrokenInvariant);

    let (successor_evaluation, current_base_after, current_quote_after) = socialized_loss_checkpoint_and_navs(
        market,
        *debt_asset,
        *post_live_reserve,
        *current_slot,
        *successor_checkpoint,
    )?;
    require_eq!(current_base_after, *base_nav_after_nad, ErrorCode::BrokenInvariant);
    require_eq!(current_quote_after, *quote_nav_after_nad, ErrorCode::BrokenInvariant);
    let expected_rebase = HlpSocializedLossRebase {
        base_nav_delta_nad: current_base_after
            .checked_sub(current_before.0)
            .ok_or(ErrorCode::MarketMathOverflow)?,
        quote_nav_delta_nad: current_quote_after
            .checked_sub(current_before.1)
            .ok_or(ErrorCode::MarketMathOverflow)?,
    };
    require!(expected_rebase == *rebase, ErrorCode::BrokenInvariant);

    let mut expected_post_amm = market.amm;
    let q_per_share_nad = market.curve_q_per_share_nad(successor_evaluation.balanced_equivalent_q)?;
    expected_post_amm.commit_invariant(successor_evaluation.invariant_d)?;
    expected_post_amm.checkpoint_recenter_or_loss(q_per_share_nad);
    require!(expected_post_amm == *post_amm, ErrorCode::BrokenInvariant);

    market.side_mut(*debt_asset).reserves.live_reserve = *post_live_reserve;
    market.amm = *post_amm;
    Ok(*rebase)
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

    fn cap_tracking_unrealized_interest(&mut self, asset: MarketAsset, amount: u64) {
        cap_hlp_tracking_unrealized_interest(&mut self.base_pre_rebalance, asset, amount);
        cap_hlp_tracking_unrealized_interest(&mut self.quote_pre_rebalance, asset, amount);
    }
}

impl LeverageSwapQuote {
    pub(crate) fn from_amm(quote: AmmSwapQuote, current_slot: u64) -> Self {
        Self {
            explicit_curve: quote.is_explicit(),
            asset_in: quote.asset_in.code(),
            quoted_slot: current_slot,
            amount_in: quote.fee.reserve_credit,
            amount_in_after_fee: quote.fee.amount_in_for_quote,
            reserve_input_credit: quote.fee.reserve_input_credit,
            amount_out: quote.amount_out,
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
        let base = u64::try_from(
            (total_credit as u128)
                .checked_mul(fee.base_fee_debit as u128)
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
    pub interest_paid: u64,
    pub collateral_sold: u64,
    pub closeout_value: u64,
    pub residual: u64,
    pub swap: LeverageSwapQuote,
    pub fees: FeesReceipt,
    pub base_hlp_rebalance: HlpRebalanceReceipt,
    pub quote_hlp_rebalance: HlpRebalanceReceipt,
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
    pub(crate) fn apply_leverage_lifecycle_transition(
        &mut self,
        policy: SwapCashPolicy,
        asset_in: MarketAsset,
        amount_in_after_fee: u64,
        amount_out: u64,
    ) -> Result<LeverageLifecycleTransition> {
        let plan = derive_leverage_lifecycle_plan(self, policy, asset_in, amount_in_after_fee, amount_out)?;
        apply_leverage_lifecycle_plan(self, plan)
    }

    pub(crate) fn checkpoint_leverage_lifecycle_inventory(
        &mut self,
        asset_in: MarketAsset,
        retained_surcharge: u64,
        current_slot: u64,
    ) -> Result<()> {
        self.checkpoint_amm_neutral_inventory_raw(current_slot)?;
        if retained_surcharge > 0 {
            self.side_mut(asset_in).credit_reserve(retained_surcharge, true)?;
            let evaluation = self.evaluate_current_amm_liquidity(current_slot)?;
            let q_per_share_nad = self.curve_q_per_share_nad(evaluation.balanced_equivalent_q)?;
            self.amm.commit_invariant(evaluation.invariant_d)?;
            self.amm.checkpoint_retained_surcharge(q_per_share_nad)?;
        }
        Ok(())
    }

    /// Checkpoints the same two-stage leverage lifecycle from the
    /// identity-bound endpoints already proved by an authoritative quote.
    /// Unlike the public swap checkpoint, this raw composite helper leaves
    /// retention-target deferral to the enclosing lifecycle, preserving the
    /// existing neutral-then-retained ordering without solving either curve
    /// state again.
    pub(crate) fn checkpoint_leverage_lifecycle_inventory_from_quote(
        &mut self,
        asset_in: MarketAsset,
        retained_surcharge: u64,
        current_slot: u64,
        trade_endpoint: CurveCheckpoint,
        reserve_endpoint: CurveCheckpoint,
    ) -> Result<()> {
        self.ensure_amm_initialized(current_slot)?;
        require!(self.amm.initialized, ErrorCode::BrokenInvariant);
        let trade_evaluation = trade_endpoint.validated_evaluation(self, current_slot)?;
        let trade_q_per_share_nad = self.curve_q_per_share_nad(trade_evaluation.balanced_equivalent_q)?;
        self.amm.commit_invariant(trade_evaluation.invariant_d)?;
        self.amm.checkpoint_neutral_liquidity(trade_q_per_share_nad);

        if retained_surcharge > 0 {
            self.side_mut(asset_in).credit_reserve(retained_surcharge, true)?;
            let reserve_evaluation = reserve_endpoint.validated_evaluation(self, current_slot)?;
            let reserve_q_per_share_nad = self.curve_q_per_share_nad(reserve_evaluation.balanced_equivalent_q)?;
            self.amm.commit_invariant(reserve_evaluation.invariant_d)?;
            self.amm.checkpoint_retained_surcharge(reserve_q_per_share_nad)?;
        }
        Ok(())
    }

    fn rebase_explicit_curve_at_current_ordinary_point(&mut self) -> Result<u128> {
        let ordinary = self.integrated_curve_state_nad()?;
        let parameters = self
            .config
            .amm
            .explicit_curve_parameters()?
            .ok_or(ErrorCode::InvalidMarketConfig)?;
        self.amm.explicit_curve_cache = prepare_explicit_cache_at_point(
            ordinary.ordinary_base,
            ordinary.ordinary_quote,
            self.current_curve_center_price_nad()?,
            parameters,
        )?;
        self.amm.clear_invariant();
        let q_nad = self
            .amm
            .explicit_curve_cache
            .tail_liquidity
            .checked_add(self.amm.explicit_curve_cache.concentrated_liquidity)
            .ok_or(ErrorCode::InvariantOverflow)?;
        self.curve_q_per_share_nad(q_nad)
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
            .current_explicit_spot_price_nad()?
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
        // or concentration policy. Reconstruct the unique positive explicit
        // curve through that point in O(1); the following algebraic hLP
        // transition then restores zero opposite-asset exposure.
        let q_per_share_nad = self.rebase_explicit_curve_at_current_ordinary_point()?;
        self.amm.checkpoint_recenter_or_loss(q_per_share_nad);
        self.defer_amm_retention_target()?;
        self.advance_curve_revision()?;

        let end_price = self
            .current_explicit_spot_price_nad()?
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
        let pre_state = self.dynamic_fee_pre_state(current_slot)?;
        let preliminary = self.preliminary_swap_inputs_for_state(amount_in, current_slot, pre_state)?;
        let quote = self
            .quote_explicit_integrated_with_fee(asset_in, amount_in, preliminary)?
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
        Ok(())
    }

    #[cfg(test)]
    fn leverage_amm_quote(quote: LeverageSwapQuote, asset_in: MarketAsset) -> AmmSwapQuote {
        AmmSwapQuote::new_without_endpoints(
            asset_in,
            quote.amount_out,
            quote.start_price_nad,
            quote.end_price_nad,
            quote.reserve_end_price_nad,
            quote.decayed_volatility_nad,
            quote.post_success_volatility_nad,
            quote.fee_breakdown,
        )
    }

    /// Commits the AMM leg, performs the same exact inline hLP correction used
    /// by spot, then materializes the final curve/risk identity. The returned
    /// receipts are settled by the instruction as one net token change per
    /// side; no maintenance call can be required later.
    fn finalize_leverage_swap_hlp(
        &mut self,
        prepared_swap: PreparedLeverageSwap,
        current_slot: u64,
        socialized_loss_applied: bool,
    ) -> Result<(HlpRebalanceReceipt, HlpRebalanceReceipt)> {
        if prepared_swap.swap.fee_breakdown.retained_surcharge > 0 {
            let asset_in = MarketAsset::try_from_code(prepared_swap.swap.asset_in)?;
            self.credit_protected_recenter_reserve(asset_in, prepared_swap.swap.fee_breakdown.retained_surcharge)?;
        }
        let fresh_transition;
        let transition = if socialized_loss_applied {
            fresh_transition = prepare_explicit_hlp_transition_at_current_state(self)?;
            &fresh_transition
        } else {
            prepared_swap
                .explicit_transition
                .as_deref()
                .ok_or(ErrorCode::BrokenInvariant)?
        };
        let (base, quote) = transition.consume(self)?;
        self.finalize_amm_trade_after_inventory_checkpoint(
            prepared_swap.swap.start_price_nad,
            prepared_swap.swap.reserve_end_price_nad,
            current_slot,
        )?;
        let q_nad = self
            .amm
            .explicit_curve_cache
            .tail_liquidity
            .checked_add(self.amm.explicit_curve_cache.concentrated_liquidity)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let final_price_nad = if socialized_loss_applied {
            self.current_explicit_spot_price_nad()?
                .ok_or(ErrorCode::BrokenInvariant)?
        } else {
            prepared_swap.swap.reserve_end_price_nad
        };
        self.observe_risk_from_explicit_curve(final_price_nad, q_nad, current_slot)?;
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
            self.finalize_leverage_swap_hlp(prepared_swap, opened_slot, false)?;
        let closeout_value = self.require_position_initial_leverage_health(position, opened_slot)?;
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
            self.finalize_leverage_swap_hlp(prepared_swap, current_slot, false)?;
        let closeout_value = self.require_position_initial_leverage_health(position, current_slot)?;
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
        mut prepared_swap: PreparedLeverageSwap,
        swap_fee_credit: LeverageSwapFeeCredit,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        current_slot: u64,
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
        let closeout_value = self.leverage_closeout_value(position, current_slot)?;
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
        mut prepared_swap: PreparedLeverageSwap,
        swap_fee_credit: LeverageSwapFeeCredit,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        current_slot: u64,
    ) -> Result<Box<LeverageCloseReceipt>> {
        let swap = prepared_swap.swap;
        position.require_open()?;
        let debt_asset = position.debt_asset()?;
        let collateral_asset = debt_asset.opposite();
        let displayed_debt = position.debt_amount(&self.debt)?;
        require_gt!(displayed_debt, 0, ErrorCode::ZeroDebtAmount);
        let debt_amount = self
            .debt
            .isolated_repayment_for_max(debt_asset, position.debt_shares, u64::MAX)?
            .cash_repaid;
        let collateral_sold = position.collateral_amount;
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
            debt_shares: position.debt_shares,
            debt_principal: position.debt_principal,
        };
        prepared_swap.require_cash_policy(cash_policy)?;
        let lifecycle = self.apply_leverage_lifecycle_transition(
            cash_policy,
            collateral_asset,
            swap.amount_in_after_fee,
            swap.amount_out,
        )?;
        let clearance = lifecycle.clearance;
        position.debt_shares = lifecycle.position_debt_shares;
        position.debt_principal = lifecycle.position_debt_principal;
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
        position.collateral_amount = 0;
        let (base_hlp_rebalance, quote_hlp_rebalance) =
            self.finalize_leverage_swap_hlp(prepared_swap, current_slot, false)?;
        Ok(Box::new(LeverageCloseReceipt {
            debt_repaid: clearance.cash_repaid,
            interest_paid: clearance.interest_paid,
            collateral_sold,
            closeout_value: swap.amount_out,
            residual,
            swap,
            fees,
            base_hlp_rebalance,
            quote_hlp_rebalance,
        }))
    }

    pub fn liquidate_leverage(
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
            self.finalize_leverage_swap_hlp(prepared_swap, current_slot, lifecycle.socialized_principal_loss > 0)?;
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
    ) -> Result<LeverageUpdateReceipt> {
        position.require_open()?;
        require!(borrow_amount > 0, ErrorCode::AmountZero);
        let debt_asset = position.debt_asset()?;
        let debt_before = position.debt_amount(&self.debt)?;
        let debt_after = debt_before
            .checked_add(borrow_amount)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        let collateral_asset = position.collateral_asset()?;
        let pre_finalize_closeout_quote =
            self.quote_leverage_swap(collateral_asset, position.collateral_amount, current_slot)?;
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
        let closeout_value = self.require_position_initial_leverage_health(position, current_slot)?;
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
        let collateral_asset = position.collateral_asset()?;
        self.quote_leverage_swap(collateral_asset, position.collateral_amount, current_slot)
            .map(|quote| quote.amount_out)
    }

    fn require_position_initial_leverage_health(&self, position: &LeveragePosition, current_slot: u64) -> Result<u64> {
        let collateral_asset = position.collateral_asset()?;
        let closeout_quote = self.quote_leverage_swap(collateral_asset, position.collateral_amount, current_slot)?;
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
    ) -> Result<AmmSwapQuote> {
        require_eq!(
            swap.fee_breakdown.reserve_input_credit,
            swap.fee_breakdown
                .amount_in_for_quote
                .checked_add(swap.fee_breakdown.retained_surcharge)
                .ok_or(ErrorCode::ReserveOverflow)?,
            ErrorCode::BrokenInvariant
        );
        require!(swap.explicit_curve, ErrorCode::BrokenInvariant);
        let mut state = self.integrated_curve_state_nad()?;
        let input_nad = normalize_to_nad(
            swap.fee_breakdown.reserve_input_credit as u128,
            self.side(asset_in).asset_decimals,
        )?;
        let output_nad = normalize_to_nad(swap.amount_out as u128, self.side(asset_in.opposite()).asset_decimals)?;
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
        let preliminary = self.preliminary_swap_inputs_for_state(collateral_amount, current_slot, pre_state)?;
        let quote = self
            .quote_explicit_integrated_with_fee_from_state(collateral_asset, collateral_amount, preliminary, state)?
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
        current_slot: u64,
    ) -> Result<FeesReceipt> {
        swap_fee_credit.validate_for_quote(&swap)?;
        require_eq!(
            swap.reserve_input_credit,
            swap.amount_in_after_fee
                .checked_add(swap.fee_breakdown.retained_surcharge)
                .ok_or(ErrorCode::ReserveOverflow)?,
            ErrorCode::BrokenInvariant
        );
        // The shared lifecycle transition already committed executable
        // reserves and debt. Checkpoint that exact curve state, then route the
        // retained principal identically for execution and predictive scratch.
        if !swap.explicit_curve {
            self.checkpoint_leverage_lifecycle_inventory(
                asset_in,
                swap.fee_breakdown.retained_surcharge,
                current_slot,
            )?;
        }

        let (side_in, side_out) = self.swap_sides_mut(asset_in);
        let fees = side_in.record_claimable_swap_fees(
            swap_fee_credit.base,
            swap_fee_credit.distributed_surcharge,
            protocol_fee_bps,
            protocol_auction_split,
            fee_eligible_ylp_supply,
        )?;
        side_in.assert_share_backing()?;
        side_out.assert_share_backing()?;
        side_in.fees.assert_backed()?;
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

#[cfg(test)]
mod tests {
    include!("../tests/market/leverage.rs");
}
