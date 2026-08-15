use super::*;
use crate::{
    instructions::PreparedSwap,
    market::SwapFeeBreakdown,
    math::{concentrated_hybrid_branch, ConcentratedHybridBranch},
};
use crate::state::AmmConfig;
use proptest::prelude::*;

impl<'a> NewPositionPreviewContext<'a> {
    fn new(market: &'a Market, debt_asset: MarketAsset, collateral_amount: u64, risk: &'a Risk) -> Result<Self> {
        let collateral_asset = debt_asset.opposite();
        let (collateral_virtual_reserve_nad, debt_virtual_reserve_nad) =
            market.pessimistic_virtual_reserves_nad(collateral_asset, risk, true)?;
        Ok(Self {
            market,
            debt_asset,
            collateral_amount,
            risk,
            existing_total_debt_nad: market.total_fixed_debt_nad(debt_asset)?,
            current_aggregate_contribution: match debt_asset {
                MarketAsset::Base => market.debt.global_health_quote_contribution_for_base_debt,
                MarketAsset::Quote => market.debt.global_health_base_contribution_for_quote_debt,
            },
            collateral_amount_nad: normalize_to_nad(
                collateral_amount as u128,
                market.side(collateral_asset).asset_decimals,
            )?,
            collateral_virtual_reserve_nad,
            debt_virtual_reserve_nad,
        })
    }

    fn is_accepted(&self, projected_debt_amount: u64) -> Result<bool> {
        let (terms, _) = self.terms(projected_debt_amount)?;
        Ok(terms.max_debt >= projected_debt_amount
            && terms.projected_market_health_bps >= self.market.config.borrow_market_health_floor_bps as u64)
    }
}

fn max_new_position_debt_by_dynamic_health(context: &NewPositionPreviewContext<'_>, upper_bound: u64) -> Result<u64> {
    let current_health = context.market.market_health_from_risk(context.risk)?;
    if context.market.assert_market_health_snapshot(&current_health).is_err() {
        return Ok(0);
    }

    let mut low = 0_u64;
    let mut high = upper_bound;
    while low < high {
        let midpoint = low + (high - low) / 2 + 1;
        let (terms, _) = context.terms(midpoint)?;
        if terms.max_debt >= midpoint
            && terms.projected_market_health_bps >= context.market.config.borrow_market_health_floor_bps as u64
        {
            low = midpoint;
        } else {
            high = midpoint - 1;
        }
    }
    Ok(low)
}

fn preview_test_market(existing_base_debt: u64, aggregate_quote_contribution: u64) -> Market {
    let mut market = Market::default();
    market.base_side.asset_decimals = 0;
    market.quote_side.asset_decimals = 0;
    market.base_side.reserves.live_reserve = 1_000_000;
    market.base_side.reserves.cash_reserve = 1_000_000;
    market.quote_side.reserves.live_reserve = 1_000_000;
    market.quote_side.reserves.cash_reserve = 1_000_000;
    market.debt.base_borrow_index_nad = NAD as u128;
    market.debt.quote_borrow_index_nad = NAD as u128;
    market.debt.fixed_base_shares = existing_base_debt as u128;
    market.debt.global_health_quote_contribution_for_base_debt = aggregate_quote_contribution;
    market.config.global_health_contribution_cap_bps = 15_000;
    market.config.borrow_market_health_floor_bps = 11_000;
    market.risk = Risk {
        base_price_ema_nad: NAD,
        quote_price_ema_nad: NAD,
        directional_base_price_ema_nad: NAD,
        directional_quote_price_ema_nad: NAD,
        q_ema_nad: 1_000_000_u128 * NAD as u128,
        ..Risk::default()
    };
    market
}

