use super::*;
use crate::state::AmmConfig;
use crate::{
    constants::{BPS_DENOMINATOR, MARKET_LAYOUT_VERSION},
    math::{cpmm_amount_out, hlp_opposite_exposure_nad, ideal_hlp_rebalance_nad, market_spot_price_nad},
    state::{Insurance, MarketConfig, MarketSide, Risk, YieldAccount},
};
use proptest::prelude::*;

fn empty_hlp_yield_account() -> YieldAccount {
    YieldAccount {
        owner: Pubkey::default(),
        market: Pubkey::default(),
        lp_mint: Pubkey::default(),
        asset_mint: Pubkey::default(),
        token_kind: 0,
        recipient: Pubkey::default(),
        swap_fee_checkpoint_q64: 0,
        interest_checkpoint_q64: 0,
        accrued_swap_fee_amount: 0,
        accrued_interest_amount: 0,
        swap_fee_remainder_q64: 0,
        interest_remainder_q64: 0,
        bump: 0,
    }
}

fn checkpoint_hlp_vaults(market: &mut Market) -> Result<(i128, i128)> {
    market.checkpoint_hlp_vaults()
}

fn rebalance_hlp_vaults(market: &mut Market) -> Result<(HlpRebalanceReceipt, HlpRebalanceReceipt)> {
    let current_slot = curve_slot(market);
    let base = if market.base_hlp_vault.hlp_supply > 0 || market.base_hlp_vault.residual_exposure != 0 {
        rebalance_one_hlp(market, MarketAsset::Base, current_slot)?
    } else {
        empty_hlp_rebalance_receipt(MarketAsset::Base)
    };
    let quote = if market.quote_hlp_vault.hlp_supply > 0 || market.quote_hlp_vault.residual_exposure != 0 {
        rebalance_one_hlp(market, MarketAsset::Quote, current_slot)?
    } else {
        empty_hlp_rebalance_receipt(MarketAsset::Quote)
    };
    Ok((base, quote))
}

fn apply_hlp_reserve_debit_reference(
    market: &mut Market,
    target_asset: MarketAsset,
    reserve_asset: MarketAsset,
    reserve_debit: u64,
    cash_debit: u64,
    interest_paid: u64,
) -> Result<()> {
    let synthetic_debit = reserve_debit
        .checked_sub(cash_debit)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let backing_credit = cash_debit
        .checked_sub(interest_paid)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    debit_hlp_live_reserve(market, target_asset, reserve_asset, synthetic_debit)?;
    market.side_mut(reserve_asset).debit_reserve(cash_debit, true)?;
    market
        .side_mut(reserve_asset)
        .reserves
        .credit_hlp_backing_inventory(target_asset, backing_credit)
}

/// Test-only mutation-order oracle for the extracted plan. It consumes the
/// plan's already-canonical amounts through the pre-extraction reserve/share/
/// debt methods, so the comparison checks the new atomic state commit without
/// copying any planning formula.
fn apply_hlp_rebalance_plan_reference(market: &mut Market, plan: HlpRebalancePlan) -> Result<HlpRebalanceReceipt> {
    let receipt = plan.receipt();
    match plan {
        HlpRebalancePlan::Noop { .. } => {}
        HlpRebalancePlan::LeverageUp {
            common,
            base_leg_amount,
            quote_leg_amount,
            ylp_mint_amount,
            debt_shares_added,
            debt_principal_added,
        } => {
            credit_hlp_live_reserve(market, common.target_asset, MarketAsset::Base, base_leg_amount)?;
            credit_hlp_live_reserve(market, common.target_asset, MarketAsset::Quote, quote_leg_amount)?;
            market.base_side.shares.mint(ylp_mint_amount)?;
            market.quote_side.shares.mint(ylp_mint_amount)?;
            market.base_side.assert_share_backing()?;
            market.quote_side.assert_share_backing()?;
            let vault = match common.target_asset {
                MarketAsset::Base => &mut market.base_hlp_vault,
                MarketAsset::Quote => &mut market.quote_hlp_vault,
            };
            vault.add_debt_shares(debt_shares_added)?;
            vault.add_debt_principal(debt_principal_added)?;
            vault.credit_ylp(ylp_mint_amount)?;
        }
        HlpRebalancePlan::Deleverage {
            common,
            ylp_burn_amount,
            base_reserve_debit,
            quote_reserve_debit,
            base_cash_debit,
            quote_cash_debit,
            debt_repayment,
            debt_clearance,
            interest_paid,
            ..
        } => {
            let borrowed_asset = common.target_asset.opposite();
            apply_hlp_reserve_debit_reference(
                market,
                common.target_asset,
                MarketAsset::Base,
                base_reserve_debit,
                base_cash_debit,
                (borrowed_asset == MarketAsset::Base)
                    .then_some(interest_paid)
                    .unwrap_or(0),
            )?;
            apply_hlp_reserve_debit_reference(
                market,
                common.target_asset,
                MarketAsset::Quote,
                quote_reserve_debit,
                quote_cash_debit,
                (borrowed_asset == MarketAsset::Quote)
                    .then_some(interest_paid)
                    .unwrap_or(0),
            )?;
            market.base_side.shares.burn(ylp_burn_amount)?;
            market.quote_side.shares.burn(ylp_burn_amount)?;
            market.base_side.assert_share_backing()?;
            market.quote_side.assert_share_backing()?;
            let vault = match common.target_asset {
                MarketAsset::Base => &mut market.base_hlp_vault,
                MarketAsset::Quote => &mut market.quote_hlp_vault,
            };
            let clearance = vault.clear_debt_repay(debt_repayment.shares_to_burn, common.start.borrow_index_nad)?;
            assert_eq!(clearance, debt_clearance);
            vault.debit_ylp(ylp_burn_amount)?;
        }
    }
    Ok(receipt)
}

fn assert_hlp_plan_matches_reference(market: Market, plan: HlpRebalancePlan) {
    let mut planned = market.clone();
    let planned_receipt = apply_hlp_rebalance_plan(&mut planned, plan).unwrap();
    let mut reference = market;
    let reference_receipt = apply_hlp_rebalance_plan_reference(&mut reference, plan).unwrap();
    assert_eq!(planned_receipt, reference_receipt);
    assert_eq!(planned.try_to_vec().unwrap(), reference.try_to_vec().unwrap());
}

/// Pre-pair-seam joint orchestration retained only as a differential oracle.
/// Individual mutations still use the independently-tested stage-1 kernel;
/// this copy exercises the old frozen-valuation/Base-then-Quote sequencing.
fn rebalance_hlps_after_swap_joint_reference(
    market: &mut Market,
    current_slot: u64,
    start_price_nad: Option<u64>,
) -> Result<(
    HlpRebalanceReceipt,
    HlpRebalanceReceipt,
    Option<crate::math::ConcentratedEvaluation>,
    HlpCurvePrices,
    HlpLifecycleEndpoint,
    HlpLifecycleEndpoint,
)> {
    let base_active = market.base_hlp_vault.hlp_supply > 0 || market.base_hlp_vault.residual_exposure != 0;
    let quote_active = market.quote_hlp_vault.hlp_supply > 0 || market.quote_hlp_vault.residual_exposure != 0;
    checkpoint_hlp_yield_from_ylp_pair(market, true, true)?;
    let prices = match start_price_nad {
        Some(price) => hlp_curve_prices_from_base_price_nad(price as u128)?,
        None => current_hlp_curve_prices(market)?,
    };
    let (base_values, quote_values) =
        current_hlp_inventory_values_pair_nad_with_prices(market, prices, base_active, quote_active)?;
    let base_valuation = base_active
        .then(|| hlp_valuation_from_values(base_values, prices))
        .transpose()?;
    let quote_valuation = quote_active
        .then(|| hlp_valuation_from_values(quote_values, prices))
        .transpose()?;
    let base = base_valuation
        .map(|valuation| rebalance_hlp_from_valuation(market, MarketAsset::Base, valuation))
        .transpose()?
        .unwrap_or_else(|| empty_hlp_rebalance_receipt(MarketAsset::Base));
    let quote = quote_valuation
        .map(|valuation| rebalance_hlp_from_valuation(market, MarketAsset::Quote, valuation))
        .transpose()?
        .unwrap_or_else(|| empty_hlp_rebalance_receipt(MarketAsset::Quote));
    let inventory_changed = base.ylp_mint_amount != 0
        || base.ylp_burn_amount != 0
        || quote.ylp_mint_amount != 0
        || quote.ylp_burn_amount != 0;
    let final_evaluation = inventory_changed
        .then(|| market.checkpoint_amm_neutral_inventory(current_slot))
        .transpose()?;
    let final_prices = match final_evaluation {
        Some(evaluation) => hlp_curve_prices_from_base_price_nad(evaluation.marginal_price_nad)?,
        None => prices,
    };
    let (base_final_values, quote_final_values) =
        current_hlp_inventory_values_pair_nad_with_prices(market, final_prices, base_active, quote_active)?;
    let base = if base_active {
        refresh_hlp_after_rebalance_from_valuation(
            market,
            MarketAsset::Base,
            base,
            hlp_valuation_from_values(base_final_values, final_prices)?,
        )?
    } else {
        base
    };
    let quote = if quote_active {
        refresh_hlp_after_rebalance_from_valuation(
            market,
            MarketAsset::Quote,
            quote,
            hlp_valuation_from_values(quote_final_values, final_prices)?,
        )?
    } else {
        quote
    };
    Ok((
        base,
        quote,
        final_evaluation,
        final_prices,
        hlp_lifecycle_endpoint_from_values(base_final_values)?,
        hlp_lifecycle_endpoint_from_values(quote_final_values)?,
    ))
}

fn assert_joint_pair_matches_reference(market: Market, current_slot: u64, start_price_nad: Option<u64>) {
    let mut planned = market.clone();
    let planned_result = rebalance_hlps_after_swap_joint(&mut planned, current_slot, start_price_nad).unwrap();
    let mut reference = market;
    let reference_result =
        rebalance_hlps_after_swap_joint_reference(&mut reference, current_slot, start_price_nad).unwrap();
    assert_eq!(planned_result.0, reference_result.0);
    assert_eq!(planned_result.1, reference_result.1);
    assert_eq!(planned_result.2, reference_result.2);
    assert_eq!(planned_result.3, reference_result.3);
    assert_eq!(
        (
            planned_result.4.principal_nav_nad,
            planned_result.4.opposite_exposure_nad,
        ),
        (
            reference_result.4.principal_nav_nad,
            reference_result.4.opposite_exposure_nad,
        )
    );
    assert_eq!(
        (
            planned_result.5.principal_nav_nad,
            planned_result.5.opposite_exposure_nad,
        ),
        (
            reference_result.5.principal_nav_nad,
            reference_result.5.opposite_exposure_nad,
        )
    );
    assert_eq!(planned.try_to_vec().unwrap(), reference.try_to_vec().unwrap());
}

fn require_hlp_swap_path_safe(
    market: &Market,
    start_price_nad: u64,
    end_price_nad: u64,
    base_residual_on_entry: bool,
    quote_residual_on_entry: bool,
) -> Result<()> {
    let start_prices = hlp_curve_prices_from_base_price_nad(start_price_nad as u128)?;
    let end_prices = hlp_curve_prices_from_base_price_nad(end_price_nad as u128)?;
    require_residual_hlp_swap_safe(
        market,
        MarketAsset::Base,
        start_prices,
        end_prices,
        base_residual_on_entry,
    )?;
    require_residual_hlp_swap_safe(
        market,
        MarketAsset::Quote,
        start_prices,
        end_prices,
        quote_residual_on_entry,
    )
}

/// Mirrors the hLP-deposit admission sequence in the instruction after live
/// mint-supply reconciliation. Keeping this test-only avoids restoring a
/// one-call production wrapper.
fn prepare_hlp_deposit_like_instruction(
    market: &mut Market,
    target_asset: MarketAsset,
    current_slot: u64,
) -> Result<()> {
    market.assert_current_version()?;
    market.accrue_interest_to_slot(current_slot)?;
    if market.base_side.reserves.live_reserve > 0 && market.quote_side.reserves.live_reserve > 0 {
        market.advance_amm_clock(current_slot)?;
        market.checkpoint_hlp_vaults()?;
        let prices = current_hlp_curve_prices(market)?;
        let entry = current_hlp_entry_state_with_prices(market, target_asset, prices)?;
        require!(entry.disposition.admits_entry(), ErrorCode::HlpSettlementUnavailable);
        if market.has_active_hlp()
            && market.amm.concentration_ramp.active
            && (!market.amm.applied_curve_parameters.is_cpmm() || !market.amm.concentration_ramp.target.is_cpmm())
        {
            let desired = market.amm.desired_curve_parameters(&market.config.amm, current_slot);
            require!(
                desired == market.amm.applied_curve_parameters,
                ErrorCode::HlpSettlementUnavailable
            );
        }
        market.observe_current_risk(current_slot)?;
    }
    Ok(())
}

fn valid_config() -> MarketConfig {
    MarketConfig {
        swap_fee_bps: 30,
        divergence_fee_share_cap_bps: 0,
        volatility_fee_share_cap_bps: 0,
        target_hlp_leverage_bps: BPS_DENOMINATOR * 2,
        settlement_divergence_bps: 500,
        ema_half_life_ms: 60_000,
        directional_ema_half_life_ms: 60_000,
        q_ema_half_life_ms: 60_000,
        max_daily_borrow_bps: 2_000,
        global_health_contribution_cap_bps: 15_000,
        borrow_market_health_floor_bps: 11_000,
        amm: Default::default(),
        irm: Default::default(),
        start_time: 0,
    }
}

fn seeded_market() -> Market {
    let base_mint = Pubkey::new_unique();
    let quote_mint = Pubkey::new_unique();
    let mut base_side = MarketSide {
        asset_mint: base_mint,
        asset_decimals: 0,
        ..MarketSide::default()
    };
    base_side.reserves.live_reserve = 1_000;
    base_side.reserves.cash_reserve = 1_000;
    base_side.shares.ylp_supply = 1_000;

    let mut quote_side = MarketSide {
        asset_mint: quote_mint,
        asset_decimals: 0,
        ..MarketSide::default()
    };
    quote_side.reserves.live_reserve = 2_000;
    quote_side.reserves.cash_reserve = 2_000;
    quote_side.shares.ylp_supply = 1_000;

    let mut base_hlp_vault = HlpVault::default();
    base_hlp_vault.initialize(Pubkey::new_unique());
    let mut quote_hlp_vault = HlpVault::default();
    quote_hlp_vault.initialize(Pubkey::new_unique());

    Market {
        version: MARKET_LAYOUT_VERSION,
        ylp_mint: Pubkey::new_unique(),
        base_side,
        quote_side,
        config: valid_config(),
        amm: Default::default(),
        debt: Debt {
            base_borrow_index_nad: NAD as u128,
            quote_borrow_index_nad: NAD as u128,
            ..Debt::default()
        },
        base_hlp_vault,
        quote_hlp_vault,
        risk: Risk::default(),
        insurance: Insurance::default(),
        params_hash: [7; 32],
        governance_locked_ylp: 0,
        parameter_revisions: [0; 5],
        last_marginal_observation_nad: 0,
        curve_revision: 0,
        risk_revision: 0,
        last_update_slot: 0,
        reduce_only: false,
        bump: 255,
    }
}

fn shortfall_plan_market(target_asset: MarketAsset) -> Market {
    let borrowed_asset = target_asset.opposite();
    let mut market = seeded_market();
    market.base_side.reserves.live_reserve = 1_000;
    market.quote_side.reserves.live_reserve = 1_000;
    market.base_side.shares.ylp_supply = 1_000;
    market.quote_side.shares.ylp_supply = 1_000;
    market.side_mut(target_asset).reserves.cash_reserve = 970;
    market.side_mut(borrowed_asset).reserves.cash_reserve = 900;
    let vault = match target_asset {
        MarketAsset::Base => &mut market.base_hlp_vault,
        MarketAsset::Quote => &mut market.quote_hlp_vault,
    };
    vault.ylp_shares = 500;
    vault.hlp_supply = 500;
    vault.credit_hlp_live_reserve(target_asset, 30).unwrap();
    vault.credit_hlp_live_reserve(borrowed_asset, 100).unwrap();
    vault.debt_shares = 100;
    vault.debt_principal = 100;
    match borrowed_asset {
        MarketAsset::Base => market.debt.base_borrow_index_nad = 6 * NAD as u128,
        MarketAsset::Quote => market.debt.quote_borrow_index_nad = 6 * NAD as u128,
    }
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
    market
}

fn assert_exact_out_plan_is_canonical(market: &Market, plan: HlpRebalancePlan) {
    let HlpRebalancePlan::Deleverage {
        common,
        base_entitlement_amount,
        quote_entitlement_amount,
        base_reserve_debit,
        quote_reserve_debit,
        exact_out_checkpoint: Some(checkpoint),
        ..
    } = plan
    else {
        panic!("expected exact-out deleverage plan")
    };
    assert_eq!(
        checkpoint.start_curve_reserves_nad(),
        market.curve_reserves_nad().unwrap()
    );

    let prepared = market
        .prepare_curve_for_reserves_nad(
            checkpoint.post_entitlement_curve_reserves_nad(),
            checkpoint.center_price_nad(),
            checkpoint.current_slot(),
        )
        .unwrap();
    assert_eq!(prepared.invariant_d(), checkpoint.start_invariant_d());
    let borrowed_asset = common.target_asset.opposite();
    let direction = match common.target_asset {
        MarketAsset::Base => ConcentratedSwapDirection::BaseToQuote,
        MarketAsset::Quote => ConcentratedSwapDirection::QuoteToBase,
    };
    let amount_out_nad = normalize_to_nad(
        checkpoint.borrowed_shortfall() as u128,
        market.side(borrowed_asset).asset_decimals,
    )
    .unwrap();
    let (_, canonical_input_nad) = prepared
        .quote_exact_out_input_bracket(amount_out_nad, direction)
        .unwrap();
    assert_eq!(checkpoint.selected_input_nad(), canonical_input_nad);
    assert!(prepared.quote_exact_in(canonical_input_nad, direction).unwrap() >= amount_out_nad);
    if canonical_input_nad > 0 {
        assert!(prepared.quote_exact_in(canonical_input_nad - 1, direction).unwrap() < amount_out_nad);
    }

    let start = checkpoint.start_curve_reserves_nad();
    let base_entitlement_nad =
        normalize_to_nad(base_entitlement_amount as u128, market.base_side.asset_decimals).unwrap();
    let quote_entitlement_nad =
        normalize_to_nad(quote_entitlement_amount as u128, market.quote_side.asset_decimals).unwrap();
    assert_eq!(
        checkpoint.post_entitlement_curve_reserves_nad(),
        CurveReservesNad {
            base: start.base - base_entitlement_nad,
            quote: start.quote - quote_entitlement_nad,
        }
    );
    assert_eq!(
        checkpoint.successor_curve_reserves_nad(),
        CurveReservesNad {
            base: start.base - normalize_to_nad(base_reserve_debit as u128, market.base_side.asset_decimals).unwrap(),
            quote: start.quote
                - normalize_to_nad(quote_reserve_debit as u128, market.quote_side.asset_decimals).unwrap(),
        }
    );
}

#[test]
fn individual_hlp_plan_apply_matches_reference_for_mint_and_exact_out_burn() {
    for target_asset in [MarketAsset::Base, MarketAsset::Quote] {
        let mut leverage_market = seeded_market();
        leverage_market
            .deposit_single_sided(
                target_asset,
                if target_asset == MarketAsset::Base { 100 } else { 200 },
                1,
            )
            .unwrap();
        let leverage_valuation = current_hlp_valuation(&leverage_market, target_asset).unwrap();
        let leverage_before = leverage_market.try_to_vec().unwrap();
        let leverage_plan = plan_leverage_up_proportional_with_cash_floors(
            &leverage_market,
            target_asset,
            100 * NAD as i128,
            leverage_valuation,
            SwapCashFloors::default(),
        )
        .unwrap();
        assert!(matches!(leverage_plan, HlpRebalancePlan::LeverageUp { .. }));
        assert_eq!(
            leverage_market.try_to_vec().unwrap(),
            leverage_before,
            "planning mutated leverage state"
        );

        assert_hlp_plan_matches_reference(leverage_market, leverage_plan);

        for concentrated in [false, true] {
            let mut deleverage_market = shortfall_plan_market(target_asset);
            if concentrated {
                enable_concentrated_curve(&mut deleverage_market);
            }
            let deleverage_valuation = current_hlp_valuation(&deleverage_market, target_asset).unwrap();
            assert!(deleverage_valuation.ideal_delta < 0);
            let deleverage_before = deleverage_market.try_to_vec().unwrap();
            let deleverage_plan = plan_deleverage_proportional_with_cash_floors(
                &deleverage_market,
                target_asset,
                deleverage_valuation.ideal_delta,
                deleverage_valuation,
                SwapCashFloors::default(),
            )
            .unwrap();
            let HlpRebalancePlan::Deleverage {
                exact_out_checkpoint,
                interest_paid,
                base_entitlement_amount,
                quote_entitlement_amount,
                ..
            } = deleverage_plan
            else {
                panic!("expected deleverage plan")
            };
            let borrowed_entitlement = match target_asset {
                MarketAsset::Base => quote_entitlement_amount,
                MarketAsset::Quote => base_entitlement_amount,
            };
            assert!(interest_paid > borrowed_entitlement);
            assert!(exact_out_checkpoint.is_some());
            assert_exact_out_plan_is_canonical(&deleverage_market, deleverage_plan);
            assert_eq!(
                deleverage_market.try_to_vec().unwrap(),
                deleverage_before,
                "planning mutated deleverage state"
            );

            let mut planned = deleverage_market.clone();
            assert_hlp_plan_matches_reference(deleverage_market, deleverage_plan);
            apply_hlp_rebalance_plan(&mut planned, deleverage_plan).unwrap();
            planned.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
            planned.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
        }
    }
}

#[test]
fn compact_bounded_i_greater_than_b_settlement_is_same_d_sufficient_in_both_directions() {
    for target_asset in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = shortfall_plan_market(target_asset);
        enable_concentrated_curve(&mut market);
        let fixed = HlpPlannerStatic::capture(&market).unwrap();
        let state = HlpPlannerState::capture(&market);
        let reserves = state.curve_reserves_nad(fixed).unwrap();
        let canonical = market
            .prepare_curve_for_reserves_nad(
                reserves,
                market.current_curve_center_price_nad().unwrap(),
                curve_slot(&market),
            )
            .unwrap();
        let anchor = canonical
            .prepare_guidance_successor_with_invariant(reserves.base, reserves.quote, canonical.invariant_d())
            .unwrap();
        let valuation = current_hlp_valuation(&market, target_asset).unwrap();
        assert!(valuation.ideal_delta < 0);

        HLP_COMPACT_GUIDANCE_EXACT_OUT_PROBES.with(|count| count.set(0));
        let compact = plan_compact_hlp_deleverage(
            fixed,
            state,
            target_asset,
            valuation.ideal_delta,
            valuation,
            anchor,
            state.base_side.ylp_supply,
            HlpGuidanceSettlementProbeMode::Bounded,
        )
        .unwrap();
        let proof = compact
            .guidance_settlement
            .unwrap_or_else(|| panic!("expected bounded I>B proof for {target_asset:?}"));
        let facts = proof.facts();
        assert!(facts.borrowed_shortfall > 0);
        assert!(facts.selected_input_nad > 0);
        assert!(facts.target_retained > 0);

        let HlpRebalancePlan::Deleverage { ylp_burn_amount, .. } = compact.plan
        else {
            panic!("expected compact deleverage plan for {target_asset:?}")
        };
        let post_ylp_supply = state.base_side.ylp_supply.checked_sub(ylp_burn_amount).unwrap();
        let scaled_d = mul_div_u128(
            anchor.invariant_d(),
            post_ylp_supply as u128,
            state.base_side.ylp_supply as u128,
        )
        .unwrap();
        let same_d = anchor
            .prepare_guidance_successor_with_invariant(
                facts.post_entitlement_curve_reserves_nad.base,
                facts.post_entitlement_curve_reserves_nad.quote,
                scaled_d,
            )
            .unwrap();
        let direction = match target_asset {
            MarketAsset::Base => ConcentratedSwapDirection::BaseToQuote,
            MarketAsset::Quote => ConcentratedSwapDirection::QuoteToBase,
        };
        let borrowed_asset = target_asset.opposite();
        let amount_out_nad = normalize_to_nad(
            facts.borrowed_shortfall as u128,
            fixed.decimals(borrowed_asset),
        )
        .unwrap();
        let exact_input = same_d
            .quote_exact_out_input_bracket(amount_out_nad, direction)
            .unwrap()
            .1;
        assert!(facts.selected_input_nad >= exact_input);
        assert!(
            same_d
                .quote_exact_in(facts.selected_input_nad, direction)
                .unwrap()
                >= amount_out_nad
        );
        let probes = HLP_COMPACT_GUIDANCE_EXACT_OUT_PROBES.with(Cell::get);
        assert!((1..=2).contains(&probes), "target={target_asset:?} probes={probes}");
    }
}

#[test]
fn individual_hlp_plan_rejects_stale_state_without_mutation() {
    let mut start = seeded_market();
    start.deposit_single_sided(MarketAsset::Base, 100, 1).unwrap();
    let valuation = current_hlp_valuation(&start, MarketAsset::Base).unwrap();
    let plan = plan_leverage_up_proportional_with_cash_floors(
        &start,
        MarketAsset::Base,
        100 * NAD as i128,
        valuation,
        SwapCashFloors::default(),
    )
    .unwrap();
    assert!(matches!(plan, HlpRebalancePlan::LeverageUp { .. }));

    for field in 0..14 {
        let mut market = start.clone();
        match field {
            0 => market.base_hlp_vault.ylp_shares += 1,
            1 => market.base_hlp_vault.debt_shares += 1,
            2 => market.base_hlp_vault.debt_principal += 1,
            3 => market.base_hlp_vault.base_hlp_live_reserve += 1,
            4 => market.base_hlp_vault.quote_hlp_live_reserve += 1,
            5 => market.base_side.shares.ylp_supply += 1,
            6 => market.quote_side.shares.ylp_supply += 1,
            7 => market.base_side.reserves.live_reserve += 1,
            8 => market.base_side.reserves.cash_reserve += 1,
            9 => market.base_side.reserves.base_hlp_backing_inventory += 1,
            10 => market.quote_side.reserves.live_reserve += 1,
            11 => market.quote_side.reserves.cash_reserve += 1,
            12 => market.quote_side.reserves.base_hlp_backing_inventory += 1,
            13 => market.debt.quote_borrow_index_nad += 1,
            _ => unreachable!(),
        }
        let stale_state = market.try_to_vec().unwrap();
        let error = apply_hlp_rebalance_plan(&mut market, plan).unwrap_err();
        assert_eq!(error, error!(ErrorCode::BrokenInvariant), "field {field}");
        assert_eq!(market.try_to_vec().unwrap(), stale_state, "field {field}");
    }
}

