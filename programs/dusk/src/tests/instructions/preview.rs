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

#[test]
fn exhausted_hlp_waterfall_draws_insurance_before_socializing_funding_interest() {
    let target_asset = MarketAsset::Base;
    let borrowed_asset = MarketAsset::Quote;
    let mut market = active_concentrated_preview_market();
    let vault_shares = market.base_hlp_vault.ylp_shares;
    let ylp_supply_before = market.base_side.shares.ylp_supply;
    let target_claim = u64::try_from(
        (market.base_side.reserves.live_reserve as u128) * (vault_shares as u128)
            / (ylp_supply_before as u128),
    )
    .unwrap();
    let borrowed_claim = u64::try_from(
        (market.quote_side.reserves.live_reserve as u128) * (vault_shares as u128)
            / (ylp_supply_before as u128),
    )
    .unwrap();
    let target_curve = market.curve_reserve(target_asset).unwrap();
    let borrowed_curve = market.curve_reserve(borrowed_asset).unwrap();
    let target_value = u64::try_from(
        mul_div_u128(
            target_claim as u128,
            borrowed_curve as u128,
            target_curve as u128,
        )
        .unwrap(),
    )
    .unwrap();
    let collateral = borrowed_claim.checked_add(target_value).unwrap();
    let desired_debt = collateral.checked_add(1_000).unwrap();
    market.debt.quote_borrow_index_nad = crate::math::mul_div_ceil_u128(
        desired_debt as u128,
        NAD as u128,
        market.base_hlp_vault.debt_shares,
    )
    .unwrap();
    let actual_debt = u64::try_from(
        Debt::shares_to_debt(
            market.base_hlp_vault.debt_shares,
            market.debt.quote_borrow_index_nad,
        )
        .unwrap(),
    )
    .unwrap();
    let shortfall = actual_debt.saturating_sub(collateral);
    assert!(shortfall > 600);
    assert!(shortfall <= actual_debt.saturating_sub(market.base_hlp_vault.debt_principal));
    market.insurance.quote_available = 600;

    // The permissionless instruction deliberately advances debt and the AMM
    // clock without invoking ordinary hLP settlement, which must reject an
    // already-exhausted vault.
    market.advance_amm_clock(1).unwrap();

    let mut insurance_capped = market.clone();
    assert!(insurance_capped
        .prepare_terminal_hlp_waterfall(target_asset, 599)
        .is_err());

    let plan = market
        .prepare_terminal_hlp_waterfall(target_asset, 600)
        .unwrap();
    assert_eq!(plan.insurance_request(), 600);
    let before_capped = market.try_to_vec().unwrap();
    assert!(plan.consume(&mut market, 600, 600, shortfall - 601).is_err());
    assert_eq!(market.try_to_vec().unwrap(), before_capped);

    let receipt = plan
        .consume(&mut market, 600, 600, shortfall - 600)
        .unwrap();
    assert_eq!(receipt.insurance_drawn, 600);
    assert_eq!(receipt.socialized_loss, shortfall - 600);
    assert_eq!(receipt.debt_closed, actual_debt);
    assert_eq!(receipt.ylp_burn_amount, vault_shares);
    assert_eq!(market.insurance.quote_available, 0);
    assert_eq!(market.base_hlp_vault.ylp_shares, 0);
    assert_eq!(market.base_hlp_vault.debt_shares, 0);
    assert_eq!(market.base_hlp_vault.debt_principal, 0);
    assert_eq!(market.base_side.shares.ylp_supply, ylp_supply_before - vault_shares);
    assert_eq!(market.quote_side.shares.ylp_supply, ylp_supply_before - vault_shares);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();

    let closed_supply = market.base_hlp_vault.hlp_supply;
    let first_burn = closed_supply / 2;
    let first = market
        .withdraw_single_sided(target_asset, first_burn)
        .unwrap();
    assert_eq!(first.target_amount_out, 0);
    assert_eq!(first.ylp_amount, 0);
    assert_eq!(first.hlp_supply, closed_supply - first_burn);
    let final_receipt = market
        .withdraw_single_sided(target_asset, first.hlp_supply)
        .unwrap();
    assert_eq!(final_receipt.target_amount_out, 0);
    assert_eq!(final_receipt.hlp_supply, 0);
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
        current_unix_timestamp: 0,
        asset_in: MarketAsset::Base,
        reserve_credit: 50_000,
        protocol_fee_bps: 0,
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
fn launch_fee_is_applied_to_the_real_quote_and_decays_to_the_normal_fee() {
    let mut market = active_explicit_preview_market();
    market.config.start_time = 1_000;
    market.config.swap_fee_bps = 30;
    market.config.amm.launch_fee_start_bps = 1_000;
    market.config.amm.launch_fee_duration_seconds = 100;
    market.config.amm.launch_fee_decay_mode = crate::state::LAUNCH_FEE_DECAY_LINEAR;

    let at_launch = SwapRequest {
        current_slot: 1,
        current_unix_timestamp: 1_000,
        asset_in: MarketAsset::Base,
        reserve_credit: 35_000,
        protocol_fee_bps: 0,
    }
    .prepare(&mut market.clone())
    .unwrap();
    let after_launch = SwapRequest {
        current_slot: 1,
        current_unix_timestamp: 1_100,
        asset_in: MarketAsset::Base,
        reserve_credit: 35_000,
        protocol_fee_bps: 0,
    }
    .prepare(&mut market)
    .unwrap();

    assert!(at_launch.quote.fee.base_fee_debit > after_launch.quote.fee.base_fee_debit);
    assert!(at_launch.quote.amount_out < after_launch.quote.amount_out);
}

#[test]
fn launch_buy_size_limiter_charges_only_the_configured_buy_direction() {
    let mut market = active_explicit_preview_market();
    market.config.start_time = 1_000;
    market.config.amm.launch_rate_limit_asset = crate::state::LAUNCH_RATE_LIMIT_ASSET_BASE;
    market.config.amm.launch_rate_limit_reference_nad = 10_000_000;
    market.config.amm.launch_rate_limit_increment_bps = 100;
    market.config.amm.launch_rate_limit_max_fee_bps = 1_000;
    market.config.amm.launch_rate_limit_duration_seconds = 100;

    let buy_base = SwapRequest {
        current_slot: 1,
        current_unix_timestamp: 1_000,
        asset_in: MarketAsset::Quote,
        reserve_credit: 35_000,
        protocol_fee_bps: 0,
    }
    .prepare(&mut market.clone())
    .unwrap();
    let sell_base = SwapRequest {
        current_slot: 1,
        current_unix_timestamp: 1_000,
        asset_in: MarketAsset::Base,
        reserve_credit: 35_000,
        protocol_fee_bps: 0,
    }
    .prepare(&mut market)
    .unwrap();

    assert!(buy_base.quote.fee.base_fee_debit > sell_base.quote.fee.base_fee_debit);
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
            current_unix_timestamp: 0,
            asset_in,
            reserve_credit,
            protocol_fee_bps: 2_500,
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
fn stressed_hlp_recovery_improves_the_matching_swap_and_restores_the_hedge() {
    let healthy = active_concentrated_preview_market();
    let mut stressed = healthy.clone();
    stressed.debt.quote_borrow_index_nad = mul_div_u128(stressed.debt.quote_borrow_index_nad, 17, 16).unwrap();

    // Quote input supplies the asset borrowed by the Base hLP, so the Base hLP
    // pays the incremental Base output and deleverages in the same transition.
    let request = SwapRequest {
        current_slot: 1,
        current_unix_timestamp: 0,
        asset_in: MarketAsset::Quote,
        reserve_credit: 350_000,
        protocol_fee_bps: 2_500,
    };
    let mut healthy_quote_market = healthy;
    let healthy_prepared = request.prepare(&mut healthy_quote_market).unwrap();

    let ordinary_ylp_shares_before = stressed
        .base_side
        .shares
        .ylp_supply
        .checked_sub(stressed.base_hlp_vault.ylp_shares)
        .unwrap()
        .checked_sub(stressed.quote_hlp_vault.ylp_shares)
        .unwrap();
    let prepared = request.prepare(&mut stressed).unwrap();
    assert_eq!(prepared.quote.recovery.target_asset, MarketAsset::Base.code());
    assert!(prepared.quote.recovery.funding_gap > 0);
    assert!(prepared.quote.recovery.matched_input > 0);
    assert!(prepared.quote.recovery.bonus_output > 0);
    assert!(prepared.quote.recovery.discount_bps > 0);
    assert!(prepared.quote.amount_out > healthy_prepared.quote.amount_out);

    let receipt = prepared
        .finalize_state(
            &mut stressed,
            request.current_slot,
            2_500,
            crate::state::ProtocolAuctionSplit {
                fee_auction_bps: 6_000,
                buyback_auction_bps: 4_000,
            },
        )
        .unwrap();
    assert!(receipt.base_rebalance.interest_paid > 0);
    stressed.assert_market_invariants().unwrap();

    let supply = stressed.base_side.shares.ylp_supply as u128;
    let curve_reserves = stressed.curve_reserves_nad().unwrap();
    let opposite_claim = mul_div_u128(
        curve_reserves.quote,
        stressed.base_hlp_vault.ylp_shares as u128,
        supply,
    )
    .unwrap();
    let debt = Debt::shares_to_debt(stressed.base_hlp_vault.debt_shares, stressed.debt.quote_borrow_index_nad).unwrap();
    let debt_nad = debt.checked_mul(NAD as u128).unwrap();
    assert!(
        opposite_claim.abs_diff(debt_nad) <= NAD as u128,
        "opposite_claim={opposite_claim} debt_nad={debt_nad} delta={}",
        opposite_claim.abs_diff(debt_nad)
    );

    // Recovery may burn hLP-owned yLP, but it must not confiscate ordinary
    // yLP ownership to finance the trader improvement.
    let ordinary_ylp_shares_after = stressed
        .base_side
        .shares
        .ylp_supply
        .checked_sub(stressed.base_hlp_vault.ylp_shares)
        .unwrap()
        .checked_sub(stressed.quote_hlp_vault.ylp_shares)
        .unwrap();
    assert_eq!(ordinary_ylp_shares_after, ordinary_ylp_shares_before);
}

#[test]
fn explicit_spot_reconstructs_both_hlps_without_solver_cells() {
    let mut market = active_explicit_preview_market();
    let prepared = SwapRequest {
        current_slot: 1,
        current_unix_timestamp: 0,
        asset_in: MarketAsset::Base,
        reserve_credit: 35_000,
        protocol_fee_bps: 2_500,
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
fn forty_percent_fee_compounding_is_native_to_cpmm_and_concentrated_swaps() {
    let protocol_split = crate::state::ProtocolAuctionSplit {
        fee_auction_bps: 6_000,
        buyback_auction_bps: 4_000,
    };
    let curve_parameters = [
        ExplicitCurveParameters::cpmm(),
        ExplicitCurveParameters {
            range_width_nad: 4 * NAD,
            concentrated_liquidity_share_nad: NAD / 2,
        },
    ];

    for parameters in curve_parameters {
        for fee_mode in [
            crate::state::SWAP_FEE_COLLECT_INPUT_ASSET,
            crate::state::SWAP_FEE_COLLECT_QUOTE_ONLY,
        ] {
            let mut baseline = active_concentrated_preview_market();
            baseline.config.amm.set_explicit_curve_parameters(parameters).unwrap();
            baseline.config.amm.swap_fee_collect_mode = fee_mode;
            baseline.config.amm.compounding_fee_bps = 0;
            baseline.amm = crate::state::AmmState::default();
            baseline.prepare_amm_for_swap(1).unwrap();
            let mut compounded = baseline.clone();
            compounded.config.amm.compounding_fee_bps = 4_000;
            let request = SwapRequest {
                current_slot: 1,
                current_unix_timestamp: 0,
                asset_in: MarketAsset::Base,
                reserve_credit: 350_000,
                protocol_fee_bps: 2_500,
            };

            let baseline_prepared = request.prepare(&mut baseline).unwrap();
            let compounded_prepared = request.prepare(&mut compounded).unwrap();
            let baseline_quote = baseline_prepared.quote;
            let compounded_quote = compounded_prepared.quote;
            assert_eq!(compounded_quote.amount_out, baseline_quote.amount_out);
            assert_eq!(compounded_quote.gross_amount_out, baseline_quote.gross_amount_out);
            assert_eq!(compounded_quote.fee.base_fee_debit, baseline_quote.fee.base_fee_debit);
            assert_eq!(compounded_quote.fee.dynamic_surcharge_debit, baseline_quote.fee.dynamic_surcharge_debit);

            let protocol_fee = baseline_quote.fee.base_fee_debit as u128 * 2_500 / BPS_DENOMINATOR as u128;
            let lp_base_fee = baseline_quote.fee.base_fee_debit as u128 - protocol_fee;
            let expected_compounded_base = lp_base_fee * 4_000 / BPS_DENOMINATOR as u128;
            let expected_compounded_dynamic =
                baseline_quote.fee.distributed_surcharge_debit as u128 * 4_000 / BPS_DENOMINATOR as u128;
            let expected_compounded = u64::try_from(expected_compounded_base + expected_compounded_dynamic).unwrap();
            assert!(expected_compounded > 0);
            assert_eq!(compounded_quote.fee.compounded_fee_debit, expected_compounded);
            assert_eq!(
                baseline_quote.fee.claimable_fee_debit - compounded_quote.fee.claimable_fee_debit,
                expected_compounded
            );

            baseline_prepared
                .finalize_state(&mut baseline, 1, 2_500, protocol_split)
                .unwrap();
            compounded_prepared
                .finalize_state(&mut compounded, 1, 2_500, protocol_split)
                .unwrap();
            let fee_asset = MarketAsset::try_from_code(compounded_quote.fee.fee_asset).unwrap();
            assert_eq!(
                baseline.side(fee_asset).fees.total_liability().unwrap()
                    - compounded.side(fee_asset).fees.total_liability().unwrap(),
                expected_compounded
            );
            assert_eq!(
                compounded.side(fee_asset).reserves.cash_reserve - baseline.side(fee_asset).reserves.cash_reserve,
                expected_compounded
            );
            assert!(compounded.base_hlp_vault.last_nav_nad > baseline.base_hlp_vault.last_nav_nad);
            assert!(compounded.quote_hlp_vault.last_nav_nad > baseline.quote_hlp_vault.last_nav_nad);
            compounded.assert_market_invariants().unwrap();

            let supply = compounded.base_side.shares.ylp_supply as u128;
            let base_hlp_quote_claim = mul_div_u128(
                compounded.quote_side.reserves.live_reserve as u128,
                compounded.base_hlp_vault.ylp_shares as u128,
                supply,
            )
            .unwrap();
            let quote_hlp_base_claim = mul_div_u128(
                compounded.base_side.reserves.live_reserve as u128,
                compounded.quote_hlp_vault.ylp_shares as u128,
                supply,
            )
            .unwrap();
            let base_hlp_quote_debt = Debt::shares_to_debt(
                compounded.base_hlp_vault.debt_shares,
                compounded.debt.quote_borrow_index_nad,
            )
            .unwrap();
            let quote_hlp_base_debt = Debt::shares_to_debt(
                compounded.quote_hlp_vault.debt_shares,
                compounded.debt.base_borrow_index_nad,
            )
            .unwrap();
            assert!(base_hlp_quote_claim.abs_diff(base_hlp_quote_debt) <= 1);
            assert!(quote_hlp_base_claim.abs_diff(quote_hlp_base_debt) <= 1);
        }
    }
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
            current_unix_timestamp: 0,
            asset_in,
            reserve_credit: 350_000,
            protocol_fee_bps: 2_500,
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
    market.checkpoint_amm_neutral_inventory_raw(1).unwrap();
    let request = SwapRequest {
        current_slot: 1,
        current_unix_timestamp: 0,
        asset_in: MarketAsset::Base,
        reserve_credit: 350_000,
        protocol_fee_bps: 2_500,
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
        current_unix_timestamp: 0,
        asset_in: MarketAsset::Base,
        reserve_credit: 50_000,
        protocol_fee_bps: 2_500,
    }
    .prepare(&mut retained_market)
    .unwrap();
    first
        .finalize_state(&mut retained_market, 1, 2_500, protocol_split)
        .unwrap();
    assert!(retained_market.amm.retain_dynamic_surcharge);

    let retained_request = SwapRequest {
        current_slot: 1,
        current_unix_timestamp: 0,
        asset_in: MarketAsset::Base,
        reserve_credit: 10_000,
        protocol_fee_bps: 2_500,
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