fn active_concentrated_preview_market() -> Market {
    let mut market = preview_test_market(0, 0);
    market.quote_side.reserves.live_reserve = 2_000_000;
    market.quote_side.reserves.cash_reserve = 2_000_000;
    market.base_side.shares.ylp_supply = 1_000_000;
    market.quote_side.shares.ylp_supply = 1_000_000;
    market.config.swap_fee_bps = 30;
    market.config.target_hlp_leverage_bps = 20_000;
    market.config.settlement_divergence_bps = 10_000;
    market.config.max_daily_borrow_bps = crate::state::MAX_DAILY_BORROW_BPS;
    market
        .deposit_single_sided(MarketAsset::Base, 100_000, 1)
        .unwrap();
    market
        .deposit_single_sided(MarketAsset::Quote, 200_000, 1)
        .unwrap();
    market.config.amm = AmmConfig {
        peak_depth_nad: 200 * NAD,
        fade_scale_nad: NAD / 10,
        center_ema_half_life_ms: MIN_HALF_LIFE_MS,
        volatility_half_life_ms: MIN_HALF_LIFE_MS,
        volatility_shock_cap_nad: NAD / 10,
        volatility_cap_nad: NAD,
        ..AmmConfig::default()
    };
    market.checkpoint_amm_neutral_inventory(1).unwrap();
    assert!(market.has_active_hlp());
    assert!(!market.current_curve_parameters(1).is_cpmm());
    market
}

fn branch_for_reserves(
    market: &Market,
    base_reserve_nad: u128,
    quote_reserve_nad: u128,
) -> ConcentratedHybridBranch {
    let parameters = market.current_curve_parameters(1);
    concentrated_hybrid_branch(
        base_reserve_nad,
        quote_reserve_nad,
        market.current_curve_center_price_nad().unwrap() as u128,
        parameters.peak_depth_nad as u128,
        parameters.fade_scale_nad as u128,
    )
    .unwrap()
}

fn assert_prepared_swaps_equal(preview: &PreparedSwap, execution: &PreparedSwap) {
    assert_eq!(preview.quote, execution.quote);
    assert_eq!(preview.base_pre_rebalance, execution.base_pre_rebalance);
    assert_eq!(preview.quote_pre_rebalance, execution.quote_pre_rebalance);
    assert_eq!(preview.fee_eligible_ylp_supply, execution.fee_eligible_ylp_supply);
    assert_eq!(preview.interest_eligibility, execution.interest_eligibility);
}

#[test]
fn dynamic_health_binary_search_matches_brute_force() {
    let market = preview_test_market(50_000, 75_000);
    let upper_bound = 5_000;
    let context = NewPositionPreviewContext::new(&market, MarketAsset::Base, 5_000, &market.risk).unwrap();
    let binary = max_new_position_debt_by_dynamic_health(&context, upper_bound).unwrap();
    let brute = (0..=upper_bound)
        .filter(|candidate| context.is_accepted(*candidate).unwrap())
        .max()
        .unwrap();

    assert_eq!(binary, brute);
}

#[test]
fn reserve_custodied_fee_split_does_not_apply_a_second_transfer_fee() {
    let fee = SwapFeeBreakdown {
        base_fee_debit: 60,
        distributed_surcharge_debit: 40,
        claimable_fee_debit: 100,
        retained_surcharge: 25,
        ..SwapFeeBreakdown::default()
    };

    let (base_credit, distributed_credit) = split_claimable_fee_credit(&fee, 100).unwrap();

    assert_eq!(base_credit, 60);
    assert_eq!(distributed_credit, 40);
    assert_eq!(base_credit + distributed_credit, fee.claimable_fee_debit);
}

#[test]
fn preview_and_execution_use_identical_state_plans() {
    let mut preview_market = preview_test_market(0, 0);
    preview_market.base_side.shares.ylp_supply = 1_000_000;
    preview_market.quote_side.shares.ylp_supply = 1_000_000;
    let mut execution_market = preview_market.clone();
    let context = SwapRequest {
        current_slot: 7,
        asset_in: MarketAsset::Base,
        reserve_credit: 50_000,
    };

    let preview = context.prepare(&mut preview_market).unwrap();
    let execution = context.prepare(&mut execution_market).unwrap();

    assert_eq!(preview.quote, execution.quote);
    assert_eq!(preview.base_pre_rebalance, execution.base_pre_rebalance);
    assert_eq!(preview.quote_pre_rebalance, execution.quote_pre_rebalance);
    assert_eq!(preview.fee_eligible_ylp_supply, execution.fee_eligible_ylp_supply);
    assert_eq!(preview_market.amm, execution_market.amm);
}