#[test]
fn individual_hlp_plan_rejects_tampered_leverage_semantics_atomically() {
    let mut market = seeded_market();
    market.deposit_single_sided(MarketAsset::Base, 100, 1).unwrap();
    let valuation = current_hlp_valuation(&market, MarketAsset::Base).unwrap();
    let plan = plan_leverage_up_proportional_with_cash_floors(
        &market,
        MarketAsset::Base,
        100 * NAD as i128,
        valuation,
        SwapCashFloors::default(),
    )
    .unwrap();

    let mut tampered_mint = plan;
    let HlpRebalancePlan::LeverageUp { ylp_mint_amount, .. } = &mut tampered_mint else {
        panic!("expected leverage plan")
    };
    *ylp_mint_amount += 1;

    let mut tampered_debt = plan;
    let HlpRebalancePlan::LeverageUp { debt_shares_added, .. } = &mut tampered_debt else {
        panic!("expected leverage plan")
    };
    *debt_shares_added += 1;

    for tampered in [tampered_mint, tampered_debt] {
        let mut candidate = market.clone();
        let before = candidate.try_to_vec().unwrap();
        let error = apply_hlp_rebalance_plan(&mut candidate, tampered).unwrap_err();
        assert_eq!(error, error!(ErrorCode::BrokenInvariant));
        assert_eq!(candidate.try_to_vec().unwrap(), before);
    }
}

#[test]
fn individual_hlp_plan_reference_covers_deleverage_caps_interest_and_noops() {
    for target_asset in [MarketAsset::Base, MarketAsset::Quote] {
        for borrow_index_nad in [NAD as u128, 11 * NAD as u128 / 10] {
            let mut market = seeded_market();
            market
                .deposit_single_sided(
                    target_asset,
                    if target_asset == MarketAsset::Base { 100 } else { 200 },
                    1,
                )
                .unwrap();
            match target_asset {
                MarketAsset::Base => {
                    market.quote_side.reserves.live_reserve = 1_800;
                    market.quote_side.reserves.cash_reserve = 1_600;
                    market.debt.quote_borrow_index_nad = borrow_index_nad;
                }
                MarketAsset::Quote => {
                    market.base_side.reserves.live_reserve = 800;
                    market.base_side.reserves.cash_reserve = 700;
                    market.debt.base_borrow_index_nad = borrow_index_nad;
                }
            }
            market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
            market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
            let valuation = current_hlp_valuation(&market, target_asset).unwrap();
            assert!(
                valuation.ideal_delta < 0,
                "target={target_asset:?} index={borrow_index_nad} delta={}",
                valuation.ideal_delta
            );
            let plan = plan_deleverage_proportional_with_cash_floors(
                &market,
                target_asset,
                valuation.ideal_delta,
                valuation,
                SwapCashFloors::default(),
            )
            .unwrap();
            let HlpRebalancePlan::Deleverage {
                base_entitlement_amount,
                quote_entitlement_amount,
                interest_paid,
                exact_out_checkpoint,
                ..
            } = plan
            else {
                panic!("expected deleverage plan")
            };
            let borrowed_entitlement = match target_asset {
                MarketAsset::Base => quote_entitlement_amount,
                MarketAsset::Quote => base_entitlement_amount,
            };
            if borrow_index_nad == NAD as u128 {
                assert_eq!(interest_paid, 0);
            } else {
                assert!(interest_paid > 0);
            }
            assert!(interest_paid <= borrowed_entitlement);
            assert!(exact_out_checkpoint.is_none());
            assert_hlp_plan_matches_reference(market, plan);
        }
    }

    let scale = 1_000_000_u64;
    let mut capped_market = seeded_market();
    capped_market.base_side.asset_decimals = 6;
    capped_market.quote_side.asset_decimals = 6;
    configure_market_depth(&mut capped_market, 1_000_000 * scale, 20_000);
    capped_market
        .deposit_single_sided(MarketAsset::Base, 100_000 * scale, 1)
        .unwrap();
    capped_market.debt.quote_borrow_index_nad = 2 * NAD as u128;
    let valuation = current_hlp_valuation(&capped_market, MarketAsset::Base).unwrap();
    let desired_burn = capped_market.base_hlp_vault.ylp_shares / 2;
    let full_interest =
        hlp_deleverage_interest_for_burn(&capped_market, MarketAsset::Base, desired_burn, valuation).unwrap();
    let mut partial_floors = SwapCashFloors::default();
    partial_floors.set(
        MarketAsset::Quote,
        capped_market.quote_side.reserves.cash_reserve - full_interest / 2,
    );
    let partial_plan = plan_deleverage_proportional_with_cash_floors(
        &capped_market,
        MarketAsset::Base,
        valuation.ideal_delta,
        valuation,
        partial_floors,
    )
    .unwrap();
    let HlpRebalancePlan::Deleverage {
        common,
        ylp_burn_amount,
        ..
    } = partial_plan
    else {
        panic!("expected capped deleverage plan")
    };
    assert!(common.capacity_bound);
    assert!(ylp_burn_amount > 0 && ylp_burn_amount < desired_burn);
    assert_hlp_plan_matches_reference(capped_market.clone(), partial_plan);

    let mut zero_floors = SwapCashFloors::default();
    zero_floors.set(MarketAsset::Base, capped_market.base_side.reserves.cash_reserve);
    zero_floors.set(MarketAsset::Quote, capped_market.quote_side.reserves.cash_reserve);
    let zero_cap_plan = plan_deleverage_proportional_with_cash_floors(
        &capped_market,
        MarketAsset::Base,
        valuation.ideal_delta,
        valuation,
        zero_floors,
    )
    .unwrap();
    assert!(matches!(
        zero_cap_plan,
        HlpRebalancePlan::Noop {
            common: HlpRebalancePlanCommon {
                capacity_bound: true,
                ..
            },
            reason: HlpRebalanceNoopReason::CapacityOrGranularity,
        }
    ));
    assert_hlp_plan_matches_reference(capped_market.clone(), zero_cap_plan);

    for (ideal_delta, capacity_bound, reason) in [
        (0, false, HlpRebalanceNoopReason::Settled),
        (-1, false, HlpRebalanceNoopReason::Unhedgeable),
        (1, true, HlpRebalanceNoopReason::CapacityOrGranularity),
    ] {
        let noop = plan_hlp_noop(
            &capped_market,
            MarketAsset::Base,
            ideal_delta,
            valuation,
            capacity_bound,
            reason,
        );
        assert_hlp_plan_matches_reference(capped_market.clone(), noop);
    }
}

fn frozen_pair_valuations(market: &Market, prices: HlpCurvePrices) -> (Option<HlpValuation>, Option<HlpValuation>) {
    let base_active = market.base_hlp_vault.hlp_supply > 0 || market.base_hlp_vault.residual_exposure != 0;
    let quote_active = market.quote_hlp_vault.hlp_supply > 0 || market.quote_hlp_vault.residual_exposure != 0;
    let (base_values, quote_values) =
        current_hlp_inventory_values_pair_nad_with_prices(market, prices, base_active, quote_active).unwrap();
    (
        base_active.then(|| hlp_valuation_from_values(base_values, prices).unwrap()),
        quote_active.then(|| hlp_valuation_from_values(quote_values, prices).unwrap()),
    )
}

fn mixed_pair_rebalance_market(asset_in: MarketAsset) -> Market {
    let mut market = active_hlp_market();
    let amount_in = 50_000;
    let asset_out = asset_in.opposite();
    let amount_out = cpmm_amount_out(
        market.side(asset_in).reserves.live_reserve,
        market.side(asset_out).reserves.live_reserve,
        amount_in,
    )
    .unwrap();
    market
        .swap_reserves(
            asset_in,
            amount_in,
            amount_out,
            0,
            0,
            crate::state::ProtocolAuctionSplit::default(),
        )
        .unwrap();
    let prices = current_hlp_curve_prices(&market).unwrap();
    let (base, quote) = frozen_pair_valuations(&market, prices);
    let base_delta = base.unwrap().ideal_delta;
    let quote_delta = quote.unwrap().ideal_delta;
    assert_ne!(base_delta, 0);
    assert_ne!(quote_delta, 0);
    assert_ne!(base_delta.is_positive(), quote_delta.is_positive());
    market
}

#[test]
fn joint_hlp_pair_plan_matches_legacy_for_mixed_and_exact_out_states() {
    for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
        assert_joint_pair_matches_reference(mixed_pair_rebalance_market(asset_in), 0, None);
    }

    let scale = 1_000_000_u64;
    let mut exact_out = active_concentrated_hlp_market_with_decimals(6);
    exact_out.base_side.credit_reserve(500_000 * scale, true).unwrap();
    exact_out.quote_side.credit_reserve(1_000_000 * scale, true).unwrap();
    exact_out.checkpoint_amm_neutral_inventory(0).unwrap();
    exact_out.debt.base_borrow_index_nad = 21 * NAD as u128 / 10;
    exact_out.debt.quote_borrow_index_nad = 21 * NAD as u128 / 10;

    let mut planning = exact_out.clone();
    checkpoint_hlp_yield_from_ylp_pair(&mut planning, true, true).unwrap();
    let prices = current_hlp_curve_prices(&planning).unwrap();
    let (base_valuation, quote_valuation) = frozen_pair_valuations(&planning, prices);
    let before_plan = planning.try_to_vec().unwrap();
    let plan = plan_hlp_rebalance_pair(&mut planning, base_valuation, quote_valuation).unwrap();
    assert_eq!(planning.try_to_vec().unwrap(), before_plan);
    let checkpoint_for_leg = |leg| match leg {
        HlpRebalancePairLegPlan::Active(HlpRebalancePlan::Deleverage {
            exact_out_checkpoint, ..
        }) => exact_out_checkpoint,
        _ => None,
    };
    let initial_curve = planning.curve_reserves_nad().unwrap();
    if let Some(checkpoint) = checkpoint_for_leg(plan.base) {
        assert_eq!(checkpoint.start_curve_reserves_nad(), initial_curve);
    }
    let mut base_after = planning.clone();
    if let HlpRebalancePairLegPlan::Active(base_plan) = plan.base {
        let base_post = derive_hlp_rebalance_post_state(&base_after, &base_plan).unwrap();
        commit_hlp_rebalance_state(&mut base_after, MarketAsset::Base, base_post);
    }
    if let Some(checkpoint) = checkpoint_for_leg(plan.quote) {
        assert_eq!(
            checkpoint.start_curve_reserves_nad(),
            base_after.curve_reserves_nad().unwrap()
        );
    }
    assert!(checkpoint_for_leg(plan.base).is_some() || checkpoint_for_leg(plan.quote).is_some());
    assert_joint_pair_matches_reference(exact_out, 0, None);
}

#[test]
fn joint_hlp_pair_plan_rejects_stale_and_second_leg_tampering_atomically() {
    let mut market = mixed_pair_rebalance_market(MarketAsset::Base);
    checkpoint_hlp_yield_from_ylp_pair(&mut market, true, true).unwrap();
    let prices = current_hlp_curve_prices(&market).unwrap();
    let (base_valuation, quote_valuation) = frozen_pair_valuations(&market, prices);
    let before_plan = market.try_to_vec().unwrap();
    let plan = plan_hlp_rebalance_pair(&mut market, base_valuation, quote_valuation).unwrap();
    assert_eq!(market.try_to_vec().unwrap(), before_plan);

    let mut invalid_quote = quote_valuation.unwrap();
    invalid_quote.ideal_delta = -1_000_000_000_000;
    invalid_quote.values.target_inventory_value_nad = u128::MAX;
    invalid_quote.values.opposite_inventory_value_nad = u128::MAX;
    let planning_error = plan_hlp_rebalance_pair(&mut market, base_valuation, Some(invalid_quote)).unwrap_err();
    assert_eq!(planning_error, error!(ErrorCode::MarketMathOverflow));
    assert_eq!(market.try_to_vec().unwrap(), before_plan);

    let mut stale = market.clone();
    stale.debt.base_borrow_index_nad += 1;
    let stale_before = stale.try_to_vec().unwrap();
    let error = apply_hlp_rebalance_pair_plan(&mut stale, plan).unwrap_err();
    assert_eq!(error, error!(ErrorCode::BrokenInvariant));
    assert_eq!(stale.try_to_vec().unwrap(), stale_before);

    let mut tampered = plan;
    let HlpRebalancePairLegPlan::Active(quote_plan) = &mut tampered.quote else {
        panic!("expected active Quote plan")
    };
    quote_plan.common_mut().start.quote_live_reserve += 1;
    let mut candidate = market.clone();
    let candidate_before = candidate.try_to_vec().unwrap();
    let error = apply_hlp_rebalance_pair_plan(&mut candidate, tampered).unwrap_err();
    assert_eq!(error, error!(ErrorCode::BrokenInvariant));
    assert_eq!(candidate.try_to_vec().unwrap(), candidate_before);

    let mut inactive_tamper = plan;
    inactive_tamper.quote = HlpRebalancePairLegPlan::Inactive {
        target_asset: MarketAsset::Quote,
    };
    let mut candidate = market;
    let candidate_before = candidate.try_to_vec().unwrap();
    let error = apply_hlp_rebalance_pair_plan(&mut candidate, inactive_tamper).unwrap_err();
    assert_eq!(error, error!(ErrorCode::BrokenInvariant));
    assert_eq!(candidate.try_to_vec().unwrap(), candidate_before);
}

fn market_with_symmetric_unpaid_interest() -> Market {
    let mut market = seeded_market();
    market.base_side.reserves.live_reserve = 1_100;
    market.base_side.reserves.cash_reserve = 900;
    market.quote_side.reserves.live_reserve = 1_100;
    market.quote_side.reserves.cash_reserve = 900;
    market.debt.base_borrow_index_nad = 2 * NAD as u128;
    market.debt.quote_borrow_index_nad = 2 * NAD as u128;
    market.debt.fixed_base_shares = 100;
    market.debt.fixed_quote_shares = 100;
    market.debt.fixed_base_principal = 100;
    market.debt.fixed_quote_principal = 100;

    assert_eq!(market.curve_reserve(MarketAsset::Base).unwrap(), 1_000);
    assert_eq!(market.curve_reserve(MarketAsset::Quote).unwrap(), 1_000);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
    market
}

