use super::*;
use crate::state::AmmConfig;
use crate::{
    instructions::PreparedSwap,
    market::SwapFeeBreakdown,
    math::{mul_div_u128, ExplicitCurveParameters},
    state::Debt,
};
use proptest::prelude::*;

impl<'a> NewPositionPreviewContext<'a> {
    fn new(market: &'a Market, debt_asset: MarketAsset, collateral_amount: u64, risk: &'a Risk) -> Result<Self> {
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
    // Every tradable market has a nonzero ordinary yLP supply. The explicit
    // curve derives ordinary versus hLP ownership from this canonical supply.
    market.base_side.shares.ylp_supply = 1_000_000;
    market.quote_side.shares.ylp_supply = 1_000_000;
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
    market.prepare_amm_for_swap(0).unwrap();
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
    market.deposit_single_sided(MarketAsset::Base, 100_000, 1).unwrap();
    market.deposit_single_sided(MarketAsset::Quote, 200_000, 1).unwrap();
    market.config.amm = AmmConfig {
        range_width_nad: 4 * NAD,
        concentrated_liquidity_share_nad: NAD / 2,
        center_ema_half_life_ms: MIN_HALF_LIFE_MS,
        volatility_half_life_ms: MIN_HALF_LIFE_MS,
        volatility_shock_cap_nad: NAD / 10,
        volatility_cap_nad: NAD,
        ..AmmConfig::default()
    };
    market.amm = crate::state::AmmState::default();
    market.prepare_amm_for_swap(1).unwrap();
    assert!(market.has_active_hlp());
    market
}

fn active_explicit_preview_market() -> Market {
    let mut market = active_concentrated_preview_market();
    market.config.amm.range_width_nad = 0;
    market.config.amm.concentrated_liquidity_share_nad = 0;
    market
        .config
        .amm
        .set_explicit_curve_parameters(ExplicitCurveParameters {
            range_width_nad: 4 * NAD,
            concentrated_liquidity_share_nad: NAD / 2,
        })
        .unwrap();
    market.amm = crate::state::AmmState::default();
    market.prepare_amm_for_swap(1).unwrap();
    market
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
        market.base_side.daily_borrow_bucket.remaining(limit, slot).unwrap()
    );
}

#[test]
fn concentrated_hlp_preview_and_execution_share_the_same_accepted_plan_in_both_directions() {
    let market = active_concentrated_preview_market();
    let protocol_split = crate::state::ProtocolAuctionSplit {
        fee_auction_bps: 6_000,
        buyback_auction_bps: 4_000,
    };

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
        assert_eq!(
            preview_market.try_to_vec().unwrap(),
            execution_market.try_to_vec().unwrap()
        );
        assert!(preview.quote.amount_out > 0);
        let fast = preview
            .finalize_state(&mut preview_market, request.current_slot, 2_500, protocol_split)
            .unwrap();
        let execution_receipt = execution
            .finalize_state(&mut execution_market, request.current_slot, 2_500, protocol_split)
            .unwrap();
        assert_eq!(fast, execution_receipt, "receipt mismatch for {asset_in:?}");
        assert!(
            crate::instructions::hlp_receipt_mutates_curve_inventory(&fast.base_rebalance)
                || crate::instructions::hlp_receipt_mutates_curve_inventory(&fast.quote_rebalance),
            "the integrated endpoint must reconstruct hLP ownership for {asset_in:?}"
        );
        assert_eq!(
            preview_market.try_to_vec().unwrap(),
            execution_market.try_to_vec().unwrap(),
            "terminal/legacy market mismatch for {asset_in:?}"
        );
    }
}

#[test]
fn explicit_spot_reconstructs_both_hlps_without_solver_cells() {
    let mut market = active_explicit_preview_market();
    let prepared = SwapRequest {
        current_slot: 1,
        asset_in: MarketAsset::Base,
        reserve_credit: 35_000,
    }
    .prepare(&mut market)
    .unwrap();
    let finalized = prepared
        .finalize_state(
            &mut market,
            1,
            2_500,
            crate::state::ProtocolAuctionSplit {
                fee_auction_bps: 6_000,
                buyback_auction_bps: 4_000,
            },
        )
        .unwrap();
    assert!(
        finalized.base_rebalance.ylp_mint_amount > 0 || finalized.base_rebalance.ylp_burn_amount > 0
    );
    assert!(
        finalized.quote_rebalance.ylp_mint_amount > 0 || finalized.quote_rebalance.ylp_burn_amount > 0
    );
    market.assert_market_invariants().unwrap();

    let supply = market.base_side.shares.ylp_supply as u128;
    let base_hlp_quote_claim = mul_div_u128(
        market.quote_side.reserves.live_reserve as u128,
        market.base_hlp_vault.ylp_shares as u128,
        supply,
    )
    .unwrap();
    let quote_hlp_base_claim = mul_div_u128(
        market.base_side.reserves.live_reserve as u128,
        market.quote_hlp_vault.ylp_shares as u128,
        supply,
    )
    .unwrap();
    let base_hlp_quote_debt =
        Debt::shares_to_debt(market.base_hlp_vault.debt_shares, market.debt.quote_borrow_index_nad).unwrap();
    let quote_hlp_base_debt =
        Debt::shares_to_debt(market.quote_hlp_vault.debt_shares, market.debt.base_borrow_index_nad).unwrap();
    assert!(base_hlp_quote_claim.abs_diff(base_hlp_quote_debt) <= 1);
    assert!(quote_hlp_base_claim.abs_diff(quote_hlp_base_debt) <= 1);
}