#[test]
fn preview_daily_remaining_uses_the_authoritative_bucket_decay() {
    let mut market = preview_test_market(0, 0);
    market.config.max_daily_borrow_bps = 2_000;
    let limit = market
        .daily_limit_for_side(MarketAsset::Base, market.config.max_daily_borrow_bps)
        .unwrap();
    market
        .side_mut(MarketAsset::Base)
        .daily_borrow_bucket
        .record_borrow(limit / 2, limit, 0)
        .unwrap();
    let slot = crate::constants::MS_PER_DAY / crate::constants::TARGET_MS_PER_SLOT / 4;

    assert_eq!(
        daily_borrow_remaining(&market, MarketAsset::Base, slot).unwrap(),
        market
            .base_side
            .daily_borrow_bucket
            .remaining(limit, slot)
            .unwrap()
    );
}

#[test]
fn concentrated_hlp_preview_and_execution_share_the_same_accepted_plan_in_both_directions() {
    let market = active_concentrated_preview_market();

    for (asset_in, reserve_credit) in [(MarketAsset::Base, 350_000), (MarketAsset::Quote, 350_000)] {
        let request = SwapRequest {
            current_slot: 1,
            asset_in,
            reserve_credit,
        };
        let mut preview_market = market.clone();
        let preview = request.prepare(&mut preview_market).unwrap();
        let mut execution_market = market.clone();
        let execution = request.prepare(&mut execution_market).unwrap();

        assert_prepared_swaps_equal(&preview, &execution);
        assert_eq!(preview_market.try_to_vec().unwrap(), execution_market.try_to_vec().unwrap());
        assert!(preview.quote.amount_out > 0);
        assert!(
            crate::instructions::hlp_receipt_mutates_curve_inventory(&preview.base_pre_rebalance)
                || crate::instructions::hlp_receipt_mutates_curve_inventory(&preview.quote_pre_rebalance),
            "the accepted concentrated path must exercise predictive hLP positioning for {asset_in:?}"
        );
    }
}

#[test]
fn concentrated_hlp_preview_and_execution_share_an_accepted_branch_crossing() {
    let market = active_concentrated_preview_market();
    let request = SwapRequest {
        current_slot: 1,
        asset_in: MarketAsset::Base,
        reserve_credit: 350_000,
    };
    let mut preview_market = market.clone();
    let preview = request.prepare(&mut preview_market).unwrap();
    let mut execution_market = market.clone();
    let execution = request.prepare(&mut execution_market).unwrap();

    assert_prepared_swaps_equal(&preview, &execution);
    assert_eq!(preview_market.try_to_vec().unwrap(), execution_market.try_to_vec().unwrap());
    let start = preview_market.curve_reserves_nad().unwrap();
    let endpoint = preview.quote.trade_endpoint().unwrap().reserves;
    let start_branch = branch_for_reserves(&preview_market, start.base, start.quote);
    let endpoint_branch = branch_for_reserves(&preview_market, endpoint.base, endpoint.quote);
    assert_eq!(start_branch, ConcentratedHybridBranch::Inner);
    assert_eq!(endpoint_branch, ConcentratedHybridBranch::QuoteScarceTransition);
}