#[test]
fn open_hlp_keeps_leverage_debt_on_aggregate_vault() {
    let mut market = seeded_market();

    let receipt = market.deposit_single_sided(MarketAsset::Base, 100, 1).unwrap();

    assert_eq!(receipt.borrowed_amount, 200);
    assert_eq!(receipt.ylp_amount, 100);
    assert_eq!(receipt.hlp_amount, 100);
    assert_eq!(market.debt.fixed_quote_shares, 0);
    assert!(market.base_hlp_vault.debt_shares > 0);
    assert_eq!(market.base_hlp_vault.debt_principal, 200);
    assert_eq!(market.quote_side.daily_borrow_bucket.borrowed_bucket, 0);
    assert_eq!(market.base_side.daily_borrow_bucket.borrowed_bucket, 0);
    assert_eq!(market.base_hlp_vault.ylp_shares, 100);
    assert_eq!(market.base_hlp_vault.base_hlp_live_reserve, 0);
    assert_eq!(market.base_hlp_vault.quote_hlp_live_reserve, 200);
    assert_eq!(market.base_side.reserves.cash_reserve, 1_100);
    assert_eq!(market.quote_side.reserves.cash_reserve, 2_000);
    assert_eq!(market.base_hlp_vault.last_nav_nad, 100 * NAD as u128);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn direct_hlp_deposit_does_not_consume_the_public_borrow_bucket() {
    let mut market = seeded_market();

    let receipt = market.deposit_single_sided(MarketAsset::Quote, 200, 1).unwrap();

    assert_eq!(receipt.borrowed_amount, 100);
    assert_eq!(market.base_side.daily_borrow_bucket.borrowed_bucket, 0);
    assert_eq!(market.base_side.daily_borrow_bucket.last_decay_slot, 0);
    assert_eq!(market.quote_side.daily_borrow_bucket.borrowed_bucket, 0);
}

#[test]
fn direct_hlp_deposit_ignores_a_full_public_borrow_bucket() {
    let mut market = seeded_market();
    let borrowed_asset = MarketAsset::Quote;
    let limit = market
        .daily_limit_for_side(borrowed_asset, market.config.max_daily_borrow_bps)
        .unwrap();
    market.side_mut(borrowed_asset).daily_borrow_bucket.borrowed_bucket = limit;
    let receipt = market.deposit_single_sided(MarketAsset::Base, 100, 1).unwrap();

    assert_eq!(receipt.borrowed_amount, 200);
    assert_eq!(market.side(borrowed_asset).daily_borrow_bucket.borrowed_bucket, limit);
    assert_eq!(market.base_hlp_vault.debt_principal, 200);
}

#[test]
fn open_hlp_requires_borrowed_side_cash_headroom() {
    let mut market = seeded_market();
    market.quote_side.reserves.cash_reserve = 199;

    let err = market.deposit_single_sided(MarketAsset::Base, 100, 1).unwrap_err();

    assert_eq!(err, error!(ErrorCode::InsufficientBorrowHeadroom));
}

#[test]
fn aggregate_hlp_funding_cannot_reuse_the_same_cash() {
    let mut market = seeded_market();

    let first = market.deposit_single_sided(MarketAsset::Base, 1_000, 1).unwrap();
    assert_eq!(first.borrowed_amount, 2_000);
    assert_eq!(market.hlp_funding_debt(MarketAsset::Quote).unwrap(), 2_000);
    assert_eq!(market.hlp_funding_headroom(MarketAsset::Quote).unwrap(), 0);

    let before = market.clone();
    let err = market.deposit_single_sided(MarketAsset::Base, 1, 1).unwrap_err();
    assert_eq!(err, error!(ErrorCode::InsufficientBorrowHeadroom));
    assert_eq!(market.base_hlp_vault.debt_shares, before.base_hlp_vault.debt_shares);
    assert_eq!(market.base_hlp_vault.ylp_shares, before.base_hlp_vault.ylp_shares);
    assert_eq!(
        market.base_side.reserves.cash_reserve,
        before.base_side.reserves.cash_reserve
    );
    assert_eq!(
        market.quote_side.reserves.cash_reserve,
        before.quote_side.reserves.cash_reserve
    );
}

#[test]
fn aggregate_hlp_funding_headroom_uses_indexed_debt() {
    let mut market = seeded_market();
    market.deposit_single_sided(MarketAsset::Base, 500, 1).unwrap();
    assert_eq!(market.base_hlp_vault.debt_principal, 1_000);
    assert_eq!(market.hlp_funding_headroom(MarketAsset::Quote).unwrap(), 1_000);

    market.debt.quote_borrow_index_nad = 2 * NAD as u128;
    assert_eq!(market.hlp_funding_debt(MarketAsset::Quote).unwrap(), 2_000);
    assert_eq!(market.hlp_funding_headroom(MarketAsset::Quote).unwrap(), 0);
    assert_eq!(
        require_hlp_borrow_headroom(&market, MarketAsset::Quote, 1).unwrap_err(),
        error!(ErrorCode::InsufficientBorrowHeadroom)
    );
}

#[test]
fn aggregate_hlp_funding_headroom_accounts_for_share_rounding() {
    let mut market = seeded_market();
    market.quote_side.reserves.cash_reserve = 5;
    market.base_hlp_vault.debt_shares = 2;
    market.debt.quote_borrow_index_nad = 3 * NAD as u128 / 2;

    assert_eq!(market.hlp_funding_debt(MarketAsset::Quote).unwrap(), 3);
    assert_eq!(market.hlp_funding_headroom(MarketAsset::Quote).unwrap(), 1);
    assert_eq!(require_hlp_borrow_headroom(&market, MarketAsset::Quote, 1).unwrap(), 1);
    assert_eq!(
        require_hlp_borrow_headroom(&market, MarketAsset::Quote, 2).unwrap_err(),
        error!(ErrorCode::InsufficientBorrowHeadroom)
    );
}

#[test]
fn repeated_open_hlp_mints_against_delta_nav() {
    let mut market = seeded_market();

    let first = market.deposit_single_sided(MarketAsset::Base, 100, 1).unwrap();
    let second = market.deposit_single_sided(MarketAsset::Base, 120, 1).unwrap();

    assert_eq!(first.hlp_amount, 100);
    assert_eq!(second.hlp_amount, 120);
    assert_eq!(market.base_hlp_vault.hlp_supply, 220);
    assert_eq!(market.base_hlp_vault.ylp_shares, 220);
    assert_eq!(market.base_hlp_vault.last_nav_nad, 220 * NAD as u128);
}

#[test]
fn repeated_cpmm_hlp_open_admits_only_non_worsening_controller_granularity() {
    let mut market = seeded_market();
    market.base_side.asset_decimals = 6;
    market.quote_side.asset_decimals = 6;
    market.base_side.reserves.live_reserve = 100_000;
    market.base_side.reserves.cash_reserve = 100_000;
    market.quote_side.reserves.live_reserve = 200_000;
    market.quote_side.reserves.cash_reserve = 200_000;
    market.base_side.shares.ylp_supply = 141_421;
    market.quote_side.shares.ylp_supply = 141_421;

    market.deposit_single_sided(MarketAsset::Base, 5_000, 1).unwrap();
    assert_eq!(market.base_hlp_vault.residual_exposure, -1_000);
    assert_eq!(market.base_hlp_vault.last_nav_nad, 4_998_500);
    let reference = market.base_hlp_vault.cached_settlement_price_nad;
    let before =
        current_hlp_entry_state_with_prices(&market, MarketAsset::Base, current_hlp_curve_prices(&market).unwrap())
            .unwrap();
    assert_eq!(before.disposition, HlpEntryDisposition::ControllerGranularityLimited);

    // This is the on-chain ordering: update/checkpoint admission runs
    // before the transferred amount is applied to the aggregate vault.
    prepare_hlp_deposit_like_instruction(&mut market, MarketAsset::Base, 1).unwrap();
    let receipt = market.deposit_single_sided(MarketAsset::Base, 6_000, 1).unwrap();

    assert_eq!(receipt.hlp_amount, 6_001);
    assert_eq!(market.base_hlp_vault.hlp_supply, 11_001);
    assert_eq!(market.base_hlp_vault.ylp_shares, 15_556);
    assert_eq!(market.base_hlp_vault.residual_exposure, -1_000);
    assert_eq!(market.base_hlp_vault.last_nav_nad, 10_998_500);
    assert_eq!(market.base_hlp_vault.cached_settlement_price_nad, reference);
    let after =
        current_hlp_entry_state_with_prices(&market, MarketAsset::Base, current_hlp_curve_prices(&market).unwrap())
            .unwrap();
    assert_eq!(after.disposition, HlpEntryDisposition::ControllerGranularityLimited);
    assert_eq!(
        after.residual_exposure.unsigned_abs(),
        before.residual_exposure.unsigned_abs()
    );
    assert!(after.nav_nad > before.nav_nad);
}

#[test]
fn h_lp_nav_values_collateral_and_debt_in_target_numeraire() {
    let mut market = seeded_market();

    market.deposit_single_sided(MarketAsset::Base, 100, 1).unwrap();

    assert_eq!(
        hlp_collateral_value_nad(&market, MarketAsset::Base, &market.base_hlp_vault).unwrap(),
        200 * NAD as u128
    );
    assert_eq!(
        hlp_debt_value_nad(&market, MarketAsset::Base).unwrap(),
        100 * NAD as u128
    );
    assert_eq!(hlp_nav_nad(&market, MarketAsset::Base).unwrap(), 100 * NAD as u128);
}

#[test]
fn cpmm_hlp_price_fast_path_matches_prepared_curve_rounding() {
    let mut market = seeded_market();
    for (base_decimals, quote_decimals, base_reserve, quote_reserve) in [
        (6, 6, 100_000_003, 299_999_999),
        (6, 9, 9_876_543_211, 1_234_567_891),
        (9, 0, 4_321_000_007, 7_654_321),
    ] {
        market.base_side.asset_decimals = base_decimals;
        market.quote_side.asset_decimals = quote_decimals;
        market.base_side.reserves.live_reserve = base_reserve;
        market.base_side.reserves.cash_reserve = base_reserve;
        market.quote_side.reserves.live_reserve = quote_reserve;
        market.quote_side.reserves.cash_reserve = quote_reserve;

        let fast = current_hlp_curve_prices(&market).unwrap();
        let prepared =
            hlp_curve_prices_from_base_price_nad(market.curve_marginal_price_nad(curve_slot(&market)).unwrap() as u128)
                .unwrap();
        assert_eq!(fast, prepared);
    }
}

#[test]
fn accrued_interest_grows_hlp_debt_and_reduces_nav() {
    let mut market = seeded_market();
    market.deposit_single_sided(MarketAsset::Base, 100, 1).unwrap();
    let debt_before = hlp_debt_value_nad(&market, MarketAsset::Base).unwrap();
    let nav_before = hlp_nav_nad(&market, MarketAsset::Base).unwrap();

    // Simulate 10% borrow-interest accrual on the quote index. The base-hLP
    // borrows quote, so its funding debt grows and its NAV falls. hLP
    // funding interest does not grow virtual reserves because the hLP live
    // component is tracked separately from cash-backed debt.
    market.debt.quote_borrow_index_nad = (NAD as u128) * 110 / 100;

    let debt_after = hlp_debt_value_nad(&market, MarketAsset::Base).unwrap();
    let nav_after = hlp_nav_nad(&market, MarketAsset::Base).unwrap();
    assert!(debt_after > debt_before);
    assert!(nav_after < nav_before);
    assert_eq!(market.base_hlp_vault.debt_principal, 200);
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn unrealized_lending_interest_does_not_inflate_hlp_executable_inventory() {
    let mut market = seeded_market();
    market.deposit_single_sided(MarketAsset::Base, 100, 1).unwrap();

    // First add 100 of cash-backed fixed principal to the executable
    // reserve, then accrue another 100 of unpaid interest into live
    // reserve. Only the principal changes the hLP's curve claim.
    market.debt.fixed_base_shares = 100;
    market.debt.fixed_base_principal = 100;
    market.base_side.reserves.live_reserve += 100;
    let after_principal = current_hlp_inventory_values_nad(&market, MarketAsset::Base).unwrap();

    market.debt.base_borrow_index_nad = 2 * NAD as u128;
    market.base_side.reserves.live_reserve += 100;
    let after_interest = current_hlp_inventory_values_nad(&market, MarketAsset::Base).unwrap();

    assert_eq!(market.unrealized_interest(MarketAsset::Base).unwrap(), 100);
    assert_eq!(
        after_interest.target_inventory_value_nad,
        after_principal.target_inventory_value_nad
    );
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
}

#[test]
fn hlp_uses_ordinary_ylp_live_basis_and_cannot_capture_existing_interest() {
    let reference = market_with_symmetric_unpaid_interest();
    let ordinary = reference.preview_add_liquidity(100, 100).unwrap();
    assert_eq!(ordinary.ylp_amount, 90);

    for target_asset in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = market_with_symmetric_unpaid_interest();
        let (ylp_amount, hlp_amount, _, _) = match target_asset {
            MarketAsset::Base => deposit_base_hlp(&mut market, 100, 100).unwrap(),
            MarketAsset::Quote => deposit_quote_hlp(&mut market, 100, 100).unwrap(),
        };

        // live=1_100, curve=1_000, supply=1_000, contribution=100+100.
        // Ordinary yLP and hLP must both mint floor(100*1_000/1_100)=90.
        assert_eq!(ylp_amount, ordinary.ylp_amount);
        assert_eq!(ylp_amount, 90);
        assert_eq!(
            ylp_live_underlying_amount(&market, MarketAsset::Base, ylp_amount).unwrap(),
            99
        );
        assert_eq!(
            ylp_live_underlying_amount(&market, MarketAsset::Quote, ylp_amount).unwrap(),
            99
        );

        // This fixture isolates incumbent-interest accounting and deliberately
        // has no borrower collateral ledger.
        market.config.borrow_market_health_floor_bps = 0;
        let close = market.withdraw_single_sided(target_asset, hlp_amount).unwrap();

        assert!(
            close.target_amount_out <= 100,
            "same-slot hLP round trip extracted incumbent interest: {close:?}"
        );
        assert_eq!(close.debt_repaid, 100);
        assert_eq!(market.unrealized_interest(MarketAsset::Base).unwrap(), 100);
        assert_eq!(market.unrealized_interest(MarketAsset::Quote).unwrap(), 100);
        assert_eq!(market.base_side.shares.ylp_supply, 1_000);
        assert_eq!(market.quote_side.shares.ylp_supply, 1_000);
        market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
        market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
    }
}

#[test]
fn close_hlp_burns_vault_ylp_and_repays_vault_debt() {
    let mut market = seeded_market();
    let deposit_receipt = market.deposit_single_sided(MarketAsset::Base, 100, 1).unwrap();

    let withdraw_receipt = market
        .withdraw_single_sided(MarketAsset::Base, deposit_receipt.hlp_amount)
        .unwrap();

    assert_eq!(withdraw_receipt.target_amount_out, 100);
    assert_eq!(withdraw_receipt.debt_repaid, 200);
    assert_eq!(market.base_hlp_vault.hlp_supply, 0);
    assert_eq!(market.base_hlp_vault.debt_shares, 0);
    assert_eq!(market.base_hlp_vault.debt_principal, 0);
    assert_eq!(market.base_hlp_vault.residual_exposure, 0);
    assert_eq!(market.base_hlp_vault.ylp_shares, 0);
    assert_eq!(market.base_hlp_vault.base_hlp_live_reserve, 0);
    assert_eq!(market.base_hlp_vault.quote_hlp_live_reserve, 0);
    assert_eq!(market.debt.fixed_quote_shares, 0);
    assert_eq!(market.base_side.reserves.live_reserve, 1_000);
    assert_eq!(market.base_side.reserves.cash_reserve, 1_000);
    assert_eq!(market.quote_side.reserves.live_reserve, 2_000);
    assert_eq!(market.quote_side.reserves.cash_reserve, 2_000);
    assert_eq!(market.base_side.shares.ylp_supply, 1_000);
    assert_eq!(market.quote_side.shares.ylp_supply, 1_000);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn close_hlp_realizes_interest_from_borrowed_side_cash() {
    let mut market = seeded_market();
    let deposit_receipt = market.deposit_single_sided(MarketAsset::Base, 100, 1).unwrap();
    market.debt.quote_borrow_index_nad = (NAD as u128) * 110 / 100;

    let withdraw_receipt = market
        .withdraw_single_sided(MarketAsset::Base, deposit_receipt.hlp_amount)
        .unwrap();

    assert_eq!(withdraw_receipt.debt_repaid, 220);
    assert_eq!(withdraw_receipt.interest_paid, 20);
    assert_eq!(market.base_hlp_vault.debt_principal, 0);
    assert_eq!(market.base_hlp_vault.quote_hlp_live_reserve, 0);
    assert_eq!(market.quote_side.reserves.live_reserve, 1_980);
    assert_eq!(market.quote_side.reserves.cash_reserve, 1_980);
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn close_quote_hlp_realizes_interest_symmetrically_from_base_cash() {
    let mut market = seeded_market();
    let deposit = market.deposit_single_sided(MarketAsset::Quote, 200, 1).unwrap();
    market.debt.base_borrow_index_nad = 11 * NAD as u128 / 10;

    let withdrawal = market
        .withdraw_single_sided(MarketAsset::Quote, deposit.hlp_amount)
        .unwrap();

    assert_eq!(withdrawal.target_amount_out, 179);
    assert_eq!(withdrawal.debt_repaid, 110);
    assert_eq!(withdrawal.interest_paid, 10);
    assert_eq!(market.quote_hlp_vault.debt_principal, 0);
    assert_eq!(market.quote_hlp_vault.base_hlp_live_reserve, 0);
    assert_eq!(market.base_side.reserves.live_reserve, 990);
    assert_eq!(market.base_side.reserves.cash_reserve, 990);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
}

#[test]
fn full_hlp_exit_pays_interest_without_replaying_burned_shares_or_funding_rebate() {
    let mut market = seeded_market();
    configure_market_depth(&mut market, 1_000_000, 20_000);
    let deposit = market.deposit_single_sided(MarketAsset::Base, 100_000, 1).unwrap();
    market
        .quote_side
        .record_interest_credit_with_supply(
            1,
            0,
            crate::state::ProtocolAuctionSplit::default(),
            0,
            market.quote_side.shares.ylp_supply,
        )
        .unwrap();
    assert!(market.quote_side.fees.interest_growth_remainder_scaled > 0);
    market.debt.quote_borrow_index_nad = 11 * NAD as u128 / 10;
    let eligibility = crate::state::HlpYieldEligibility {
        ylp_supply: market.base_side.shares.ylp_supply,
        base_hlp_ylp_shares: market.base_hlp_vault.ylp_shares,
        quote_hlp_ylp_shares: market.quote_hlp_vault.ylp_shares,
    };

    let withdrawal = market
        .withdraw_single_sided(MarketAsset::Base, deposit.hlp_amount)
        .unwrap();
    assert_eq!(withdrawal.hlp_supply, 0);
    assert_eq!(withdrawal.interest_paid, 20_000);
    assert_eq!(withdrawal.target_amount_out, 89_898);
    assert!(market.base_hlp_vault.quote_interest_remainder_q64 > 0);
    let burned_vault_nested_before_funding = (
        market.base_hlp_vault.quote_interest_growth_index_q64,
        market.base_hlp_vault.quote_interest_remainder_q64,
        market.base_hlp_vault.quote_interest_growth_remainder_scaled,
        market.base_hlp_vault.unallocated_quote_interest_amount,
    );
    let mut public_only_market = market.clone();
    let mut expected_base_yield = empty_hlp_yield_account();
    let mut expected_quote_yield = empty_hlp_yield_account();
    public_only_market
        .drain_hlp_unallocated_yield(MarketAsset::Base, &mut expected_base_yield, &mut expected_quote_yield)
        .unwrap();

    crate::record_inline_hlp_interest_credit(
        &mut market,
        MarketAsset::Quote,
        withdrawal.interest_paid,
        0,
        crate::state::ProtocolAuctionSplit::default(),
        eligibility,
    )
    .unwrap();

    assert_eq!(
        burned_vault_nested_before_funding,
        (
            market.base_hlp_vault.quote_interest_growth_index_q64,
            market.base_hlp_vault.quote_interest_remainder_q64,
            market.base_hlp_vault.quote_interest_growth_remainder_scaled,
            market.base_hlp_vault.unallocated_quote_interest_amount,
        )
    );
    assert_eq!(market.base_hlp_vault.ylp_shares, 0);
    assert_eq!(
        market.base_hlp_vault.quote_interest_checkpoint_q64,
        market.quote_side.fees.interest_growth_index_q64
    );
    assert_eq!(market.quote_side.fees.interest_liability, 20_001);
    assert_eq!(market.quote_side.fees.unallocated_interest_liability, 0);
    let mut actual_base_yield = empty_hlp_yield_account();
    let mut actual_quote_yield = empty_hlp_yield_account();
    market
        .drain_hlp_unallocated_yield(MarketAsset::Base, &mut actual_base_yield, &mut actual_quote_yield)
        .unwrap();
    assert_eq!(
        (
            actual_base_yield.accrued_swap_fee_amount,
            actual_base_yield.accrued_interest_amount,
            actual_base_yield.swap_fee_remainder_q64,
            actual_base_yield.interest_remainder_q64,
        ),
        (
            expected_base_yield.accrued_swap_fee_amount,
            expected_base_yield.accrued_interest_amount,
            expected_base_yield.swap_fee_remainder_q64,
            expected_base_yield.interest_remainder_q64,
        )
    );
    assert_eq!(
        (
            actual_quote_yield.accrued_swap_fee_amount,
            actual_quote_yield.accrued_interest_amount,
            actual_quote_yield.swap_fee_remainder_q64,
            actual_quote_yield.interest_remainder_q64,
        ),
        (
            expected_quote_yield.accrued_swap_fee_amount,
            expected_quote_yield.accrued_interest_amount,
            expected_quote_yield.swap_fee_remainder_q64,
            expected_quote_yield.interest_remainder_q64,
        )
    );
    market.quote_side.fees.assert_backed().unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn close_hlp_converts_borrowed_side_surplus_into_target_out() {
    let mut market = seeded_market();
    let deposit_receipt = market.deposit_single_sided(MarketAsset::Base, 100, 1).unwrap();
    market.quote_side.reserves.live_reserve = 2_300;
    market.quote_side.reserves.cash_reserve = 2_100;
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();

    let withdraw_receipt = market
        .withdraw_single_sided(MarketAsset::Base, deposit_receipt.hlp_amount)
        .unwrap();

    assert!(withdraw_receipt.target_amount_out > 100);
    assert_eq!(withdraw_receipt.debt_repaid, 200);
    assert_eq!(market.base_hlp_vault.hlp_supply, 0);
    assert_eq!(market.quote_side.reserves.live_reserve, 2_100);
    assert_eq!(market.quote_side.reserves.cash_reserve, 2_100);
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn close_hlp_uses_target_side_value_for_borrowed_side_shortfall() {
    let mut market = seeded_market();
    let deposit_receipt = market.deposit_single_sided(MarketAsset::Base, 100, 1).unwrap();
    market.quote_side.reserves.live_reserve = 2_110;
    market.quote_side.reserves.cash_reserve = 1_910;
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();

    let withdraw_receipt = market
        .withdraw_single_sided(MarketAsset::Base, deposit_receipt.hlp_amount)
        .unwrap();

    assert!(withdraw_receipt.target_amount_out < 100);
    assert_eq!(withdraw_receipt.debt_repaid, 200);
    assert_eq!(market.base_hlp_vault.hlp_supply, 0);
    assert_eq!(market.quote_side.reserves.live_reserve, 1_910);
    assert_eq!(market.quote_side.reserves.cash_reserve, 1_910);
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn concentrated_close_surplus_uses_concentrated_exact_curve_quote() {
    let mut market = seeded_market();
    configure_market_depth(&mut market, 1_000_000, 20_000);
    let deposit = market.deposit_single_sided(MarketAsset::Base, 100_000, 1).unwrap();
    enable_concentrated_curve(&mut market);
    market.quote_side.reserves.live_reserve = 2_420_000;
    market.quote_side.reserves.cash_reserve = 2_220_000;
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();

    let ylp_amount = market.base_hlp_vault.ylp_shares;
    let supply = market.base_side.shares.ylp_supply;
    let base_redeemed = proportional(market.base_side.reserves.live_reserve, ylp_amount, supply).unwrap();
    let quote_redeemed = proportional(market.quote_side.reserves.live_reserve, ylp_amount, supply).unwrap();
    let debt_repaid =
        Debt::shares_to_debt(market.base_hlp_vault.debt_shares, market.debt.quote_borrow_index_nad).unwrap() as u64;
    let surplus = quote_redeemed - debt_repaid;
    assert!(surplus > 0);

    let mut post_burn = market.clone();
    let base_curve_burn = ylp_curve_underlying_amount(&market, MarketAsset::Base, ylp_amount).unwrap();
    let quote_curve_burn = ylp_curve_underlying_amount(&market, MarketAsset::Quote, ylp_amount).unwrap();
    post_burn.base_side.reserves.live_reserve -= base_curve_burn;
    post_burn.quote_side.reserves.live_reserve -= quote_curve_burn;
    let concentrated_out = post_burn
        .quote_curve_exact_in(MarketAsset::Quote, surplus, 0)
        .unwrap()
        .amount_out;
    let cpmm_out = cpmm_amount_out(
        post_burn.quote_side.reserves.live_reserve,
        post_burn.base_side.reserves.live_reserve,
        surplus,
    )
    .unwrap();
    assert_ne!(
        concentrated_out, cpmm_out,
        "fixture must distinguish CONCENTRATED from CPMM"
    );

    let receipt = market
        .withdraw_single_sided(MarketAsset::Base, deposit.hlp_amount)
        .unwrap();
    assert_eq!(receipt.target_amount_out, base_redeemed + concentrated_out);
}

#[test]
fn open_hlp_rejects_settlement_price_divergence() {
    let mut market = seeded_market();
    market.deposit_single_sided(MarketAsset::Base, 100, 1).unwrap();

    market.quote_side.reserves.live_reserve = 4_000;
    market.quote_side.reserves.cash_reserve = 3_800;
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
    let err = market.deposit_single_sided(MarketAsset::Base, 100, 1).unwrap_err();

    assert_eq!(err, error!(ErrorCode::HlpSettlementUnavailable));
}

#[test]
fn close_hlp_rejects_settlement_price_divergence() {
    let mut market = seeded_market();
    let receipt = market.deposit_single_sided(MarketAsset::Base, 100, 1).unwrap();

    market.quote_side.reserves.live_reserve = 4_000;
    market.quote_side.reserves.cash_reserve = 3_800;
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
    let err = market
        .withdraw_single_sided(MarketAsset::Base, receipt.hlp_amount)
        .unwrap_err();

    assert_eq!(err, error!(ErrorCode::HlpSettlementUnavailable));
}

#[test]
fn h_lp_checkpoint_preserves_last_settlement_reference() {
    let mut market = seeded_market();
    market.deposit_single_sided(MarketAsset::Base, 100, 1).unwrap();
    let settlement_reference = market.base_hlp_vault.cached_settlement_price_nad;
    market.quote_side.reserves.live_reserve = 2_080;
    market.quote_side.reserves.cash_reserve = 1_880;
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();

    checkpoint_hlp_vaults(&mut market).unwrap();
    assert_eq!(market.base_hlp_vault.cached_settlement_price_nad, settlement_reference);
    assert_ne!(
        settlement_reference,
        current_settlement_price_nad(&market, MarketAsset::Base).unwrap(),
        "a generic accounting checkpoint must not re-anchor the settlement guard"
    );
}

fn assert_hlp_near_target(market: &Market, target_asset: MarketAsset, max_gap_nad: u128) {
    let gap = current_hlp_ideal_delta(market, target_asset).unwrap();
    assert!(
        gap.unsigned_abs() <= max_gap_nad,
        "hLP target gap {} exceeds {}",
        gap,
        max_gap_nad
    );
}

fn assert_market_hlp_invariants(market: &Market) {
    market.base_side.assert_share_backing().unwrap();
    market.quote_side.assert_share_backing().unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

fn price_diff_bps(before_nad: u64, after_nad: u64) -> u64 {
    if before_nad == 0 {
        return 0;
    }
    before_nad.abs_diff(after_nad).saturating_mul(BPS_DENOMINATOR as u64) / before_nad
}

fn set_side_live_preserving_hlp_invariant(market: &mut Market, asset: MarketAsset, live_reserve: u64) {
    let hlp_live = market.hlp_live_reserve(asset).unwrap() as u64;
    let live_reserve = live_reserve.max(hlp_live + 1);
    let cash_reserve = live_reserve - hlp_live;
    let side = market.side_mut(asset);
    side.reserves.live_reserve = live_reserve;
    side.reserves.cash_reserve = cash_reserve;
    match asset {
        MarketAsset::Base => {
            market.debt.fixed_base_shares = 0;
            market.debt.fixed_base_principal = 0;
        }
        MarketAsset::Quote => {
            market.debt.fixed_quote_shares = 0;
            market.debt.fixed_quote_principal = 0;
        }
    }
}

fn constrain_side_cash_preserving_hlp_invariant(market: &mut Market, asset: MarketAsset, cash_bps: u64) {
    let live_reserve = market.side(asset).reserves.live_reserve;
    let hlp_live = market.hlp_live_reserve(asset).unwrap() as u64;
    let non_hlp_backing = live_reserve.checked_sub(hlp_live).unwrap();
    let cash_reserve = non_hlp_backing
        .checked_mul(cash_bps)
        .unwrap()
        .checked_div(BPS_DENOMINATOR as u64)
        .unwrap();
    let cash_backed_debt = non_hlp_backing.checked_sub(cash_reserve).unwrap();
    market.side_mut(asset).reserves.cash_reserve = cash_reserve;
    match asset {
        MarketAsset::Base => {
            market.debt.fixed_base_shares = cash_backed_debt as u128;
            market.debt.fixed_base_principal = cash_backed_debt;
        }
        MarketAsset::Quote => {
            market.debt.fixed_quote_shares = cash_backed_debt as u128;
            market.debt.fixed_quote_principal = cash_backed_debt;
        }
    }
}

#[derive(Debug)]
struct TestCompositeSwapReceipt {
    amount_out: u64,
    base_pre_rebalance: HlpRebalanceReceipt,
    quote_pre_rebalance: HlpRebalanceReceipt,
    base_rebalance: HlpRebalanceReceipt,
    quote_rebalance: HlpRebalanceReceipt,
}

fn active_hlp_market() -> Market {
    let mut market = seeded_market();
    configure_market_depth(&mut market, 1_000_000, 20_000);
    market.deposit_single_sided(MarketAsset::Base, 100_000, 1).unwrap();
    market.deposit_single_sided(MarketAsset::Quote, 200_000, 1).unwrap();
    assert_market_hlp_invariants(&market);
    market
}

fn matched_symmetric_hlp_market() -> Market {
    let mut market = seeded_market();
    configure_market_depth(&mut market, 1_000_000, BPS_DENOMINATOR as u64);
    market.deposit_single_sided(MarketAsset::Base, 100_000, 1).unwrap();
    market.deposit_single_sided(MarketAsset::Quote, 100_000, 1).unwrap();
    assert_market_hlp_invariants(&market);
    market
}

#[test]
fn matched_cpmm_trace_accounts_rebalance_principal_and_releases_it_on_close() {
    let mut market = matched_symmetric_hlp_market();

    apply_test_composite_swap(&mut market, MarketAsset::Base, 350_000);

    assert_eq!(market.base_side.reserves.base_hlp_backing_inventory, 25_735);
    assert_eq!(market.base_side.reserves.quote_hlp_backing_inventory, 0);
    assert_eq!(market.quote_side.reserves.total_hlp_backing_inventory().unwrap(), 0);

    let supply_before = market.base_hlp_vault.hlp_supply;
    market
        .withdraw_single_sided(MarketAsset::Base, supply_before / 2)
        .unwrap();
    assert_eq!(market.base_side.reserves.base_hlp_backing_inventory, 12_868);

    let remaining = market.base_hlp_vault.hlp_supply;
    market.withdraw_single_sided(MarketAsset::Base, remaining).unwrap();
    assert_eq!(market.base_side.reserves.base_hlp_backing_inventory, 0);
    assert_eq!(market.quote_side.reserves.base_hlp_backing_inventory, 0);
}

#[test]
fn partial_interest_exit_with_second_hlp_and_backing_excludes_both_nested_claims() {
    let mut market = matched_symmetric_hlp_market();
    CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(|count| count.set(0));
    CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(|count| count.set(0));
    apply_test_composite_swap(&mut market, MarketAsset::Base, 350_000);
    assert_eq!(CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(Cell::get), 6);
    assert_eq!(CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(Cell::get), 1);
    let backing_before = market.base_side.reserves.base_hlp_backing_inventory;
    assert!(backing_before > 0);
    market.debt.quote_borrow_index_nad = 11 * NAD as u128 / 10;
    let eligibility = crate::state::HlpYieldEligibility {
        ylp_supply: market.base_side.shares.ylp_supply,
        base_hlp_ylp_shares: market.base_hlp_vault.ylp_shares,
        quote_hlp_ylp_shares: market.quote_hlp_vault.ylp_shares,
    };
    let exit_amount = market.base_hlp_vault.hlp_supply / 2;

    let withdrawal = market.withdraw_single_sided(MarketAsset::Base, exit_amount).unwrap();
    assert!(withdrawal.interest_paid > 0);
    assert!(withdrawal.hlp_supply > 0);
    assert!(market.quote_hlp_vault.hlp_supply > 0);
    assert!(market.base_side.reserves.base_hlp_backing_inventory < backing_before);
    assert!(market.base_side.reserves.base_hlp_backing_inventory > 0);
    let nested_before_funding = (
        market.base_hlp_vault.quote_interest_growth_index_q64,
        market.base_hlp_vault.quote_interest_remainder_q64,
        market.base_hlp_vault.quote_interest_growth_remainder_scaled,
        market.base_hlp_vault.unallocated_quote_interest_amount,
        market.quote_hlp_vault.quote_interest_growth_index_q64,
        market.quote_hlp_vault.quote_interest_remainder_q64,
        market.quote_hlp_vault.quote_interest_growth_remainder_scaled,
        market.quote_hlp_vault.unallocated_quote_interest_amount,
    );

    crate::record_inline_hlp_interest_credit(
        &mut market,
        MarketAsset::Quote,
        withdrawal.interest_paid,
        0,
        crate::state::ProtocolAuctionSplit::default(),
        eligibility,
    )
    .unwrap();

    assert_eq!(
        nested_before_funding,
        (
            market.base_hlp_vault.quote_interest_growth_index_q64,
            market.base_hlp_vault.quote_interest_remainder_q64,
            market.base_hlp_vault.quote_interest_growth_remainder_scaled,
            market.base_hlp_vault.unallocated_quote_interest_amount,
            market.quote_hlp_vault.quote_interest_growth_index_q64,
            market.quote_hlp_vault.quote_interest_remainder_q64,
            market.quote_hlp_vault.quote_interest_growth_remainder_scaled,
            market.quote_hlp_vault.unallocated_quote_interest_amount,
        )
    );
    market.quote_side.fees.assert_backed().unwrap();
}

#[test]
fn deleverage_interest_comes_from_payer_burn_leg_without_taxing_other_hlp() {
    let mut market = seeded_market();
    market.deposit_single_sided(MarketAsset::Base, 100, 1).unwrap();
    market.deposit_single_sided(MarketAsset::Quote, 200, 1).unwrap();
    market.debt.quote_borrow_index_nad = 5 * NAD as u128 / 4;

    let nonpayer_before = current_hlp_valuation(&market, MarketAsset::Quote).unwrap();
    let mut before_exit = market.clone();
    let before_exit = before_exit
        .withdraw_single_sided(MarketAsset::Quote, before_exit.quote_hlp_vault.hlp_supply)
        .unwrap();

    let valuation = current_hlp_valuation(&market, MarketAsset::Base).unwrap();
    let ylp_burn = 50;
    let base_leg = ylp_live_underlying_amount(&market, MarketAsset::Base, ylp_burn).unwrap();
    let quote_leg = ylp_live_underlying_amount(&market, MarketAsset::Quote, ylp_burn).unwrap();
    let interest_paid = hlp_deleverage_interest_for_burn(&market, MarketAsset::Base, ylp_burn, valuation).unwrap();
    assert_eq!((base_leg, quote_leg, interest_paid), (50, 100, 40));

    let base_live_before = market.base_side.reserves.live_reserve;
    let base_cash_before = market.base_side.reserves.cash_reserve;
    let quote_live_before = market.quote_side.reserves.live_reserve;
    let quote_cash_before = market.quote_side.reserves.cash_reserve;
    let payer_quote_synthetic_before = market.base_hlp_vault.quote_hlp_live_reserve;
    let base_backing_before = market.base_side.reserves.base_hlp_backing_inventory;
    let quote_backing_before = market.quote_side.reserves.base_hlp_backing_inventory;

    debit_hlp_rebalance_reserve(&mut market, MarketAsset::Base, MarketAsset::Base, base_leg, 0).unwrap();
    debit_hlp_rebalance_reserve(
        &mut market,
        MarketAsset::Base,
        MarketAsset::Quote,
        quote_leg,
        interest_paid,
    )
    .unwrap();

    assert_eq!(base_live_before - market.base_side.reserves.live_reserve, base_leg);
    assert_eq!(base_cash_before - market.base_side.reserves.cash_reserve, base_leg);
    assert_eq!(quote_live_before - market.quote_side.reserves.live_reserve, quote_leg);
    assert_eq!(
        quote_cash_before - market.quote_side.reserves.cash_reserve,
        interest_paid
    );
    assert_eq!(
        payer_quote_synthetic_before - market.base_hlp_vault.quote_hlp_live_reserve,
        quote_leg - interest_paid
    );
    assert_eq!(
        market.base_side.reserves.base_hlp_backing_inventory - base_backing_before,
        base_leg
    );
    assert_eq!(
        market.quote_side.reserves.base_hlp_backing_inventory,
        quote_backing_before
    );

    market.base_side.shares.burn(ylp_burn).unwrap();
    market.quote_side.shares.burn(ylp_burn).unwrap();
    market.base_hlp_vault.debit_ylp(ylp_burn).unwrap();
    let repayment = market
        .base_hlp_vault
        .repayment_for_max(200, market.debt.quote_borrow_index_nad)
        .unwrap();
    let clearance = market
        .base_hlp_vault
        .clear_debt_repay(repayment.shares_to_burn, market.debt.quote_borrow_index_nad)
        .unwrap();
    assert_eq!(clearance.interest_paid, interest_paid);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();

    let nonpayer_after = current_hlp_valuation(&market, MarketAsset::Quote).unwrap();
    assert_eq!(nonpayer_after.values, nonpayer_before.values);
    assert_eq!(nonpayer_after.nav_nad, nonpayer_before.nav_nad);
    let mut after_exit = market;
    let after_exit = after_exit
        .withdraw_single_sided(MarketAsset::Quote, after_exit.quote_hlp_vault.hlp_supply)
        .unwrap();
    assert_eq!(after_exit.target_amount_out, before_exit.target_amount_out);
}

fn enable_concentrated_curve(market: &mut Market) {
    market.config.amm = AmmConfig {
        peak_depth_nad: 200 * NAD,
        fade_scale_nad: NAD / 10,
        adjustment_threshold_nad: NAD / 100,
        adjustment_step_nad: NAD / 1_000,
        min_adjustment_interval_slots: 1,
        ..AmmConfig::default()
    };
    market.ensure_amm_initialized(0).unwrap();
    assert!(!market.current_curve_parameters(0).is_cpmm());
}

fn active_concentrated_hlp_market() -> Market {
    let mut market = seeded_market();
    configure_market_depth(&mut market, 1_000_000, 20_000);
    market.deposit_single_sided(MarketAsset::Base, 100_000, 1).unwrap();
    market.deposit_single_sided(MarketAsset::Quote, 200_000, 1).unwrap();
    enable_concentrated_curve(&mut market);
    assert_market_hlp_invariants(&market);
    market
}

fn active_concentrated_hlp_market_with_decimals(decimals: u8) -> Market {
    let scale = 10_u64.pow(decimals as u32);
    let mut market = seeded_market();
    market.base_side.asset_decimals = decimals;
    market.quote_side.asset_decimals = decimals;
    configure_market_depth(&mut market, 1_000_000 * scale, 20_000);
    market
        .deposit_single_sided(MarketAsset::Base, 100_000 * scale, 1)
        .unwrap();
    market
        .deposit_single_sided(MarketAsset::Quote, 200_000 * scale, 1)
        .unwrap();
    enable_concentrated_curve(&mut market);
    assert_market_hlp_invariants(&market);
    market
}

fn solve_concentrated_hlp_swap(
    market: &mut Market,
    asset_in: MarketAsset,
    reserve_credit: u64,
) -> Result<(HlpRebalanceReceipt, HlpRebalanceReceipt, AmmSwapQuote)> {
    let current_slot = curve_slot(market);
    let pre_state = market.dynamic_fee_pre_state(current_slot)?;
    let preliminary = market.preliminary_swap_inputs_for_state(reserve_credit, current_slot, pre_state)?;
    pre_solve_hlps_for_swap_joint(
        market,
        asset_in,
        reserve_credit,
        current_slot,
        pre_state,
        preliminary,
        SwapCashPolicy::Spot,
    )
}

fn apply_concentrated_hlp_swap(
    market: &mut Market,
    asset_in: MarketAsset,
    reserve_credit: u64,
) -> Result<AmmSwapQuote> {
    let prepared = crate::instructions::SwapRequest {
        current_slot: 0,
        asset_in,
        reserve_credit,
    }
    .prepare(market)?;
    let quote = prepared.quote;
    market.swap_reserves_with_fee_supply(
        asset_in,
        quote.fee.reserve_input_credit,
        quote.amount_out,
        quote.fee.claimable_fee_debit,
        0,
        crate::state::ProtocolAuctionSplit::default(),
        Some(prepared.fee_eligible_ylp_supply),
    )?;
    market.finalize_amm_trade(quote.start_price_nad, quote.end_price_nad, 0)?;
    market.finalize_hlp_vaults_for_swap(
        prepared.base_pre_rebalance,
        prepared.quote_pre_rebalance,
        0,
        Some(quote.reserve_end_price_nad),
    )?;
    market.checkpoint_amm_neutral_inventory(0)?;
    Ok(quote)
}

fn concentrated_branch_at_reserves(
    market: &Market,
    reserves: crate::market::amm::CurveReservesNad,
) -> crate::math::ConcentratedHybridBranch {
    let parameters = market.current_curve_parameters(curve_slot(market));
    crate::math::concentrated_hybrid_branch(
        reserves.base,
        reserves.quote,
        market.current_curve_center_price_nad().unwrap() as u128,
        parameters.peak_depth_nad as u128,
        parameters.fade_scale_nad as u128,
    )
    .unwrap()
}

fn assert_concentrated_candidate_safe(
    candidate: &Market,
    base_receipt: HlpRebalanceReceipt,
    quote_receipt: HlpRebalanceReceipt,
    quote: AmmSwapQuote,
) {
    let trade = quote.trade_endpoint().unwrap();
    let trade_base = denormalize_from_nad_floor(trade.reserves.base, candidate.base_side.asset_decimals).unwrap();
    let trade_quote = denormalize_from_nad_floor(trade.reserves.quote, candidate.quote_side.asset_decimals).unwrap();
    let trade_prices = hlp_curve_prices_from_base_price_nad(quote.end_price_nad as u128).unwrap();
    let reserve = quote.reserve_endpoint().unwrap();
    let reserve_base = denormalize_from_nad_floor(reserve.reserves.base, candidate.base_side.asset_decimals).unwrap();
    let reserve_quote =
        denormalize_from_nad_floor(reserve.reserves.quote, candidate.quote_side.asset_decimals).unwrap();
    let reserve_prices = hlp_curve_prices_from_base_price_nad(quote.reserve_end_price_nad as u128).unwrap();
    for receipt in [base_receipt, quote_receipt] {
        if receipt.tracking_loss_budget_nad == 0 {
            continue;
        }
        let tracking = HlpTrackingReference {
            principal_nav_nad: receipt.tracking_start_nav_nad,
            loss_budget_nad: receipt.tracking_loss_budget_nad,
            base_unrealized_interest: receipt.tracking_base_unrealized_interest,
            quote_unrealized_interest: receipt.tracking_quote_unrealized_interest,
            start_ylp_shares: receipt.tracking_start_ylp_shares,
            start_ylp_supply: receipt.tracking_start_ylp_supply,
        };
        let trade_endpoint =
            concentrated_hlp_endpoint(candidate, receipt.target_asset, trade_base, trade_quote, trade_prices).unwrap();
        let reserve_endpoint = concentrated_hlp_endpoint(
            candidate,
            receipt.target_asset,
            reserve_base,
            reserve_quote,
            reserve_prices,
        )
        .unwrap();
        let trade_at_reserve_mark =
            concentrated_hlp_endpoint(candidate, receipt.target_asset, trade_base, trade_quote, reserve_prices)
                .unwrap();
        let trade_error = hlp_tracking_deltas_nad(
            candidate,
            receipt.target_asset,
            trade_prices,
            trade_endpoint.nav_nad,
            tracking,
        )
        .unwrap()
        .2;
        let reserve_error = hlp_tracking_deltas_nad(
            candidate,
            receipt.target_asset,
            reserve_prices,
            reserve_endpoint.nav_nad,
            tracking,
        )
        .unwrap()
        .2;
        let trade_at_reserve_mark_error = hlp_tracking_deltas_nad(
            candidate,
            receipt.target_asset,
            reserve_prices,
            trade_at_reserve_mark.nav_nad,
            tracking,
        )
        .unwrap()
        .2;
        let budget = i128::try_from(receipt.tracking_loss_budget_nad).unwrap();
        let target_atom_nad =
            i128::try_from(normalize_to_nad(1, candidate.side(receipt.target_asset).asset_decimals).unwrap()).unwrap();
        assert!(trade_error >= -budget);
        assert!(reserve_error >= trade_error - target_atom_nad);
        assert_eq!(
            receipt.tracking_retained_contribution_nad,
            reserve_error - trade_at_reserve_mark_error
        );
    }
}

fn final_hlp_combined_tracking_delta(
    market: &Market,
    receipt: HlpRebalanceReceipt,
    _base_final: HlpRebalanceReceipt,
    _quote_final: HlpRebalanceReceipt,
) -> i128 {
    if receipt.tracking_loss_budget_nad == 0 {
        return 0;
    }
    let tracking = HlpTrackingReference {
        principal_nav_nad: receipt.tracking_start_nav_nad,
        loss_budget_nad: receipt.tracking_loss_budget_nad,
        base_unrealized_interest: receipt.tracking_base_unrealized_interest,
        quote_unrealized_interest: receipt.tracking_quote_unrealized_interest,
        start_ylp_shares: receipt.tracking_start_ylp_shares,
        start_ylp_supply: receipt.tracking_start_ylp_supply,
    };
    let prices = current_hlp_curve_prices(market).unwrap();
    let values = current_hlp_inventory_values_nad_with_prices(market, receipt.target_asset, prices).unwrap();
    let final_principal_nav = signed_value_difference(
        values.target_inventory_value_nad + values.opposite_inventory_value_nad,
        values.debt_value_nad,
    )
    .unwrap();
    hlp_tracking_deltas_nad(market, receipt.target_asset, prices, final_principal_nav, tracking)
        .unwrap()
        .2
        - receipt.tracking_retained_contribution_nad
}

#[test]
fn compact_guidance_lifecycle_matches_full_fixed_endpoints_for_spot_directions() {
    let _verification = VerifyCompactHlpGuidanceGuard::enable();
    let scale = 1_000_000_u64;
    for retain_dynamic_surcharge in [false, true] {
        for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
            let mut market = active_concentrated_hlp_market_with_decimals(6);
            market.config.divergence_fee_share_cap_bps = 2_000;
            market.config.amm.divergence_fee_coefficient_nad = 10 * NAD;
            market.amm.retain_dynamic_surcharge = retain_dynamic_surcharge;
            let (_, _, quote) = solve_concentrated_hlp_swap(&mut market, asset_in, 350_000 * scale)
                .unwrap_or_else(|error| {
                    panic!(
                        "retain={retain_dynamic_surcharge} asset_in={asset_in:?}: {error:?}"
                    )
                });
            if retain_dynamic_surcharge {
                assert!(quote.fee.retained_surcharge > 0);
            }
        }
    }
}

#[test]
fn compact_bounded_basis_quote_drives_the_same_fixed_endpoint_lifecycle() {
    let scale = 1_000_000_u64;
    for retain_dynamic_surcharge in [false, true] {
        for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
            let mut market = active_concentrated_hlp_market_with_decimals(6);
            market.config.divergence_fee_share_cap_bps = 2_000;
            market.config.amm.divergence_fee_coefficient_nad = 10 * NAD;
            market.amm.retain_dynamic_surcharge = retain_dynamic_surcharge;
            let current_slot = curve_slot(&market);
            let reserve_credit = 100_000 * scale;
            let pre_state = market.dynamic_fee_pre_state(current_slot).unwrap();
            let preliminary = market
                .preliminary_swap_inputs_for_state(reserve_credit, current_slot, pre_state)
                .unwrap();
            let start_reserves = market.curve_reserves_nad().unwrap();
            let start_prepared = market
                .prepare_curve_for_reserves_nad(
                    start_reserves,
                    market.current_curve_center_price_nad().unwrap(),
                    current_slot,
                )
                .unwrap();
            let frozen_prices =
                hlp_curve_prices_from_base_price_nad(start_prepared.marginal_price_nad().unwrap()).unwrap();
            let context = ConcentratedHlpSolveContext {
                base_start: concentrated_hlp_start(&market, MarketAsset::Base, frozen_prices).unwrap(),
                quote_start: concentrated_hlp_start(&market, MarketAsset::Quote, frozen_prices).unwrap(),
                frozen_prices,
                asset_in,
                reserve_credit,
                current_slot,
                pre_state,
                preliminary,
                cash_policy: SwapCashPolicy::Spot,
                guidance_start_prepared: start_prepared,
                guidance_start_ylp_supply: market.base_side.shares.ylp_supply,
            };

            // The immutable anchor is captured from the operation start. The
            // mutable state may later carry a different candidate supply; do
            // not recapture this static from a prepositioned candidate.
            let fixed = HlpPlannerStatic::capture(&market).unwrap();
            let state = HlpPlannerState::capture(&market);
            let basis = ConcentratedGuidanceBasis::capture(&market, &context).unwrap();
            HLP_COMPACT_GUIDANCE_CELLS.with(|count| count.set(0));
            HLP_COMPACT_GUIDANCE_EXACT_IN_PROBES.with(|count| count.set(0));
            let bounded = basis
                .quote_bounded(&market, &context, fixed, state, false)
                .unwrap_or_else(|error| {
                    panic!(
                        "retain={retain_dynamic_surcharge} asset_in={asset_in:?}: {error:?}"
                    )
                });
            assert_eq!(HLP_COMPACT_GUIDANCE_CELLS.with(Cell::get), 1);
            assert!((1..=2).contains(&HLP_COMPACT_GUIDANCE_EXACT_IN_PROBES.with(Cell::get)));

            let exact = project_concentrated_hlp_candidate(&market, &context, preliminary, false)
                .unwrap()
                .common();
            assert_eq!(bounded.common.amount_in_after_fee, exact.amount_in_after_fee);
            assert!(bounded.common.amount_out <= exact.amount_out);
            assert_eq!(bounded.start_ylp_supply, state.base_side.ylp_supply);

            let endpoints = HlpGuidanceEndpointCapability {
                current_slot,
                curve_revision: market.curve_revision,
                center_price_nad: market.current_curve_center_price_nad().unwrap(),
                parameters: market.current_curve_parameters(current_slot),
                retain_dynamic_surcharge: bounded.retain_dynamic_surcharge,
                trade_prepared: bounded.trade,
                reserve_prepared: bounded.reserve,
            };
            let args = HlpAuthoritativeLifecycleArgs {
                amount_in_after_fee: bounded.common.amount_in_after_fee,
                retained_surcharge: bounded.common.retained_surcharge,
                amount_out: bounded.common.amount_out,
                endpoints: HlpLifecycleEndpointMode::Guidance(endpoints),
                expected_trade_price_nad: bounded.common.end_price_nad,
                expected_reserve_price_nad: bounded.common.reserve_end_price_nad,
            };
            let compact = compact_hlp_lifecycle_tracking(&market, &context, &args).unwrap();
            let mut scratch = Market::default();
            let full = scratch_authoritative_result_preserving_preposition(
                &mut scratch,
                &market,
                &context,
                &args,
            )
            .unwrap();
            assert_eq!(compact, full);
        }
    }
}

#[test]
fn concentrated_predictor_jointly_tracks_both_hlps_with_bounded_exact_quotes() {
    for decimals in [6_u8, 9] {
        let scale = 10_u64.pow(decimals as u32);
        for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
            for whole_credit in [25_000_u64, 100_000, 350_000, 500_000] {
                let mut market = active_concentrated_hlp_market_with_decimals(decimals);
                CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(|count| count.set(0));

                let (base, quote, swap_quote) =
                    solve_concentrated_hlp_swap(&mut market, asset_in, whole_credit * scale).unwrap();
                let evaluations = CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(Cell::get);

                assert!(evaluations <= HLP_CONCENTRATED_MAX_CANDIDATE_EVALUATIONS);
                assert!(swap_quote.amount_out > 0);
                assert_concentrated_candidate_safe(&market, base, quote, swap_quote);
                if whole_credit >= 350_000 {
                    assert!(
                        base.ylp_mint_amount != 0
                            || base.ylp_burn_amount != 0
                            || quote.ylp_mint_amount != 0
                            || quote.ylp_burn_amount != 0,
                        "large {asset_in:?} swap at {decimals} decimals must pre-position hLP depth"
                    );
                }
                assert_market_hlp_invariants(&market);
            }
        }
    }
}

#[test]
fn concentrated_predictor_covers_inner_transition_and_stateful_exact_tail() {
    use crate::math::ConcentratedHybridBranch::{BaseScarceTransition, Inner, QuoteScarceTail, QuoteScarceTransition};

    let scale = 1_000_000_u64;
    for (asset_in, amount, expected) in [
        (MarketAsset::Base, 100_000, Inner),
        (MarketAsset::Quote, 100_000, Inner),
        (MarketAsset::Base, 350_000, QuoteScarceTransition),
        (MarketAsset::Quote, 500_000, BaseScarceTransition),
    ] {
        let mut market = active_concentrated_hlp_market_with_decimals(6);
        assert_eq!(
            concentrated_branch_at_reserves(&market, market.curve_reserves_nad().unwrap()),
            Inner
        );
        let (base, quote_receipt, quote) = solve_concentrated_hlp_swap(&mut market, asset_in, amount * scale).unwrap();
        assert_eq!(
            concentrated_branch_at_reserves(&market, quote.trade_endpoint().unwrap().reserves),
            expected
        );
        assert_concentrated_candidate_safe(&market, base, quote_receipt, quote);
    }

    let mut tail = active_concentrated_hlp_market_with_decimals(6);
    tail.config.settlement_divergence_bps = BPS_DENOMINATOR;
    let mut tail_step = None;
    for step in 1..=80 {
        apply_concentrated_hlp_swap(&mut tail, MarketAsset::Base, 25_000 * scale).unwrap();
        if concentrated_branch_at_reserves(&tail, tail.curve_reserves_nad().unwrap()) == QuoteScarceTail {
            tail_step = Some(step);
            break;
        }
    }
    assert!(tail_step.is_some(), "bounded accepted swaps must reach the exact tail");
    assert_eq!(
        concentrated_branch_at_reserves(&tail, tail.curve_reserves_nad().unwrap()),
        QuoteScarceTail
    );
    let (base, quote, restoring) = solve_concentrated_hlp_swap(&mut tail, MarketAsset::Quote, 10_000 * scale).unwrap();
    assert_eq!(
        concentrated_branch_at_reserves(&tail, restoring.trade_endpoint().unwrap().reserves),
        QuoteScarceTransition
    );
    assert_concentrated_candidate_safe(&tail, base, quote, restoring);
}

#[test]
fn concentrated_predictor_handles_one_active_hlp_and_restores_when_none_are_active() {
    let scale = 1_000_000_u64;
    let mut one_active = seeded_market();
    one_active.base_side.asset_decimals = 6;
    one_active.quote_side.asset_decimals = 6;
    configure_market_depth(&mut one_active, 1_000_000 * scale, 20_000);
    one_active
        .deposit_single_sided(MarketAsset::Base, 100_000 * scale, 1)
        .unwrap();
    enable_concentrated_curve(&mut one_active);
    for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = one_active.clone();
        let (base, quote, swap_quote) = solve_concentrated_hlp_swap(&mut market, asset_in, 100_000 * scale).unwrap();
        assert_eq!(quote, empty_hlp_rebalance_receipt(MarketAsset::Quote));
        assert!(base.nav_nad > 0 || base.ylp_mint_amount > 0 || base.ylp_burn_amount > 0);
        assert_concentrated_candidate_safe(&market, base, quote, swap_quote);
    }

    let mut neither = seeded_market();
    neither.base_side.asset_decimals = 6;
    neither.quote_side.asset_decimals = 6;
    configure_market_depth(&mut neither, 1_000_000 * scale, 20_000);
    enable_concentrated_curve(&mut neither);
    let before = neither.try_to_vec().unwrap();
    assert_eq!(
        solve_concentrated_hlp_swap(&mut neither, MarketAsset::Base, 100_000 * scale).unwrap_err(),
        error!(ErrorCode::HlpSettlementUnavailable)
    );
    assert_eq!(neither.try_to_vec().unwrap(), before);
}

#[test]
fn concentrated_predictor_is_deterministic_and_restores_state_on_fail_closed_coarse_flow() {
    let original = active_concentrated_hlp_market_with_decimals(9);
    let mut first = original.clone();
    let mut second = original.clone();
    let first_result = solve_concentrated_hlp_swap(&mut first, MarketAsset::Base, 350_000 * NAD).unwrap();
    let second_result = solve_concentrated_hlp_swap(&mut second, MarketAsset::Base, 350_000 * NAD).unwrap();
    assert_eq!(first_result, second_result);
    assert_eq!(first.try_to_vec().unwrap(), second.try_to_vec().unwrap());

    let mut coarse = active_concentrated_hlp_market();
    coarse.base_side.reserves.cash_reserve = hlp_debt_amount(&coarse, MarketAsset::Quote).unwrap();
    coarse.quote_side.reserves.cash_reserve = hlp_debt_amount(&coarse, MarketAsset::Base).unwrap();
    coarse.base_side.reserves.live_reserve = coarse.base_side.reserves.cash_reserve
        + u64::try_from(coarse.hlp_live_reserve(MarketAsset::Base).unwrap()).unwrap();
    coarse.quote_side.reserves.live_reserve = coarse.quote_side.reserves.cash_reserve
        + u64::try_from(coarse.hlp_live_reserve(MarketAsset::Quote).unwrap()).unwrap();
    coarse.checkpoint_amm_neutral_inventory(0).unwrap();
    assert_eq!(coarse.hlp_funding_headroom(MarketAsset::Base).unwrap(), 0);
    assert_eq!(coarse.hlp_funding_headroom(MarketAsset::Quote).unwrap(), 0);
    let coarse_before = coarse.try_to_vec().unwrap();
    for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
        let mut capped = coarse.clone();
        CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(|count| count.set(0));
        assert_eq!(
            solve_concentrated_hlp_swap(&mut capped, asset_in, 500_000).unwrap_err(),
            error!(ErrorCode::HlpSettlementUnavailable)
        );
        let evaluations = CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(Cell::get);
        assert!(evaluations > 1 && evaluations <= HLP_CONCENTRATED_MAX_CANDIDATE_EVALUATIONS);
        assert_eq!(capped.try_to_vec().unwrap(), coarse_before);
    }

    let mut cash_capped = active_concentrated_hlp_market();
    constrain_side_cash_preserving_hlp_invariant(&mut cash_capped, MarketAsset::Base, 0);
    constrain_side_cash_preserving_hlp_invariant(&mut cash_capped, MarketAsset::Quote, 0);
    cash_capped.checkpoint_amm_neutral_inventory(0).unwrap();
    assert_eq!(cash_capped.base_side.reserves.cash_reserve, 0);
    assert_eq!(cash_capped.quote_side.reserves.cash_reserve, 0);
    let cash_capped_before = cash_capped.try_to_vec().unwrap();
    for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
        let mut capped = cash_capped.clone();
        assert_eq!(
            solve_concentrated_hlp_swap(&mut capped, asset_in, 500_000).unwrap_err(),
            error!(ErrorCode::HlpSettlementUnavailable)
        );
        assert_eq!(capped.try_to_vec().unwrap(), cash_capped_before);
    }
}

#[test]
fn concentrated_predictor_tracks_trade_nav_and_bounds_retained_principal_endpoint() {
    let scale = 1_000_000_u64;
    for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = active_concentrated_hlp_market_with_decimals(6);
        market.config.divergence_fee_share_cap_bps = 2_000;
        market.config.amm.divergence_fee_coefficient_nad = 10 * NAD;
        market.amm.retain_dynamic_surcharge = true;
        let original = market.clone();

        let pre_state = original.dynamic_fee_pre_state(0).unwrap();
        let preliminary = original
            .preliminary_swap_inputs_for_state(350_000 * scale, 0, pre_state)
            .unwrap();
        let exact_guidance_input = original
            .exact_swap_input_for_guidance(
                asset_in,
                350_000 * scale,
                0,
                original.curve_reserves_nad().unwrap(),
                pre_state,
                preliminary,
            )
            .unwrap();
        let zero_plan_quote = original
            .quote_amm_swap_for_reserves_nad(
                asset_in,
                350_000 * scale,
                0,
                original.curve_reserves_nad().unwrap(),
                pre_state,
                preliminary,
            )
            .unwrap();
        assert_eq!(exact_guidance_input, zero_plan_quote.fee.amount_in_for_quote);

        CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(|count| count.set(0));
        CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(|count| count.set(0));
        let prepared = crate::instructions::SwapRequest {
            current_slot: 0,
            asset_in,
            reserve_credit: 350_000 * scale,
        }
        .prepare(&mut market)
        .unwrap();
        let quote = prepared.quote;
        let evaluations = CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(Cell::get);
        let authoritative_evaluations = CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(Cell::get);
        assert_eq!(evaluations, 6, "asset_in={asset_in:?}");
        assert_eq!(authoritative_evaluations, 1, "asset_in={asset_in:?}");
        assert!(authoritative_evaluations <= HLP_CONCENTRATED_MAX_AUTHORITATIVE_EVALUATIONS);

        assert!(quote.fee.retained_surcharge > 0);
        assert!(prepared.base_pre_rebalance.tracking_retained_contribution_nad > 0);
        assert!(prepared.quote_pre_rebalance.tracking_retained_contribution_nad > 0);
        assert_ne!(
            quote.trade_endpoint().unwrap().reserves,
            quote.reserve_endpoint().unwrap().reserves
        );
        let base_contribution = u128::try_from(prepared.base_pre_rebalance.tracking_retained_contribution_nad).unwrap();
        let quote_contribution =
            u128::try_from(prepared.quote_pre_rebalance.tracking_retained_contribution_nad).unwrap();
        let reserve_price_nad = quote.reserve_end_price_nad as u128;
        let contribution_in_input_nad = match asset_in {
            MarketAsset::Base => {
                base_contribution + mul_div_u128(quote_contribution, NAD as u128, reserve_price_nad).unwrap()
            }
            MarketAsset::Quote => {
                quote_contribution + mul_div_u128(base_contribution, reserve_price_nad, NAD as u128).unwrap()
            }
        };
        let retained_input_nad = normalize_to_nad(
            quote.fee.retained_surcharge as u128,
            market.side(asset_in).asset_decimals,
        )
        .unwrap();
        let tracked_hlp_shares =
            u128::from(market.base_hlp_vault.ylp_shares) + u128::from(market.quote_hlp_vault.ylp_shares);
        let retained_input_claim_nad = mul_div_u128(
            retained_input_nad,
            tracked_hlp_shares,
            market.base_side.shares.ylp_supply as u128,
        )
        .unwrap();
        let raw_rounding_nad = 2 * normalize_to_nad(1, market.side(asset_in).asset_decimals).unwrap();
        assert!(contribution_in_input_nad <= retained_input_claim_nad + raw_rounding_nad);
        assert_concentrated_candidate_safe(
            &market,
            prepared.base_pre_rebalance,
            prepared.quote_pre_rebalance,
            quote,
        );

        let retained = market.amm.retain_dynamic_surcharge;
        market.checkpoint_amm_neutral_inventory(0).unwrap();
        assert_eq!(market.amm.retain_dynamic_surcharge, retained);
        let pre_state = market.dynamic_fee_pre_state(0).unwrap();
        let preliminary = market
            .preliminary_swap_inputs_for_state(350_000 * scale, 0, pre_state)
            .unwrap();
        let replay = market
            .quote_amm_swap_for_reserves_nad(
                asset_in,
                350_000 * scale,
                0,
                market.curve_reserves_nad().unwrap(),
                pre_state,
                preliminary,
            )
            .unwrap();
        assert_eq!(replay, quote);

        let base_pre = prepared.base_pre_rebalance;
        let quote_pre = prepared.quote_pre_rebalance;
        let finalized = prepared
            .finalize_state(&mut market, 0, 0, crate::state::ProtocolAuctionSplit::default())
            .unwrap();
        let base_error =
            final_hlp_combined_tracking_delta(&market, base_pre, finalized.base_rebalance, finalized.quote_rebalance);
        let quote_error =
            final_hlp_combined_tracking_delta(&market, quote_pre, finalized.base_rebalance, finalized.quote_rebalance);
        assert!(base_error.unsigned_abs() <= base_pre.tracking_loss_budget_nad);
        assert!(quote_error.unsigned_abs() <= quote_pre.tracking_loss_budget_nad);
    }
}