#[test]
fn concentrated_swap_preserves_preexisting_fee_and_hlp_yield_state() {
    let mut market = active_concentrated_preview_market();
    let protocol_split = crate::state::ProtocolAuctionSplit {
        fee_auction_bps: 6_000,
        buyback_auction_bps: 4_000,
    };
    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        let eligible_ylp_supply = market.side(asset).shares.ylp_supply;
        let side = market.side_mut(asset);
        side.record_claimable_swap_fees(137, 41, 2_500, protocol_split, eligible_ylp_supply)
            .unwrap();
        side.record_interest_credit(113, 2_500, protocol_split, 0).unwrap();
    }
    market.checkpoint_hlp_yield_from_ylp(MarketAsset::Base).unwrap();
    market.checkpoint_hlp_yield_from_ylp(MarketAsset::Quote).unwrap();

    let seeded_base_swap_index = market.base_side.fees.swap_fee_growth_index_q64;
    let seeded_quote_interest_index = market.quote_side.fees.interest_growth_index_q64;
    let seeded_base_hlp_swap_checkpoint = market.base_hlp_vault.base_swap_fee_checkpoint_q64;
    let seeded_quote_hlp_interest_checkpoint = market.quote_hlp_vault.quote_interest_checkpoint_q64;
    assert!(seeded_base_swap_index > 0);
    assert!(seeded_quote_interest_index > 0);
    assert!(seeded_base_hlp_swap_checkpoint > 0);
    assert!(seeded_quote_hlp_interest_checkpoint > 0);

    for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
        let request = SwapRequest {
            current_slot: 1,
            asset_in,
            reserve_credit: 350_000,
        };
        let mut terminal_market = market.clone();
        let terminal = request.prepare(&mut terminal_market).unwrap();
        let mut replay_market = market.clone();
        let replay = request.prepare(&mut replay_market).unwrap();

        let terminal_receipt = terminal
            .finalize_state(&mut terminal_market, request.current_slot, 2_500, protocol_split)
            .unwrap();
        let replay_receipt = replay
            .finalize_state(&mut replay_market, request.current_slot, 2_500, protocol_split)
            .unwrap();

        assert_eq!(terminal_receipt, replay_receipt, "receipt mismatch for {asset_in:?}");
        assert_eq!(
            terminal_market.try_to_vec().unwrap(),
            replay_market.try_to_vec().unwrap(),
            "deterministic market mismatch for {asset_in:?}"
        );
        assert!(terminal_market.base_side.fees.swap_fee_growth_index_q64 >= seeded_base_swap_index);
        assert!(terminal_market.quote_side.fees.interest_growth_index_q64 >= seeded_quote_interest_index);
        assert!(terminal_market.base_hlp_vault.base_swap_fee_checkpoint_q64 >= seeded_base_hlp_swap_checkpoint);
        assert!(
            terminal_market.quote_hlp_vault.quote_interest_checkpoint_q64 >= seeded_quote_hlp_interest_checkpoint
        );
    }
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
    market.deposit_single_sided(MarketAsset::Base, 100_000, 1).unwrap();
    market.deposit_single_sided(MarketAsset::Quote, 200_000, 1).unwrap();
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
    assert!(preview_market.amm.explicit_curve_cache.tail_liquidity > 0);
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
        range_width_nad: 4 * NAD,
        concentrated_liquidity_share_nad: NAD / 2,
        adjustment_threshold_nad: NAD / 100,
        adjustment_step_nad: NAD / 100,
        min_adjustment_interval_slots: 1,
        divergence_fee_coefficient_nad: 10 * NAD,
        ..AmmConfig::default()
    };
    retained_market.amm = crate::state::AmmState::default();
    retained_market.prepare_amm_for_swap(1).unwrap();
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
    let retained_execution = retained_request.prepare(&mut retained_execution_market).unwrap();
    assert_prepared_swaps_equal(&retained_preview, &retained_execution);
    assert!(retained_preview.quote.fee.retained_surcharge > 0);
    let protected_before = retained_preview_market.base_side.reserves.protected_recenter_reserve;
    let curve_before = retained_preview_market.amm.explicit_curve_cache;
    let q_before = retained_preview_market.amm.q_per_share_nad;
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
    // Retained atoms are physically funded after the endpoint but remain
    // outside executable reserves, yLP NAV, and withdrawal claims. The current
    // quote never moves or rebuilds its own curve.
    assert!(retained_preview_market.amm.retention_target_stale);
    assert_eq!(retained_preview_market.amm.explicit_curve_cache, curve_before);
    assert_eq!(retained_preview_market.amm.q_per_share_nad, q_before);
    assert_eq!(
        retained_preview_market.base_side.reserves.protected_recenter_reserve,
        protected_before + retained_preview.quote.fee.retained_surcharge
    );
    assert_eq!(retained_preview_market.quote_side.reserves.protected_recenter_reserve, 0);

    let protected_after = retained_preview_market.base_side.reserves.protected_recenter_reserve;
    let live_before_withdraw = retained_preview_market.base_side.reserves.live_reserve;
    let supply_before_withdraw = retained_preview_market.base_side.shares.ylp_supply;
    let burn = 1_000;
    let withdrawal = retained_preview_market.remove_liquidity(burn).unwrap();
    assert_eq!(
        retained_preview_market.base_side.reserves.protected_recenter_reserve,
        protected_after
    );
    assert_eq!(
        withdrawal.base_amount_out,
        (live_before_withdraw as u128 * burn as u128 / supply_before_withdraw as u128) as u64
    );
    assert!(withdrawal.base_amount_out < protected_after.saturating_add(withdrawal.base_amount_out));
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