#[test]
fn preview_and_spot_share_the_exact_post_quote_state_lifecycle() {
    let mut market = preview_test_market(0, 0);
    market.quote_side.reserves.live_reserve = 2_000_000;
    market.quote_side.reserves.cash_reserve = 2_000_000;
    market.base_side.shares.ylp_supply = 1_000_000;
    market.quote_side.shares.ylp_supply = 1_000_000;
    market.config.swap_fee_bps = 30;
    market.config.target_hlp_leverage_bps = 20_000;
    market.config.settlement_divergence_bps = 10_000;
    market.config.max_daily_borrow_bps = crate::state::MAX_DAILY_BORROW_BPS;
    market
        .deposit_single_sided(MarketAsset::Base, 100_000, 1)
        .unwrap();
    market
        .deposit_single_sided(MarketAsset::Quote, 200_000, 1)
        .unwrap();
    market.config.divergence_fee_share_cap_bps = 2_000;
    market.config.amm.divergence_fee_coefficient_nad = 10 * NAD;
    market.checkpoint_amm_neutral_inventory(1).unwrap();
    let request = SwapRequest {
        current_slot: 1,
        asset_in: MarketAsset::Base,
        reserve_credit: 350_000,
    };
    let protocol_split = crate::state::ProtocolAuctionSplit {
        fee_auction_bps: 6_000,
        buyback_auction_bps: 4_000,
    };

    let mut preview_market = market.clone();
    let preview = request.prepare(&mut preview_market).unwrap();
    let mut execution_market = market;
    let execution = request.prepare(&mut execution_market).unwrap();
    assert_prepared_swaps_equal(&preview, &execution);
    assert!(preview.quote.fee.dynamic_surcharge_debit > 0);

    let preview_finalized = preview
        .finalize_state(&mut preview_market, request.current_slot, 2_500, protocol_split)
        .unwrap();
    let execution_finalized = execution
        .finalize_state(&mut execution_market, request.current_slot, 2_500, protocol_split)
        .unwrap();

    assert_eq!(preview_finalized, execution_finalized);
    assert_eq!(preview_market.amm, execution_market.amm);
    assert_eq!(preview_market.risk, execution_market.risk);
    assert_eq!(
        preview_market.base_hlp_vault.try_to_vec().unwrap(),
        execution_market.base_hlp_vault.try_to_vec().unwrap()
    );
    assert_eq!(
        preview_market.quote_hlp_vault.try_to_vec().unwrap(),
        execution_market.quote_hlp_vault.try_to_vec().unwrap()
    );
    assert_eq!(
        preview_market.try_to_vec().unwrap(),
        execution_market.try_to_vec().unwrap()
    );
    assert!(preview_market.amm.invariant_d_nad > 0);
    assert!(preview_market.amm.q_per_share_nad > 0);
    assert!(preview_market.risk.cached_q_nad > 0);

    // Retention is armed by an outward concentrated trade, then the following
    // quote commits the surcharge as protected principal through the same path.
    let mut retained_market = preview_test_market(0, 0);
    retained_market.quote_side.reserves.live_reserve = 2_000_000;
    retained_market.quote_side.reserves.cash_reserve = 2_000_000;
    retained_market.base_side.shares.ylp_supply = 1_000_000;
    retained_market.quote_side.shares.ylp_supply = 1_000_000;
    retained_market.config.swap_fee_bps = 0;
    retained_market.config.divergence_fee_share_cap_bps = 5_000;
    retained_market.config.amm = AmmConfig {
        peak_depth_nad: 200 * NAD,
        fade_scale_nad: NAD / 10,
        adjustment_threshold_nad: NAD / 100,
        adjustment_step_nad: NAD / 100,
        min_adjustment_interval_slots: 1,
        divergence_fee_coefficient_nad: 10 * NAD,
        ..AmmConfig::default()
    };
    retained_market.checkpoint_amm_neutral_inventory(1).unwrap();
    let first = SwapRequest {
        current_slot: 1,
        asset_in: MarketAsset::Base,
        reserve_credit: 50_000,
    }
    .prepare(&mut retained_market)
    .unwrap();
    first
        .finalize_state(&mut retained_market, 1, 2_500, protocol_split)
        .unwrap();
    assert!(retained_market.amm.retain_dynamic_surcharge);

    let retained_request = SwapRequest {
        current_slot: 1,
        asset_in: MarketAsset::Base,
        reserve_credit: 10_000,
    };
    let mut retained_preview_market = retained_market.clone();
    let retained_preview = retained_request.prepare(&mut retained_preview_market).unwrap();
    let mut retained_execution_market = retained_market;
    let retained_execution = retained_request
        .prepare(&mut retained_execution_market)
        .unwrap();
    assert_prepared_swaps_equal(&retained_preview, &retained_execution);
    assert!(retained_preview.quote.fee.retained_surcharge > 0);
    let protected_before = retained_preview_market.amm.spendable_protected_profit_nad();
    let retained_preview_finalized = retained_preview
        .finalize_state(&mut retained_preview_market, 1, 2_500, protocol_split)
        .unwrap();
    let retained_execution_finalized = retained_execution
        .finalize_state(&mut retained_execution_market, 1, 2_500, protocol_split)
        .unwrap();
    assert_eq!(retained_preview_finalized, retained_execution_finalized);
    assert_eq!(retained_preview_market.amm, retained_execution_market.amm);
    assert_eq!(retained_preview_market.risk, retained_execution_market.risk);
    assert_eq!(
        retained_preview_market.try_to_vec().unwrap(),
        retained_execution_market.try_to_vec().unwrap()
    );
    assert!(retained_preview_market.amm.spendable_protected_profit_nad() > protected_before);
}