#[test]
fn retained_contribution_and_quote_endpoint_identity_reject_tampering() {
    let scale = 1_000_000_u64;
    for tamper in 0..3 {
        let mut market = active_concentrated_hlp_market_with_decimals(6);
        market.config.divergence_fee_share_cap_bps = 2_000;
        market.config.amm.divergence_fee_coefficient_nad = 10 * NAD;
        market.amm.retain_dynamic_surcharge = true;
        let mut prepared = crate::instructions::SwapRequest {
            current_slot: 0,
            asset_in: MarketAsset::Base,
            reserve_credit: 350_000 * scale,
        }
        .prepare(&mut market)
        .unwrap();
        assert!(prepared.quote.fee.retained_surcharge > 0);

        match tamper {
            0 => {
                assert!(
                    prepared
                        .base_pre_rebalance
                        .tracking_retained_contribution_nad
                        .unsigned_abs()
                        > prepared.base_pre_rebalance.tracking_loss_budget_nad
                );
                prepared.base_pre_rebalance.tracking_retained_contribution_nad = 0;
            }
            1 => market.amm.retain_dynamic_surcharge = false,
            2 => market.side_mut(MarketAsset::Base).credit_reserve(1, true).unwrap(),
            _ => unreachable!(),
        }

        assert!(prepared
            .finalize_state(&mut market, 0, 0, crate::state::ProtocolAuctionSplit::default(),)
            .is_err());
    }
}

#[test]
fn concentrated_predictor_handles_exact_high_divergence_with_distributed_surcharge() {
    let scale = 1_000_000_u64;
    for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = active_concentrated_hlp_market_with_decimals(6);
        market.config.divergence_fee_share_cap_bps = 2_000;
        market.config.amm.divergence_fee_coefficient_nad = 10 * NAD;
        market.config.amm.adjustment_step_nad = 0;
        market.amm.retain_dynamic_surcharge = false;
        CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(|count| count.set(0));
        let (base, quote_receipt, quote) = solve_concentrated_hlp_swap(&mut market, asset_in, 350_000 * scale).unwrap();
        let evaluations = CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(Cell::get);

        assert!(evaluations <= HLP_CONCENTRATED_MAX_CANDIDATE_EVALUATIONS);
        assert!(quote.fee.divergence_surcharge_debit > 0);
        assert_eq!(quote.fee.retained_surcharge, 0);
        assert_eq!(quote.trade_endpoint().unwrap(), quote.reserve_endpoint().unwrap());
        assert_concentrated_candidate_safe(&market, base, quote_receipt, quote);
    }
}

#[test]
fn joint_predictor_tracks_asymmetric_unpaid_interest_in_both_curve_modes_and_directions() {
    let scale = 1_000_000_u64;
    for concentrated in [false, true] {
        for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
            let mut market = seeded_market();
            market.base_side.asset_decimals = 6;
            market.quote_side.asset_decimals = 6;
            configure_market_depth(&mut market, 1_000_000 * scale, 20_000);
            market
                .deposit_single_sided(MarketAsset::Base, 100_000 * scale, 1)
                .unwrap();
            if concentrated {
                enable_concentrated_curve(&mut market);
            }
            let curve_before = market.curve_reserves_nad().unwrap();
            let price_before = market.curve_marginal_price_nad(0).unwrap();

            let principal = 200_000 * scale;
            let unpaid_interest = 500 * scale;
            market.base_side.reserves.cash_reserve -= principal;
            market.debt.fixed_base_shares = principal as u128;
            market.debt.fixed_base_principal = principal;
            market.debt.base_borrow_index_nad =
                ((principal as u128 + unpaid_interest as u128) * NAD as u128) / principal as u128;
            market.base_side.reserves.live_reserve += unpaid_interest;
            assert_eq!(
                market.unrealized_interest(MarketAsset::Base).unwrap(),
                unpaid_interest as u128
            );
            assert_eq!(market.curve_reserves_nad().unwrap(), curve_before);
            assert_eq!(market.curve_marginal_price_nad(0).unwrap(), price_before);
            assert_market_hlp_invariants(&market);

            let prepared = crate::instructions::SwapRequest {
                current_slot: 0,
                asset_in,
                reserve_credit: 350_000 * scale,
            }
            .prepare(&mut market)
            .unwrap_or_else(|error| panic!("concentrated={concentrated} asset_in={asset_in:?}: {error:?}"));
            let base_pre = prepared.base_pre_rebalance;
            assert_eq!(base_pre.tracking_retained_contribution_nad, 0);
            let finalized = prepared
                .finalize_state(&mut market, 0, 0, crate::state::ProtocolAuctionSplit::default())
                .unwrap();
            let combined = final_hlp_combined_tracking_delta(
                &market,
                base_pre,
                finalized.base_rebalance,
                finalized.quote_rebalance,
            );
            assert!(combined.unsigned_abs() <= base_pre.tracking_loss_budget_nad);
            assert_market_hlp_invariants(&market);
        }
    }
}

#[test]
fn concentrated_predictor_excludes_hlp_funding_interest_from_tracking_and_nested_yield() {
    let scale = 1_000_000_u64;
    let mut market = seeded_market();
    market.base_side.asset_decimals = 6;
    market.quote_side.asset_decimals = 6;
    configure_market_depth(&mut market, 1_000_000 * scale, 20_000);
    market
        .deposit_single_sided(MarketAsset::Base, 100_000 * scale, 1)
        .unwrap();
    enable_concentrated_curve(&mut market);
    market.debt.quote_borrow_index_nad = 11 * NAD as u128 / 10;
    assert_eq!(market.unrealized_interest(MarketAsset::Quote).unwrap(), 0);
    assert!(market.hlp_funding_debt(MarketAsset::Quote).unwrap() > market.base_hlp_vault.debt_principal as u128);

    let prepared = crate::instructions::SwapRequest {
        current_slot: 0,
        asset_in: MarketAsset::Base,
        reserve_credit: 350_000 * scale,
    }
    .prepare(&mut market)
    .unwrap();
    assert!(prepared.base_pre_rebalance.ylp_burn_amount > 0);
    let finalized = prepared
        .finalize_state(&mut market, 0, 0, crate::state::ProtocolAuctionSplit::default())
        .unwrap();
    let funding_interest_credit = finalized.base_rebalance.interest_paid;
    assert!(funding_interest_credit > 0);
    let base_nested_before = (
        market.base_hlp_vault.quote_interest_growth_index_q64,
        market.base_hlp_vault.quote_interest_remainder_q64,
        market.base_hlp_vault.quote_interest_growth_remainder_scaled,
    );
    let quote_nested_before = (
        market.quote_hlp_vault.quote_interest_growth_index_q64,
        market.quote_hlp_vault.quote_interest_remainder_q64,
        market.quote_hlp_vault.quote_interest_growth_remainder_scaled,
    );
    crate::record_inline_hlp_interest_credit(
        &mut market,
        MarketAsset::Quote,
        funding_interest_credit,
        0,
        crate::state::ProtocolAuctionSplit::default(),
        prepared.interest_eligibility,
    )
    .unwrap();
    assert_eq!(
        base_nested_before,
        (
            market.base_hlp_vault.quote_interest_growth_index_q64,
            market.base_hlp_vault.quote_interest_remainder_q64,
            market.base_hlp_vault.quote_interest_growth_remainder_scaled,
        )
    );
    assert_eq!(
        quote_nested_before,
        (
            market.quote_hlp_vault.quote_interest_growth_index_q64,
            market.quote_hlp_vault.quote_interest_remainder_q64,
            market.quote_hlp_vault.quote_interest_growth_remainder_scaled,
        )
    );
    let global_interest_index = market.quote_side.fees.interest_growth_index_q64;
    assert_eq!(
        market.base_hlp_vault.quote_interest_checkpoint_q64,
        global_interest_index
    );
    assert_eq!(
        market.quote_hlp_vault.quote_interest_checkpoint_q64,
        global_interest_index
    );
    let combined = final_hlp_combined_tracking_delta(
        &market,
        prepared.base_pre_rebalance,
        finalized.base_rebalance,
        finalized.quote_rebalance,
    );
    assert!(combined.unsigned_abs() <= prepared.base_pre_rebalance.tracking_loss_budget_nad);
    assert_market_hlp_invariants(&market);
}

#[test]
fn two_active_concentrated_hlps_with_funding_interest_settle_both_directions_within_six_evaluations() {
    let scale = 1_000_000_u64;
    for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = active_concentrated_hlp_market_with_decimals(6);
        market.debt.base_borrow_index_nad = 11 * NAD as u128 / 10;
        market.debt.quote_borrow_index_nad = 11 * NAD as u128 / 10;
        assert_eq!(market.unrealized_interest(MarketAsset::Base).unwrap(), 0);
        assert_eq!(market.unrealized_interest(MarketAsset::Quote).unwrap(), 0);
        CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(|count| count.set(0));
        CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(|count| count.set(0));

        let prepared = crate::instructions::SwapRequest {
            current_slot: 0,
            asset_in,
            reserve_credit: 350_000 * scale,
        }
        .prepare(&mut market)
        .unwrap_or_else(|error| panic!("asset_in={asset_in:?}: {error:?}"));
        let evaluations = CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(Cell::get);
        let authoritative_evaluations = CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(Cell::get);
        assert!(evaluations <= HLP_CONCENTRATED_MAX_CANDIDATE_EVALUATIONS);
        assert!(authoritative_evaluations <= HLP_CONCENTRATED_MAX_AUTHORITATIVE_EVALUATIONS);
        assert_eq!(authoritative_evaluations, 1, "asset_in={asset_in:?}");
        let finalized = prepared
            .finalize_state(&mut market, 0, 0, crate::state::ProtocolAuctionSplit::default())
            .unwrap();
        assert!(
            finalized.base_rebalance.interest_paid > 0 || finalized.quote_rebalance.interest_paid > 0,
            "asset_in={asset_in:?}"
        );
        let nested_before = (
            market.base_hlp_vault.base_interest_growth_index_q64,
            market.base_hlp_vault.base_interest_remainder_q64,
            market.base_hlp_vault.base_interest_growth_remainder_scaled,
            market.base_hlp_vault.quote_interest_growth_index_q64,
            market.base_hlp_vault.quote_interest_remainder_q64,
            market.base_hlp_vault.quote_interest_growth_remainder_scaled,
            market.quote_hlp_vault.base_interest_growth_index_q64,
            market.quote_hlp_vault.base_interest_remainder_q64,
            market.quote_hlp_vault.base_interest_growth_remainder_scaled,
            market.quote_hlp_vault.quote_interest_growth_index_q64,
            market.quote_hlp_vault.quote_interest_remainder_q64,
            market.quote_hlp_vault.quote_interest_growth_remainder_scaled,
        );
        crate::record_inline_hlp_interest_credit(
            &mut market,
            MarketAsset::Quote,
            finalized.base_rebalance.interest_paid,
            0,
            crate::state::ProtocolAuctionSplit::default(),
            prepared.interest_eligibility,
        )
        .unwrap();
        crate::record_inline_hlp_interest_credit(
            &mut market,
            MarketAsset::Base,
            finalized.quote_rebalance.interest_paid,
            0,
            crate::state::ProtocolAuctionSplit::default(),
            prepared.interest_eligibility,
        )
        .unwrap();
        assert_eq!(
            nested_before,
            (
                market.base_hlp_vault.base_interest_growth_index_q64,
                market.base_hlp_vault.base_interest_remainder_q64,
                market.base_hlp_vault.base_interest_growth_remainder_scaled,
                market.base_hlp_vault.quote_interest_growth_index_q64,
                market.base_hlp_vault.quote_interest_remainder_q64,
                market.base_hlp_vault.quote_interest_growth_remainder_scaled,
                market.quote_hlp_vault.base_interest_growth_index_q64,
                market.quote_hlp_vault.base_interest_remainder_q64,
                market.quote_hlp_vault.base_interest_growth_remainder_scaled,
                market.quote_hlp_vault.quote_interest_growth_index_q64,
                market.quote_hlp_vault.quote_interest_remainder_q64,
                market.quote_hlp_vault.quote_interest_growth_remainder_scaled,
            ),
            "asset_in={asset_in:?}"
        );
        assert_market_hlp_invariants(&market);
    }
}

#[test]
fn litesvm_scale_insolvent_concentrated_hlps_with_2_1x_funding_fail_closed_both_directions() {
    for (asset_in, reserve_credit) in [
        (MarketAsset::Quote, 15_000_000_u64),
        (MarketAsset::Base, 35_000_000_u64),
    ] {
        let mut market = seeded_market();
        market.base_side.asset_decimals = 6;
        market.quote_side.asset_decimals = 6;
        configure_market_depth(&mut market, 150_000_000, 20_000);
        market.deposit_single_sided(MarketAsset::Base, 10_000_000, 1).unwrap();
        market.deposit_single_sided(MarketAsset::Quote, 20_000_000, 1).unwrap();
        enable_concentrated_curve(&mut market);
        market.debt.base_borrow_index_nad = 21 * NAD as u128 / 10;
        market.debt.quote_borrow_index_nad = 21 * NAD as u128 / 10;
        assert_eq!(market.base_hlp_vault.hlp_supply, 10_000_000);
        assert_eq!(market.quote_hlp_vault.hlp_supply, 20_000_000);

        let prices = current_hlp_curve_prices(&market).unwrap();
        assert_eq!(
            concentrated_hlp_start(&market, MarketAsset::Base, prices)
                .unwrap()
                .economic_nav_nad,
            0
        );
        assert_eq!(
            concentrated_hlp_start(&market, MarketAsset::Quote, prices)
                .unwrap()
                .economic_nav_nad,
            0
        );

        let request = crate::instructions::SwapRequest {
            current_slot: 0,
            asset_in,
            reserve_credit,
        };
        let mut prepare_market = market.clone();
        CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(|count| count.set(0));
        CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(|count| count.set(0));
        let error = request.prepare(&mut prepare_market).unwrap_err();
        assert_eq!(error, error!(ErrorCode::HlpSettlementUnavailable));
        let prepare_evaluations = CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(Cell::get);
        let prepare_authorities = CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(Cell::get);
        assert!(prepare_evaluations <= HLP_CONCENTRATED_MAX_CANDIDATE_EVALUATIONS);
        assert!(prepare_authorities <= HLP_CONCENTRATED_MAX_AUTHORITATIVE_EVALUATIONS);

        let mut solver_market = market;
        solver_market.accrue_interest_to_slot(request.current_slot).unwrap();
        solver_market.prepare_amm_for_swap(request.current_slot).unwrap();
        solver_market
            .advance_one_amm_controller_target(request.current_slot)
            .unwrap();
        let pre_state = solver_market.dynamic_fee_pre_state(request.current_slot).unwrap();
        let preliminary = solver_market
            .preliminary_swap_inputs_for_state(request.reserve_credit, request.current_slot, pre_state)
            .unwrap();
        let before_solver = solver_market.try_to_vec().unwrap();
        CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(|count| count.set(0));
        CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(|count| count.set(0));
        let solver_error = pre_solve_hlps_for_swap_joint(
            &mut solver_market,
            request.asset_in,
            request.reserve_credit,
            request.current_slot,
            pre_state,
            preliminary,
            SwapCashPolicy::Spot,
        )
        .unwrap_err();
        assert_eq!(solver_error, error);
        assert_eq!(solver_market.try_to_vec().unwrap(), before_solver);
        let solver_evaluations = CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(Cell::get);
        let solver_authorities = CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(Cell::get);
        assert_eq!(solver_evaluations, prepare_evaluations);
        assert_eq!(solver_authorities, prepare_authorities);
        assert!(solver_authorities <= HLP_CONCENTRATED_MAX_AUTHORITATIVE_EVALUATIONS);
    }
}

#[test]
fn concentrated_interest_shortfall_is_minimal_and_replays_exactly() {
    let scale = 1_000_000_u64;
    let mut fixture = active_concentrated_hlp_market_with_decimals(6);
    fixture.base_side.credit_reserve(500_000 * scale, true).unwrap();
    fixture.quote_side.credit_reserve(1_000_000 * scale, true).unwrap();
    fixture.checkpoint_amm_neutral_inventory(0).unwrap();
    fixture.debt.base_borrow_index_nad = 21 * NAD as u128 / 10;
    fixture.debt.quote_borrow_index_nad = 21 * NAD as u128 / 10;
    assert!(!fixture.current_curve_parameters(0).is_cpmm());

    let request = crate::instructions::SwapRequest {
        current_slot: 0,
        asset_in: MarketAsset::Base,
        reserve_credit: 350_000 * scale,
    };
    let mut first = fixture.clone();
    let mut replay = fixture.clone();
    CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(|count| count.set(0));
    CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(|count| count.set(0));
    let prepared = request.prepare(&mut first).unwrap();
    let first_evaluations = CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(Cell::get);
    let first_authoritative_evaluations = CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(Cell::get);
    CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(|count| count.set(0));
    CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(|count| count.set(0));
    let replay_prepared = request.prepare(&mut replay).unwrap();
    let replay_evaluations = CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(Cell::get);
    let replay_authoritative_evaluations = CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(Cell::get);

    assert_eq!(first_evaluations, 6);
    assert_eq!(replay_evaluations, first_evaluations);
    assert_eq!(first_authoritative_evaluations, 1);
    assert_eq!(replay_authoritative_evaluations, first_authoritative_evaluations);
    assert!(first_evaluations <= HLP_CONCENTRATED_MAX_CANDIDATE_EVALUATIONS);
    assert!(first_authoritative_evaluations <= HLP_CONCENTRATED_MAX_AUTHORITATIVE_EVALUATIONS);
    assert_eq!(prepared.quote, replay_prepared.quote);
    assert_eq!(prepared.base_pre_rebalance, replay_prepared.base_pre_rebalance);
    assert_eq!(prepared.quote_pre_rebalance, replay_prepared.quote_pre_rebalance);
    assert_eq!(prepared.interest_eligibility, replay_prepared.interest_eligibility);
    assert_eq!(prepared.cash_policy, replay_prepared.cash_policy);
    assert_eq!(first.try_to_vec().unwrap(), replay.try_to_vec().unwrap());
    assert_concentrated_candidate_safe(
        &first,
        prepared.base_pre_rebalance,
        prepared.quote_pre_rebalance,
        prepared.quote,
    );

    let base_receipt = prepared.base_pre_rebalance;
    let target_leg = ylp_live_underlying_amount(&fixture, MarketAsset::Base, base_receipt.ylp_burn_amount).unwrap();
    let borrowed_leg = ylp_live_underlying_amount(&fixture, MarketAsset::Quote, base_receipt.ylp_burn_amount).unwrap();
    let interest = base_receipt.interest_paid;
    assert_eq!(base_receipt.ylp_burn_amount, 56_417_254_452);
    assert_eq!(target_leg, 79_924_443_807);
    assert_eq!(borrowed_leg, 159_848_887_614);
    assert_eq!(interest, 167_460_739_405);
    assert!(interest > borrowed_leg);

    let shortfall = interest - borrowed_leg;
    let target_debit =
        settled_close_target_amount(&fixture, MarketAsset::Base, target_leg, borrowed_leg, interest).unwrap();
    let exact_target_input = target_leg - target_debit;
    assert_eq!(shortfall, 7_611_851_791);
    assert_eq!(exact_target_input, 3_805_970_384);
    assert_eq!(target_debit, 76_118_473_423);

    let mut post_burn_reserves = fixture.curve_reserves_nad().unwrap();
    post_burn_reserves.base -= normalize_to_nad(target_leg as u128, fixture.base_side.asset_decimals).unwrap();
    post_burn_reserves.quote -= normalize_to_nad(borrowed_leg as u128, fixture.quote_side.asset_decimals).unwrap();
    assert_eq!(
        denormalize_from_nad_floor(post_burn_reserves.base, fixture.base_side.asset_decimals).unwrap(),
        1_620_075_556_193
    );
    assert_eq!(
        denormalize_from_nad_floor(post_burn_reserves.quote, fixture.quote_side.asset_decimals).unwrap(),
        3_240_151_112_386
    );
    let insufficient_curve = fixture
        .prepare_curve_for_reserves_nad(post_burn_reserves, fixture.current_curve_center_price_nad().unwrap(), 0)
        .unwrap();
    let insufficient_out = fixture
        .quote_curve_exact_in_for_prepared_nad(MarketAsset::Base, exact_target_input - 1, insufficient_curve, 0)
        .unwrap()
        .amount_out;
    let sufficient_curve = fixture
        .prepare_curve_for_reserves_nad(post_burn_reserves, fixture.current_curve_center_price_nad().unwrap(), 0)
        .unwrap();
    let sufficient_out = fixture
        .quote_curve_exact_in_for_prepared_nad(MarketAsset::Base, exact_target_input, sufficient_curve, 0)
        .unwrap()
        .amount_out;
    assert_eq!(insufficient_out, shortfall - 2);
    assert_eq!(sufficient_out, shortfall);
    assert!(insufficient_out < shortfall && sufficient_out >= shortfall);

    let post_base_raw = 1_620_075_556_193_u128;
    let post_quote_raw = 3_240_151_112_386_u128;
    let cpmm_denominator = post_quote_raw - u128::from(shortfall);
    let cpmm_input = (post_base_raw * u128::from(shortfall) + cpmm_denominator - 1) / cpmm_denominator;
    assert_eq!(cpmm_input, 3_814_887_935);
    assert_ne!(u128::from(exact_target_input), cpmm_input);
    assert_eq!(prepared.quote.amount_out, 679_953_442_637);

    let finalized = prepared
        .finalize_state(&mut first, 0, 0, crate::state::ProtocolAuctionSplit::default())
        .unwrap();
    let replay_finalized = replay_prepared
        .finalize_state(&mut replay, 0, 0, crate::state::ProtocolAuctionSplit::default())
        .unwrap();
    assert_eq!(finalized, replay_finalized);
    assert_eq!(first.try_to_vec().unwrap(), replay.try_to_vec().unwrap());
    let base_tracking_delta = final_hlp_combined_tracking_delta(
        &first,
        prepared.base_pre_rebalance,
        finalized.base_rebalance,
        finalized.quote_rebalance,
    );
    let quote_tracking_delta = final_hlp_combined_tracking_delta(
        &first,
        prepared.quote_pre_rebalance,
        finalized.base_rebalance,
        finalized.quote_rebalance,
    );
    assert_eq!((base_tracking_delta, quote_tracking_delta), (-5_756_704, 213_120));
    assert!(base_tracking_delta.unsigned_abs() <= prepared.base_pre_rebalance.tracking_loss_budget_nad);
    assert!(quote_tracking_delta.unsigned_abs() <= prepared.quote_pre_rebalance.tracking_loss_budget_nad);
    let final_reserves = first.curve_reserves_nad().unwrap();
    assert_eq!(final_reserves.base, 1_921_372_038_371_000);
    assert_eq!(final_reserves.quote, 2_456_704_225_285_000);
    first.checkpoint_amm_neutral_inventory(0).unwrap();
    replay.checkpoint_amm_neutral_inventory(0).unwrap();
    assert_eq!(first.try_to_vec().unwrap(), replay.try_to_vec().unwrap());
    assert_market_hlp_invariants(&first);
}