#[test]
fn concentrated_hlp_preview_and_execution_fail_closed_without_partial_state() {
    let market = active_concentrated_preview_market();
    let context = SwapRequest {
        current_slot: 1,
        asset_in: MarketAsset::Base,
        // This raw-granularity case has no candidate inside both hLP tracking
        // budgets and must fail closed after the bounded solver evaluations.
        reserve_credit: 1_000,
    };
    let mut preview_market = market.clone();
    let preview_error = context.prepare(&mut preview_market).unwrap_err();
    let mut execution_market = market.clone();
    let execution_error = context.prepare(&mut execution_market).unwrap_err();

    assert_eq!(preview_error, error!(ErrorCode::HlpSettlementUnavailable));
    assert_eq!(execution_error, preview_error);

    // `SwapRequest::prepare` is transaction-atomic in execution and operates
    // on a disposable clone in preview. The solver's own stronger guarantee
    // is restoration to its post-accrual/controller snapshot when no bounded
    // candidate is safe.
    let mut solver_market = market.clone();
    solver_market.accrue_interest_to_slot(context.current_slot).unwrap();
    solver_market.prepare_amm_for_swap(context.current_slot).unwrap();
    solver_market
        .advance_one_amm_controller_target(context.current_slot)
        .unwrap();
    let pre_state = solver_market.dynamic_fee_pre_state(context.current_slot).unwrap();
    let preliminary = solver_market
        .preliminary_swap_inputs_for_state(context.reserve_credit, context.current_slot, pre_state)
        .unwrap();
    let before_solver = solver_market.try_to_vec().unwrap();
    let solver_error = crate::market::liquidity::pre_solve_hlps_for_swap_joint(
        &mut solver_market,
        context.asset_in,
        context.reserve_credit,
        context.current_slot,
        pre_state,
        preliminary,
        crate::market::liquidity::SwapCashPolicy::Spot,
    )
    .unwrap_err();

    assert_eq!(solver_error, error!(ErrorCode::HlpSettlementUnavailable));
    assert_eq!(solver_market.try_to_vec().unwrap(), before_solver);
}

proptest! {
    #[test]
    fn dynamic_health_acceptance_is_monotonic(
        existing_debt in 0_u64..100_000,
        existing_contribution_bps in 13_000_u64..=15_000,
        collateral_amount in 1_u64..500_000,
        lower_candidate in 0_u64..300_000,
        candidate_delta in 0_u64..300_000,
    ) {
        let aggregate_contribution = existing_debt
            .saturating_mul(existing_contribution_bps)
            / BPS_DENOMINATOR as u64;
        let market = preview_test_market(existing_debt, aggregate_contribution);
        let context = NewPositionPreviewContext::new(
            &market,
            MarketAsset::Base,
            collateral_amount,
            &market.risk,
        )
        .unwrap();
        let higher_candidate = lower_candidate.saturating_add(candidate_delta);
        let lower_accepted = context.is_accepted(lower_candidate).unwrap();
        let higher_accepted = context.is_accepted(higher_candidate).unwrap();

        let (cached_terms, cached_contribution) = context.terms(lower_candidate).unwrap();
        let projected_debt_nad = normalize_to_nad(lower_candidate as u128, 0).unwrap();
        let projected_aggregate = aggregate_contribution
            .checked_add(cached_contribution)
            .unwrap();
        let full_terms = market
            .dynamic_borrow_terms(
                MarketAsset::Base,
                collateral_amount,
                existing_debt as u128 * NAD as u128,
                existing_debt as u128 * NAD as u128 + projected_debt_nad,
                projected_aggregate,
                &market.risk,
            )
            .unwrap();

        prop_assert!(!higher_accepted || lower_accepted);
        prop_assert_eq!(cached_terms, full_terms);

        let upper_bound = 600_000;
        let maximum = max_new_position_debt_by_dynamic_health(&context, upper_bound).unwrap();
        if maximum > 0 {
            prop_assert!(context.is_accepted(maximum).unwrap());
        }
        if maximum < upper_bound {
            prop_assert!(!context.is_accepted(maximum + 1).unwrap());
        }
    }
}