#[test]
fn concentrated_predictor_commits_and_finalizes_within_the_same_nav_budget() {
    let scale = 1_000_000_u64;
    for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = active_concentrated_hlp_market_with_decimals(6);
        let start_prices = current_hlp_curve_prices(&market).unwrap();
        let base_start = concentrated_hlp_start(&market, MarketAsset::Base, start_prices).unwrap();
        let quote_start = concentrated_hlp_start(&market, MarketAsset::Quote, start_prices).unwrap();
        let (base_pre, quote_pre, quote) = solve_concentrated_hlp_swap(&mut market, asset_in, 350_000 * scale).unwrap();
        let fee_eligible_supply = market
            .side(asset_in)
            .shares
            .ylp_supply
            .checked_sub(base_pre.ylp_mint_amount + quote_pre.ylp_mint_amount)
            .unwrap();

        market
            .swap_reserves_with_fee_supply(
                asset_in,
                quote.fee.reserve_input_credit,
                quote.amount_out,
                quote.fee.claimable_fee_debit,
                0,
                crate::state::ProtocolAuctionSplit::default(),
                Some(fee_eligible_supply),
            )
            .unwrap();
        assert_eq!(
            market.curve_reserves_nad().unwrap(),
            quote.reserve_endpoint().unwrap().reserves
        );
        market
            .finalize_amm_trade(quote.start_price_nad, quote.end_price_nad, 0)
            .unwrap();
        let (base_final, quote_final, _) = market
            .finalize_hlp_vaults_for_swap(base_pre, quote_pre, 0, Some(quote.reserve_end_price_nad))
            .unwrap();

        let final_prices = current_hlp_curve_prices(&market).unwrap();
        let base_end = concentrated_hlp_start(&market, MarketAsset::Base, final_prices).unwrap();
        let quote_end = concentrated_hlp_start(&market, MarketAsset::Quote, final_prices).unwrap();
        assert!(
            base_end.tracking.principal_nav_nad
                >= base_start
                    .tracking
                    .principal_nav_nad
                    .saturating_sub(base_start.tracking.loss_budget_nad as i128)
        );
        assert!(
            quote_end.tracking.principal_nav_nad
                >= quote_start
                    .tracking
                    .principal_nav_nad
                    .saturating_sub(quote_start.tracking.loss_budget_nad as i128)
        );
        assert_eq!(base_final.nav_nad, base_end.tracking.principal_nav_nad as u128);
        assert_eq!(quote_final.nav_nad, quote_end.tracking.principal_nav_nad as u128);
        assert_eq!(base_final.residual_exposure, market.base_hlp_vault.residual_exposure);
        assert_eq!(quote_final.residual_exposure, market.quote_hlp_vault.residual_exposure);
        assert_eq!(
            market.base_hlp_vault.last_nav_nad,
            base_end.tracking.principal_nav_nad as u128
        );
        assert_eq!(
            market.quote_hlp_vault.last_nav_nad,
            quote_end.tracking.principal_nav_nad as u128
        );
        assert_market_hlp_invariants(&market);
    }
}

#[test]
fn concentrated_tracking_guard_is_symmetric_and_bounded() {
    let start = ConcentratedHlpStart {
        active: true,
        tracking: HlpTrackingReference {
            principal_nav_nad: 100,
            loss_budget_nad: 1,
            ..HlpTrackingReference::default()
        },
        inventory_values: HlpInventoryValuesNad::default(),
        target_atom_nad: 1,
        economic_nav_nad: 100,
    };
    assert!(concentrated_hlp_candidate_is_safe(start, 1, 50));
    assert!(concentrated_hlp_candidate_is_safe(start, -1, 50));
    assert!(!concentrated_hlp_candidate_is_safe(start, 2, 49));
    assert!(!concentrated_hlp_candidate_is_safe(start, -2, 49));
    assert!(!concentrated_hlp_candidate_is_safe(start, 2, 50));
}

#[test]
fn concentrated_payoff_projection_handles_u64_reserve_cross_products_wider_than_u128() {
    let start_pool_value = normalize_to_nad(u64::MAX as u128, 0).unwrap();
    let end_pool_value = start_pool_value / 2;
    let opposite_start_price = u64::MAX as u128;
    let opposite_end_price = opposite_start_price / 4;
    assert!(end_pool_value.checked_mul(opposite_start_price).is_none());
    assert!(start_pool_value.checked_mul(opposite_end_price).is_none());

    let adjustment = concentrated_hlp_payoff_adjustment(
        -1_000_000_000,
        start_pool_value,
        end_pool_value,
        opposite_start_price,
        opposite_end_price,
    )
    .unwrap();

    assert_eq!(adjustment, 4_000_000_000);
}

#[test]
fn hlp_value_conversions_handle_products_wider_than_u128() {
    let mut market = Market::default();
    market.base_side.asset_decimals = 9;
    market.quote_side.asset_decimals = 9;
    let amount = u64::MAX;
    let price_nad = (u64::MAX as u128) * 2;
    let prices = HlpCurvePrices {
        base_in_quote_nad: price_nad,
        quote_in_base_nad: 1,
    };
    let amount_nad = normalize_to_nad(amount as u128, 9).unwrap();
    assert!(amount_nad.checked_mul(price_nad).is_none());

    let value =
        asset_value_in_target_nad_with_prices(&market, prices, MarketAsset::Base, amount, MarketAsset::Quote).unwrap();
    assert!(value.checked_mul(NAD as u128).is_none());
    let roundtrip =
        raw_amount_from_target_value_nad_with_prices(&market, prices, MarketAsset::Base, MarketAsset::Quote, value)
            .unwrap();
    assert!(roundtrip <= amount && amount - roundtrip <= 1);
}

#[test]
fn hlp_deleverage_cash_cap_executes_the_largest_feasible_partial_burn() {
    let scale = 1_000_000_u64;
    let mut market = seeded_market();
    market.base_side.asset_decimals = 6;
    market.quote_side.asset_decimals = 6;
    configure_market_depth(&mut market, 1_000_000 * scale, 20_000);
    market
        .deposit_single_sided(MarketAsset::Base, 100_000 * scale, 1)
        .unwrap();
    market.debt.quote_borrow_index_nad = 2 * NAD as u128;
    assert_market_hlp_invariants(&market);

    let valuation = current_hlp_valuation(&market, MarketAsset::Base).unwrap();
    let desired_burn = market.base_hlp_vault.ylp_shares / 2;
    let full_interest = hlp_deleverage_interest_for_burn(&market, MarketAsset::Base, desired_burn, valuation).unwrap();
    assert!(full_interest > 1);
    reset_hlp_deleverage_cap_probe_counts();
    let uncapped = cap_hlp_deleverage_ylp_burn(
        &market,
        MarketAsset::Base,
        desired_burn,
        valuation,
        SwapCashFloors::default(),
    )
    .unwrap();
    assert_eq!(uncapped.ylp_burn_amount, desired_burn);
    assert_eq!(
        hlp_deleverage_cap_probe_counts(),
        HlpDeleverageCapProbeCounts {
            full_capacity: 1,
            cheap_repayment: 0,
            legacy_capacity: 1,
        }
    );

    let available_interest_cash = full_interest / 2;
    let cash_floor = market.quote_side.reserves.cash_reserve - available_interest_cash;
    let mut cash_floors = SwapCashFloors::default();
    cash_floors.set(MarketAsset::Quote, cash_floor);
    reset_hlp_deleverage_cap_probe_counts();
    let capped = cap_hlp_deleverage_ylp_burn(&market, MarketAsset::Base, desired_burn, valuation, cash_floors)
        .unwrap()
        .ylp_burn_amount;
    let counts = hlp_deleverage_cap_probe_counts();
    assert_eq!(counts.full_capacity, 3);
    assert!(counts.cheap_repayment <= 64);
    assert!((1..=65).contains(&counts.legacy_capacity));
    assert!(counts.full_capacity < counts.legacy_capacity);

    assert!(capped > 0 && capped < desired_burn);
    assert!(
        hlp_deleverage_interest_for_burn(&market, MarketAsset::Base, capped, valuation).unwrap()
            <= available_interest_cash
    );
    assert!(
        hlp_deleverage_interest_for_burn(&market, MarketAsset::Base, capped + 1, valuation).unwrap()
            > available_interest_cash
    );

    assert!(valuation.ideal_delta < 0);
    let receipt = deleverage_proportional_with_cash_floor(
        &mut market,
        MarketAsset::Base,
        valuation.ideal_delta,
        valuation,
        Some((MarketAsset::Quote, cash_floor)),
    )
    .unwrap();
    assert!(receipt.ylp_burn_amount > 0);
    let post = current_hlp_valuation(&market, MarketAsset::Base).unwrap();
    assert!(post.ideal_delta.unsigned_abs() < valuation.ideal_delta.unsigned_abs());
    assert_market_hlp_invariants(&market);
}

#[test]
fn hlp_deleverage_direct_entitlement_cap_matches_every_small_floor_phase() {
    for supply in 1_u64..=32 {
        for live_reserve in 0_u64..=32 {
            for leg_cap in 0_u64..=32 {
                let expected = (0..=supply)
                    .filter(|burn| ((*burn as u128) * live_reserve as u128 / supply as u128) <= leg_cap as u128)
                    .max()
                    .unwrap();
                assert_eq!(
                    maximum_ylp_burn_for_leg_cap(live_reserve, supply, leg_cap).unwrap(),
                    expected,
                    "supply={supply} live={live_reserve} cap={leg_cap}"
                );
            }
        }
    }
}

#[test]
fn hlp_deleverage_interest_repay_phase_cap_matches_every_small_raw_input() {
    for shares in 1_u128..=16 {
        for index in [
            NAD as u128,
            NAD as u128 + 1,
            3 * NAD as u128 / 2,
            2 * NAD as u128,
            3 * NAD as u128,
        ] {
            let debt = Debt::shares_to_debt(shares, index).unwrap();
            let minimum = debt - Debt::shares_to_debt(shares - 1, index).unwrap();
            let debt_cap = u64::try_from(debt).unwrap();
            for principal in [0_u64, u64::try_from(debt / 2).unwrap(), u64::try_from(debt).unwrap()] {
                let vault = HlpVault {
                    debt_shares: shares,
                    debt_principal: principal,
                    ..HlpVault::default()
                };
                for cash in 0..=debt_cap {
                    let mut expected = None;
                    for repay_input in 1..=debt_cap {
                        let Ok(repayment) = vault.repayment_for_max(repay_input, index) else {
                            continue;
                        };
                        let (_, interest) = crate::math::realized_interest_split(
                            repayment.position_debt_reduced,
                            debt,
                            u128::from(principal).min(debt),
                        )
                        .unwrap();
                        if interest <= cash {
                            expected = Some(repay_input);
                        }
                    }
                    assert_eq!(
                        maximum_hlp_interest_safe_repay_input(&vault, index, debt, minimum, cash).unwrap(),
                        expected,
                        "shares={shares} index={index} principal={principal} cash={cash}"
                    );
                }
            }
        }
    }
}

#[test]
fn hlp_deleverage_cash_cap_crosses_indexed_share_granularity_before_finding_the_maximum() {
    let mut market = seeded_market();
    market.base_side.reserves.live_reserve = 1_000;
    market.quote_side.reserves.live_reserve = 1_000;
    market.base_side.reserves.cash_reserve = 1_000;
    market.quote_side.reserves.cash_reserve = 1_000;
    market.base_side.shares.ylp_supply = 1_000_000;
    market.quote_side.shares.ylp_supply = 1_000_000;
    market.base_hlp_vault.ylp_shares = 100_000;
    market.base_hlp_vault.debt_shares = 100;
    market.base_hlp_vault.debt_principal = 100;
    market.debt.quote_borrow_index_nad = 2 * NAD as u128;
    let valuation = HlpValuation {
        prices: HlpCurvePrices {
            base_in_quote_nad: NAD as u128,
            quote_in_base_nad: NAD as u128,
        },
        ..HlpValuation::default()
    };
    let desired_burn = 10_000;
    let cash_probe_burn = 3_000;
    let target_cash_available = ylp_live_underlying_amount(&market, MarketAsset::Base, cash_probe_burn).unwrap();
    let borrowed_cash_available = ylp_live_underlying_amount(&market, MarketAsset::Quote, cash_probe_burn).unwrap()
        + hlp_deleverage_interest_for_burn(&market, MarketAsset::Base, cash_probe_burn, valuation).unwrap();
    let mut cash_floors = SwapCashFloors::default();
    cash_floors.set(
        MarketAsset::Base,
        market.base_side.reserves.cash_reserve - target_cash_available,
    );
    cash_floors.set(
        MarketAsset::Quote,
        market.quote_side.reserves.cash_reserve - borrowed_cash_available,
    );

    let first_executable = (1..=desired_burn)
        .find(|burn| {
            hlp_deleverage_interest_if_executable(&market, MarketAsset::Base, *burn, valuation)
                .unwrap()
                .is_some()
        })
        .unwrap();
    let expected = (first_executable..=desired_burn)
        .filter(|burn| {
            let interest = hlp_deleverage_interest_if_executable(&market, MarketAsset::Base, *burn, valuation)
                .unwrap()
                .unwrap();
            ylp_live_underlying_amount(&market, MarketAsset::Base, *burn).unwrap() <= target_cash_available
                && ylp_live_underlying_amount(&market, MarketAsset::Quote, *burn).unwrap() + interest
                    <= borrowed_cash_available
        })
        .max()
        .unwrap();
    let capped = cap_hlp_deleverage_ylp_burn(&market, MarketAsset::Base, desired_burn, valuation, cash_floors)
        .unwrap()
        .ylp_burn_amount;

    assert!(first_executable > 1);
    assert!(expected > 0 && expected < desired_burn);
    assert_eq!(capped, expected);
}

#[test]
fn hlp_deleverage_cash_cap_crosses_borrowed_leg_interest_gap_before_maximum() {
    let mut market = seeded_market();
    market.base_side.reserves.live_reserve = 31;
    market.base_side.reserves.cash_reserve = 8;
    market.quote_side.reserves.live_reserve = 24;
    market.quote_side.reserves.cash_reserve = 8;
    market.base_side.shares.ylp_supply = 117;
    market.quote_side.shares.ylp_supply = 117;
    market.base_hlp_vault.ylp_shares = 116;
    market.base_hlp_vault.hlp_supply = 116;
    market.base_hlp_vault.base_hlp_live_reserve = 23;
    market.base_hlp_vault.quote_hlp_live_reserve = 16;
    market.base_hlp_vault.debt_shares = 10;
    market.base_hlp_vault.debt_principal = 10;
    market.debt.quote_borrow_index_nad = 3 * NAD as u128;
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();

    let valuation = HlpValuation {
        prices: HlpCurvePrices {
            base_in_quote_nad: NAD as u128,
            quote_in_base_nad: NAD as u128,
        },
        ..HlpValuation::default()
    };
    let desired_burn = 33;
    let mut cash_floors = SwapCashFloors::default();
    cash_floors.set(MarketAsset::Quote, 6);
    let target_cash_available = 8;
    let borrowed_cash_available = 2;
    let expected = (1..=desired_burn)
        .filter(|burn| {
            let Some(interest) =
                hlp_deleverage_interest_if_executable(&market, MarketAsset::Base, *burn, valuation).unwrap()
            else {
                return false;
            };
            let target_leg = ylp_live_underlying_amount(&market, MarketAsset::Base, *burn).unwrap();
            let borrowed_leg = ylp_live_underlying_amount(&market, MarketAsset::Quote, *burn).unwrap();
            interest <= borrowed_leg
                && target_leg.saturating_sub(market.base_hlp_vault.base_hlp_live_reserve) <= target_cash_available
                && borrowed_leg
                    .saturating_sub(market.base_hlp_vault.quote_hlp_live_reserve)
                    .max(interest)
                    <= borrowed_cash_available
        })
        .max()
        .unwrap();
    let capped = cap_hlp_deleverage_ylp_burn(&market, MarketAsset::Base, desired_burn, valuation, cash_floors)
        .unwrap()
        .ylp_burn_amount;

    assert_eq!(expected, 14);
    assert_eq!(capped, expected);
    for burn in 8..=9 {
        let interest = hlp_deleverage_interest_for_burn(&market, MarketAsset::Base, burn, valuation).unwrap();
        let borrowed_leg = ylp_live_underlying_amount(&market, MarketAsset::Quote, burn).unwrap();
        assert!(borrowed_leg < interest);
    }
}

#[test]
fn deleverage_interest_shortfall_settles_exactly_from_target_leg_both_directions() {
    for target_asset in [MarketAsset::Base, MarketAsset::Quote] {
        let borrowed_asset = target_asset.opposite();
        let mut market = seeded_market();
        market.base_side.shares.ylp_supply = 117;
        market.quote_side.shares.ylp_supply = 117;
        market.side_mut(target_asset).reserves.live_reserve = 31;
        market.side_mut(target_asset).reserves.cash_reserve = 8;
        market.side_mut(borrowed_asset).reserves.live_reserve = 24;
        market.side_mut(borrowed_asset).reserves.cash_reserve = 8;
        let vault = match target_asset {
            MarketAsset::Base => &mut market.base_hlp_vault,
            MarketAsset::Quote => &mut market.quote_hlp_vault,
        };
        vault.ylp_shares = 116;
        vault.hlp_supply = 116;
        vault.credit_hlp_live_reserve(target_asset, 23).unwrap();
        vault.credit_hlp_live_reserve(borrowed_asset, 16).unwrap();
        vault.debt_shares = 10;
        vault.debt_principal = 10;
        match borrowed_asset {
            MarketAsset::Base => market.debt.base_borrow_index_nad = 3 * NAD as u128,
            MarketAsset::Quote => market.debt.quote_borrow_index_nad = 3 * NAD as u128,
        }
        market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
        market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();

        let prices = HlpCurvePrices {
            base_in_quote_nad: NAD as u128,
            quote_in_base_nad: NAD as u128,
        };
        let valuation = HlpValuation {
            prices,
            ..HlpValuation::default()
        };
        let ylp_burn = 8;
        let target_leg = ylp_live_underlying_amount(&market, target_asset, ylp_burn).unwrap();
        let borrowed_leg = ylp_live_underlying_amount(&market, borrowed_asset, ylp_burn).unwrap();
        let interest = hlp_deleverage_interest_for_burn(&market, target_asset, ylp_burn, valuation).unwrap();
        assert_eq!((target_leg, borrowed_leg, interest), (2, 1, 2));
        let nav_before = hlp_nav_nad_with_prices(&market, target_asset, prices).unwrap();
        let target_live_before = market.side(target_asset).reserves.live_reserve;
        let target_cash_before = market.side(target_asset).reserves.cash_reserve;
        let target_backing_before = market.side(target_asset).reserves.hlp_backing_inventory(target_asset);
        let borrowed_live_before = market.side(borrowed_asset).reserves.live_reserve;
        let borrowed_cash_and_backing_before = market
            .side(borrowed_asset)
            .reserves
            .cash_reserve
            .checked_add(
                market
                    .side(borrowed_asset)
                    .reserves
                    .total_hlp_backing_inventory()
                    .unwrap(),
            )
            .unwrap();
        let borrowed_hlp_live_before = match target_asset {
            MarketAsset::Base => market.base_hlp_vault.hlp_live_reserve(borrowed_asset),
            MarketAsset::Quote => market.quote_hlp_vault.hlp_live_reserve(borrowed_asset),
        };
        let k_before = u128::from(market.curve_reserve(MarketAsset::Base).unwrap())
            * u128::from(market.curve_reserve(MarketAsset::Quote).unwrap());

        let (target_debit, borrowed_debit) =
            debit_hlp_deleverage_reserve_legs(&mut market, target_asset, target_leg, borrowed_leg, interest).unwrap();
        assert_eq!((target_debit, borrowed_debit), (0, interest));
        assert_eq!(target_leg - target_debit, 2);

        market.base_side.shares.burn(ylp_burn).unwrap();
        market.quote_side.shares.burn(ylp_burn).unwrap();
        let borrow_index = market.debt.borrow_index(borrowed_asset);
        let repayment = match target_asset {
            MarketAsset::Base => market.base_hlp_vault.repayment_for_max(3, borrow_index),
            MarketAsset::Quote => market.quote_hlp_vault.repayment_for_max(3, borrow_index),
        }
        .unwrap();
        let clearance = match target_asset {
            MarketAsset::Base => {
                market.base_hlp_vault.debit_ylp(ylp_burn).unwrap();
                market
                    .base_hlp_vault
                    .clear_debt_repay(repayment.shares_to_burn, borrow_index)
            }
            MarketAsset::Quote => {
                market.quote_hlp_vault.debit_ylp(ylp_burn).unwrap();
                market
                    .quote_hlp_vault
                    .clear_debt_repay(repayment.shares_to_burn, borrow_index)
            }
        }
        .unwrap();

        assert_eq!(repayment.shares_to_burn, 1);
        assert_eq!(clearance.debt_reduced, 3);
        assert_eq!(clearance.principal_paid, 1);
        assert_eq!(clearance.interest_paid, interest);
        assert_eq!(market.base_side.shares.ylp_supply, 109);
        assert_eq!(market.quote_side.shares.ylp_supply, 109);
        let vault = match target_asset {
            MarketAsset::Base => &market.base_hlp_vault,
            MarketAsset::Quote => &market.quote_hlp_vault,
        };
        assert_eq!(vault.ylp_shares, 108);
        assert_eq!(vault.debt_shares, 9);
        assert_eq!(vault.debt_principal, 9);
        assert_eq!(
            target_live_before - market.side(target_asset).reserves.live_reserve,
            target_debit
        );
        assert_eq!(
            borrowed_live_before - market.side(borrowed_asset).reserves.live_reserve,
            interest
        );
        assert_eq!(
            target_cash_before - market.side(target_asset).reserves.cash_reserve,
            market.side(target_asset).reserves.hlp_backing_inventory(target_asset) - target_backing_before
        );
        assert_eq!(vault.hlp_live_reserve(borrowed_asset), borrowed_hlp_live_before);
        let borrowed_cash_and_backing_after = market
            .side(borrowed_asset)
            .reserves
            .cash_reserve
            .checked_add(
                market
                    .side(borrowed_asset)
                    .reserves
                    .total_hlp_backing_inventory()
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(
            borrowed_cash_and_backing_before - borrowed_cash_and_backing_after,
            interest
        );
        market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
        market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();

        let k_after = u128::from(market.curve_reserve(MarketAsset::Base).unwrap())
            * u128::from(market.curve_reserve(MarketAsset::Quote).unwrap());
        assert!(
            k_after * 117_u128.pow(2) >= k_before * 109_u128.pow(2),
            "target={target_asset:?}: curve Q per share fell"
        );
        assert_eq!(nav_before, 23 * NAD as u128);
        assert_eq!(
            hlp_nav_nad_with_prices(&market, target_asset, prices).unwrap(),
            24 * NAD as u128
        );
    }
}

#[test]
fn deleverage_interest_shortfall_rejects_atomically_when_exact_out_input_exceeds_target_leg() {
    let mut market = seeded_market();
    market.quote_side.reserves.live_reserve = 1_000;
    market.quote_side.reserves.cash_reserve = 1_000;
    market.quote_side.shares.ylp_supply = 1_000;
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();

    // After redeeming A=B=100 the CPMM curve is 900/900. Buying the
    // 95-atom borrowed shortfall requires ceil(900*95/(900-95)) = 107
    // target atoms, which is larger than the entire A=100 target leg.
    for target_asset in [MarketAsset::Base, MarketAsset::Quote] {
        let mut candidate = market.clone();
        let before = candidate.try_to_vec().unwrap();
        let error = debit_hlp_deleverage_reserve_legs(&mut candidate, target_asset, 100, 100, 195).unwrap_err();

        assert_eq!(error, error!(ErrorCode::HlpSettlementUnavailable));
        assert_eq!(candidate.try_to_vec().unwrap(), before);
    }
}

#[test]
fn deleverage_interest_shortfall_mixed_cash_and_synthetic_matches_numeric_oracle() {
    let mut market = seeded_market();
    market.base_side.reserves.live_reserve = 1_000;
    market.base_side.reserves.cash_reserve = 970;
    market.quote_side.reserves.live_reserve = 1_000;
    market.quote_side.reserves.cash_reserve = 900;
    market.base_side.shares.ylp_supply = 1_000;
    market.quote_side.shares.ylp_supply = 1_000;
    market.base_hlp_vault.ylp_shares = 500;
    market.base_hlp_vault.hlp_supply = 500;
    market.base_hlp_vault.base_hlp_live_reserve = 30;
    market.base_hlp_vault.quote_hlp_live_reserve = 100;
    market.base_hlp_vault.debt_shares = 100;
    market.base_hlp_vault.debt_principal = 100;
    market.debt.quote_borrow_index_nad = 4 * NAD as u128;
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();

    let target_live_before = market.base_side.reserves.live_reserve;
    let target_cash_before = market.base_side.reserves.cash_reserve;
    let borrowed_live_before = market.quote_side.reserves.live_reserve;
    let borrowed_cash_before = market.quote_side.reserves.cash_reserve;
    let target_backing_before = market.base_side.reserves.hlp_backing_inventory(MarketAsset::Base);
    let borrowed_backing_before = market.quote_side.reserves.hlp_backing_inventory(MarketAsset::Base);

    // On the post-burn 900/900 CPMM curve the 50-atom shortfall needs
    // X=ceil(900*50/(900-50))=53 target atoms. Therefore the actual
    // reserve debits are target A-X=47 and borrowed I=150.
    let (target_debit, borrowed_debit) =
        debit_hlp_deleverage_reserve_legs(&mut market, MarketAsset::Base, 100, 100, 150).unwrap();
    assert_eq!((target_debit, borrowed_debit), (47, 150));
    assert_eq!(100 - target_debit, 53);

    market.base_side.shares.burn(100).unwrap();
    market.quote_side.shares.burn(100).unwrap();
    market.base_hlp_vault.debit_ylp(100).unwrap();
    let clearance = market
        .base_hlp_vault
        .clear_debt_repay(50, market.debt.quote_borrow_index_nad)
        .unwrap();

    assert_eq!(clearance.debt_reduced, 200);
    assert_eq!(clearance.principal_paid, 50);
    assert_eq!(clearance.interest_paid, 150);
    assert_eq!(target_live_before - market.base_side.reserves.live_reserve, 47);
    assert_eq!(target_cash_before - market.base_side.reserves.cash_reserve, 17);
    assert_eq!(market.base_hlp_vault.base_hlp_live_reserve, 0);
    assert_eq!(
        market.base_side.reserves.hlp_backing_inventory(MarketAsset::Base) - target_backing_before,
        17
    );
    assert_eq!(borrowed_live_before - market.quote_side.reserves.live_reserve, 150);
    assert_eq!(borrowed_cash_before - market.quote_side.reserves.cash_reserve, 150);
    assert_eq!(market.base_hlp_vault.quote_hlp_live_reserve, 100);
    assert_eq!(
        market.quote_side.reserves.hlp_backing_inventory(MarketAsset::Base),
        borrowed_backing_before
    );
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn new_hlp_deposit_supports_the_applied_concentrated_curve() {
    let mut market = seeded_market();
    enable_concentrated_curve(&mut market);
    assert_eq!(
        market.config.amm.curve_parameters(),
        market.amm.applied_curve_parameters
    );

    let base = market.deposit_single_sided(MarketAsset::Base, 100, 1).unwrap();
    let quote = market.deposit_single_sided(MarketAsset::Quote, 100, 1).unwrap();

    assert!(base.hlp_amount > 0 && quote.hlp_amount > 0);
    assert_market_hlp_invariants(&market);
}

#[test]
fn concentrated_legacy_hlp_can_still_withdraw() {
    let mut market = active_concentrated_hlp_market();
    let supply = market.base_hlp_vault.hlp_supply;

    let receipt = market.withdraw_single_sided(MarketAsset::Base, supply).unwrap();

    assert_eq!(receipt.hlp_supply, 0);
    assert_eq!(market.base_hlp_vault.hlp_supply, 0);
    assert!(market.quote_hlp_vault.hlp_supply > 0);
}

#[test]
fn concentrated_hlp_guard_rejects_without_mutating_state() {
    let mut market = active_concentrated_hlp_market();
    let reference = u64::try_from(market.base_hlp_vault.cached_settlement_price_nad).unwrap();
    let stale_trade = market.quote_curve_exact_in(MarketAsset::Base, 150_000, 0).unwrap();
    market
        .swap_reserves(
            MarketAsset::Base,
            150_000,
            stale_trade.amount_out,
            0,
            0,
            crate::state::ProtocolAuctionSplit::default(),
        )
        .unwrap();
    market.checkpoint_amm_neutral_inventory(0).unwrap();
    checkpoint_hlp_vaults(&mut market).unwrap();
    assert_ne!(market.base_hlp_vault.residual_exposure, 0);
    let start = market.curve_marginal_price_nad(0).unwrap();
    let end = if start < reference {
        start / 2
    } else {
        start.checked_mul(2).unwrap()
    };
    let base_before = market.base_hlp_vault;
    let quote_before = market.quote_hlp_vault;

    let read_only_error = require_hlp_swap_path_safe(&market, start, end, true, false).unwrap_err();
    assert_eq!(read_only_error, error!(ErrorCode::HlpSettlementUnavailable));
    assert_eq!(market.base_hlp_vault.hlp_supply, base_before.hlp_supply);
    assert_eq!(market.base_hlp_vault.residual_exposure, base_before.residual_exposure);
    assert_eq!(market.base_hlp_vault.last_nav_nad, base_before.last_nav_nad);
    assert_eq!(market.quote_hlp_vault.hlp_supply, quote_before.hlp_supply);
    assert_eq!(market.quote_hlp_vault.residual_exposure, quote_before.residual_exposure);
    assert_eq!(market.quote_hlp_vault.last_nav_nad, quote_before.last_nav_nad);
}

#[test]
fn cpmm_hlp_path_keeps_inside_and_restoring_trades_live_but_rejects_worsening_flow() {
    let mut market = seeded_market();
    configure_market_depth(&mut market, 1_000_000, 20_000);
    market.deposit_single_sided(MarketAsset::Base, 100_000, 1).unwrap();
    market.config.settlement_divergence_bps = 500;
    let reference = u64::try_from(market.base_hlp_vault.cached_settlement_price_nad).unwrap();

    // A fully settled vault may accept a first large move because the inline
    // controller will recompute and apply the maximum-safe correction from the
    // actual post-trade state.
    require_hlp_swap_path_safe(&market, reference, reference * 2, false, false).unwrap();

    // Create an actual post-trade remainder. Once it exists, the old settlement reference
    // admits inside-band and restoring flow but blocks further deterioration.
    let stale_trade = market.quote_curve_exact_in(MarketAsset::Base, 250_000, 0).unwrap();
    market
        .swap_reserves(
            MarketAsset::Base,
            250_000,
            stale_trade.amount_out,
            0,
            0,
            crate::state::ProtocolAuctionSplit::default(),
        )
        .unwrap();
    checkpoint_hlp_vaults(&mut market).unwrap();
    assert_ne!(market.base_hlp_vault.residual_exposure, 0);
    require_hlp_swap_path_safe(&market, reference, reference * 104 / 100, true, false).unwrap();
    require_hlp_swap_path_safe(&market, reference * 110 / 100, reference * 106 / 100, true, false).unwrap();
    assert_eq!(
        require_hlp_swap_path_safe(&market, reference * 106 / 100, reference * 110 / 100, true, false,).unwrap_err(),
        error!(ErrorCode::HlpSettlementUnavailable)
    );
}

#[test]
fn concentrated_hlp_checkpoint_records_exposure_without_moving_inventory() {
    let mut market = active_concentrated_hlp_market();
    // First create actual off-center inventory without settling the hLP.
    // The supplied start is farther from the last settlement reference
    // than that executable endpoint, so the later admission is restorative.
    let swap = market.quote_curve_exact_in(MarketAsset::Base, 150_000, 0).unwrap();
    market
        .swap_reserves(
            MarketAsset::Base,
            150_000,
            swap.amount_out,
            0,
            0,
            crate::state::ProtocolAuctionSplit::default(),
        )
        .unwrap();
    let base_live_before = market.base_side.reserves.live_reserve;
    let base_cash_before = market.base_side.reserves.cash_reserve;
    let quote_live_before = market.quote_side.reserves.live_reserve;
    let quote_cash_before = market.quote_side.reserves.cash_reserve;
    let base_ylp_before = market.base_hlp_vault.ylp_shares;
    let quote_ylp_before = market.quote_hlp_vault.ylp_shares;

    let (base_delta, quote_delta) = checkpoint_hlp_vaults(&mut market).unwrap();

    assert_eq!(market.base_side.reserves.live_reserve, base_live_before);
    assert_eq!(market.base_side.reserves.cash_reserve, base_cash_before);
    assert_eq!(market.quote_side.reserves.live_reserve, quote_live_before);
    assert_eq!(market.quote_side.reserves.cash_reserve, quote_cash_before);
    assert_eq!(market.base_hlp_vault.ylp_shares, base_ylp_before);
    assert_eq!(market.quote_hlp_vault.ylp_shares, quote_ylp_before);
    assert_eq!(base_delta, market.base_hlp_vault.residual_exposure);
    assert_eq!(quote_delta, market.quote_hlp_vault.residual_exposure);
    assert!(base_delta != 0 || quote_delta != 0);
}

fn funded_due_ramp_with_residual_base_hlp() -> (Market, u64) {
    let mut market = seeded_market();
    configure_market_depth(&mut market, 1_000_000, 20_000);
    market.deposit_single_sided(MarketAsset::Base, 100_000, 1).unwrap();
    enable_concentrated_curve(&mut market);

    let trade = market.quote_curve_exact_in(MarketAsset::Base, 250_000, 0).unwrap();
    market.base_side.credit_reserve(trade.amount_in, true).unwrap();
    market.quote_side.debit_reserve(trade.amount_out, true).unwrap();
    market.checkpoint_amm_neutral_inventory(0).unwrap();

    // Fund the parameter move with retained principal, then checkpoint the
    // incumbent hLP at the still-applied pre-ramp curve.
    let base_retained = market.base_side.reserves.live_reserve / 100;
    let quote_retained = market.quote_side.reserves.live_reserve / 100;
    market.base_side.credit_reserve(base_retained, true).unwrap();
    market.quote_side.credit_reserve(quote_retained, true).unwrap();
    market.checkpoint_amm_retained_surcharge(0).unwrap();
    checkpoint_hlp_vaults(&mut market).unwrap();

    let values = current_hlp_inventory_values_nad(&market, MarketAsset::Base).unwrap();
    assert_ne!(hlp_opposite_exposure_nad(values).unwrap(), 0);
    assert!(market.amm.spendable_protected_profit_nad() > 0);

    let applied = market.amm.applied_curve_parameters;
    let mut target = market.config.amm;
    target.peak_depth_nad = 220 * NAD;
    target.fade_scale_nad = 11 * NAD / 100;
    market.amm.start_concentration_ramp(applied, &target, 0).unwrap();
    market.config.amm = target;
    let due_slot = market.amm.concentration_ramp.end_slot;
    market.debt.base_last_accrual_slot = due_slot;
    market.debt.quote_last_accrual_slot = due_slot;
    (market, due_slot)
}

fn apply_test_composite_swap(
    market: &mut Market,
    asset_in: MarketAsset,
    reserve_credit: u64,
) -> TestCompositeSwapReceipt {
    let current_slot = curve_slot(market);
    let prepared = crate::instructions::SwapRequest {
        current_slot,
        asset_in,
        reserve_credit,
    }
    .prepare(market)
    .unwrap();
    let amount_out = prepared.quote.amount_out;
    let base_pre_rebalance = prepared.base_pre_rebalance;
    let quote_pre_rebalance = prepared.quote_pre_rebalance;
    let finalized = prepared
        .finalize_state(market, current_slot, 0, crate::state::ProtocolAuctionSplit::default())
        .unwrap();
    let base_rebalance = finalized.base_rebalance;
    let quote_rebalance = finalized.quote_rebalance;
    assert_market_hlp_invariants(market);
    TestCompositeSwapReceipt {
        amount_out,
        base_pre_rebalance,
        quote_pre_rebalance,
        base_rebalance,
        quote_rebalance,
    }
}

fn assert_no_hlp_residuals(market: &Market) {
    assert_eq!(market.base_hlp_vault.hlp_supply, 0);
    assert_eq!(market.base_hlp_vault.ylp_shares, 0);
    assert_eq!(market.base_hlp_vault.debt_shares, 0);
    assert_eq!(market.base_hlp_vault.debt_principal, 0);
    assert_eq!(market.base_hlp_vault.residual_exposure, 0);
    assert_eq!(market.base_hlp_vault.base_hlp_live_reserve, 0);
    assert_eq!(market.base_hlp_vault.quote_hlp_live_reserve, 0);
    assert_eq!(market.quote_hlp_vault.hlp_supply, 0);
    assert_eq!(market.quote_hlp_vault.ylp_shares, 0);
    assert_eq!(market.quote_hlp_vault.debt_shares, 0);
    assert_eq!(market.quote_hlp_vault.debt_principal, 0);
    assert_eq!(market.quote_hlp_vault.residual_exposure, 0);
    assert_eq!(market.quote_hlp_vault.base_hlp_live_reserve, 0);
    assert_eq!(market.quote_hlp_vault.quote_hlp_live_reserve, 0);
    assert_eq!(market.base_side.reserves.total_hlp_backing_inventory().unwrap(), 0);
    assert_eq!(market.quote_side.reserves.total_hlp_backing_inventory().unwrap(), 0);
    assert_market_hlp_invariants(market);
}

fn configure_market_depth(market: &mut Market, base_reserve: u64, price_bps: u64) {
    let quote_reserve = (base_reserve as u128)
        .checked_mul(price_bps as u128)
        .unwrap()
        .checked_div(BPS_DENOMINATOR as u128)
        .unwrap() as u64;
    market.base_side.reserves.live_reserve = base_reserve;
    market.base_side.reserves.cash_reserve = base_reserve;
    market.base_side.shares.ylp_supply = base_reserve;
    market.quote_side.reserves.live_reserve = quote_reserve;
    market.quote_side.reserves.cash_reserve = quote_reserve;
    market.quote_side.shares.ylp_supply = base_reserve;
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn hlp_rebalance_preserves_virtual_invariant_under_price_and_cash_sweeps(
        target_is_base in any::<bool>(),
        base_reserve in 500_000u64..5_000_000,
        price_bps in 5_000u64..30_000,
        deposit_bps in 100u64..2_000,
        move_bps in 6_500u64..15_000,
        borrowed_cash_bps in 0u64..=10_000,
    ) {
        let target_asset = if target_is_base {
            MarketAsset::Base
        } else {
            MarketAsset::Quote
        };
        let borrowed_asset = target_asset.opposite();
        let mut market = seeded_market();
        configure_market_depth(&mut market, base_reserve, price_bps);
        assert_market_hlp_invariants(&market);

        let target_reserve = market.side(target_asset).reserves.live_reserve;
        let deposit_amount = target_reserve
            .checked_mul(deposit_bps)
            .unwrap()
            .checked_div(BPS_DENOMINATOR as u64)
            .unwrap()
            .max(1);
        market.deposit_single_sided(target_asset, deposit_amount, 1)
            .unwrap();
        assert_market_hlp_invariants(&market);

        let moved_live = market
            .side(borrowed_asset)
            .reserves
            .live_reserve
            .checked_mul(move_bps)
            .unwrap()
            .checked_div(BPS_DENOMINATOR as u64)
            .unwrap();
        set_side_live_preserving_hlp_invariant(&mut market, borrowed_asset, moved_live);
        constrain_side_cash_preserving_hlp_invariant(
            &mut market,
            borrowed_asset,
            borrowed_cash_bps,
        );
        assert_market_hlp_invariants(&market);

        let price_before =
            market_spot_price_nad(&market.base_side, &market.quote_side).unwrap();
        let (base_receipt, quote_receipt) = rebalance_hlp_vaults(&mut market).unwrap();
        let price_after =
            market_spot_price_nad(&market.base_side, &market.quote_side).unwrap();

        assert_market_hlp_invariants(&market);
        prop_assert_eq!(
            base_receipt.residual_exposure,
            base_receipt.ideal_delta - base_receipt.executed_delta
        );
        prop_assert_eq!(
            quote_receipt.residual_exposure,
            quote_receipt.ideal_delta - quote_receipt.executed_delta
        );
        let target_receipt = if target_asset == MarketAsset::Base {
            base_receipt
        } else {
            quote_receipt
        };
        let post_ideal = current_hlp_ideal_delta(&market, target_asset).unwrap();
        let post_nav = hlp_nav_nad(&market, target_asset).unwrap();
        prop_assert_eq!(
            target_receipt.residual_exposure,
            recognized_hlp_residual_exposure(post_ideal, post_nav)
        );
        prop_assert!(
            price_diff_bps(price_before, price_after) <= 2,
            "hLP rebalance moved spot by more than 2 bps: before {}, after {}, base receipt {:?}, quote receipt {:?}",
            price_before,
            price_after,
            base_receipt,
            quote_receipt
        );
    }
}

#[test]
fn rebalance_hlp_leverages_up_with_balanced_ylp() {
    let mut market = seeded_market();
    market.deposit_single_sided(MarketAsset::Base, 100, 1).unwrap();
    market.quote_side.reserves.live_reserve = 2_400;
    market.quote_side.reserves.cash_reserve = 2_200;
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
    let ylp_before = market.base_hlp_vault.ylp_shares;
    let debt_before = market.base_hlp_vault.debt_shares;
    let principal_before = market.base_hlp_vault.debt_principal;

    let (base_receipt, _) = rebalance_hlp_vaults(&mut market).unwrap();

    assert!(base_receipt.ideal_delta > 0);
    assert!(base_receipt.executed_delta > 0);
    assert!(base_receipt.ylp_mint_amount > 0);
    assert_eq!(base_receipt.ylp_burn_amount, 0);
    assert!(market.base_hlp_vault.ylp_shares > ylp_before);
    assert!(market.base_hlp_vault.debt_shares > debt_before);
    assert!(market.base_hlp_vault.debt_principal > principal_before);
    assert!(market.base_hlp_vault.base_hlp_live_reserve > 0);
    assert!(market.base_hlp_vault.quote_hlp_live_reserve > 200);
    assert_eq!(market.base_hlp_vault.residual_exposure, base_receipt.residual_exposure);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
    assert_hlp_near_target(&market, MarketAsset::Base, 2 * NAD as u128);
}

#[test]
fn close_hlp_after_rebalance_retires_synthetic_live_reserves() {
    let mut market = seeded_market();
    market.deposit_single_sided(MarketAsset::Base, 100, 1).unwrap();
    market.quote_side.reserves.live_reserve = 2_400;
    market.quote_side.reserves.cash_reserve = 2_200;
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();

    let (base_receipt, _) = rebalance_hlp_vaults(&mut market).unwrap();

    assert!(base_receipt.ylp_mint_amount > 0);
    assert!(market.base_hlp_vault.base_hlp_live_reserve > 0);
    assert!(market.base_hlp_vault.quote_hlp_live_reserve > 200);

    let hlp_amount = market.base_hlp_vault.hlp_supply;
    market.withdraw_single_sided(MarketAsset::Base, hlp_amount).unwrap();

    assert_eq!(market.base_hlp_vault.hlp_supply, 0);
    assert_eq!(market.base_hlp_vault.ylp_shares, 0);
    assert_eq!(market.base_hlp_vault.debt_shares, 0);
    assert_eq!(market.base_hlp_vault.debt_principal, 0);
    assert_eq!(market.base_hlp_vault.base_hlp_live_reserve, 0);
    assert_eq!(market.base_hlp_vault.quote_hlp_live_reserve, 0);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn rebalance_hlp_leverage_up_does_not_increase_over_cap_debt() {
    let mut market = seeded_market();
    market.deposit_single_sided(MarketAsset::Base, 100, 1).unwrap();
    let settlement_reference_before = market.base_hlp_vault.cached_settlement_price_nad;
    market.quote_side.reserves.live_reserve = 2_400;
    market.quote_side.reserves.cash_reserve = 50;
    market.debt.fixed_quote_shares = 2_150;
    market.debt.fixed_quote_principal = 2_150;
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
    let ideal_before = current_hlp_ideal_delta(&market, MarketAsset::Base).unwrap();
    assert!(ideal_before > 0);

    let (base_receipt, _) = rebalance_hlp_vaults(&mut market).unwrap();

    assert_eq!(base_receipt.executed_delta, 0);
    assert_eq!(base_receipt.residual_exposure, ideal_before);
    let post_ideal = current_hlp_ideal_delta(&market, MarketAsset::Base).unwrap();
    let post_nav = hlp_nav_nad(&market, MarketAsset::Base).unwrap();
    assert_eq!(
        base_receipt.residual_exposure,
        recognized_hlp_residual_exposure(post_ideal, post_nav)
    );
    assert_eq!(base_receipt.debt_delta, 0);
    assert_eq!(market.base_hlp_vault.residual_exposure, base_receipt.residual_exposure);
    assert_eq!(
        market.base_hlp_vault.cached_settlement_price_nad, settlement_reference_before,
        "partial hedge execution must not ratchet the settlement reference"
    );

    let (retry, _) = rebalance_hlp_vaults(&mut market).unwrap();
    assert_eq!(retry.executed_delta, 0);
    assert_eq!(retry.residual_exposure, base_receipt.residual_exposure);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn recognized_hlp_residual_exposure_enforces_absolute_and_relative_boundaries() {
    let large_nav = 20_000 * NAD as u128;
    assert_eq!(recognized_hlp_residual_exposure(10_000, large_nav), 0);
    assert_eq!(recognized_hlp_residual_exposure(-10_000, large_nav), 0);
    assert_eq!(recognized_hlp_residual_exposure(10_001, large_nav), 10_001);
    assert_eq!(recognized_hlp_residual_exposure(-10_001, large_nav), -10_001);

    let small_nav = 9_999 * HLP_REBALANCE_DUST_NAV_DENOMINATOR;
    assert_eq!(recognized_hlp_residual_exposure(9_999, small_nav), 0);
    assert_eq!(recognized_hlp_residual_exposure(10_000, small_nav), 10_000);
}

#[test]
fn zero_target_claim_is_fail_closed_without_bricking_checkpoint() {
    let mut market = seeded_market();
    market.base_side.shares.ylp_supply = 1_500;
    market.quote_side.shares.ylp_supply = 1_500;
    market.base_hlp_vault.hlp_supply = 1;
    market.base_hlp_vault.ylp_shares = 1;

    let values = current_hlp_inventory_values_nad(&market, MarketAsset::Base).unwrap();
    assert_eq!(values.target_inventory_value_nad, 0);
    assert!(values.opposite_inventory_value_nad > 0);

    let (base, quote) = checkpoint_hlp_vaults(&mut market).unwrap();
    assert!(base > 0);
    assert_eq!(quote, 0);
    assert_eq!(market.base_hlp_vault.residual_exposure, base);

    let receipt = rebalance_one_hlp(&mut market, MarketAsset::Base, 0).unwrap();
    assert_eq!(receipt.executed_delta, 0);
    assert_eq!(receipt.residual_exposure, base);

    let prices = current_hlp_curve_prices(&market).unwrap();
    let normalized = refresh_hlp_after_rebalance(
        &mut market,
        MarketAsset::Base,
        HlpRebalanceReceipt {
            target_asset: MarketAsset::Base,
            ideal_delta: base + 1,
            executed_delta: 1,
            ylp_burn_amount: 1,
            ..HlpRebalanceReceipt::default()
        },
        prices,
    )
    .unwrap();
    assert_eq!(normalized.ideal_delta, base);
    assert_eq!(normalized.executed_delta, 0);
    assert_eq!(normalized.ylp_burn_amount, 1);

    let error = prepare_hlp_deposit_like_instruction(&mut market, MarketAsset::Base, 1).unwrap_err();
    assert_eq!(error, error!(ErrorCode::HlpSettlementUnavailable));
    prepare_hlp_deposit_like_instruction(&mut market, MarketAsset::Quote, 1).unwrap();
}

#[test]
fn underwater_zero_claim_vault_cannot_block_global_market_update() {
    let mut market = seeded_market();
    market.base_hlp_vault.hlp_supply = 1;
    market.base_hlp_vault.debt_shares = 1;
    market.base_hlp_vault.debt_principal = 1;

    market.accrue_interest_to_slot(1).unwrap();
    market.advance_amm_clock(1).unwrap();
    market.checkpoint_hlp_vaults().unwrap();
    market.refresh_risk().unwrap();

    assert_eq!(market.base_hlp_vault.last_nav_nad, 0);
    assert!(market.base_hlp_vault.residual_exposure < 0);
    let receipt = rebalance_one_hlp(&mut market, MarketAsset::Base, 1).unwrap();
    assert_eq!(receipt.executed_delta, 0);
    assert_eq!(receipt.residual_exposure, market.base_hlp_vault.residual_exposure);
}

#[test]
fn solvent_zero_target_claim_can_still_exit_fully() {
    let mut market = seeded_market();
    market.deposit_single_sided(MarketAsset::Base, 1, 1).unwrap();

    let moved = apply_test_composite_swap(&mut market, MarketAsset::Quote, 3);
    assert_eq!(moved.amount_out, 1);
    let values = current_hlp_inventory_values_nad(&market, MarketAsset::Base).unwrap();
    assert_eq!(values.target_inventory_value_nad, 0);
    assert_eq!(values.opposite_inventory_value_nad, values.debt_value_nad);
    assert_eq!(market.base_hlp_vault.residual_exposure, 0);

    let supply = market.base_hlp_vault.hlp_supply;
    let receipt = market.withdraw_single_sided(MarketAsset::Base, supply).unwrap();
    assert_eq!(receipt.hlp_supply, 0);
    assert_no_hlp_residuals(&market);
}

#[test]
fn full_exit_clears_stale_residual_exposure_for_both_hlp_vaults() {
    for (target_asset, deposit_amount) in [(MarketAsset::Base, 100), (MarketAsset::Quote, 200)] {
        let mut market = seeded_market();
        market.deposit_single_sided(target_asset, deposit_amount, 1).unwrap();
        let vault = match target_asset {
            MarketAsset::Base => &mut market.base_hlp_vault,
            MarketAsset::Quote => &mut market.quote_hlp_vault,
        };
        vault.residual_exposure = 123;
        let supply = vault.hlp_supply;

        market.withdraw_single_sided(target_asset, supply).unwrap();

        let vault = match target_asset {
            MarketAsset::Base => &market.base_hlp_vault,
            MarketAsset::Quote => &market.quote_hlp_vault,
        };
        assert_eq!(vault.hlp_supply, 0);
        assert_eq!(vault.residual_exposure, 0);
    }
}

#[test]
fn cpmm_swap_skips_unhedgeable_zero_target_vault_without_freezing() {
    let mut market = seeded_market();
    market.deposit_single_sided(MarketAsset::Base, 1, 1).unwrap();
    apply_test_composite_swap(&mut market, MarketAsset::Quote, 3);
    market.debt.quote_borrow_index_nad = (NAD as u128) * 2;
    checkpoint_hlp_vaults(&mut market).unwrap();
    let residual_exposure = market.base_hlp_vault.residual_exposure;
    assert!(residual_exposure < 0);

    let receipt = apply_test_composite_swap(&mut market, MarketAsset::Base, 1);

    assert_eq!(receipt.base_pre_rebalance.executed_delta, 0);
    assert_eq!(receipt.base_rebalance.executed_delta, 0);
    assert_ne!(market.base_hlp_vault.residual_exposure, 0);
}

#[test]
fn post_state_residual_exposure_tracks_high_index_and_coarse_share_rounding_for_both_assets() {
    for target_asset in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = seeded_market();
        market.base_side.shares.ylp_supply = 101;
        market.quote_side.shares.ylp_supply = 101;
        market.deposit_single_sided(target_asset, 100, 1).unwrap();
        match target_asset {
            MarketAsset::Base => market.debt.quote_borrow_index_nad = (NAD as u128) * 110 / 100,
            MarketAsset::Quote => market.debt.base_borrow_index_nad = (NAD as u128) * 110 / 100,
        }

        let (base, quote) = rebalance_hlp_vaults(&mut market).unwrap();
        let receipt = if target_asset == MarketAsset::Base { base } else { quote };
        let post_ideal = current_hlp_ideal_delta(&market, target_asset).unwrap();
        let post_nav = hlp_nav_nad(&market, target_asset).unwrap();
        assert_eq!(
            receipt.residual_exposure,
            recognized_hlp_residual_exposure(post_ideal, post_nav)
        );
        assert_eq!(receipt.executed_delta, receipt.ideal_delta - receipt.residual_exposure);
        assert_eq!(
            match target_asset {
                MarketAsset::Base => market.base_hlp_vault.residual_exposure,
                MarketAsset::Quote => market.quote_hlp_vault.residual_exposure,
            },
            receipt.residual_exposure
        );
    }
}

#[test]
fn rebalance_hlp_leverage_up_keeps_swap_live_without_borrow_cash() {
    let mut market = seeded_market();
    market.deposit_single_sided(MarketAsset::Base, 100, 1).unwrap();
    market.quote_side.reserves.live_reserve = 2_400;
    market.quote_side.reserves.cash_reserve = 0;
    market.debt.fixed_quote_shares = 2_200;
    market.debt.fixed_quote_principal = 2_200;
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
    let ideal_before = current_hlp_ideal_delta(&market, MarketAsset::Base).unwrap();
    assert!(ideal_before > 0);

    let (base_receipt, _) = rebalance_hlp_vaults(&mut market).unwrap();

    assert_eq!(base_receipt.executed_delta, 0);
    assert_eq!(base_receipt.residual_exposure, ideal_before);
    assert_eq!(base_receipt.debt_delta, 0);
    assert_eq!(market.base_hlp_vault.residual_exposure, ideal_before);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn rebalance_hlp_leverage_up_ignores_exhausted_public_borrow_capacity() {
    let mut market = seeded_market();
    market.deposit_single_sided(MarketAsset::Base, 100, 1).unwrap();
    market.quote_side.reserves.live_reserve = 2_400;
    market.quote_side.reserves.cash_reserve = 2_200;
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
    let limit = market
        .daily_limit_for_side(MarketAsset::Quote, market.config.max_daily_borrow_bps)
        .unwrap();
    market.quote_side.daily_borrow_bucket.borrowed_bucket = limit;
    let receipt = rebalance_one_hlp(&mut market, MarketAsset::Base, 0).unwrap();

    assert!(receipt.executed_delta > 0);
    assert!(receipt.debt_delta > 0);
    assert_eq!(market.quote_side.daily_borrow_bucket.borrowed_bucket, limit);
}

#[test]
fn rebalance_hlp_does_not_consume_available_public_borrow_capacity() {
    let mut market = seeded_market();
    market.deposit_single_sided(MarketAsset::Base, 100, 1).unwrap();
    market.quote_side.reserves.live_reserve = 2_400;
    market.quote_side.reserves.cash_reserve = 2_200;
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
    let explicit_borrow = 10;
    let limit = market
        .daily_limit_for_side(MarketAsset::Quote, market.config.max_daily_borrow_bps)
        .unwrap();
    market.quote_side.daily_borrow_bucket.borrowed_bucket = limit - explicit_borrow;

    let receipt = rebalance_one_hlp(&mut market, MarketAsset::Base, 0).unwrap();

    assert!(receipt.executed_delta > 0);
    assert!(receipt.debt_delta > 0);
    assert_eq!(
        market.quote_side.daily_borrow_bucket.borrowed_bucket,
        limit - explicit_borrow
    );
    market
        .side_mut(MarketAsset::Quote)
        .daily_borrow_bucket
        .record_borrow(explicit_borrow, limit, 0)
        .unwrap();
    assert_eq!(market.quote_side.daily_borrow_bucket.borrowed_bucket, limit);
}

#[test]
fn rebalance_hlp_deleverages_with_balanced_ylp() {
    let mut market = seeded_market();
    market.deposit_single_sided(MarketAsset::Base, 100, 1).unwrap();
    market.quote_side.reserves.live_reserve = 1_800;
    market.quote_side.reserves.cash_reserve = 1_600;
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
    let ylp_before = market.base_hlp_vault.ylp_shares;
    let debt_before = market.base_hlp_vault.debt_shares;
    let principal_before = market.base_hlp_vault.debt_principal;

    let (base_receipt, _) = rebalance_hlp_vaults(&mut market).unwrap();

    assert!(base_receipt.ideal_delta < 0);
    assert!(base_receipt.executed_delta < 0);
    assert!(base_receipt.ylp_burn_amount > 0);
    assert_eq!(base_receipt.ylp_mint_amount, 0);
    assert!(market.base_hlp_vault.ylp_shares < ylp_before);
    assert!(market.base_hlp_vault.debt_shares < debt_before);
    assert!(market.base_hlp_vault.debt_principal < principal_before);
    assert_eq!(market.base_hlp_vault.residual_exposure, base_receipt.residual_exposure);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
    assert_hlp_near_target(&market, MarketAsset::Base, 2 * NAD as u128);
}

#[test]
fn rebalance_hlp_deleverage_pays_accrued_interest_from_borrowed_cash() {
    let mut market = seeded_market();
    market.deposit_single_sided(MarketAsset::Base, 100, 1).unwrap();
    market.quote_side.reserves.live_reserve = 1_800;
    market.quote_side.reserves.cash_reserve = 1_600;
    market.debt.quote_borrow_index_nad = (NAD as u128) * 110 / 100;
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
    let quote_cash_before = market.quote_side.reserves.cash_reserve;
    let principal_before = market.base_hlp_vault.debt_principal;

    let (base_receipt, _) = rebalance_hlp_vaults(&mut market).unwrap();

    assert!(base_receipt.executed_delta < 0);
    let principal_repaid = principal_before
        .checked_sub(market.base_hlp_vault.debt_principal)
        .unwrap();
    let interest_paid = base_receipt
        .debt_delta
        .unsigned_abs()
        .checked_sub(u128::from(principal_repaid))
        .unwrap();
    assert!(interest_paid > 0);
    assert_eq!(base_receipt.interest_paid as u128, interest_paid);
    assert!(
        quote_cash_before
            .checked_sub(market.quote_side.reserves.cash_reserve)
            .unwrap() as u128
            >= interest_paid
    );
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn quote_hlp_rebalance_moves_both_ylp_sides() {
    let mut market = seeded_market();
    market.deposit_single_sided(MarketAsset::Quote, 200, 1).unwrap();
    market.base_side.reserves.live_reserve = 1_200;
    market.base_side.reserves.cash_reserve = 1_100;
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    let ylp_before = market.quote_hlp_vault.ylp_shares;
    let debt_before = market.quote_hlp_vault.debt_shares;
    let principal_before = market.quote_hlp_vault.debt_principal;

    let (_, quote_receipt) = rebalance_hlp_vaults(&mut market).unwrap();

    assert!(quote_receipt.ideal_delta > 0);
    assert!(quote_receipt.executed_delta > 0);
    assert!(quote_receipt.ylp_mint_amount > 0);
    assert!(market.quote_hlp_vault.ylp_shares > ylp_before);
    assert!(market.quote_hlp_vault.debt_shares > debt_before);
    assert!(market.quote_hlp_vault.debt_principal > principal_before);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
    assert_hlp_near_target(&market, MarketAsset::Quote, 7 * NAD as u128);
}

#[test]
fn swap_rebalance_is_price_neutral_after_user_quote() {
    let mut market = seeded_market();
    market.base_side.reserves.live_reserve = 1_000_000;
    market.base_side.reserves.cash_reserve = 1_000_000;
    market.base_side.shares.ylp_supply = 1_000_000;
    market.quote_side.reserves.live_reserve = 2_000_000;
    market.quote_side.reserves.cash_reserve = 2_000_000;
    market.quote_side.shares.ylp_supply = 1_000_000;

    market.deposit_single_sided(MarketAsset::Base, 100_000, 1).unwrap();
    market.deposit_single_sided(MarketAsset::Quote, 200_000, 1).unwrap();

    let amount_in_after_fee = 50_000;
    let amount_out = cpmm_amount_out(
        market.base_side.reserves.live_reserve,
        market.quote_side.reserves.live_reserve,
        amount_in_after_fee,
    )
    .unwrap();
    market
        .swap_reserves(
            MarketAsset::Base,
            amount_in_after_fee,
            amount_out,
            0,
            0,
            crate::state::ProtocolAuctionSplit::default(),
        )
        .unwrap();

    let quoted_post_swap_price = market_spot_price_nad(&market.base_side, &market.quote_side).unwrap();
    let base_liquidity_before = market.base_side.reserves.live_reserve;
    let quote_liquidity_before = market.quote_side.reserves.live_reserve;

    let (base_receipt, quote_receipt) = rebalance_hlp_vaults(&mut market).unwrap();

    assert!(
        base_receipt.executed_delta != 0 || quote_receipt.executed_delta != 0,
        "test must exercise an hLP rebalance"
    );
    assert_ne!(market.base_side.reserves.live_reserve, base_liquidity_before);
    assert_ne!(market.quote_side.reserves.live_reserve, quote_liquidity_before);

    let post_rebalance_price = market_spot_price_nad(&market.base_side, &market.quote_side).unwrap();
    let price_diff = quoted_post_swap_price.abs_diff(post_rebalance_price);
    assert!(
        price_diff <= quoted_post_swap_price / BPS_DENOMINATOR as u64 + 1,
        "hLP rebalance moved post-swap spot by more than rounding: quoted {}, final {}",
        quoted_post_swap_price,
        post_rebalance_price
    );
}

#[test]
fn small_swap_skips_hlp_pre_solve() {
    let mut market = seeded_market();
    configure_market_depth(&mut market, 1_000_000, 20_000);
    market.deposit_single_sided(MarketAsset::Base, 100_000, 1).unwrap();

    let prepared = crate::instructions::SwapRequest {
        current_slot: 0,
        asset_in: MarketAsset::Base,
        reserve_credit: 1,
    }
    .prepare(&mut market)
    .unwrap();
    let base_receipt = prepared.base_pre_rebalance;
    let quote_receipt = prepared.quote_pre_rebalance;

    assert_eq!(base_receipt.executed_delta, 0);
    assert_eq!(quote_receipt.executed_delta, 0);
    assert_eq!(base_receipt.ylp_mint_amount, 0);
    assert_eq!(quote_receipt.ylp_mint_amount, 0);
    assert_market_hlp_invariants(&market);
}

#[test]
fn probe_litesvm_active_hlp_fixture() {
    let mut market = seeded_market();
    market.base_side.asset_decimals = 6;
    market.quote_side.asset_decimals = 6;
    configure_market_depth(&mut market, 100_000, 20_000);
    market.deposit_single_sided(MarketAsset::Base, 10_000, 1).unwrap();
    CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(|count| count.set(0));
    let result = (crate::instructions::SwapRequest {
        current_slot: 1,
        asset_in: MarketAsset::Base,
        reserve_credit: 1_000,
    })
    .prepare(&mut market);
    eprintln!(
        "result={:?} evaluations={}",
        result.as_ref().map(|prepared| (
            prepared.base_pre_rebalance,
            prepared.quote_pre_rebalance,
            prepared.quote.amount_out,
        )),
        CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(Cell::get),
    );
}

#[test]
fn concentrated_rebalance_uses_actual_inventory_exposure_and_preserves_curve_price() {
    let mut market = active_concentrated_hlp_market();
    let swap = market.quote_curve_exact_in(MarketAsset::Base, 150_000, 0).unwrap();
    market
        .swap_reserves(
            MarketAsset::Base,
            150_000,
            swap.amount_out,
            0,
            0,
            crate::state::ProtocolAuctionSplit::default(),
        )
        .unwrap();

    let values_before = current_hlp_inventory_values_nad(&market, MarketAsset::Base).unwrap();
    assert_ne!(
        values_before.target_inventory_value_nad, values_before.opposite_inventory_value_nad,
        "the fixture must leave the 50/50-value CPMM special case"
    );
    let expected = ideal_hlp_rebalance_nad(values_before)
        .unwrap()
        .total_liquidity_value_nad;
    let legacy_two_x = i128::try_from(
        values_before
            .target_inventory_value_nad
            .checked_add(values_before.opposite_inventory_value_nad)
            .unwrap(),
    )
    .unwrap()
        - i128::try_from(values_before.debt_value_nad.checked_mul(2).unwrap()).unwrap();
    assert_eq!(current_hlp_ideal_delta(&market, MarketAsset::Base).unwrap(), expected);
    assert_ne!(expected, legacy_two_x);

    let exposure_before = hlp_opposite_exposure_nad(values_before).unwrap().unsigned_abs();
    let price_before = market.curve_marginal_price_nad(0).unwrap();
    let (base_receipt, _) = rebalance_hlp_vaults(&mut market).unwrap();
    assert_ne!(base_receipt.executed_delta, 0);
    let price_after = market.curve_marginal_price_nad(0).unwrap();
    let exposure_after =
        hlp_opposite_exposure_nad(current_hlp_inventory_values_nad(&market, MarketAsset::Base).unwrap())
            .unwrap()
            .unsigned_abs();

    assert!(
        exposure_after < exposure_before,
        "curve-aware rebalance must reduce opposite exposure: before {}, after {}",
        exposure_before,
        exposure_after
    );
    assert!(
        price_diff_bps(price_before, price_after) <= 2,
        "proportional executable reserve claims moved CONCENTRATED marginal price: before {}, after {}",
        price_before,
        price_after
    );
}

#[test]
fn due_funded_ramp_blocks_new_hlp_deposit_until_a_swap_like_operation_advances_it() {
    let (mut market, due_slot) = funded_due_ramp_with_residual_base_hlp();
    let applied = market.amm.applied_curve_parameters;
    let settlement_reference = market.base_hlp_vault.cached_settlement_price_nad;
    let settlement_reference_u64 = u64::try_from(settlement_reference).unwrap();
    let worsening_start = settlement_reference_u64.checked_mul(106).unwrap() / 100;
    let worsening_end = settlement_reference_u64.checked_mul(110).unwrap() / 100;
    assert_eq!(
        require_hlp_swap_path_safe(&market, worsening_start, worsening_end, true, false,).unwrap_err(),
        error!(ErrorCode::HlpSettlementUnavailable)
    );

    let error = prepare_hlp_deposit_like_instruction(&mut market, MarketAsset::Base, due_slot).unwrap_err();
    assert_eq!(error, error!(ErrorCode::HlpSettlementUnavailable));
    assert_eq!(market.amm.applied_curve_parameters, applied);

    let exit_amount = (market.base_hlp_vault.hlp_supply / 10).max(1);
    let exit = market.withdraw_single_sided(MarketAsset::Base, exit_amount).unwrap();
    assert!(exit.target_amount_out > 0);
    assert_eq!(market.amm.applied_curve_parameters, applied);
    assert_ne!(market.base_hlp_vault.residual_exposure, 0);
    assert_eq!(
        market.base_hlp_vault.cached_settlement_price_nad, settlement_reference,
        "a partial exit must not ratchet the settlement band around residual inventory"
    );
    assert_eq!(
        require_hlp_swap_path_safe(&market, worsening_start, worsening_end, true, false,).unwrap_err(),
        error!(ErrorCode::HlpSettlementUnavailable)
    );
}

#[test]
fn residual_hlp_exposure_cannot_freeze_the_lazy_controller() {
    let (mut market, due_slot) = funded_due_ramp_with_residual_base_hlp();
    let applied = market.amm.applied_curve_parameters;
    assert_ne!(market.base_hlp_vault.residual_exposure, 0);

    market.prepare_amm_for_swap(due_slot).unwrap();
    let moved = market.advance_one_amm_controller_target(due_slot).unwrap();

    assert!(moved);
    assert_ne!(market.amm.applied_curve_parameters, applied);
    assert_ne!(market.base_hlp_vault.residual_exposure, 0);
}

#[test]
fn hlp_deposit_refreshes_actual_exposure_before_entry_gate() {
    let mut market = active_concentrated_hlp_market();
    assert_eq!(market.base_hlp_vault.residual_exposure, 0);
    market.debt.quote_borrow_index_nad = (NAD as u128) * 101 / 100;

    let error = prepare_hlp_deposit_like_instruction(&mut market, MarketAsset::Base, 1).unwrap_err();

    assert_eq!(error, error!(ErrorCode::HlpSettlementUnavailable));
    assert_ne!(market.base_hlp_vault.residual_exposure, 0);
}

#[test]
fn the_next_user_operation_refreshes_exposure_without_being_blocked_by_it() {
    let mut market = active_concentrated_hlp_market();
    assert_eq!(market.base_hlp_vault.residual_exposure, 0);
    let center = market.amm.center_price_nad;
    market.debt.quote_borrow_index_nad = (NAD as u128) * 101 / 100;

    let (base_residual_exposure, quote_residual_exposure) = market.checkpoint_hlp_vaults().unwrap();
    market.prepare_amm_for_swap(1).unwrap();
    let moved = market.advance_one_amm_controller_target(1).unwrap();

    assert!(!moved);
    assert_ne!(base_residual_exposure, 0);
    assert_eq!(quote_residual_exposure, 0);
    assert_ne!(market.base_hlp_vault.residual_exposure, 0);
    assert_eq!(market.amm.center_price_nad, center);
}

#[test]
fn recognized_checkpoint_dust_does_not_reappear_on_the_next_user_operation() {
    let mut market = seeded_market();
    let scale = NAD;
    market.base_side.asset_decimals = 9;
    market.quote_side.asset_decimals = 9;
    market.base_side.reserves.live_reserve *= scale;
    market.base_side.reserves.cash_reserve *= scale;
    market.quote_side.reserves.live_reserve *= scale;
    market.quote_side.reserves.cash_reserve *= scale;
    market.base_side.shares.ylp_supply *= scale;
    market.quote_side.shares.ylp_supply *= scale;
    market.deposit_single_sided(MarketAsset::Base, 100 * scale, 1).unwrap();
    enable_concentrated_curve(&mut market);

    market.debt.quote_borrow_index_nad = (NAD + 1) as u128;
    let actual = current_hlp_ideal_delta(&market, MarketAsset::Base).unwrap();
    let nav = hlp_nav_nad(&market, MarketAsset::Base).unwrap();
    assert_ne!(actual, 0);
    assert_eq!(recognized_hlp_residual_exposure(actual, nav), 0);

    let (base, quote) = checkpoint_hlp_vaults(&mut market).unwrap();
    assert_eq!((base, quote), (0, 0));
    assert_eq!(market.base_hlp_vault.residual_exposure, 0);
    market.prepare_amm_for_swap(1).unwrap();
    assert!(!market.advance_one_amm_controller_target(1).unwrap());
    let (base, quote) = checkpoint_hlp_vaults(&mut market).unwrap();
    assert_eq!((base, quote), (0, 0));
    assert_eq!(market.base_hlp_vault.residual_exposure, 0);
}

#[test]
fn pre_and_post_hlp_settlement_nets_to_one_token_cpi_per_side() {
    let mint_then_burn = combine_hlp_rebalance_receipts(
        HlpRebalanceReceipt {
            target_asset: MarketAsset::Base,
            ylp_mint_amount: 10,
            interest_paid: 2,
            ..HlpRebalanceReceipt::default()
        },
        HlpRebalanceReceipt {
            target_asset: MarketAsset::Base,
            ylp_burn_amount: 4,
            interest_paid: 3,
            ..HlpRebalanceReceipt::default()
        },
    )
    .unwrap();
    assert_eq!(mint_then_burn.ylp_mint_amount, 6);
    assert_eq!(mint_then_burn.ylp_burn_amount, 0);
    assert_eq!(mint_then_burn.interest_paid, 5);

    let burn_then_mint = combine_hlp_rebalance_receipts(
        HlpRebalanceReceipt {
            target_asset: MarketAsset::Quote,
            ylp_burn_amount: 10,
            ..HlpRebalanceReceipt::default()
        },
        HlpRebalanceReceipt {
            target_asset: MarketAsset::Quote,
            ylp_mint_amount: 4,
            ..HlpRebalanceReceipt::default()
        },
    )
    .unwrap();
    assert_eq!(burn_then_mint.ylp_mint_amount, 0);
    assert_eq!(burn_then_mint.ylp_burn_amount, 6);
}

#[test]
fn quote_simulation_matches_composite_swap_execution() {
    let mut quoted_market = active_hlp_market();
    let mut executed_market = active_hlp_market();
    let amount_in_after_fee = 350_000;

    let quoted = apply_test_composite_swap(&mut quoted_market, MarketAsset::Base, amount_in_after_fee);
    let executed = apply_test_composite_swap(&mut executed_market, MarketAsset::Base, amount_in_after_fee);

    assert_eq!(quoted.amount_out, executed.amount_out);
    assert_eq!(
        quoted.base_pre_rebalance.executed_delta,
        executed.base_pre_rebalance.executed_delta
    );
    assert_eq!(
        quoted.quote_pre_rebalance.executed_delta,
        executed.quote_pre_rebalance.executed_delta
    );
    assert_eq!(
        quoted_market.base_side.reserves.live_reserve,
        executed_market.base_side.reserves.live_reserve
    );
    assert_eq!(
        quoted_market.quote_side.reserves.live_reserve,
        executed_market.quote_side.reserves.live_reserve
    );
    assert!(
        quoted.base_rebalance.executed_delta != quoted.base_pre_rebalance.executed_delta
            || quoted.quote_rebalance.executed_delta != quoted.quote_pre_rebalance.executed_delta,
        "test must exercise the post-swap hLP phase too"
    );
}

#[test]
fn price_moving_swap_keeps_both_hlp_vaults_close_to_their_deposit_assets() {
    let mut market = active_hlp_market();
    let base_deposit = 100_000u64;
    let quote_deposit = 200_000u64;

    // Quote-in moves the base price up. Both one-sided vaults need a
    // pre-adjustment for the finite move and a post-adjustment back to 2x.
    let swap = apply_test_composite_swap(&mut market, MarketAsset::Quote, 350_000);
    assert!(swap.base_pre_rebalance.executed_delta > 0);
    assert!(swap.quote_pre_rebalance.executed_delta < 0);
    assert!(swap.base_rebalance.executed_delta > swap.base_pre_rebalance.executed_delta);
    assert!(swap.quote_rebalance.executed_delta < swap.quote_pre_rebalance.executed_delta);

    let mut base_close_market = market.clone();
    let base_close = base_close_market
        .withdraw_single_sided(MarketAsset::Base, base_close_market.base_hlp_vault.hlp_supply)
        .unwrap();

    let mut quote_close_market = market;
    let quote_close = quote_close_market
        .withdraw_single_sided(MarketAsset::Quote, quote_close_market.quote_hlp_vault.hlp_supply)
        .unwrap();

    let base_tracking_error_bps = base_close
        .target_amount_out
        .abs_diff(base_deposit)
        .saturating_mul(BPS_DENOMINATOR as u64)
        / base_deposit;
    let quote_tracking_error_bps = quote_close
        .target_amount_out
        .abs_diff(quote_deposit)
        .saturating_mul(BPS_DENOMINATOR as u64)
        / quote_deposit;

    assert!(
        base_tracking_error_bps <= 5,
        "base hLP redemption drifted {} bps: deposited {}, redeemed {}",
        base_tracking_error_bps,
        base_deposit,
        base_close.target_amount_out
    );
    assert!(
        quote_tracking_error_bps <= 5,
        "quote hLP redemption drifted {} bps: deposited {}, redeemed {}",
        quote_tracking_error_bps,
        quote_deposit,
        quote_close.target_amount_out
    );
}

#[test]
fn swap_round_trip_then_hlp_close_leaves_no_synthetic_residuals() {
    let mut market = active_hlp_market();

    CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(|count| count.set(0));
    CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(|count| count.set(0));
    let first_swap = apply_test_composite_swap(&mut market, MarketAsset::Base, 350_000);
    let first_counts = (
        CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(Cell::get),
        CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(Cell::get),
    );
    CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(|count| count.set(0));
    CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(|count| count.set(0));
    let _second_swap = apply_test_composite_swap(&mut market, MarketAsset::Quote, first_swap.amount_out);
    let second_counts = (
        CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(Cell::get),
        CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(Cell::get),
    );
    assert_eq!(first_counts, (6, 1));
    assert_eq!(second_counts, (6, 1));

    // Close must realize no more than the marked pre-close NAV. Comparing to
    // the original deposit confounds close accounting with PnL earned or lost
    // during the two swaps and previously hid unaccounted backing principal.
    let prices = current_hlp_curve_prices(&market).unwrap();
    let base_nav_nad = current_hlp_valuation(&market, MarketAsset::Base).unwrap().nav_nad;
    let quote_nav_nad = current_hlp_valuation(&market, MarketAsset::Quote).unwrap().nav_nad;
    let aggregate_nav_in_quote_nad = base_nav_nad
        .checked_mul(prices.base_in_quote_nad)
        .unwrap()
        .checked_div(NAD as u128)
        .unwrap()
        .checked_add(quote_nav_nad)
        .unwrap();

    let base_hlp_supply = market.base_hlp_vault.hlp_supply;
    let quote_hlp_supply = market.quote_hlp_vault.hlp_supply;
    let base_close = market
        .withdraw_single_sided(MarketAsset::Base, base_hlp_supply)
        .unwrap();
    let quote_close = market
        .withdraw_single_sided(MarketAsset::Quote, quote_hlp_supply)
        .unwrap();

    let realized_value_in_quote_nad =
        normalize_to_nad(base_close.target_amount_out as u128, market.base_side.asset_decimals)
            .unwrap()
            .checked_mul(prices.base_in_quote_nad)
            .unwrap()
            .checked_div(NAD as u128)
            .unwrap()
            .checked_add(
                normalize_to_nad(quote_close.target_amount_out as u128, market.quote_side.asset_decimals).unwrap(),
            )
            .unwrap();
    assert!(
        realized_value_in_quote_nad <= aggregate_nav_in_quote_nad + NAD as u128,
        "hLP close exceeded marked NAV: base {:?}, quote {:?}, realized {}, nav {}",
        base_close,
        quote_close,
        realized_value_in_quote_nad,
        aggregate_nav_in_quote_nad
    );
    assert_no_hlp_residuals(&market);
}

#[test]
fn mass_unwind_is_order_independent_when_cash_is_available() {
    let mut close_first = active_hlp_market();
    let mut ylp_first = active_hlp_market();
    let public_ylp_supply = 1_000_000;

    close_first
        .withdraw_single_sided(MarketAsset::Base, close_first.base_hlp_vault.hlp_supply)
        .unwrap();
    close_first
        .withdraw_single_sided(MarketAsset::Quote, close_first.quote_hlp_vault.hlp_supply)
        .unwrap();
    assert_no_hlp_residuals(&close_first);
    close_first.remove_liquidity(public_ylp_supply).unwrap();
    assert_eq!(close_first.base_side.reserves.live_reserve, 0);
    assert_eq!(close_first.quote_side.reserves.live_reserve, 0);
    assert_eq!(close_first.base_side.shares.ylp_supply, 0);
    assert_eq!(close_first.quote_side.shares.ylp_supply, 0);

    ylp_first.remove_liquidity(public_ylp_supply).unwrap();
    ylp_first
        .withdraw_single_sided(MarketAsset::Base, ylp_first.base_hlp_vault.hlp_supply)
        .unwrap();
    ylp_first
        .withdraw_single_sided(MarketAsset::Quote, ylp_first.quote_hlp_vault.hlp_supply)
        .unwrap();
    assert_no_hlp_residuals(&ylp_first);
    assert_eq!(ylp_first.base_side.reserves.live_reserve, 0);
    assert_eq!(ylp_first.quote_side.reserves.live_reserve, 0);
    assert_eq!(ylp_first.base_side.shares.ylp_supply, 0);
    assert_eq!(ylp_first.quote_side.shares.ylp_supply, 0);
}
