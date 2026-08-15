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
        HLP_COMPACT_GUIDANCE_EXACT_OUT_CANONICAL_FALLBACKS.with(|count| count.set(0));
        let compact = plan_compact_hlp_deleverage(
            fixed,
            state,
            target_asset,
            valuation.ideal_delta,
            valuation,
            &anchor,
            state.base_side.ylp_supply,
            HlpGuidanceSettlementProbeMode::Bounded,
            SwapCashFloors::default(),
        )
        .unwrap();
        let proof = compact
            .guidance_settlement
            .unwrap_or_else(|| panic!("expected bounded I>B proof for {target_asset:?}"));
        let facts = proof.facts();
        assert_eq!(
            normalize_to_nad(facts.target_retained as u128, fixed.decimals(target_asset)).unwrap(),
            facts.selected_input_nad
        );
        assert!(matches!(
            proof.sample_mode(),
            HlpGuidanceSettlementSampleMode::BoundedP2High | HlpGuidanceSettlementSampleMode::BoundedP3Positive
        ));
        assert!(facts.borrowed_shortfall > 0);
        assert!(facts.selected_input_nad > 0);
        assert!(facts.target_retained > 0);

        let HlpRebalancePlan::Deleverage { ylp_burn_amount, .. } = compact.plan else {
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
            .prepare_supply_scaled_guidance_successor(
                facts.post_entitlement_curve_reserves_nad.base,
                facts.post_entitlement_curve_reserves_nad.quote,
                state.base_side.ylp_supply,
                post_ylp_supply,
            )
            .unwrap();
        assert!(same_d.invariant_d() >= scaled_d);
        let direction = match target_asset {
            MarketAsset::Base => ConcentratedSwapDirection::BaseToQuote,
            MarketAsset::Quote => ConcentratedSwapDirection::QuoteToBase,
        };
        let borrowed_asset = target_asset.opposite();
        let amount_out_nad =
            normalize_to_nad(facts.borrowed_shortfall as u128, fixed.decimals(borrowed_asset)).unwrap();
        let exact_input = same_d
            .quote_exact_out_input_bracket(amount_out_nad, direction)
            .unwrap()
            .1;
        assert!(facts.selected_input_nad >= exact_input);
        assert!(same_d.quote_exact_in(facts.selected_input_nad, direction).unwrap() >= amount_out_nad);
        let probes = HLP_COMPACT_GUIDANCE_EXACT_OUT_PROBES.with(Cell::get);
        assert!(
            (2..=MAX_RESIDUAL_PROBES_PER_BOUNDED_EXACT_OUT_LEG as u32).contains(&probes),
            "target={target_asset:?} probes={probes}"
        );
        assert_eq!(HLP_COMPACT_GUIDANCE_EXACT_OUT_CANONICAL_FALLBACKS.with(Cell::get), 0);
    }
}

#[test]
fn compact_bounded_i_greater_than_b_stays_live_at_solvent_two_point_one_funding() {
    let scale = 1_000_000_u64;
    let mut fixture = active_concentrated_hlp_market_with_decimals(6);
    fixture.base_side.credit_reserve(500_000 * scale, true).unwrap();
    fixture.quote_side.credit_reserve(1_000_000 * scale, true).unwrap();
    fixture.checkpoint_amm_neutral_inventory(0).unwrap();
    fixture.debt.base_borrow_index_nad = 21 * NAD as u128 / 10;
    fixture.debt.quote_borrow_index_nad = 21 * NAD as u128 / 10;

    let mut radially_raised = 0_u32;
    for target_asset in [MarketAsset::Base, MarketAsset::Quote] {
        let market = fixture.clone();
        let fixed = HlpPlannerStatic::capture(&market).unwrap();
        let start_state = HlpPlannerState::capture(&market);
        let reserves = start_state.curve_reserves_nad(fixed).unwrap();
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
        assert!(valuation.ideal_delta < 0, "target={target_asset:?}");

        HLP_COMPACT_GUIDANCE_EXACT_OUT_PROBES.with(|count| count.set(0));
        HLP_COMPACT_GUIDANCE_EXACT_OUT_CANONICAL_FALLBACKS.with(|count| count.set(0));
        let compact = plan_compact_hlp_deleverage(
            fixed,
            start_state,
            target_asset,
            valuation.ideal_delta,
            valuation,
            &anchor,
            start_state.base_side.ylp_supply,
            HlpGuidanceSettlementProbeMode::Bounded,
            SwapCashFloors::default(),
        )
        .unwrap_or_else(|error| panic!("target={target_asset:?}: {error:?}"));
        let proof = compact
            .guidance_settlement
            .unwrap_or_else(|| panic!("expected bounded I>B proof for {target_asset:?}"));
        let facts = proof.facts();
        assert_eq!(
            normalize_to_nad(facts.target_retained as u128, fixed.decimals(target_asset)).unwrap(),
            facts.selected_input_nad
        );
        assert!(matches!(
            proof.sample_mode(),
            HlpGuidanceSettlementSampleMode::BoundedP2High | HlpGuidanceSettlementSampleMode::BoundedP3Positive
        ));
        let HlpRebalancePlan::Deleverage {
            ylp_burn_amount,
            interest_paid,
            base_entitlement_amount,
            quote_entitlement_amount,
            ..
        } = compact.plan
        else {
            panic!("expected compact deleverage plan for {target_asset:?}")
        };
        let borrowed_entitlement = match target_asset {
            MarketAsset::Base => quote_entitlement_amount,
            MarketAsset::Quote => base_entitlement_amount,
        };
        assert!(interest_paid > borrowed_entitlement);
        let post_supply = start_state.base_side.ylp_supply.checked_sub(ylp_burn_amount).unwrap();
        let scaled_d = mul_div_u128(
            anchor.invariant_d(),
            post_supply as u128,
            start_state.base_side.ylp_supply as u128,
        )
        .unwrap();
        let prepared = anchor
            .prepare_supply_scaled_guidance_successor(
                facts.post_entitlement_curve_reserves_nad.base,
                facts.post_entitlement_curve_reserves_nad.quote,
                start_state.base_side.ylp_supply,
                post_supply,
            )
            .unwrap();
        radially_raised += u32::from(prepared.invariant_d() > scaled_d);

        let probes = HLP_COMPACT_GUIDANCE_EXACT_OUT_PROBES.with(Cell::get);
        assert!(
            (2..=MAX_RESIDUAL_PROBES_PER_BOUNDED_EXACT_OUT_LEG as u32).contains(&probes),
            "target={target_asset:?} probes={probes}"
        );
        assert_eq!(HLP_COMPACT_GUIDANCE_EXACT_OUT_CANONICAL_FALLBACKS.with(Cell::get), 0);
        let mut post_state = start_state;
        let receipt = apply_compact_hlp_rebalance_plan_to_planner_state(fixed, &mut post_state, compact).unwrap();
        assert_eq!(receipt.ylp_burn_amount, ylp_burn_amount);
        assert!(post_state.base_side.ylp_supply < start_state.base_side.ylp_supply);
        assert_eq!(post_state.base_side.ylp_supply, post_state.quote_side.ylp_supply);
        assert!(post_state.vault(target_asset).debt_shares < start_state.vault(target_asset).debt_shares);
        let post_reserves = post_state.curve_reserves_nad(fixed).unwrap();
        assert!(post_reserves.base > 0 && post_reserves.quote > 0);
    }
    assert!(radially_raised > 0);
}

#[test]
fn compact_i_greater_than_b_fallback_matches_only_insufficient_liquidity() {
    assert!(hlp_guidance_exact_out_settlement::is_insufficient_liquidity(&error!(
        ErrorCode::InsufficientLiquidity
    )));
    assert!(!hlp_guidance_exact_out_settlement::is_insufficient_liquidity(&error!(
        ErrorCode::BrokenInvariant
    )));

    let market = active_concentrated_hlp_market_with_decimals(6);
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
    let borrowed_reserve = state.curve_reserve(fixed, MarketAsset::Quote).unwrap();

    HLP_COMPACT_GUIDANCE_EXACT_OUT_PROBES.with(|count| count.set(0));
    HLP_COMPACT_GUIDANCE_EXACT_OUT_CANONICAL_FALLBACKS.with(|count| count.set(0));
    let error = HlpGuidanceSettlementProof::plan(
        fixed,
        state,
        &anchor,
        state.base_side.ylp_supply,
        MarketAsset::Base,
        1,
        0,
        borrowed_reserve,
        state.base_side.ylp_supply - 1,
        HlpGuidanceSettlementProbeMode::Bounded,
    )
    .unwrap_err();
    assert_eq!(error, error!(ErrorCode::InsufficientLiquidity));
    assert_eq!(HLP_COMPACT_GUIDANCE_EXACT_OUT_PROBES.with(Cell::get), 0);
    assert_eq!(HLP_COMPACT_GUIDANCE_EXACT_OUT_CANONICAL_FALLBACKS.with(Cell::get), 1);
}

#[test]
fn compact_cash_floors_match_market_for_isolated_lifecycle_policies() {
    let mut market = Market::default();
    market.debt.base_borrow_index_nad = NAD as u128 + 123_456_789;
    market.debt.quote_borrow_index_nad = NAD as u128 + 987_654_321;
    let base_principal = 700_000_u64;
    let quote_principal = 900_000_u64;
    let base_position_shares = market
        .debt
        .add_isolated_debt(MarketAsset::Base, base_principal)
        .unwrap();
    market.debt.add_isolated_debt(MarketAsset::Base, 300_000).unwrap();
    let quote_position_shares = market
        .debt
        .add_isolated_debt(MarketAsset::Quote, quote_principal)
        .unwrap();
    market.debt.add_isolated_debt(MarketAsset::Quote, 400_000).unwrap();
    let fixed = HlpPlannerStatic::capture(&market).unwrap();
    let state = HlpPlannerState::capture(&market);

    for (debt_asset, debt_shares, debt_principal) in [
        (MarketAsset::Base, base_position_shares, base_principal as u128),
        (MarketAsset::Quote, quote_position_shares, quote_principal as u128),
    ] {
        let asset_in = debt_asset.opposite();
        for policy in [
            SwapCashPolicy::Decrease {
                debt_asset,
                debt_shares,
                debt_principal,
            },
            SwapCashPolicy::Close {
                debt_asset,
                debt_shares,
                debt_principal,
            },
            SwapCashPolicy::Liquidate {
                debt_asset,
                debt_shares,
                debt_principal,
            },
        ] {
            // A one-token repayment can legitimately be too small to burn one
            // share once the borrow index has accrued above 1.0. Keep every
            // case inside the executable repayment domain so this test isolates
            // compact/full floor parity rather than minimum-repayment behavior.
            for amount_out in [2_u64, 333_333, 2_000_000] {
                assert_eq!(
                    policy.floors(&market, asset_in, amount_out).unwrap(),
                    policy.floors_from_planner(fixed, state, asset_in, amount_out).unwrap(),
                    "policy={policy:?} debt_asset={debt_asset:?} amount_out={amount_out}",
                );
            }
        }
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
    let error = apply_hlp_rebalance_pair_plan(&mut stale, &plan).unwrap_err();
    assert_eq!(error, error!(ErrorCode::BrokenInvariant));
    assert_eq!(stale.try_to_vec().unwrap(), stale_before);

    let mut tampered = plan;
    let HlpRebalancePairLegPlan::Active(quote_plan) = &mut tampered.quote else {
        panic!("expected active Quote plan")
    };
    quote_plan.common_mut().start.quote_live_reserve += 1;
    let mut candidate = market.clone();
    let candidate_before = candidate.try_to_vec().unwrap();
    let error = apply_hlp_rebalance_pair_plan(&mut candidate, &tampered).unwrap_err();
    assert_eq!(error, error!(ErrorCode::BrokenInvariant));
    assert_eq!(candidate.try_to_vec().unwrap(), candidate_before);

    let mut inactive_tamper = plan;
    inactive_tamper.quote = HlpRebalancePairLegPlan::Inactive {
        target_asset: MarketAsset::Quote,
    };
    let mut candidate = market;
    let candidate_before = candidate.try_to_vec().unwrap();
    let error = apply_hlp_rebalance_pair_plan(&mut candidate, &inactive_tamper).unwrap_err();
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
    let trace = stage4b2a_last_trace();
    assert_eq!(trace.a1_coordinate, Some((-22_491_640_730_838, 29_009_707_431_716)));
    assert_eq!(trace.a2_coordinate, Some((-22_491_536_532_954, 29_016_042_212_191)));
    assert_eq!(
        trace.a2_rows,
        Some([7_677_827, 399_077_244, 2_951_054_032, 1_653_230_718, 0, 0])
    );
    assert_eq!(trace.a1_topology, trace.a2_topology);
    assert!(trace.raw_coordinate.is_none());

    assert_eq!(market.base_side.reserves.base_hlp_backing_inventory, 25_734);
    assert_eq!(market.base_side.reserves.quote_hlp_backing_inventory, 0);
    assert_eq!(market.quote_side.reserves.total_hlp_backing_inventory().unwrap(), 0);

    let supply_before = market.base_hlp_vault.hlp_supply;
    market
        .withdraw_single_sided(MarketAsset::Base, supply_before / 2)
        .unwrap();
    assert_eq!(market.base_side.reserves.base_hlp_backing_inventory, 12_867);

    let remaining = market.base_hlp_vault.hlp_supply;
    market.withdraw_single_sided(MarketAsset::Base, remaining).unwrap();
    assert_eq!(market.base_side.reserves.base_hlp_backing_inventory, 0);
    assert_eq!(market.quote_side.reserves.base_hlp_backing_inventory, 0);
}

#[test]
fn partial_interest_exit_with_second_hlp_and_backing_excludes_both_nested_claims() {
    let mut market = matched_symmetric_hlp_market();
    CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(|count| count.set(0));
    CONCENTRATED_PRE_SOLVE_COMPACT_EVALUATIONS.with(|count| count.set(0));
    CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(|count| count.set(0));
    apply_test_composite_swap(&mut market, MarketAsset::Base, 350_000);
    assert_eq!(CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(Cell::get), 6);
    assert_eq!(CONCENTRATED_PRE_SOLVE_COMPACT_EVALUATIONS.with(Cell::get), 4);
    assert_eq!(CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(Cell::get), 2);
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
            let before = market.try_to_vec().unwrap();
            HLP_COMPACT_GUIDANCE_CELLS.with(|count| count.set(0));
            HLP_COMPACT_GUIDANCE_EXACT_IN_PROBES.with(|count| count.set(0));
            let result = solve_concentrated_hlp_swap(&mut market, asset_in, 350_000 * scale);
            let cells = HLP_COMPACT_GUIDANCE_CELLS.with(Cell::get);
            let probes = HLP_COMPACT_GUIDANCE_EXACT_IN_PROBES.with(Cell::get);
            eprintln!(
                "compact-spot retain={retain_dynamic_surcharge} asset_in={asset_in:?} cells={cells} exact_in_probes={probes}"
            );
            assert!(cells > 0);
            assert!(probes <= cells.saturating_mul(MAX_RESIDUAL_PROBES_PER_VARIABLE_LEG as u32));
            if asset_in == MarketAsset::Quote {
                assert_eq!(result.unwrap_err(), error!(ErrorCode::HlpSettlementUnavailable));
                assert_eq!(market.try_to_vec().unwrap(), before);
                continue;
            }
            let (_, _, quote) = result
                .unwrap_or_else(|error| panic!("retain={retain_dynamic_surcharge} asset_in={asset_in:?}: {error:?}"));
            if retain_dynamic_surcharge {
                assert!(quote.fee.retained_surcharge > 0);
            }
        }
    }
}

#[test]
fn fixed_d_final_mark_interval_is_conservative_for_the_future_compact_scorer() {
    assert_eq!(hlp_guidance_final_mark_epsilon_nad(0), 0);
    assert_eq!(hlp_guidance_final_mark_epsilon_nad(1), 1);
    assert_eq!(hlp_guidance_final_mark_epsilon_nad(4), 1);
    assert_eq!(hlp_guidance_final_mark_epsilon_nad(5), 2);

    let budget_nad = 100_u128;
    let boundary = HlpGuidanceFinalMarkInterval::around(75, budget_nad).unwrap();
    assert_eq!(boundary.lower_nad, 50);
    assert_eq!(boundary.upper_nad, 100);
    assert!(boundary.wholly_inside_budget(budget_nad));

    let outside = HlpGuidanceFinalMarkInterval::around(76, budget_nad).unwrap();
    assert!(!outside.wholly_inside_budget(budget_nad));

    let ambiguous = HlpGuidanceFinalMarkInterval::around(20, budget_nad).unwrap();
    assert!(ambiguous.contains(0));
    assert!(!ambiguous.excludes_zero());

    let directional = HlpGuidanceFinalMarkInterval::around(26, budget_nad).unwrap();
    assert!(!directional.contains(0));
    assert!(directional.excludes_zero());
}

#[test]
fn compact_guidance_guard_covers_partial_backing_roundtrip_interest_and_borrow_policy() {
    let _verification = VerifyCompactHlpGuidanceGuard::enable();
    let scale = 1_000_000_u64;

    let mut partial = active_concentrated_hlp_market_with_decimals(6);
    constrain_side_cash_preserving_hlp_invariant(&mut partial, MarketAsset::Base, 5_000);
    constrain_side_cash_preserving_hlp_invariant(&mut partial, MarketAsset::Quote, 5_000);
    partial.checkpoint_amm_neutral_inventory(0).unwrap();
    for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = partial.clone();
        let (_, _, quote) = solve_concentrated_hlp_swap(&mut market, asset_in, 100_000 * scale)
            .unwrap_or_else(|error| panic!("partial asset_in={asset_in:?}: {error:?}"));
        assert!(quote.amount_out > 0);
    }

    let mut roundtrip = active_concentrated_hlp_market_with_decimals(6);
    let first = apply_concentrated_hlp_swap(&mut roundtrip, MarketAsset::Quote, 100_000 * scale).unwrap();
    let reverse =
        apply_concentrated_hlp_swap(&mut roundtrip, MarketAsset::Base, (first.amount_out / 2).max(1)).unwrap();
    assert!(reverse.amount_out > 0);

    for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = active_concentrated_hlp_market_with_decimals(6);
        let principal = 200_000 * scale;
        let unpaid_interest = 500 * scale;
        market.base_side.reserves.cash_reserve -= principal;
        market.debt.fixed_base_shares = principal as u128;
        market.debt.fixed_base_principal = principal;
        market.debt.base_borrow_index_nad =
            ((principal as u128 + unpaid_interest as u128) * NAD as u128) / principal as u128;
        market.base_side.reserves.live_reserve += unpaid_interest;
        HLP_COMPACT_GUIDANCE_CELLS.with(|count| count.set(0));
        reset_stage4b2a_test_state();
        let before = market.try_to_vec().unwrap();
        let result = solve_concentrated_hlp_swap(&mut market, asset_in, 100_000 * scale);
        // This asymmetric-interest oracle currently fails closed at exact A1.
        // Two bounded quote cells and four compact evaluations prove that the
        // center/axis scheduler ran first. The quote-axis trace is allocated
        // only after its cell/fingerprint comparison; a default trace proves
        // that comparison matched without a reflected-axis fallback.
        assert_eq!(result.unwrap_err(), error!(ErrorCode::HlpSettlementUnavailable));
        assert_eq!(market.try_to_vec().unwrap(), before);
        assert_eq!(HLP_COMPACT_GUIDANCE_CELLS.with(Cell::get), 2);
        assert_stage4c_counts(4, 0, 1);
        assert_eq!(
            HLP_STAGE4B2A_LAST_TRACE.with(|trace| *trace.borrow()),
            Some(HlpStage4B2aTestTrace::default())
        );
    }

    for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = active_concentrated_hlp_market_with_decimals(6);
        let reserve_credit = 100_000 * scale;
        let current_slot = curve_slot(&market);
        let pre_state = market.dynamic_fee_pre_state(current_slot).unwrap();
        let preliminary = market
            .preliminary_swap_inputs_for_state(reserve_credit, current_slot, pre_state)
            .unwrap();
        let (_, _, quote) = pre_solve_hlps_for_swap_joint(
            &mut market,
            asset_in,
            reserve_credit,
            current_slot,
            pre_state,
            preliminary,
            SwapCashPolicy::Borrow {
                asset: asset_in,
                amount: 25_000 * scale,
            },
        )
        .unwrap_or_else(|error| panic!("borrow asset_in={asset_in:?}: {error:?}"));
        assert!(quote.amount_out > 0);
    }
}

#[test]
fn compact_bounded_basis_quote_drives_the_same_fixed_endpoint_lifecycle() {
    let scale = 1_000_000_u64;
    let mut bounded_outputs = [[0_u64; 2]; 2];
    for with_public_interest in [false, true] {
        for retain_dynamic_surcharge in [false, true] {
            for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
                let mut market = active_concentrated_hlp_market_with_decimals(6);
                if with_public_interest {
                    let curve_before = market.curve_reserves_nad().unwrap();
                    let principal = 200_000 * scale;
                    let unpaid_interest = 500 * scale;
                    market.base_side.reserves.cash_reserve -= principal;
                    market.debt.fixed_base_shares = principal as u128;
                    market.debt.fixed_base_principal = principal;
                    market.debt.base_borrow_index_nad =
                        ((principal as u128 + unpaid_interest as u128) * NAD as u128) / principal as u128;
                    market.base_side.reserves.live_reserve += unpaid_interest;
                    assert_eq!(market.curve_reserves_nad().unwrap(), curve_before);
                    assert_eq!(
                        market.unrealized_interest(MarketAsset::Base).unwrap(),
                        unpaid_interest as u128,
                    );
                }
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
                    guidance_start_fixed: HlpPlannerStatic::capture(&market).unwrap(),
                };

                // The immutable anchor is captured from the operation start. The
                // mutable state may later carry a different candidate supply; do
                // not recapture this static from a prepositioned candidate.
                let fixed = HlpPlannerStatic::capture(&market).unwrap();
                let state = HlpPlannerState::capture(&market);
                let basis = ConcentratedGuidanceBasis::capture(&market, &context).unwrap();
                HLP_COMPACT_GUIDANCE_CELLS.with(|count| count.set(0));
                HLP_COMPACT_GUIDANCE_EXACT_IN_PROBES.with(|count| count.set(0));
                HLP_COMPACT_GUIDANCE_BRACKET_QUOTES.with(|count| count.set(0));
                HLP_COMPACT_GUIDANCE_STRUCTURAL_GAPS.with(|count| count.set(0));
                let bounded = basis
                    .quote_bounded(&market, &context, fixed, state, false)
                    .unwrap_or_else(|error| {
                        panic!("retain={retain_dynamic_surcharge} asset_in={asset_in:?}: {error:?}")
                    });
                assert_eq!(HLP_COMPACT_GUIDANCE_CELLS.with(Cell::get), 1);
                assert!((1..=MAX_RESIDUAL_PROBES_PER_VARIABLE_LEG)
                    .contains(&(HLP_COMPACT_GUIDANCE_EXACT_IN_PROBES.with(Cell::get) as usize)));
                assert_eq!(bounded.exact_in_mode, ConcentratedGuidanceExactInMode::Bracket);
                assert_eq!(HLP_COMPACT_GUIDANCE_BRACKET_QUOTES.with(Cell::get), 1);
                assert_eq!(HLP_COMPACT_GUIDANCE_STRUCTURAL_GAPS.with(Cell::get), 0);

                let mut exact_projection = None;
                project_concentrated_hlp_candidate(&market, &context, preliminary, false, &mut exact_projection)
                    .unwrap();
                let exact = exact_projection.unwrap().common();
                assert_eq!(bounded.common.amount_in_after_fee, exact.amount_in_after_fee);
                assert!(bounded.common.amount_out <= exact.amount_out);
                assert_eq!(bounded.start_ylp_supply, state.base_side.ylp_supply);
                if !with_public_interest {
                    bounded_outputs[usize::from(retain_dynamic_surcharge)]
                        [usize::from(asset_in == MarketAsset::Quote)] = bounded.common.amount_out;
                }
                if retain_dynamic_surcharge {
                    assert!(bounded.common.retained_surcharge > 0);
                    assert!(bounded.reserve.invariant_d() >= bounded.trade.invariant_d());
                }

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
                    trade_start_price_nad: bounded.common.start_price_nad,
                    start_checkpoint: None,
                    endpoints: HlpLifecycleEndpointMode::Guidance(endpoints),
                    expected_trade_price_nad: bounded.common.end_price_nad,
                    expected_reserve_price_nad: bounded.common.reserve_end_price_nad,
                };
                let preposition = HlpCandidatePreposition {
                    base_receipt: empty_hlp_rebalance_receipt(MarketAsset::Base),
                    quote_receipt: empty_hlp_rebalance_receipt(MarketAsset::Quote),
                    base_settlement_mode: HlpGuidanceSettlementSampleMode::None,
                    quote_settlement_mode: HlpGuidanceSettlementSampleMode::None,
                    base_settlement_d_action: None,
                    quote_settlement_d_action: None,
                    base_topology: HlpPackedPlanTopology::inactive(),
                    quote_topology: HlpPackedPlanTopology::inactive(),
                    preliminary,
                };
                let compact =
                    compact_hlp_lifecycle_tracking_from_state(&market, &context, &args, fixed, state, preposition)
                        .unwrap();

                let mut frozen_cached = None;
                for freeze_post_rebalance_mark in [false, true] {
                    CACHED_HLP_LIFECYCLE_RESULT.with(|result| {
                        result.borrow_mut().take();
                    });
                    let mut cached_state = state;
                    let mut settlement_trace = HlpGuidanceSettlementSampleTrace {
                        pre_base: preposition.base_settlement_mode,
                        pre_quote: preposition.quote_settlement_mode,
                        post_base: HlpGuidanceSettlementSampleMode::None,
                        post_quote: HlpGuidanceSettlementSampleMode::None,
                    };
                    let mut structural_topology = HlpStructuralTopologyTrace {
                        pre_base: preposition.base_topology,
                        pre_quote: preposition.quote_topology,
                        ..HlpStructuralTopologyTrace::default()
                    };
                    let mut guidance_d_actions = HlpGuidanceDActionTrace::default();
                    let cached_tracking = compact_hlp_lifecycle_tracking_stream(
                        &context,
                        fixed,
                        &mut cached_state,
                        &preposition,
                        bounded.common,
                        &endpoints,
                        &mut settlement_trace,
                        &mut guidance_d_actions,
                        &mut structural_topology,
                        HlpGuidanceSettlementProbeMode::Bounded,
                        freeze_post_rebalance_mark,
                    )
                    .unwrap();
                    let cached = CACHED_HLP_LIFECYCLE_RESULT
                        .with(|result| result.borrow_mut().take())
                        .expect("cached lifecycle witness");
                    assert_eq!(cached.state, cached_state);
                    assert_eq!(cached.tracking, cached_tracking);

                    let uncached = compact_hlp_lifecycle_tracking_from_state_with_options(
                        &market,
                        &context,
                        &args,
                        fixed,
                        state,
                        preposition,
                        HlpGuidanceSettlementProbeMode::Bounded,
                        freeze_post_rebalance_mark,
                    )
                    .unwrap();
                    assert_eq!(
                    cached, uncached,
                    "cached/uncached lifecycle diverged retain={retain_dynamic_surcharge} asset_in={asset_in:?} freeze={freeze_post_rebalance_mark}",
                );
                    if freeze_post_rebalance_mark {
                        frozen_cached = Some(cached);
                    } else {
                        assert_eq!(cached, compact);
                    }
                }
                let mut scratch = Market::default();
                let full = scratch_authoritative_result_preserving_preposition(&mut scratch, &market, &context, &args)
                    .unwrap();
                assert_eq!(compact, full);
                let frozen_cached = frozen_cached.expect("frozen cached lifecycle witness");
                assert_eq!(frozen_cached.state, full.state);
                assert_eq!(frozen_cached.base_post_receipt, full.base_post_receipt);
                assert_eq!(frozen_cached.quote_post_receipt, full.quote_post_receipt);
                assert_eq!(frozen_cached.transition, full.transition);
                assert_eq!(
                    (
                        frozen_cached.tracking.base_trade_error_nad,
                        frozen_cached.tracking.base_reserve_error_nad,
                        frozen_cached.tracking.base_retained_contribution_nad,
                        frozen_cached.tracking.quote_trade_error_nad,
                        frozen_cached.tracking.quote_reserve_error_nad,
                        frozen_cached.tracking.quote_retained_contribution_nad,
                    ),
                    (
                        full.tracking.base_trade_error_nad,
                        full.tracking.base_reserve_error_nad,
                        full.tracking.base_retained_contribution_nad,
                        full.tracking.quote_trade_error_nad,
                        full.tracking.quote_reserve_error_nad,
                        full.tracking.quote_retained_contribution_nad,
                    ),
                );
            }
        }
    }
    assert_eq!(bounded_outputs[0], bounded_outputs[1]);
}

#[test]
fn compact_structural_gap_quote_drives_the_same_fixed_endpoint_lifecycle() {
    let market = active_concentrated_hlp_market_with_decimals(6);
    let asset_in = MarketAsset::Quote;
    let reserve_credit = 3;
    let current_slot = curve_slot(&market);
    assert_eq!(market.current_curve_center_price_nad().unwrap(), 2 * NAD);
    let pre_state = market.dynamic_fee_pre_state(current_slot).unwrap();
    let preliminary = market
        .preliminary_swap_inputs_for_state(reserve_credit, current_slot, pre_state)
        .unwrap();
    assert_eq!(preliminary.amount_in_for_quote, reserve_credit);
    let start_reserves = market.curve_reserves_nad().unwrap();
    let start_prepared = market
        .prepare_curve_for_reserves_nad(
            start_reserves,
            market.current_curve_center_price_nad().unwrap(),
            current_slot,
        )
        .unwrap();
    let frozen_prices = hlp_curve_prices_from_base_price_nad(start_prepared.marginal_price_nad().unwrap()).unwrap();
    let fixed = HlpPlannerStatic::capture(&market).unwrap();
    let state = HlpPlannerState::capture(&market);
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
        guidance_start_fixed: fixed,
    };
    let basis = ConcentratedGuidanceBasis::capture(&market, &context).unwrap();

    HLP_COMPACT_GUIDANCE_CELLS.with(|count| count.set(0));
    HLP_COMPACT_GUIDANCE_EXACT_IN_PROBES.with(|count| count.set(0));
    HLP_COMPACT_GUIDANCE_BRACKET_QUOTES.with(|count| count.set(0));
    HLP_COMPACT_GUIDANCE_STRUCTURAL_GAPS.with(|count| count.set(0));
    let bounded = basis.quote_bounded(&market, &context, fixed, state, false).unwrap();

    assert_eq!(bounded.exact_in_mode, ConcentratedGuidanceExactInMode::StructuralGap);
    assert_eq!(bounded.common.amount_out, 1);
    assert_eq!(HLP_COMPACT_GUIDANCE_CELLS.with(Cell::get), 1);
    assert_eq!(HLP_COMPACT_GUIDANCE_EXACT_IN_PROBES.with(Cell::get), 0);
    assert_eq!(HLP_COMPACT_GUIDANCE_BRACKET_QUOTES.with(Cell::get), 0);
    assert_eq!(HLP_COMPACT_GUIDANCE_STRUCTURAL_GAPS.with(Cell::get), 1);

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
        trade_start_price_nad: bounded.common.start_price_nad,
        start_checkpoint: None,
        endpoints: HlpLifecycleEndpointMode::Guidance(endpoints),
        expected_trade_price_nad: bounded.common.end_price_nad,
        expected_reserve_price_nad: bounded.common.reserve_end_price_nad,
    };
    let compact = compact_hlp_lifecycle_tracking_from_state(
        &market,
        &context,
        &args,
        fixed,
        state,
        HlpCandidatePreposition {
            base_receipt: empty_hlp_rebalance_receipt(MarketAsset::Base),
            quote_receipt: empty_hlp_rebalance_receipt(MarketAsset::Quote),
            base_settlement_mode: HlpGuidanceSettlementSampleMode::None,
            quote_settlement_mode: HlpGuidanceSettlementSampleMode::None,
            base_settlement_d_action: None,
            quote_settlement_d_action: None,
            base_topology: HlpPackedPlanTopology::inactive(),
            quote_topology: HlpPackedPlanTopology::inactive(),
            preliminary,
        },
    )
    .unwrap();
    let mut scratch = Market::default();
    let full = scratch_authoritative_result_preserving_preposition(&mut scratch, &market, &context, &args).unwrap();
    assert_eq!(compact, full);
}

#[test]
fn compact_bounded_basis_keeps_operation_anchor_across_mint_and_burn_prepositions() {
    let scale = 1_000_000_u64;
    let delta = 10_000_i128 * NAD as i128;
    for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
        let operation_start = active_concentrated_hlp_market_with_decimals(6);
        let current_slot = curve_slot(&operation_start);
        let reserve_credit = 100_000 * scale;
        let pre_state = operation_start.dynamic_fee_pre_state(current_slot).unwrap();
        let preliminary = operation_start
            .preliminary_swap_inputs_for_state(reserve_credit, current_slot, pre_state)
            .unwrap();
        let start_reserves = operation_start.curve_reserves_nad().unwrap();
        let start_prepared = operation_start
            .prepare_curve_for_reserves_nad(
                start_reserves,
                operation_start.current_curve_center_price_nad().unwrap(),
                current_slot,
            )
            .unwrap();
        let frozen_prices = hlp_curve_prices_from_base_price_nad(start_prepared.marginal_price_nad().unwrap()).unwrap();
        let context = ConcentratedHlpSolveContext {
            base_start: concentrated_hlp_start(&operation_start, MarketAsset::Base, frozen_prices).unwrap(),
            quote_start: concentrated_hlp_start(&operation_start, MarketAsset::Quote, frozen_prices).unwrap(),
            frozen_prices,
            asset_in,
            reserve_credit,
            current_slot,
            pre_state,
            preliminary,
            cash_policy: SwapCashPolicy::Spot,
            guidance_start_prepared: start_prepared,
            guidance_start_ylp_supply: operation_start.base_side.shares.ylp_supply,
            guidance_start_fixed: HlpPlannerStatic::capture(&operation_start).unwrap(),
        };
        let operation_fixed = HlpPlannerStatic::capture(&operation_start).unwrap();
        let basis = ConcentratedGuidanceBasis::capture(&operation_start, &context).unwrap();

        for (target_asset, requested_delta) in [
            (MarketAsset::Base, delta),
            (MarketAsset::Base, -delta),
            (MarketAsset::Quote, delta),
            (MarketAsset::Quote, -delta),
        ] {
            let mut candidate = operation_start.clone();
            let (base_delta, quote_delta) = match target_asset {
                MarketAsset::Base => (requested_delta, 0),
                MarketAsset::Quote => (0, requested_delta),
            };
            let preposition = apply_hlp_candidate_preposition(
                &mut candidate,
                &context,
                SwapCashFloors::default(),
                base_delta,
                quote_delta,
            )
            .unwrap_or_else(|error| {
                panic!("asset_in={asset_in:?} target={target_asset:?} delta={requested_delta}: {error:?}")
            });
            let receipt = match target_asset {
                MarketAsset::Base => preposition.base_receipt,
                MarketAsset::Quote => preposition.quote_receipt,
            };
            if requested_delta > 0 {
                assert!(receipt.ylp_mint_amount > 0);
            } else {
                assert!(receipt.ylp_burn_amount > 0);
            }
            let candidate_state = HlpPlannerState::capture(&candidate);
            assert_ne!(candidate_state.base_side.ylp_supply, operation_fixed.start_ylp_supply);

            let bounded = basis
                .quote_bounded(&candidate, &context, operation_fixed, candidate_state, true)
                .unwrap_or_else(|error| {
                    panic!("bounded asset_in={asset_in:?} target={target_asset:?} delta={requested_delta}: {error:?}")
                });
            let mut exact_projection = None;
            project_concentrated_hlp_candidate(
                &candidate,
                &context,
                preposition.preliminary,
                false,
                &mut exact_projection,
            )
            .unwrap();
            let exact = exact_projection.unwrap().common();
            assert_eq!(bounded.common.amount_in_after_fee, exact.amount_in_after_fee);
            assert!(bounded.common.amount_out <= exact.amount_out);

            let endpoints = HlpGuidanceEndpointCapability {
                current_slot,
                curve_revision: candidate.curve_revision,
                center_price_nad: candidate.current_curve_center_price_nad().unwrap(),
                parameters: candidate.current_curve_parameters(current_slot),
                retain_dynamic_surcharge: bounded.retain_dynamic_surcharge,
                trade_prepared: bounded.trade,
                reserve_prepared: bounded.reserve,
            };
            let args = HlpAuthoritativeLifecycleArgs {
                amount_in_after_fee: bounded.common.amount_in_after_fee,
                retained_surcharge: bounded.common.retained_surcharge,
                amount_out: bounded.common.amount_out,
                trade_start_price_nad: bounded.common.start_price_nad,
                start_checkpoint: None,
                endpoints: HlpLifecycleEndpointMode::Guidance(endpoints),
                expected_trade_price_nad: bounded.common.end_price_nad,
                expected_reserve_price_nad: bounded.common.reserve_end_price_nad,
            };
            let compact = compact_hlp_lifecycle_tracking_from_state(
                &operation_start,
                &context,
                &args,
                operation_fixed,
                candidate_state,
                preposition,
            )
            .unwrap();
            let mut scratch = Market::default();
            let full =
                scratch_authoritative_result_preserving_preposition(&mut scratch, &candidate, &context, &args).unwrap();
            assert_eq!(compact, full);
        }
    }
}

fn reset_stage4b2a_test_state() {
    CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(|count| count.set(0));
    CONCENTRATED_PRE_SOLVE_COMPACT_EVALUATIONS.with(|count| count.set(0));
    CONCENTRATED_PRE_SOLVE_RAW_EVALUATIONS.with(|count| count.set(0));
    CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(|count| count.set(0));
    HLP_RAW_CANONICAL_SCALAR_CALLS.with(|count| count.set(0));
    HLP_RAW_CANONICAL_SCALAR_RESIDUALS.with(|count| count.set(0));
    HLP_STAGE4B2A_LAST_TRACE.with(|trace| *trace.borrow_mut() = None);
}

fn stage4b2a_last_trace() -> HlpStage4B2aTestTrace {
    HLP_STAGE4B2A_LAST_TRACE.with(|trace| trace.borrow().expect("Stage4B2a trace"))
}

fn assert_stage4c_rawless_success_trace(
    expected_a1: (i128, i128),
    expected_a2: (i128, i128),
    expected_a2_rows: [i128; 6],
    expected_post_kinds: (HlpPlanTopologyKind, HlpPlanTopologyKind),
) {
    let trace = stage4b2a_last_trace();
    assert_eq!(trace.a1_coordinate, Some(expected_a1));
    assert_eq!(trace.raw_coordinate, None);
    assert_eq!(trace.raw_rows, None);
    assert_eq!(trace.raw_topology, None);
    assert_eq!(trace.raw_signature, None);
    assert_eq!(trace.raw_robust, None);
    assert_eq!(trace.a2_coordinate, Some(expected_a2));
    assert_eq!(trace.a2_rows, Some(expected_a2_rows));
    assert_eq!(trace.a1_topology, trace.a2_topology);
    let a1_signature = trace.a1_signature.expect("exact A1 signature");
    let a2_signature = trace.a2_signature.expect("exact A2 signature");
    assert!(hlp_preposition_signature_class_matches(
        a1_signature.base,
        a2_signature.base,
    ));
    assert!(hlp_preposition_signature_class_matches(
        a1_signature.quote,
        a2_signature.quote,
    ));
    let topology = trace.a2_topology.expect("exact A2 topology");
    assert_eq!(topology.post_base.kind(), expected_post_kinds.0);
    assert_eq!(topology.post_quote.kind(), expected_post_kinds.1);
}

fn assert_stage4c_counts(compact: u32, raw: u32, authorities: u32) {
    assert_eq!(CONCENTRATED_PRE_SOLVE_COMPACT_EVALUATIONS.with(Cell::get), compact);
    assert_eq!(CONCENTRATED_PRE_SOLVE_RAW_EVALUATIONS.with(Cell::get), raw);
    assert_eq!(
        CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(Cell::get),
        authorities
    );
    assert_eq!(
        CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(Cell::get),
        compact + authorities
    );
}

fn stage4b2a_spot_market(retain_dynamic_surcharge: bool) -> Market {
    let mut market = active_concentrated_hlp_market_with_decimals(6);
    market.config.divergence_fee_share_cap_bps = 2_000;
    market.config.amm.divergence_fee_coefficient_nad = 10 * NAD;
    market.amm.retain_dynamic_surcharge = retain_dynamic_surcharge;
    market
}

fn stage4c_signed_round_away_divisor(value: i128, divisor: u128) -> Option<i128> {
    if value == 0 || divisor == 0 {
        return None;
    }
    let magnitude = value
        .unsigned_abs()
        .checked_add(divisor.checked_sub(1)?)?
        .checked_div(divisor)?;
    let magnitude = i128::try_from(magnitude).ok()?;
    if value < 0 {
        magnitude.checked_neg()
    } else {
        Some(magnitude)
    }
}

fn stage4c_opposite_half_budget_target(current: i128, budget: u128) -> Option<i128> {
    let half = i128::try_from(budget.checked_div(2)?).ok()?;
    if current > 0 {
        half.checked_neg()
    } else if current < 0 {
        Some(half)
    } else {
        Some(0)
    }
}

/// Test-only, scalar-only seed. It integrates operation-start marginal
/// geometry without running a quote, checkpoint, or lifecycle.
fn stage4c_zero_quote_geometry_seed(
    compact: &HlpCompactSolveContext,
    context: &ConcentratedHlpSolveContext,
    reserve_credit: u64,
) -> ((i128, i128), u64, u64, u128) {
    let start = *compact.guidance.start.guidance();
    let start_base = compact
        .start_state
        .curve_reserve(compact.fixed, MarketAsset::Base)
        .unwrap();
    let start_quote = compact
        .start_state
        .curve_reserve(compact.fixed, MarketAsset::Quote)
        .unwrap();
    let start_reserves_nad = compact.start_state.curve_reserves_nad(compact.fixed).unwrap();
    let (divergence_surcharge, _) = divergence_surcharge_for_guidance(
        context.asset_in,
        compact.fixed.decimals(context.asset_in),
        reserve_credit,
        start_reserves_nad,
        context.pre_state,
        context.preliminary,
        compact.guidance.fee_config,
        &start,
    )
    .unwrap();
    let scalar_input = context
        .preliminary
        .amount_in_for_quote
        .checked_sub(divergence_surcharge)
        .unwrap();
    let input_nad = normalize_to_nad(scalar_input as u128, compact.fixed.decimals(context.asset_in)).unwrap();
    let start_input_price_nad = context.frozen_prices.for_asset(context.asset_in);
    let direction = match context.asset_in {
        MarketAsset::Base => ConcentratedSwapDirection::BaseToQuote,
        MarketAsset::Quote => ConcentratedSwapDirection::QuoteToBase,
    };

    let half_input = scalar_input / 2;
    let half_input_nad = normalize_to_nad(half_input as u128, compact.fixed.decimals(context.asset_in)).unwrap();
    let half_output_nad = mul_div_u128(half_input_nad, start_input_price_nad, NAD as u128).unwrap();
    let half_output =
        denormalize_from_nad_floor(half_output_nad, compact.fixed.decimals(context.asset_in.opposite())).unwrap();
    let (midpoint_base, midpoint_quote) = match context.asset_in {
        MarketAsset::Base => (
            start_base.checked_add(half_input).unwrap(),
            start_quote.checked_sub(half_output).unwrap(),
        ),
        MarketAsset::Quote => (
            start_base.checked_sub(half_output).unwrap(),
            start_quote.checked_add(half_input).unwrap(),
        ),
    };
    let midpoint = start
        .prepare_guidance_successor(
            normalize_to_nad(midpoint_base as u128, compact.fixed.base_decimals).unwrap(),
            normalize_to_nad(midpoint_quote as u128, compact.fixed.quote_decimals).unwrap(),
        )
        .unwrap();
    let midpoint_base_price_nad = midpoint.marginal_price_nad().unwrap();
    let midpoint_input_price_nad = match direction {
        ConcentratedSwapDirection::BaseToQuote => midpoint_base_price_nad,
        ConcentratedSwapDirection::QuoteToBase => {
            mul_div_u128(NAD as u128, NAD as u128, midpoint_base_price_nad).unwrap()
        }
    };
    let midpoint_output_nad = mul_div_u128(input_nad, midpoint_input_price_nad, NAD as u128).unwrap();
    let midpoint_output =
        denormalize_from_nad_floor(midpoint_output_nad, compact.fixed.decimals(context.asset_in.opposite())).unwrap();
    let (provisional_base, provisional_quote) = match context.asset_in {
        MarketAsset::Base => (
            start_base.checked_add(scalar_input).unwrap(),
            start_quote.checked_sub(midpoint_output).unwrap(),
        ),
        MarketAsset::Quote => (
            start_base.checked_sub(midpoint_output).unwrap(),
            start_quote.checked_add(scalar_input).unwrap(),
        ),
    };
    let provisional = start
        .prepare_guidance_successor(
            normalize_to_nad(provisional_base as u128, compact.fixed.base_decimals).unwrap(),
            normalize_to_nad(provisional_quote as u128, compact.fixed.quote_decimals).unwrap(),
        )
        .unwrap();
    let provisional_base_price_nad = provisional.marginal_price_nad().unwrap();
    let provisional_input_price_nad = match direction {
        ConcentratedSwapDirection::BaseToQuote => provisional_base_price_nad,
        ConcentratedSwapDirection::QuoteToBase => {
            mul_div_u128(NAD as u128, NAD as u128, provisional_base_price_nad).unwrap()
        }
    };
    let simpson_input_price_nad = start_input_price_nad
        .checked_add(midpoint_input_price_nad.checked_mul(4).unwrap())
        .and_then(|value| value.checked_add(provisional_input_price_nad))
        .unwrap()
        / 6;
    let output_nad = mul_div_u128(input_nad, simpson_input_price_nad, NAD as u128).unwrap();
    let output = denormalize_from_nad_floor(output_nad, compact.fixed.decimals(context.asset_in.opposite())).unwrap();
    let (endpoint_base, endpoint_quote) = match context.asset_in {
        MarketAsset::Base => (
            start_base.checked_add(scalar_input).unwrap(),
            start_quote.checked_sub(output).unwrap(),
        ),
        MarketAsset::Quote => (
            start_base.checked_sub(output).unwrap(),
            start_quote.checked_add(scalar_input).unwrap(),
        ),
    };

    // Interpolate the small Simpson output correction along the already
    // sampled marginal-price chord; this adds no residual/quote capability.
    let (midpoint_output_reserve, provisional_output_reserve, endpoint_output_reserve) = match context.asset_in {
        MarketAsset::Base => (midpoint_quote, provisional_quote, endpoint_quote),
        MarketAsset::Quote => (midpoint_base, provisional_base, endpoint_base),
    };
    let reserve_span = midpoint_output_reserve.abs_diff(provisional_output_reserve);
    let reserve_offset = endpoint_output_reserve
        .abs_diff(provisional_output_reserve)
        .min(reserve_span);
    let price_offset = mul_div_u128(
        midpoint_base_price_nad.abs_diff(provisional_base_price_nad),
        reserve_offset as u128,
        reserve_span as u128,
    )
    .unwrap();
    let endpoint_base_price_nad = if midpoint_base_price_nad >= provisional_base_price_nad {
        provisional_base_price_nad.checked_add(price_offset).unwrap()
    } else {
        provisional_base_price_nad.checked_sub(price_offset).unwrap()
    };
    let endpoint_prices = hlp_curve_prices_from_base_price_nad(endpoint_base_price_nad).unwrap();
    let coordinate = (
        if context.base_start.active {
            compact_concentrated_hlp_needed_delta(
                compact.fixed,
                compact.start_state,
                compact.start_state,
                MarketAsset::Base,
                context.frozen_prices,
                endpoint_prices,
                endpoint_base,
                endpoint_quote,
                context.base_start.tracking.principal_nav_nad,
            )
            .unwrap()
        } else {
            0
        },
        if context.quote_start.active {
            compact_concentrated_hlp_needed_delta(
                compact.fixed,
                compact.start_state,
                compact.start_state,
                MarketAsset::Quote,
                context.frozen_prices,
                endpoint_prices,
                endpoint_base,
                endpoint_quote,
                context.quote_start.tracking.principal_nav_nad,
            )
            .unwrap()
        } else {
            0
        },
    );
    (coordinate, scalar_input, output, endpoint_base_price_nad)
}

/// Test-only characterization of frozen-center scalar cells using the same
/// bounded exact-out settlement mode as the full projected center. The
/// frozen quote and all three prepared-curve marks remain unchanged.
#[allow(clippy::too_many_arguments)]
fn evaluate_stage4c_frozen_center_bounded_axis(
    compact: &HlpCompactSolveContext,
    context: &ConcentratedHlpSolveContext,
    cash_floors: SwapCashFloors,
    base_delta_nad: i128,
    quote_delta_nad: i128,
    frozen: &HlpFrozenA1Projection,
    candidate_out: &mut Option<ConcentratedHlpCandidate>,
) -> Result<()> {
    CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(|count| count.set(count.get().saturating_add(1)));
    CONCENTRATED_PRE_SOLVE_COMPACT_EVALUATIONS.with(|count| count.set(count.get().saturating_add(1)));
    let (mut state, mut preposition, inventory_changed) = apply_compact_hlp_candidate_preposition(
        compact,
        context,
        cash_floors,
        base_delta_nad,
        quote_delta_nad,
        HlpGuidanceSettlementProbeMode::Bounded,
    )?;
    require_eq!(
        frozen.retain_dynamic_surcharge,
        compact.fixed.retain_dynamic_surcharge(inventory_changed),
        ErrorCode::BrokenInvariant
    );
    let (common, endpoints, anchor_d_actions) = frozen_cell_projection(compact, context, state, frozen)?;
    let required_cash_floors =
        context
            .cash_policy
            .floors_from_planner(compact.fixed, state, context.asset_in, common.amount_out)?;
    let settlement_cash_available = required_cash_floors.available_from_planner(state);
    let start_prices = hlp_curve_prices_from_base_price_nad(common.start_price_nad as u128)?;
    refresh_compact_hlp_candidate_preposition(
        compact.fixed,
        &mut state,
        context,
        &mut preposition,
        base_delta_nad,
        quote_delta_nad,
        start_prices,
    )?;
    let mut guidance_settlement_trace = HlpGuidanceSettlementSampleTrace {
        pre_base: preposition.base_settlement_mode,
        pre_quote: preposition.quote_settlement_mode,
        post_base: HlpGuidanceSettlementSampleMode::None,
        post_quote: HlpGuidanceSettlementSampleMode::None,
    };
    let mut guidance_d_actions = HlpGuidanceDActionTrace {
        anchors: anchor_d_actions,
        pre_base: preposition.base_settlement_d_action,
        pre_quote: preposition.quote_settlement_d_action,
        ..HlpGuidanceDActionTrace::default()
    };
    let mut structural_topology = HlpStructuralTopologyTrace {
        pre_base: preposition.base_topology,
        pre_quote: preposition.quote_topology,
        ..HlpStructuralTopologyTrace::default()
    };
    let lifecycle = compact_hlp_lifecycle_tracking_stream(
        context,
        compact.fixed,
        &mut state,
        &preposition,
        common,
        &endpoints,
        &mut guidance_settlement_trace,
        &mut guidance_d_actions,
        &mut structural_topology,
        HlpGuidanceSettlementProbeMode::Bounded,
        true,
    )?;
    let base_tracking_error_nad = lifecycle
        .base_error_nad
        .checked_sub(lifecycle.base_retained_contribution_nad)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let quote_tracking_error_nad = lifecycle
        .quote_error_nad
        .checked_sub(lifecycle.quote_retained_contribution_nad)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    preposition.base_receipt.tracking_retained_contribution_nad = lifecycle.base_retained_contribution_nad;
    preposition.quote_receipt.tracking_retained_contribution_nad = lifecycle.quote_retained_contribution_nad;
    *candidate_out = Some(ConcentratedHlpCandidate {
        base_receipt: preposition.base_receipt,
        quote_receipt: preposition.quote_receipt,
        authoritative: None,
        guidance_exact_in_mode: None,
        guidance_settlement_trace: Some(guidance_settlement_trace),
        guidance_d_actions: Some(guidance_d_actions),
        structural_topology,
        base_principal_tracking_error_nad: lifecycle.base_principal_error_nad,
        quote_principal_tracking_error_nad: lifecycle.quote_principal_error_nad,
        base_tracking_error_nad,
        quote_tracking_error_nad,
        base_trade_tracking_error_nad: lifecycle.base_trade_error_nad,
        quote_trade_tracking_error_nad: lifecycle.quote_trade_error_nad,
        base_reserve_tracking_error_nad: lifecycle.base_reserve_error_nad,
        quote_reserve_tracking_error_nad: lifecycle.quote_reserve_error_nad,
        base_endpoint_exposure_nad: lifecycle.base_exposure_nad,
        quote_endpoint_exposure_nad: lifecycle.quote_exposure_nad,
        base_trade_endpoint_safe: concentrated_hlp_trade_endpoint_is_safe(
            context.base_start,
            lifecycle.base_trade_error_nad,
        ),
        quote_trade_endpoint_safe: concentrated_hlp_trade_endpoint_is_safe(
            context.quote_start,
            lifecycle.quote_trade_error_nad,
        ),
        reserve_endpoint_safe: concentrated_hlp_reserve_is_safe(
            context.base_start,
            lifecycle.base_trade_error_nad,
            lifecycle.base_reserve_error_nad,
        ) && concentrated_hlp_reserve_is_safe(
            context.quote_start,
            lifecycle.quote_trade_error_nad,
            lifecycle.quote_reserve_error_nad,
        ),
        settlement_cash_available,
        next_base_delta_nad: base_delta_nad,
        next_quote_delta_nad: quote_delta_nad,
    });
    Ok(())
}

/// Test-only low-cell schedule: two full projected cells establish P0 and a
/// fresh center; the center's complete bounded quote/marks are then frozen
/// into two raw scalar axes before the unchanged exact-A1/raw/exact-A2 tail.
fn assert_stage4c_center_fresh_active(
    snapshot: Market,
    asset_in: MarketAsset,
    label: &str,
    expect_axis_trace_match: Option<bool>,
    bounded_axes: bool,
    axis_divisor: i128,
    rawless_expected_safe: Option<bool>,
) {
    assert_stage4c_center_fresh_active_with_credit(
        snapshot,
        asset_in,
        350_000 * 1_000_000,
        label,
        expect_axis_trace_match,
        bounded_axes,
        axis_divisor,
        rawless_expected_safe,
        None,
    );
}

#[allow(clippy::too_many_arguments)]
fn assert_stage4c_center_fresh_active_with_credit(
    mut snapshot: Market,
    asset_in: MarketAsset,
    reserve_credit: u64,
    label: &str,
    expect_axis_trace_match: Option<bool>,
    bounded_axes: bool,
    axis_divisor: i128,
    rawless_expected_safe: Option<bool>,
    frozen_a1_local_expected_safe: Option<bool>,
) {
    assert_ne!(axis_divisor, 0);
    let current_slot = curve_slot(&snapshot);
    snapshot.prepare_amm_for_swap(current_slot).unwrap();
    let pre_state = snapshot.dynamic_fee_pre_state(current_slot).unwrap();
    let preliminary = snapshot
        .preliminary_swap_inputs_for_state(reserve_credit, current_slot, pre_state)
        .unwrap();
    let mut context = None;
    capture_concentrated_hlp_solve_context_into(
        &snapshot,
        asset_in,
        reserve_credit,
        current_slot,
        pre_state,
        preliminary,
        SwapCashPolicy::Spot,
        &mut context,
    )
    .unwrap();
    let context = context.unwrap();
    assert!(context.base_start.active);
    let active_axis_count = u8::from(context.base_start.active) + u8::from(context.quote_start.active);
    let mut compact = None;
    HlpCompactSolveContext::capture_into(&snapshot, &context, &mut compact).unwrap();
    let compact = compact.unwrap();
    let cash_floors = context.cash_policy.floors(&snapshot, asset_in, 0).unwrap();

    reset_stage4b2a_test_state();
    HLP_COMPACT_GUIDANCE_EXACT_IN_PROBES.with(|count| count.set(0));
    HLP_COMPACT_GUIDANCE_EXACT_OUT_PROBES.with(|count| count.set(0));
    HLP_RAW_CANONICAL_SCALAR_CALLS.with(|count| count.set(0));
    HLP_RAW_CANONICAL_SCALAR_RESIDUALS.with(|count| count.set(0));

    let mut p0_projection = None;
    let mut p0_candidate = None;
    evaluate_compact_concentrated_hlp_candidate(
        &compact,
        &context,
        cash_floors,
        0,
        0,
        false,
        &mut p0_projection,
        &mut p0_candidate,
    )
    .unwrap();
    let p0_candidate = p0_candidate.unwrap();
    let center_coordinate = (p0_candidate.next_base_delta_nad, p0_candidate.next_quote_delta_nad);
    assert_ne!(center_coordinate, (0, 0));
    let p0_in = HLP_COMPACT_GUIDANCE_EXACT_IN_PROBES.with(Cell::get);
    let p0_out = HLP_COMPACT_GUIDANCE_EXACT_OUT_PROBES.with(Cell::get);

    let center_in_before = HLP_COMPACT_GUIDANCE_EXACT_IN_PROBES.with(Cell::get);
    let center_out_before = HLP_COMPACT_GUIDANCE_EXACT_OUT_PROBES.with(Cell::get);
    let mut center_projection = None;
    let mut center_candidate = None;
    evaluate_compact_concentrated_hlp_candidate(
        &compact,
        &context,
        cash_floors,
        center_coordinate.0,
        center_coordinate.1,
        true,
        &mut center_projection,
        &mut center_candidate,
    )
    .unwrap();
    let center_candidate = center_candidate.unwrap();
    let center_in = HLP_COMPACT_GUIDANCE_EXACT_IN_PROBES
        .with(Cell::get)
        .saturating_sub(center_in_before);
    let center_out = HLP_COMPACT_GUIDANCE_EXACT_OUT_PROBES
        .with(Cell::get)
        .saturating_sub(center_out_before);
    assert!(center_candidate.settlement_cash_available);
    assert!(hlp_exact_derivative_sample_is_unbound(&center_candidate, &context,));
    let center_topology = center_candidate.structural_topology;
    let center_trace = center_candidate.guidance_settlement_trace.unwrap();
    let center_signature = hlp_preposition_pair_signature(&center_candidate);
    let center_rows = HlpExactSampleRows::from_candidate(&center_candidate);
    let base_row = hlp_exact_control_row(&center_candidate, MarketAsset::Base, context.base_start).unwrap();
    let quote_row = hlp_exact_control_row(&center_candidate, MarketAsset::Quote, context.quote_start).unwrap();
    let center_base_error = center_rows.value(MarketAsset::Base, base_row);
    let center_quote_error = center_rows.value(MarketAsset::Quote, quote_row);

    let ConcentratedHlpProjection::FrozenCenter(frozen_center_projection) = center_projection.unwrap() else {
        panic!("{label}: center must freeze bounded guidance")
    };
    let exact_in_mode = frozen_center_projection.exact_in_mode;
    let frozen_center = frozen_center_projection.frozen;

    // The finite-difference origin and axes must score the same scalar
    // function. Re-evaluate the exact center through its frozen payload and
    // bind every lifecycle row and structural fingerprint before forming J.
    let mut frozen_center_candidate = None;
    evaluate_frozen_center_axis_concentrated_hlp_candidate(
        &compact,
        &context,
        cash_floors,
        center_coordinate.0,
        center_coordinate.1,
        &frozen_center_projection,
        &mut frozen_center_candidate,
    )
    .unwrap();
    let frozen_center_candidate = frozen_center_candidate.unwrap();
    assert_eq!(
        hlp_stage4b2a_test_rows(&frozen_center_candidate),
        hlp_stage4b2a_test_rows(&center_candidate),
        "{label}: frozen-center rows changed at the origin",
    );
    assert_eq!(frozen_center_candidate.structural_topology, center_topology);
    assert_eq!(
        frozen_center_candidate.guidance_settlement_trace,
        center_candidate.guidance_settlement_trace,
    );
    assert_eq!(
        frozen_center_candidate.guidance_exact_in_mode,
        center_candidate.guidance_exact_in_mode,
    );
    assert_eq!(frozen_center_candidate.base_receipt, center_candidate.base_receipt);
    assert_eq!(frozen_center_candidate.quote_receipt, center_candidate.quote_receipt);
    assert_eq!(
        hlp_preposition_pair_signature(&frozen_center_candidate),
        center_signature,
    );
    // This extra witness is test-only and is excluded from the conceptual
    // production schedule counters asserted below.
    CONCENTRATED_PRE_SOLVE_COMPACT_EVALUATIONS.with(|count| count.set(count.get() - 1));
    CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(|count| count.set(count.get() - 1));

    let base_probe_delta = signed_mul_div_round_i128(
        hlp_exact_axis_probe_delta(center_coordinate.0, context.base_start, center_base_error).unwrap(),
        1,
        axis_divisor,
    )
    .unwrap();
    let quote_probe_delta = signed_mul_div_round_i128(
        hlp_exact_axis_probe_delta(center_coordinate.1, context.quote_start, center_quote_error).unwrap(),
        1,
        axis_divisor,
    )
    .unwrap();
    let base_axis_coordinate = (center_coordinate.0 + base_probe_delta, center_coordinate.1);
    let quote_axis_coordinate = (center_coordinate.0, center_coordinate.1 + quote_probe_delta);
    eprintln!(
        "CENTER-FRESH AXIS CELL {label} divisor={axis_divisor} center={center_coordinate:?} base_axis={base_axis_coordinate:?} quote_axis={quote_axis_coordinate:?}"
    );
    let axes_out_before = HLP_COMPACT_GUIDANCE_EXACT_OUT_PROBES.with(Cell::get);
    let axes_scalar_calls_before = HLP_RAW_CANONICAL_SCALAR_CALLS.with(Cell::get);
    let axes_scalar_residuals_before = HLP_RAW_CANONICAL_SCALAR_RESIDUALS.with(Cell::get);
    let mut base_axis_candidate = None;
    if bounded_axes {
        evaluate_stage4c_frozen_center_bounded_axis(
            &compact,
            &context,
            cash_floors,
            base_axis_coordinate.0,
            base_axis_coordinate.1,
            &frozen_center,
            &mut base_axis_candidate,
        )
        .unwrap();
    } else {
        evaluate_frozen_a1_raw_concentrated_hlp_candidate(
            &compact,
            &context,
            cash_floors,
            base_axis_coordinate.0,
            base_axis_coordinate.1,
            &frozen_center,
            &mut base_axis_candidate,
        )
        .unwrap();
    }
    let base_axis_candidate = base_axis_candidate.unwrap();
    let quote_axis_candidate = if context.quote_start.active {
        let mut candidate = None;
        if bounded_axes {
            evaluate_stage4c_frozen_center_bounded_axis(
                &compact,
                &context,
                cash_floors,
                quote_axis_coordinate.0,
                quote_axis_coordinate.1,
                &frozen_center,
                &mut candidate,
            )
            .unwrap();
        } else {
            evaluate_frozen_a1_raw_concentrated_hlp_candidate(
                &compact,
                &context,
                cash_floors,
                quote_axis_coordinate.0,
                quote_axis_coordinate.1,
                &frozen_center,
                &mut candidate,
            )
            .unwrap();
        }
        candidate.unwrap()
    } else {
        center_candidate
    };
    let axes_out = HLP_COMPACT_GUIDANCE_EXACT_OUT_PROBES
        .with(Cell::get)
        .saturating_sub(axes_out_before);
    let axes_scalar_calls = HLP_RAW_CANONICAL_SCALAR_CALLS
        .with(Cell::get)
        .saturating_sub(axes_scalar_calls_before);
    let axes_scalar_residuals = HLP_RAW_CANONICAL_SCALAR_RESIDUALS
        .with(Cell::get)
        .saturating_sub(axes_scalar_residuals_before);
    for (asset, axis) in [
        (MarketAsset::Base, &base_axis_candidate),
        (MarketAsset::Quote, &quote_axis_candidate),
    ] {
        let signature = hlp_preposition_pair_signature(axis);
        assert!(axis.settlement_cash_available, "{label}: {asset:?} axis cash");
        assert!(hlp_exact_derivative_sample_is_unbound(axis, &context));
        assert_eq!(axis.structural_topology, center_topology);
        let same_signature_class = |left: HlpPrepositionSignature, right: HlpPrepositionSignature| {
            (left.ylp_mint_amount > 0) == (right.ylp_mint_amount > 0)
                && (left.ylp_burn_amount > 0) == (right.ylp_burn_amount > 0)
                && left.debt_delta.signum() == right.debt_delta.signum()
        };
        assert!(same_signature_class(center_signature.base, signature.base));
        assert!(same_signature_class(center_signature.quote, signature.quote));
    }
    let base_axis_trace = base_axis_candidate.guidance_settlement_trace.unwrap();
    let quote_axis_trace = quote_axis_candidate.guidance_settlement_trace.unwrap();
    eprintln!(
        "CENTER-FRESH {label} P0={center_coordinate:?} center_rows={:?} center_mode={exact_in_mode:?} center_topology={center_topology:?} center_trace={center_trace:?} center_signature={center_signature:?} center_d={:?} base_axis={base_axis_coordinate:?}/{:?}/trace={base_axis_trace:?}/signature={:?}/d={:?}/d_match={} quote_axis={quote_axis_coordinate:?}/{:?}/trace={quote_axis_trace:?}/signature={:?}/d={:?}/d_match={} active_axes={active_axis_count} probes=(p0_in={p0_in},p0_out={p0_out},center_in={center_in},center_out={center_out},axes_out={axes_out},axes_scalar_calls={axes_scalar_calls},axes_scalar_residuals={axes_scalar_residuals})",
        hlp_stage4b2a_test_rows(&center_candidate),
        center_candidate.guidance_d_actions,
        hlp_stage4b2a_test_rows(&base_axis_candidate),
        hlp_preposition_pair_signature(&base_axis_candidate),
        base_axis_candidate.guidance_d_actions,
        base_axis_candidate.guidance_d_actions == center_candidate.guidance_d_actions,
        hlp_stage4b2a_test_rows(&quote_axis_candidate),
        hlp_preposition_pair_signature(&quote_axis_candidate),
        quote_axis_candidate.guidance_d_actions,
        quote_axis_candidate.guidance_d_actions == center_candidate.guidance_d_actions,
    );
    let axis_trace_matches = (!context.base_start.active || base_axis_trace == center_trace)
        && (!context.quote_start.active || quote_axis_trace == center_trace);
    let Some(expect_axis_trace_match) = expect_axis_trace_match else {
        return;
    };
    assert_eq!(axis_trace_matches, expect_axis_trace_match);
    if !axis_trace_matches {
        return;
    }

    let basis = HlpFiniteDifferenceBasis {
        origin: center_rows,
        base_probe_delta_nad: base_probe_delta,
        base_probe: HlpExactSampleRows::from_candidate(&base_axis_candidate),
        quote_probe_delta_nad: quote_probe_delta,
        quote_probe: HlpExactSampleRows::from_candidate(&quote_axis_candidate),
        base_probe_recorded: context.base_start.active,
        quote_probe_recorded: context.quote_start.active,
        ..HlpFiniteDifferenceBasis::default()
    };
    let zero_step = basis.solve_step(
        &context,
        center_coordinate.0,
        center_coordinate.1,
        base_row,
        quote_row,
        center_base_error,
        center_quote_error,
    );
    let reflected_step = zero_step
        .is_none()
        .then(|| {
            hlp_reflected_guidance_step(
                &context,
                center_coordinate.0,
                center_coordinate.1,
                center_candidate.next_base_delta_nad,
                center_candidate.next_quote_delta_nad,
                base_axis_candidate.next_base_delta_nad,
                quote_axis_candidate.next_quote_delta_nad,
            )
        })
        .flatten();
    let boundary_base_error = hlp_initial_active_set_residual(center_base_error, context.base_start).unwrap();
    let boundary_quote_error = hlp_initial_active_set_residual(center_quote_error, context.quote_start).unwrap();
    let (base_step, quote_step) = zero_step
        .or(reflected_step)
        .or_else(|| {
            basis.solve_step(
                &context,
                center_coordinate.0,
                center_coordinate.1,
                base_row,
                quote_row,
                boundary_base_error,
                boundary_quote_error,
            )
        })
        .unwrap();
    let a1_coordinate = (center_coordinate.0 + base_step, center_coordinate.1 + quote_step);
    assert!(hlp_coordinate_within_center_trust(
        center_coordinate.0,
        a1_coordinate.0,
        context.base_start,
    ));
    assert!(hlp_coordinate_within_center_trust(
        center_coordinate.1,
        a1_coordinate.1,
        context.quote_start,
    ));

    let mut lifecycle_scratch = Market::default();
    let mut a1_market = snapshot.clone();
    let mut a1_projection = None;
    let mut a1_candidate_slot = None;
    let mut a1_terminal_scratch = HlpTerminalSwapScratch::default();
    evaluate_concentrated_hlp_candidate(
        &mut a1_market,
        &mut lifecycle_scratch,
        &snapshot,
        &context,
        cash_floors,
        a1_coordinate.0,
        a1_coordinate.1,
        HlpCandidateEvaluationMode::Authoritative,
        &mut a1_projection,
        &mut a1_candidate_slot,
        &mut a1_terminal_scratch,
    )
    .unwrap();
    let a1_candidate = a1_candidate_slot.as_ref().unwrap();
    assert!(a1_candidate.settlement_cash_available);
    assert!(a1_candidate.reserve_endpoint_safe);
    assert!(hlp_exact_derivative_sample_is_unbound(&a1_candidate, &context));
    let a1_topology = a1_candidate.structural_topology;
    let a1_signature = hlp_preposition_pair_signature(&a1_candidate);
    assert_eq!(a1_topology, center_topology);
    assert!(hlp_preposition_signature_class_matches(
        center_signature.base,
        a1_signature.base,
    ));
    assert!(hlp_preposition_signature_class_matches(
        center_signature.quote,
        a1_signature.quote,
    ));
    let retry_base_row = hlp_exact_control_row(&a1_candidate, MarketAsset::Base, context.base_start).unwrap();
    let retry_quote_row = hlp_exact_control_row(&a1_candidate, MarketAsset::Quote, context.quote_start).unwrap();
    let current_base_error = hlp_exact_control_value(&a1_candidate, MarketAsset::Base, retry_base_row);
    let current_quote_error = hlp_exact_control_value(&a1_candidate, MarketAsset::Quote, retry_quote_row);
    if let Some(expected_safe) = frozen_a1_local_expected_safe {
        let snapshot_before = snapshot.try_to_vec().unwrap();
        let a1_rows = hlp_stage4b2a_test_rows(&a1_candidate).unwrap();
        let a1_base_safe = concentrated_hlp_candidate_components_are_safe(
            context.base_start,
            a1_candidate.base_principal_tracking_error_nad,
            a1_candidate.base_tracking_error_nad,
            a1_candidate.base_endpoint_exposure_nad,
        ) && a1_candidate.base_trade_endpoint_safe;
        let a1_quote_safe = concentrated_hlp_candidate_components_are_safe(
            context.quote_start,
            a1_candidate.quote_principal_tracking_error_nad,
            a1_candidate.quote_tracking_error_nad,
            a1_candidate.quote_endpoint_exposure_nad,
        ) && a1_candidate.quote_trade_endpoint_safe;
        let a1_safe = a1_base_safe && a1_quote_safe && a1_candidate.reserve_endpoint_safe;
        if a1_safe {
            eprintln!(
                "FROZEN-A1-LOCAL {label} exact-safe A1={a1_coordinate:?} rows={a1_rows:?} counts=(compact={},authority={})",
                CONCENTRATED_PRE_SOLVE_COMPACT_EVALUATIONS.with(Cell::get),
                CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(Cell::get),
            );
            assert!(expected_safe);
            assert_eq!(CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(Cell::get), 1);
            return;
        }

        let exact_a1 = *a1_candidate;
        let inventory_changed = exact_a1.base_receipt.ylp_mint_amount != 0
            || exact_a1.base_receipt.ylp_burn_amount != 0
            || exact_a1.quote_receipt.ylp_mint_amount != 0
            || exact_a1.quote_receipt.ylp_burn_amount != 0;
        downgrade_authoritative_a1_projection(
            a1_projection.as_mut().unwrap(),
            &context,
            &compact,
            a1_market.base_side.shares.ylp_supply,
            inventory_changed,
            a1_topology,
        )
        .unwrap();
        let Some(ConcentratedHlpProjection::FrozenA1(frozen_a1)) = a1_projection.take() else {
            panic!("{label}: exact A1 projection must destructively downgrade")
        };
        let mut frozen_a1_slot = Some(frozen_a1);
        a1_candidate_slot = None;
        a1_market.clone_from(&snapshot);
        lifecycle_scratch.clone_from(&snapshot);
        assert!(a1_projection.is_none());
        assert!(a1_candidate_slot.is_none());

        let mut local_origin_slot = None;
        evaluate_stage4c_frozen_center_bounded_axis(
            &compact,
            &context,
            cash_floors,
            a1_coordinate.0,
            a1_coordinate.1,
            frozen_a1_slot.as_ref().unwrap(),
            &mut local_origin_slot,
        )
        .unwrap();
        let local_origin = local_origin_slot.as_ref().unwrap();
        let origin_rows = hlp_stage4b2a_test_rows(local_origin).unwrap();
        assert_eq!(origin_rows, a1_rows, "{label}: same-coordinate FrozenA1 rows");
        assert_eq!(local_origin.structural_topology, a1_topology);
        let origin_signature = hlp_preposition_pair_signature(local_origin);
        assert!(hlp_preposition_signature_class_matches(
            a1_signature.base,
            origin_signature.base,
        ));
        assert!(hlp_preposition_signature_class_matches(
            a1_signature.quote,
            origin_signature.quote,
        ));
        assert_eq!(
            local_origin.base_receipt.tracking_retained_contribution_nad,
            exact_a1.base_receipt.tracking_retained_contribution_nad,
        );
        assert_eq!(
            local_origin.quote_receipt.tracking_retained_contribution_nad,
            exact_a1.quote_receipt.tracking_retained_contribution_nad,
        );
        assert!(local_origin.settlement_cash_available);
        assert!(hlp_exact_derivative_sample_is_unbound(local_origin, &context));
        let origin_trace = local_origin.guidance_settlement_trace.unwrap();
        let origin_actions = local_origin.guidance_d_actions.unwrap();

        let local_base_probe_delta = if context.base_start.active {
            stage4c_signed_round_away_divisor(
                hlp_exact_axis_probe_delta(a1_coordinate.0, context.base_start, current_base_error).unwrap(),
                16,
            )
            .unwrap()
        } else {
            0
        };
        let local_quote_probe_delta = if context.quote_start.active {
            stage4c_signed_round_away_divisor(
                hlp_exact_axis_probe_delta(a1_coordinate.1, context.quote_start, current_quote_error).unwrap(),
                16,
            )
            .unwrap()
        } else {
            0
        };
        let local_base_coordinate = (
            a1_coordinate.0.checked_add(local_base_probe_delta).unwrap(),
            a1_coordinate.1,
        );
        let local_quote_coordinate = (
            a1_coordinate.0,
            a1_coordinate.1.checked_add(local_quote_probe_delta).unwrap(),
        );
        let mut local_base_slot = None;
        if context.base_start.active {
            evaluate_stage4c_frozen_center_bounded_axis(
                &compact,
                &context,
                cash_floors,
                local_base_coordinate.0,
                local_base_coordinate.1,
                frozen_a1_slot.as_ref().unwrap(),
                &mut local_base_slot,
            )
            .unwrap();
        } else {
            local_base_slot = Some(*local_origin);
        }
        let mut local_quote_slot = None;
        if context.quote_start.active {
            evaluate_stage4c_frozen_center_bounded_axis(
                &compact,
                &context,
                cash_floors,
                local_quote_coordinate.0,
                local_quote_coordinate.1,
                frozen_a1_slot.as_ref().unwrap(),
                &mut local_quote_slot,
            )
            .unwrap();
        } else {
            local_quote_slot = Some(*local_origin);
        }

        for (asset, axis) in [
            (MarketAsset::Base, local_base_slot.as_ref().unwrap()),
            (MarketAsset::Quote, local_quote_slot.as_ref().unwrap()),
        ] {
            let active = match asset {
                MarketAsset::Base => context.base_start.active,
                MarketAsset::Quote => context.quote_start.active,
            };
            if !active {
                continue;
            }
            let signature = hlp_preposition_pair_signature(axis);
            assert!(axis.settlement_cash_available, "{label}: local {asset:?} cash");
            assert!(hlp_exact_derivative_sample_is_unbound(axis, &context));
            assert_eq!(axis.structural_topology, a1_topology);
            assert_eq!(axis.guidance_settlement_trace, Some(origin_trace));
            assert!(
                axis.guidance_d_actions
                    .unwrap()
                    .frozen_cell_numeric_function_matches(origin_actions),
                "{label}: local {asset:?} D-action branch",
            );
            assert!(hlp_preposition_signature_class_matches(
                a1_signature.base,
                signature.base,
            ));
            assert!(hlp_preposition_signature_class_matches(
                a1_signature.quote,
                signature.quote,
            ));
            match asset {
                MarketAsset::Base => assert_ne!(signature.base, origin_signature.base),
                MarketAsset::Quote => assert_ne!(signature.quote, origin_signature.quote),
            }
        }

        let local_basis = HlpFiniteDifferenceBasis {
            origin: HlpExactSampleRows::from_candidate(local_origin),
            base_probe_delta_nad: local_base_probe_delta,
            base_probe: HlpExactSampleRows::from_candidate(local_base_slot.as_ref().unwrap()),
            quote_probe_delta_nad: local_quote_probe_delta,
            quote_probe: HlpExactSampleRows::from_candidate(local_quote_slot.as_ref().unwrap()),
            base_probe_recorded: context.base_start.active,
            quote_probe_recorded: context.quote_start.active,
            ..HlpFiniteDifferenceBasis::default()
        };
        let base_target =
            stage4c_opposite_half_budget_target(current_base_error, context.base_start.tracking.loss_budget_nad)
                .unwrap();
        let quote_target =
            stage4c_opposite_half_budget_target(current_quote_error, context.quote_start.tracking.loss_budget_nad)
                .unwrap();
        let (local_base_step, local_quote_step) = local_basis
            .solve_step(
                &context,
                a1_coordinate.0,
                a1_coordinate.1,
                retry_base_row,
                retry_quote_row,
                current_base_error.checked_sub(base_target).unwrap(),
                current_quote_error.checked_sub(quote_target).unwrap(),
            )
            .unwrap();
        let terminal_coordinate = (
            a1_coordinate.0.checked_add(local_base_step).unwrap(),
            a1_coordinate.1.checked_add(local_quote_step).unwrap(),
        );
        assert!(hlp_coordinate_within_center_trust(
            center_coordinate.0,
            terminal_coordinate.0,
            context.base_start,
        ));
        assert!(hlp_coordinate_within_center_trust(
            center_coordinate.1,
            terminal_coordinate.1,
            context.quote_start,
        ));

        let local_base_rows = hlp_stage4b2a_test_rows(local_base_slot.as_ref().unwrap()).unwrap();
        let local_quote_rows = hlp_stage4b2a_test_rows(local_quote_slot.as_ref().unwrap()).unwrap();
        local_origin_slot = None;
        local_base_slot = None;
        local_quote_slot = None;
        frozen_a1_slot = None;
        assert!(local_origin_slot.is_none());
        assert!(local_base_slot.is_none());
        assert!(local_quote_slot.is_none());
        assert!(frozen_a1_slot.is_none());
        a1_market.clone_from(&snapshot);
        lifecycle_scratch.clone_from(&snapshot);

        let mut a2_market = snapshot.clone();
        let mut a2_projection = None;
        let mut a2_candidate_slot = None;
        let mut a2_terminal_scratch = HlpTerminalSwapScratch::default();
        evaluate_concentrated_hlp_candidate(
            &mut a2_market,
            &mut lifecycle_scratch,
            &snapshot,
            &context,
            cash_floors,
            terminal_coordinate.0,
            terminal_coordinate.1,
            HlpCandidateEvaluationMode::Authoritative,
            &mut a2_projection,
            &mut a2_candidate_slot,
            &mut a2_terminal_scratch,
        )
        .unwrap();
        let a2_candidate = a2_candidate_slot.as_ref().unwrap();
        let a2_rows = hlp_stage4b2a_test_rows(a2_candidate).unwrap();
        let a2_base_safe = concentrated_hlp_candidate_components_are_safe(
            context.base_start,
            a2_candidate.base_principal_tracking_error_nad,
            a2_candidate.base_tracking_error_nad,
            a2_candidate.base_endpoint_exposure_nad,
        ) && a2_candidate.base_trade_endpoint_safe;
        let a2_quote_safe = concentrated_hlp_candidate_components_are_safe(
            context.quote_start,
            a2_candidate.quote_principal_tracking_error_nad,
            a2_candidate.quote_tracking_error_nad,
            a2_candidate.quote_endpoint_exposure_nad,
        ) && a2_candidate.quote_trade_endpoint_safe;
        let a2_signature = hlp_preposition_pair_signature(a2_candidate);
        let inactive_zero = (context.base_start.active || [a2_rows[0], a2_rows[2], a2_rows[4]] == [0; 3])
            && (context.quote_start.active || [a2_rows[1], a2_rows[3], a2_rows[5]] == [0; 3]);
        let identity = a2_candidate.authoritative.is_some()
            && a2_projection.as_ref().unwrap().authoritative()
            && a2_candidate.settlement_cash_available
            && a2_candidate.structural_topology == center_topology
            && a2_candidate.structural_topology == a1_topology
            && hlp_preposition_signature_class_matches(center_signature.base, a2_signature.base)
            && hlp_preposition_signature_class_matches(center_signature.quote, a2_signature.quote)
            && hlp_preposition_signature_class_matches(a1_signature.base, a2_signature.base)
            && hlp_preposition_signature_class_matches(a1_signature.quote, a2_signature.quote)
            && inactive_zero
            && hlp_exact_derivative_sample_is_unbound(a2_candidate, &context)
            && hlp_coordinate_within_center_trust(center_coordinate.0, terminal_coordinate.0, context.base_start)
            && hlp_coordinate_within_center_trust(center_coordinate.1, terminal_coordinate.1, context.quote_start);
        let terminal_safe = a2_base_safe && a2_quote_safe && a2_candidate.reserve_endpoint_safe;
        eprintln!(
            "FROZEN-A1-LOCAL {label} center={center_coordinate:?} A1={a1_coordinate:?}/rows={a1_rows:?} origin_rows={origin_rows:?}/equal={} local_base={local_base_coordinate:?}/rows={local_base_rows:?} local_quote={local_quote_coordinate:?}/rows={local_quote_rows:?} targets=({base_target},{quote_target}) correction=({local_base_step},{local_quote_step}) A2={terminal_coordinate:?}/rows={a2_rows:?}/safe=({a2_base_safe},{a2_quote_safe},{})/identity={identity} counts=(compact={},raw={},authority={},candidate={})",
            origin_rows == a1_rows,
            a2_candidate.reserve_endpoint_safe,
            CONCENTRATED_PRE_SOLVE_COMPACT_EVALUATIONS.with(Cell::get),
            CONCENTRATED_PRE_SOLVE_RAW_EVALUATIONS.with(Cell::get),
            CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(Cell::get),
            CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(Cell::get),
        );
        assert!(identity);
        assert_eq!(terminal_safe, expected_safe);
        assert_eq!(
            CONCENTRATED_PRE_SOLVE_COMPACT_EVALUATIONS.with(Cell::get),
            3 + 2 * u32::from(active_axis_count)
        );
        assert_eq!(CONCENTRATED_PRE_SOLVE_RAW_EVALUATIONS.with(Cell::get), 0);
        assert_eq!(CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(Cell::get), 2);
        assert_eq!(snapshot.try_to_vec().unwrap(), snapshot_before);
        return;
    }
    let solved_raw = basis
        .solve_broyden_candidate(
            &context,
            center_coordinate.0,
            center_coordinate.1,
            a1_coordinate.0,
            a1_coordinate.1,
            retry_base_row,
            retry_quote_row,
            current_base_error,
            current_quote_error,
        )
        .unwrap();
    let raw_coordinate = (
        hlp_force_adjacent_atom_if_needed(
            &snapshot,
            context.frozen_prices,
            context.base_start,
            a1_candidate.base_receipt,
            a1_coordinate.0,
            solved_raw.0,
            current_base_error,
        )
        .unwrap(),
        hlp_force_adjacent_atom_if_needed(
            &snapshot,
            context.frozen_prices,
            context.quote_start,
            a1_candidate.quote_receipt,
            a1_coordinate.1,
            solved_raw.1,
            current_quote_error,
        )
        .unwrap(),
    );
    if let Some(expected_safe) = rawless_expected_safe {
        let a1_rows = hlp_stage4b2a_test_rows(&a1_candidate).unwrap();
        assert!(hlp_coordinate_within_center_trust(
            center_coordinate.0,
            raw_coordinate.0,
            context.base_start,
        ));
        assert!(hlp_coordinate_within_center_trust(
            center_coordinate.1,
            raw_coordinate.1,
            context.quote_start,
        ));

        // Deliberately erase every exact-A1 capability before the terminal
        // authority. Only the scalar Broyden coordinate and structural
        // fingerprints survive this boundary.
        a1_candidate_slot = None;
        a1_projection = None;
        assert!(a1_candidate_slot.is_none());
        assert!(a1_projection.is_none());
        drop(a1_market);
        drop(lifecycle_scratch);

        let snapshot_before = snapshot.try_to_vec().unwrap();
        let mut a2_market = snapshot.clone();
        let mut a2_lifecycle_scratch = Market::default();
        let mut a2_projection = None;
        let mut a2_candidate = None;
        let mut a2_terminal_scratch = HlpTerminalSwapScratch::default();
        evaluate_concentrated_hlp_candidate(
            &mut a2_market,
            &mut a2_lifecycle_scratch,
            &snapshot,
            &context,
            cash_floors,
            raw_coordinate.0,
            raw_coordinate.1,
            HlpCandidateEvaluationMode::Authoritative,
            &mut a2_projection,
            &mut a2_candidate,
            &mut a2_terminal_scratch,
        )
        .unwrap();
        assert!(a2_projection.as_ref().unwrap().authoritative());
        let a2_candidate = a2_candidate.unwrap();
        assert!(a2_candidate.authoritative.is_some());
        let a2_rows = hlp_stage4b2a_test_rows(&a2_candidate).unwrap();
        let a2_signature = hlp_preposition_pair_signature(&a2_candidate);
        let base_safe = concentrated_hlp_candidate_components_are_safe(
            context.base_start,
            a2_candidate.base_principal_tracking_error_nad,
            a2_candidate.base_tracking_error_nad,
            a2_candidate.base_endpoint_exposure_nad,
        ) && a2_candidate.base_trade_endpoint_safe;
        let quote_safe = concentrated_hlp_candidate_components_are_safe(
            context.quote_start,
            a2_candidate.quote_principal_tracking_error_nad,
            a2_candidate.quote_tracking_error_nad,
            a2_candidate.quote_endpoint_exposure_nad,
        ) && a2_candidate.quote_trade_endpoint_safe;
        let inactive_zero = (context.base_start.active || [a2_rows[0], a2_rows[2], a2_rows[4]] == [0; 3])
            && (context.quote_start.active || [a2_rows[1], a2_rows[3], a2_rows[5]] == [0; 3]);
        let topology_matches =
            a2_candidate.structural_topology == center_topology && a2_candidate.structural_topology == a1_topology;
        let signature_class_matches = hlp_preposition_signature_class_matches(center_signature.base, a2_signature.base)
            && hlp_preposition_signature_class_matches(center_signature.quote, a2_signature.quote)
            && hlp_preposition_signature_class_matches(a1_signature.base, a2_signature.base)
            && hlp_preposition_signature_class_matches(a1_signature.quote, a2_signature.quote);
        let identity = a2_candidate.settlement_cash_available
            && topology_matches
            && signature_class_matches
            && inactive_zero
            && hlp_exact_derivative_sample_is_unbound(&a2_candidate, &context)
            && hlp_coordinate_within_center_trust(center_coordinate.0, raw_coordinate.0, context.base_start)
            && hlp_coordinate_within_center_trust(center_coordinate.1, raw_coordinate.1, context.quote_start);
        let terminal_safe = base_safe && quote_safe && a2_candidate.reserve_endpoint_safe;
        let observed_compact = CONCENTRATED_PRE_SOLVE_COMPACT_EVALUATIONS.with(Cell::get);
        let observed_raw = CONCENTRATED_PRE_SOLVE_RAW_EVALUATIONS.with(Cell::get);
        let observed_authority = CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(Cell::get);
        let observed_candidates = CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(Cell::get);
        let canonical_calls = HLP_RAW_CANONICAL_SCALAR_CALLS.with(Cell::get);
        let canonical_residuals = HLP_RAW_CANONICAL_SCALAR_RESIDUALS.with(Cell::get);
        eprintln!(
            "CENTER-FRESH RAWLESS {label} center={center_coordinate:?} A1={a1_coordinate:?}/rows={a1_rows:?}/topology={a1_topology:?}/signature={a1_signature:?} A2={raw_coordinate:?}/rows={a2_rows:?}/safe=({base_safe},{quote_safe},{})/cash={}/inactive_zero={inactive_zero}/topology_match={topology_matches}/signature_class={signature_class_matches}/signature={a2_signature:?}/identity={identity} counts=(compact={observed_compact},raw={observed_raw},authority={observed_authority},candidate={observed_candidates},canonical_calls={canonical_calls},canonical_residuals={canonical_residuals})",
            a2_candidate.reserve_endpoint_safe,
            a2_candidate.settlement_cash_available,
        );
        assert!(identity);
        assert_eq!(terminal_safe, expected_safe);
        assert_eq!(observed_compact, 2 + u32::from(active_axis_count));
        assert_eq!(observed_raw, 0);
        assert_eq!(observed_authority, 2);
        assert_eq!(observed_candidates, observed_compact + observed_authority);
        assert_eq!(canonical_calls, 0);
        assert_eq!(canonical_residuals, 0);
        assert_eq!(snapshot.try_to_vec().unwrap(), snapshot_before);
        if !expected_safe {
            a2_market.clone_from(&snapshot);
            assert_eq!(a2_market.try_to_vec().unwrap(), snapshot_before);
        }
        return;
    }
    let inventory_changed = a1_candidate.base_receipt.ylp_mint_amount != 0
        || a1_candidate.base_receipt.ylp_burn_amount != 0
        || a1_candidate.quote_receipt.ylp_mint_amount != 0
        || a1_candidate.quote_receipt.ylp_burn_amount != 0;
    downgrade_authoritative_a1_projection(
        a1_projection.as_mut().unwrap(),
        &context,
        &compact,
        a1_market.base_side.shares.ylp_supply,
        inventory_changed,
        a1_topology,
    )
    .unwrap();
    let ConcentratedHlpProjection::FrozenA1(frozen_a1) = a1_projection.unwrap() else {
        panic!("{label}: A1 must freeze")
    };
    let mut raw_candidate = None;
    evaluate_frozen_a1_raw_concentrated_hlp_candidate(
        &compact,
        &context,
        cash_floors,
        raw_coordinate.0,
        raw_coordinate.1,
        &frozen_a1,
        &mut raw_candidate,
    )
    .unwrap();
    let raw_candidate = raw_candidate.unwrap();
    let raw_signature = hlp_preposition_pair_signature(&raw_candidate);
    let raw_robust = concentrated_hlp_raw_candidate_is_robust(&context, &raw_candidate);
    eprintln!(
        "CENTER-FRESH {label} A1={a1_coordinate:?} rows={:?} topology={a1_topology:?} signature={a1_signature:?} raw={raw_coordinate:?} rows={:?} robust={raw_robust} topology={:?} signature={raw_signature:?} conceptual_counts=(full=2,scalar_axes={active_axis_count},raw=1,authority=1_so_far)",
        hlp_stage4b2a_test_rows(&a1_candidate),
        hlp_stage4b2a_test_rows(&raw_candidate),
        raw_candidate.structural_topology,
    );
    assert!(raw_candidate.settlement_cash_available);
    assert_eq!(raw_candidate.structural_topology, a1_topology);
    assert!(hlp_exact_derivative_sample_is_unbound(&raw_candidate, &context));
    assert!(raw_robust);

    let mut a2_market = snapshot.clone();
    let mut a2_projection = None;
    let mut a2_candidate = None;
    let mut a2_terminal_scratch = HlpTerminalSwapScratch::default();
    evaluate_concentrated_hlp_candidate(
        &mut a2_market,
        &mut lifecycle_scratch,
        &snapshot,
        &context,
        cash_floors,
        raw_coordinate.0,
        raw_coordinate.1,
        HlpCandidateEvaluationMode::Authoritative,
        &mut a2_projection,
        &mut a2_candidate,
        &mut a2_terminal_scratch,
    )
    .unwrap();
    let a2_candidate = a2_candidate.unwrap();
    let base_safe = concentrated_hlp_candidate_components_are_safe(
        context.base_start,
        a2_candidate.base_principal_tracking_error_nad,
        a2_candidate.base_tracking_error_nad,
        a2_candidate.base_endpoint_exposure_nad,
    ) && a2_candidate.base_trade_endpoint_safe;
    let quote_safe = concentrated_hlp_candidate_components_are_safe(
        context.quote_start,
        a2_candidate.quote_principal_tracking_error_nad,
        a2_candidate.quote_tracking_error_nad,
        a2_candidate.quote_endpoint_exposure_nad,
    ) && a2_candidate.quote_trade_endpoint_safe;
    let identity = a2_candidate.settlement_cash_available
        && a2_candidate.reserve_endpoint_safe
        && a2_candidate.structural_topology == a1_topology
        && a2_candidate.structural_topology == raw_candidate.structural_topology
        && hlp_preposition_pair_signature(&a2_candidate) == raw_signature
        && hlp_exact_derivative_sample_is_unbound(&a2_candidate, &context)
        && hlp_coordinate_within_center_trust(center_coordinate.0, raw_coordinate.0, context.base_start)
        && hlp_coordinate_within_center_trust(center_coordinate.1, raw_coordinate.1, context.quote_start);
    let observed_compact = CONCENTRATED_PRE_SOLVE_COMPACT_EVALUATIONS.with(Cell::get);
    let observed_raw = CONCENTRATED_PRE_SOLVE_RAW_EVALUATIONS.with(Cell::get);
    let observed_authority = CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(Cell::get);
    let observed_candidates = CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(Cell::get);
    let bounded_out_total = HLP_COMPACT_GUIDANCE_EXACT_OUT_PROBES.with(Cell::get);
    let canonical_calls_total = HLP_RAW_CANONICAL_SCALAR_CALLS.with(Cell::get);
    let canonical_residuals_total = HLP_RAW_CANONICAL_SCALAR_RESIDUALS.with(Cell::get);
    eprintln!(
        "CENTER-FRESH {label} A2={raw_coordinate:?} rows={:?} safe=({base_safe},{quote_safe},{}) identity={identity} topology={:?} signature={:?} conceptual_counts=(full=2,scalar_axes={active_axis_count},raw=1,authority=2) observed_counters=(compact={observed_compact},raw={observed_raw},authority={observed_authority},candidate={observed_candidates},bounded_out_residuals={bounded_out_total},canonical_calls={canonical_calls_total},canonical_residuals={canonical_residuals_total})",
        hlp_stage4b2a_test_rows(&a2_candidate),
        a2_candidate.reserve_endpoint_safe,
        a2_candidate.structural_topology,
        hlp_preposition_pair_signature(&a2_candidate),
    );
    assert!(base_safe);
    assert!(quote_safe);
    assert!(identity);
    assert_eq!(observed_authority, 2);
}

fn stage4c_candidate_is_fully_safe(
    candidate: &ConcentratedHlpCandidate,
    context: &ConcentratedHlpSolveContext,
) -> bool {
    concentrated_hlp_candidate_components_are_safe(
        context.base_start,
        candidate.base_principal_tracking_error_nad,
        candidate.base_tracking_error_nad,
        candidate.base_endpoint_exposure_nad,
    ) && candidate.base_trade_endpoint_safe
        && concentrated_hlp_candidate_components_are_safe(
            context.quote_start,
            candidate.quote_principal_tracking_error_nad,
            candidate.quote_tracking_error_nad,
            candidate.quote_endpoint_exposure_nad,
        )
        && candidate.quote_trade_endpoint_safe
        && candidate.reserve_endpoint_safe
}

/// Re-score only the final combined/principal/exposure rows at the exact A1
/// post-rebalance mark. The frozen-flow evaluator has already bound the real
/// trade and reserve anchors, settlement branches, topology, and post plans;
/// this local test helper replaces the final scalar mark without adding it to
/// a production capability or projection struct.
fn stage4c_rescore_frozen_flow_at_exact_mark(
    candidate: &mut ConcentratedHlpCandidate,
    compact: &HlpCompactSolveContext,
    context: &ConcentratedHlpSolveContext,
    exact_final_mark_base_nad: u128,
) {
    let cached = CACHED_HLP_LIFECYCLE_RESULT.with(|result| result.borrow().expect("cached compact lifecycle"));
    assert_eq!(cached.transition.socialized_principal_loss, 0);
    assert_eq!(cached.transition.removed_unrealized_interest, 0);
    let prices = hlp_curve_prices_from_base_price_nad(exact_final_mark_base_nad).unwrap();
    let base_active = cached.state.active(compact.fixed, MarketAsset::Base);
    let quote_active = cached.state.active(compact.fixed, MarketAsset::Quote);
    let (base_values, quote_values) = hlp_planner_inventory_values_pair_nad_with_prices(
        compact.fixed,
        cached.state,
        prices,
        base_active,
        quote_active,
    )
    .unwrap();
    let base = hlp_planner_tracking_from_endpoint(
        compact.fixed,
        cached.state,
        MarketAsset::Base,
        prices,
        hlp_lifecycle_endpoint_from_values(base_values).unwrap(),
        context.base_start,
    )
    .unwrap();
    let quote = hlp_planner_tracking_from_endpoint(
        compact.fixed,
        cached.state,
        MarketAsset::Quote,
        prices,
        hlp_lifecycle_endpoint_from_values(quote_values).unwrap(),
        context.quote_start,
    )
    .unwrap();
    candidate.base_principal_tracking_error_nad = base.0;
    candidate.base_tracking_error_nad = base
        .2
        .checked_sub(candidate.base_receipt.tracking_retained_contribution_nad)
        .unwrap();
    candidate.base_endpoint_exposure_nad = base.3;
    candidate.quote_principal_tracking_error_nad = quote.0;
    candidate.quote_tracking_error_nad = quote
        .2
        .checked_sub(candidate.quote_receipt.tracking_retained_contribution_nad)
        .unwrap();
    candidate.quote_endpoint_exposure_nad = quote.3;
}

/// Test-only CU-minimal experiment. Authority A1 runs at the untouched swap
/// coordinate, and its exact endpoints supply both the initial needed-delta
/// algebra and an authority-erased frozen-flow scalar function. No production
/// phase or evaluator calls this schedule.
fn assert_stage4c_zero_a1_frozen_flow_schedule(
    mut snapshot: Market,
    asset_in: MarketAsset,
    reserve_credit: u64,
    label: &str,
) {
    let current_slot = curve_slot(&snapshot);
    snapshot.prepare_amm_for_swap(current_slot).unwrap();
    let pre_state = snapshot.dynamic_fee_pre_state(current_slot).unwrap();
    let preliminary = snapshot
        .preliminary_swap_inputs_for_state(reserve_credit, current_slot, pre_state)
        .unwrap();
    let mut context_slot = None;
    capture_concentrated_hlp_solve_context_into(
        &snapshot,
        asset_in,
        reserve_credit,
        current_slot,
        pre_state,
        preliminary,
        SwapCashPolicy::Spot,
        &mut context_slot,
    )
    .unwrap();
    let context = context_slot.unwrap();
    let mut compact_slot = None;
    HlpCompactSolveContext::capture_into(&snapshot, &context, &mut compact_slot).unwrap();
    let compact = compact_slot.unwrap();
    let cash_floors = context.cash_policy.floors(&snapshot, asset_in, 0).unwrap();

    reset_stage4b2a_test_state();
    let snapshot_before = snapshot.try_to_vec().unwrap();
    let active_axis_count = u32::from(context.base_start.active) + u32::from(context.quote_start.active);

    // A1 is the first authority and always evaluates the real swap request at
    // the untouched preposition coordinate.
    let mut a1_market = snapshot.clone();
    let mut lifecycle_scratch = Market::default();
    let mut a1_projection_slot = None;
    let mut a1_candidate_slot = None;
    let mut a1_terminal_scratch = HlpTerminalSwapScratch::default();
    evaluate_concentrated_hlp_candidate(
        &mut a1_market,
        &mut lifecycle_scratch,
        &snapshot,
        &context,
        cash_floors,
        0,
        0,
        HlpCandidateEvaluationMode::Authoritative,
        &mut a1_projection_slot,
        &mut a1_candidate_slot,
        &mut a1_terminal_scratch,
    )
    .unwrap();
    let a1_candidate = *a1_candidate_slot.as_ref().unwrap();
    let a1_projection = a1_projection_slot.as_ref().unwrap();
    assert!(a1_projection.authoritative());
    assert!(a1_candidate.authoritative.is_some());
    assert!(a1_candidate.settlement_cash_available);
    assert!(hlp_exact_derivative_sample_is_unbound(&a1_candidate, &context));
    let a1_rows = hlp_stage4b2a_test_rows(&a1_candidate).unwrap();
    let a1_topology = a1_candidate.structural_topology;

    // Authoritative evaluation intentionally leaves next_* at its input in
    // production. Re-run the exact same needed-delta algebra here against the
    // exact A1 endpoint and its prepositioned planner state.
    let common = a1_projection.common();
    let start_prices = hlp_curve_prices_from_base_price_nad(common.start_price_nad as u128).unwrap();
    let endpoint_prices = hlp_curve_prices_from_base_price_nad(common.end_price_nad as u128).unwrap();
    let endpoint_base =
        denormalize_from_nad_floor(common.endpoint_reserves.base, a1_market.base_side.asset_decimals).unwrap();
    let endpoint_quote =
        denormalize_from_nad_floor(common.endpoint_reserves.quote, a1_market.quote_side.asset_decimals).unwrap();
    let center_coordinate = (
        if context.base_start.active {
            concentrated_hlp_needed_delta(
                &snapshot,
                &a1_market,
                MarketAsset::Base,
                start_prices,
                endpoint_prices,
                endpoint_base,
                endpoint_quote,
                context.base_start.tracking.principal_nav_nad,
            )
            .unwrap()
        } else {
            0
        },
        if context.quote_start.active {
            concentrated_hlp_needed_delta(
                &snapshot,
                &a1_market,
                MarketAsset::Quote,
                start_prices,
                endpoint_prices,
                endpoint_base,
                endpoint_quote,
                context.quote_start.tracking.principal_nav_nad,
            )
            .unwrap()
        } else {
            0
        },
    );
    assert_ne!(
        center_coordinate,
        (0, 0),
        "{label}: exact A1 produced no next coordinate"
    );

    // Destroy the exact quote/checkpoint capability after preserving only its
    // authority-free flow anchors and scalar final mark.
    let exact_a1_final_mark_base_nad = current_hlp_curve_prices(&lifecycle_scratch).unwrap().base_in_quote_nad;
    let inventory_changed = a1_candidate.base_receipt.ylp_mint_amount != 0
        || a1_candidate.base_receipt.ylp_burn_amount != 0
        || a1_candidate.quote_receipt.ylp_mint_amount != 0
        || a1_candidate.quote_receipt.ylp_burn_amount != 0;
    downgrade_authoritative_a1_projection(
        a1_projection_slot.as_mut().unwrap(),
        &context,
        &compact,
        a1_market.base_side.shares.ylp_supply,
        inventory_changed,
        a1_topology,
    )
    .unwrap();
    let Some(ConcentratedHlpProjection::FrozenA1(frozen)) = a1_projection_slot.take() else {
        panic!("{label}: A1 authority was not erased")
    };
    a1_candidate_slot = None;
    a1_market.clone_from(&snapshot);
    lifecycle_scratch.clone_from(&snapshot);
    assert!(a1_candidate_slot.is_none());
    assert!(a1_projection_slot.is_none());

    let mut center_slot = None;
    evaluate_stage4c_frozen_center_bounded_axis(
        &compact,
        &context,
        cash_floors,
        center_coordinate.0,
        center_coordinate.1,
        &frozen,
        &mut center_slot,
    )
    .unwrap();
    stage4c_rescore_frozen_flow_at_exact_mark(
        center_slot.as_mut().unwrap(),
        &compact,
        &context,
        exact_a1_final_mark_base_nad,
    );
    let center = *center_slot.as_ref().unwrap();
    assert!(center.settlement_cash_available);
    assert!(hlp_exact_derivative_sample_is_unbound(&center, &context));
    let center_topology = center.structural_topology;
    let center_mode = center.guidance_exact_in_mode;
    let center_trace = center.guidance_settlement_trace.unwrap();
    let center_actions = center.guidance_d_actions.unwrap();
    let center_signature = hlp_preposition_pair_signature(&center);
    let center_rows = HlpExactSampleRows::from_candidate(&center);
    let base_row = hlp_exact_control_row(&center, MarketAsset::Base, context.base_start).unwrap();
    let quote_row = hlp_exact_control_row(&center, MarketAsset::Quote, context.quote_start).unwrap();
    let center_base_error = center_rows.value(MarketAsset::Base, base_row);
    let center_quote_error = center_rows.value(MarketAsset::Quote, quote_row);

    let base_probe_delta = if context.base_start.active {
        stage4c_signed_round_away_divisor(
            hlp_exact_axis_probe_delta(center_coordinate.0, context.base_start, center_base_error).unwrap(),
            16,
        )
        .unwrap()
    } else {
        0
    };
    let quote_probe_delta = if context.quote_start.active {
        stage4c_signed_round_away_divisor(
            hlp_exact_axis_probe_delta(center_coordinate.1, context.quote_start, center_quote_error).unwrap(),
            16,
        )
        .unwrap()
    } else {
        0
    };

    let mut base_axis_slot = None;
    if context.base_start.active {
        evaluate_stage4c_frozen_center_bounded_axis(
            &compact,
            &context,
            cash_floors,
            center_coordinate.0.checked_add(base_probe_delta).unwrap(),
            center_coordinate.1,
            &frozen,
            &mut base_axis_slot,
        )
        .unwrap();
        stage4c_rescore_frozen_flow_at_exact_mark(
            base_axis_slot.as_mut().unwrap(),
            &compact,
            &context,
            exact_a1_final_mark_base_nad,
        );
    } else {
        base_axis_slot = Some(center);
    }
    let mut quote_axis_slot = None;
    if context.quote_start.active {
        evaluate_stage4c_frozen_center_bounded_axis(
            &compact,
            &context,
            cash_floors,
            center_coordinate.0,
            center_coordinate.1.checked_add(quote_probe_delta).unwrap(),
            &frozen,
            &mut quote_axis_slot,
        )
        .unwrap();
        stage4c_rescore_frozen_flow_at_exact_mark(
            quote_axis_slot.as_mut().unwrap(),
            &compact,
            &context,
            exact_a1_final_mark_base_nad,
        );
    } else {
        quote_axis_slot = Some(center);
    }

    for (asset, axis) in [
        (MarketAsset::Base, base_axis_slot.as_ref().unwrap()),
        (MarketAsset::Quote, quote_axis_slot.as_ref().unwrap()),
    ] {
        let active = match asset {
            MarketAsset::Base => context.base_start.active,
            MarketAsset::Quote => context.quote_start.active,
        };
        if !active {
            continue;
        }
        let signature = hlp_preposition_pair_signature(axis);
        assert!(axis.settlement_cash_available, "{label}: {asset:?} axis cash");
        assert!(hlp_exact_derivative_sample_is_unbound(axis, &context));
        assert_eq!(axis.structural_topology, center_topology, "{label}: {asset:?} topology");
        assert_eq!(axis.guidance_exact_in_mode, center_mode, "{label}: {asset:?} mode");
        assert_eq!(
            axis.guidance_settlement_trace,
            Some(center_trace),
            "{label}: {asset:?} trace"
        );
        assert_eq!(
            axis.guidance_d_actions,
            Some(center_actions),
            "{label}: {asset:?} D actions"
        );
        assert!(hlp_preposition_signature_class_matches(
            center_signature.base,
            signature.base,
        ));
        assert!(hlp_preposition_signature_class_matches(
            center_signature.quote,
            signature.quote,
        ));
    }

    let basis = HlpFiniteDifferenceBasis {
        origin: center_rows,
        guidance_exact_in_mode: center_mode,
        guidance_settlement_trace: Some(center_trace),
        base_probe_delta_nad: base_probe_delta,
        base_probe: HlpExactSampleRows::from_candidate(base_axis_slot.as_ref().unwrap()),
        quote_probe_delta_nad: quote_probe_delta,
        quote_probe: HlpExactSampleRows::from_candidate(quote_axis_slot.as_ref().unwrap()),
        base_signature: center_signature.base,
        quote_signature: center_signature.quote,
        base_probe_recorded: context.base_start.active,
        quote_probe_recorded: context.quote_start.active,
    };
    let base_residual = hlp_initial_active_set_residual(center_base_error, context.base_start).unwrap();
    let quote_residual = hlp_initial_active_set_residual(center_quote_error, context.quote_start).unwrap();
    let first_step = basis
        .solve_step(
            &context,
            center_coordinate.0,
            center_coordinate.1,
            base_row,
            quote_row,
            base_residual,
            quote_residual,
        )
        .unwrap();
    let first_j_coordinate = (
        center_coordinate.0.checked_add(first_step.0).unwrap(),
        center_coordinate.1.checked_add(first_step.1).unwrap(),
    );

    // One scalar terminal cell decides whether the first-J point is already
    // usable or supplies the sole good-Broyden correction.
    let mut terminal_guidance_slot = None;
    evaluate_stage4c_frozen_center_bounded_axis(
        &compact,
        &context,
        cash_floors,
        first_j_coordinate.0,
        first_j_coordinate.1,
        &frozen,
        &mut terminal_guidance_slot,
    )
    .unwrap();
    stage4c_rescore_frozen_flow_at_exact_mark(
        terminal_guidance_slot.as_mut().unwrap(),
        &compact,
        &context,
        exact_a1_final_mark_base_nad,
    );
    let terminal_guidance = *terminal_guidance_slot.as_ref().unwrap();
    let terminal_signature = hlp_preposition_pair_signature(&terminal_guidance);
    assert!(terminal_guidance.settlement_cash_available);
    assert!(hlp_exact_derivative_sample_is_unbound(&terminal_guidance, &context));
    assert_eq!(terminal_guidance.structural_topology, center_topology);
    assert_eq!(terminal_guidance.guidance_exact_in_mode, center_mode);
    assert_eq!(terminal_guidance.guidance_settlement_trace, Some(center_trace));
    assert_eq!(terminal_guidance.guidance_d_actions, Some(center_actions));
    assert!(hlp_preposition_signature_class_matches(
        center_signature.base,
        terminal_signature.base,
    ));
    assert!(hlp_preposition_signature_class_matches(
        center_signature.quote,
        terminal_signature.quote,
    ));
    let first_j_guidance_safe = stage4c_candidate_is_fully_safe(&terminal_guidance, &context);
    let terminal_coordinate = if first_j_guidance_safe {
        first_j_coordinate
    } else {
        let current_base_error = hlp_exact_control_value(&terminal_guidance, MarketAsset::Base, base_row);
        let current_quote_error = hlp_exact_control_value(&terminal_guidance, MarketAsset::Quote, quote_row);
        basis
            .solve_broyden_candidate(
                &context,
                center_coordinate.0,
                center_coordinate.1,
                first_j_coordinate.0,
                first_j_coordinate.1,
                base_row,
                quote_row,
                current_base_error,
                current_quote_error,
            )
            .unwrap()
    };
    assert!(hlp_coordinate_within_center_trust(
        center_coordinate.0,
        terminal_coordinate.0,
        context.base_start,
    ));
    assert!(hlp_coordinate_within_center_trust(
        center_coordinate.1,
        terminal_coordinate.1,
        context.quote_start,
    ));

    center_slot = None;
    base_axis_slot = None;
    quote_axis_slot = None;
    terminal_guidance_slot = None;
    let _ = frozen;
    assert!(center_slot.is_none());
    assert!(base_axis_slot.is_none());
    assert!(quote_axis_slot.is_none());
    assert!(terminal_guidance_slot.is_none());

    let mut a2_market = snapshot.clone();
    let mut a2_projection_slot = None;
    let mut a2_candidate_slot = None;
    let mut a2_terminal_scratch = HlpTerminalSwapScratch::default();
    evaluate_concentrated_hlp_candidate(
        &mut a2_market,
        &mut lifecycle_scratch,
        &snapshot,
        &context,
        cash_floors,
        terminal_coordinate.0,
        terminal_coordinate.1,
        HlpCandidateEvaluationMode::Authoritative,
        &mut a2_projection_slot,
        &mut a2_candidate_slot,
        &mut a2_terminal_scratch,
    )
    .unwrap();
    let a2 = a2_candidate_slot.as_ref().unwrap();
    let a2_rows = hlp_stage4b2a_test_rows(a2).unwrap();
    let a2_signature = hlp_preposition_pair_signature(a2);
    let inactive_rows_zero = (context.base_start.active || [a2_rows[0], a2_rows[2], a2_rows[4]] == [0; 3])
        && (context.quote_start.active || [a2_rows[1], a2_rows[3], a2_rows[5]] == [0; 3]);
    let a2_identity = a2.authoritative.is_some()
        && a2_projection_slot.as_ref().unwrap().authoritative()
        && a2.settlement_cash_available
        && a2.structural_topology == center_topology
        && hlp_preposition_signature_class_matches(center_signature.base, a2_signature.base)
        && hlp_preposition_signature_class_matches(center_signature.quote, a2_signature.quote)
        && inactive_rows_zero
        && hlp_exact_derivative_sample_is_unbound(a2, &context)
        && hlp_coordinate_within_center_trust(center_coordinate.0, terminal_coordinate.0, context.base_start)
        && hlp_coordinate_within_center_trust(center_coordinate.1, terminal_coordinate.1, context.quote_start);
    let a2_safe = stage4c_candidate_is_fully_safe(a2, &context);
    eprintln!(
        "ZERO-A1 {label} A1=(0,0)/rows={a1_rows:?}/topology={a1_topology:?} center={center_coordinate:?}/rows={:?}/topology={center_topology:?} axes=({base_probe_delta},{quote_probe_delta}) first_J={first_j_coordinate:?}/rows={:?}/guidance_safe={first_j_guidance_safe} correction={} A2={terminal_coordinate:?}/rows={a2_rows:?}/identity={a2_identity}/safe={a2_safe} counts=(scalar={},authority={})",
        hlp_stage4b2a_test_rows(&center).unwrap(),
        hlp_stage4b2a_test_rows(&terminal_guidance).unwrap(),
        !first_j_guidance_safe,
        CONCENTRATED_PRE_SOLVE_COMPACT_EVALUATIONS.with(Cell::get),
        CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(Cell::get),
    );
    assert!(a2_identity);
    assert!(!a2_safe, "{label}: zero-A1 schedule unexpectedly became safe");
    assert_eq!(
        CONCENTRATED_PRE_SOLVE_COMPACT_EVALUATIONS.with(Cell::get),
        2 + active_axis_count
    );
    assert_eq!(CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(Cell::get), 2);
    assert_eq!(snapshot.try_to_vec().unwrap(), snapshot_before);
}

#[test]
fn stage4c_zero_a1_frozen_flow_solvent_two_point_one_base_remains_red() {
    let scale = 1_000_000_u64;
    let mut market = active_concentrated_hlp_market_with_decimals(6);
    market.base_side.credit_reserve(500_000 * scale, true).unwrap();
    market.quote_side.credit_reserve(1_000_000 * scale, true).unwrap();
    market.checkpoint_amm_neutral_inventory(0).unwrap();
    market.debt.base_borrow_index_nad = 21 * NAD as u128 / 10;
    market.debt.quote_borrow_index_nad = 21 * NAD as u128 / 10;
    assert_stage4c_zero_a1_frozen_flow_schedule(market, MarketAsset::Base, 350_000 * scale, "solvent-2.1-base");
}

/*
fn assert_stage4c_zero_quote_one_authority_broyden_spot_base(
    mut snapshot: Market,
    asset_in: MarketAsset,
    reserve_credit: u64,
    label: &str,
) {
    let current_slot = curve_slot(&snapshot);
    snapshot.prepare_amm_for_swap(current_slot).unwrap();
    let pre_state = snapshot.dynamic_fee_pre_state(current_slot).unwrap();
    let preliminary = snapshot
        .preliminary_swap_inputs_for_state(reserve_credit, current_slot, pre_state)
        .unwrap();
    let mut context_slot = None;
    capture_concentrated_hlp_solve_context_into(
        &snapshot,
        asset_in,
        reserve_credit,
        current_slot,
        pre_state,
        preliminary,
        SwapCashPolicy::Spot,
        &mut context_slot,
    )
    .unwrap();
    let context = context_slot.unwrap();
    let mut compact_slot = None;
    HlpCompactSolveContext::capture_into(&snapshot, &context, &mut compact_slot).unwrap();
    let compact = compact_slot.unwrap();
    let cash_floors = context.cash_policy.floors(&snapshot, asset_in, 0).unwrap();
    let (center_coordinate, scalar_input, scalar_output, scalar_end_price) =
        stage4c_zero_quote_geometry_seed(&compact, &context, reserve_credit);
    assert_ne!(center_coordinate, (0, 0));
    let snapshot_before = snapshot.try_to_vec().unwrap();
    reset_stage4b2a_test_state();

    let mut center_projection_slot = None;
    let mut center_candidate_slot = None;
    evaluate_compact_concentrated_hlp_candidate(
        &compact,
        &context,
        cash_floors,
        center_coordinate.0,
        center_coordinate.1,
        true,
        &mut center_projection_slot,
        &mut center_candidate_slot,
    )
    .unwrap();
    let center = *center_candidate_slot.as_ref().unwrap();
    let Some(ConcentratedHlpProjection::FrozenCenter(frozen_center)) =
        center_projection_slot
    else {
        panic!("{label}: fresh center must freeze")
    };
    assert!(center.settlement_cash_available);
    assert!(hlp_exact_derivative_sample_is_unbound(&center, &context));
    let center_rows = hlp_stage4b2a_test_rows(&center).unwrap();
    let center_topology = center.structural_topology;
    let center_signature = hlp_preposition_pair_signature(&center);
    let base_row =
        hlp_exact_control_row(&center, MarketAsset::Base, context.base_start).unwrap();
    let quote_row =
        hlp_exact_control_row(&center, MarketAsset::Quote, context.quote_start).unwrap();
    let center_base_error =
        hlp_exact_control_value(&center, MarketAsset::Base, base_row);
    let center_quote_error =
        hlp_exact_control_value(&center, MarketAsset::Quote, quote_row);

    let base_probe_delta = stage4c_signed_round_away_divisor(
        hlp_exact_axis_probe_delta(
            center_coordinate.0,
            context.base_start,
            center_base_error,
        )
        .unwrap(),
        16,
    )
    .unwrap();
    let quote_probe_delta = stage4c_signed_round_away_divisor(
        hlp_exact_axis_probe_delta(
            center_coordinate.1,
            context.quote_start,
            center_quote_error,
        )
        .unwrap(),
        16,
    )
    .unwrap();
    let base_axis_coordinate = (
        center_coordinate.0.checked_add(base_probe_delta).unwrap(),
        center_coordinate.1,
    );
    let quote_axis_coordinate = (
        center_coordinate.0,
        center_coordinate.1.checked_add(quote_probe_delta).unwrap(),
    );
    let mut base_axis_slot = None;
    evaluate_stage4c_frozen_center_bounded_axis(
        &compact,
        &context,
        cash_floors,
        base_axis_coordinate.0,
        base_axis_coordinate.1,
        &frozen_center.frozen,
        &mut base_axis_slot,
    )
    .unwrap();
    let base_axis = *base_axis_slot.as_ref().unwrap();
    let mut quote_axis_slot = None;
    evaluate_stage4c_frozen_center_bounded_axis(
        &compact,
        &context,
        cash_floors,
        quote_axis_coordinate.0,
        quote_axis_coordinate.1,
        &frozen_center.frozen,
        &mut quote_axis_slot,
    )
    .unwrap();
    let quote_axis = *quote_axis_slot.as_ref().unwrap();
    for axis in [&base_axis, &quote_axis] {
        let signature = hlp_preposition_pair_signature(axis);
        assert!(axis.settlement_cash_available);
        assert!(hlp_exact_derivative_sample_is_unbound(axis, &context));
        assert_eq!(axis.structural_topology, center_topology);
        assert!(hlp_preposition_signature_class_matches(
            center_signature.base,
            signature.base,
        ));
        assert!(hlp_preposition_signature_class_matches(
            center_signature.quote,
            signature.quote,
        ));
    }
    let basis = HlpFiniteDifferenceBasis {
        origin: HlpExactSampleRows::from_candidate(&center),
        base_probe_delta_nad: base_probe_delta,
        base_probe: HlpExactSampleRows::from_candidate(&base_axis),
        quote_probe_delta_nad: quote_probe_delta,
        quote_probe: HlpExactSampleRows::from_candidate(&quote_axis),
        base_probe_recorded: true,
        quote_probe_recorded: true,
        ..HlpFiniteDifferenceBasis::default()
    };
    let base_target = stage4c_opposite_half_budget_target(
        center_base_error,
        context.base_start.tracking.loss_budget_nad,
    )
    .unwrap();
    let quote_target = stage4c_opposite_half_budget_target(
        center_quote_error,
        context.quote_start.tracking.loss_budget_nad,
    )
    .unwrap();
    let first_step = basis
        .solve_step(
            &context,
            center_coordinate.0,
            center_coordinate.1,
            base_row,
            quote_row,
            center_base_error.checked_sub(base_target).unwrap(),
            center_quote_error.checked_sub(quote_target).unwrap(),
        )
        .unwrap();
    let first_j_coordinate = (
        center_coordinate.0.checked_add(first_step.0).unwrap(),
        center_coordinate.1.checked_add(first_step.1).unwrap(),
    );
    let mut first_j_slot = None;
    evaluate_stage4c_frozen_center_bounded_axis(
        &compact,
        &context,
        cash_floors,
        first_j_coordinate.0,
        first_j_coordinate.1,
        &frozen_center.frozen,
        &mut first_j_slot,
    )
    .unwrap();
    let first_j = *first_j_slot.as_ref().unwrap();
    let first_j_rows = hlp_stage4b2a_test_rows(&first_j).unwrap();
    let first_j_base_error =
        hlp_exact_control_value(&first_j, MarketAsset::Base, base_row);
    let first_j_quote_error =
        hlp_exact_control_value(&first_j, MarketAsset::Quote, quote_row);
    let first_j_contracts =
        first_j_base_error.unsigned_abs() < center_base_error.unsigned_abs()
            && first_j_quote_error.unsigned_abs() < center_quote_error.unsigned_abs();
    let terminal_coordinate = basis
        .solve_broyden_candidate(
            &context,
            center_coordinate.0,
            center_coordinate.1,
            first_j_coordinate.0,
            first_j_coordinate.1,
            base_row,
            quote_row,
            first_j_base_error,
            first_j_quote_error,
        )
        .unwrap();
    assert!(hlp_coordinate_within_center_trust(
        center_coordinate.0,
        terminal_coordinate.0,
        context.base_start,
    ));
    assert!(hlp_coordinate_within_center_trust(
        center_coordinate.1,
        terminal_coordinate.1,
        context.quote_start,
    ));

    center_candidate_slot = None;
    base_axis_slot = None;
    quote_axis_slot = None;
    first_j_slot = None;
    let mut terminal_market = snapshot.clone();
    let mut lifecycle_scratch = Market::default();
    let mut terminal_projection_slot = None;
    let mut terminal_candidate_slot = None;
    let mut terminal_scratch = HlpTerminalSwapScratch::default();
    evaluate_concentrated_hlp_candidate(
        &mut terminal_market,
        &mut lifecycle_scratch,
        &snapshot,
        &context,
        cash_floors,
        terminal_coordinate.0,
        terminal_coordinate.1,
        HlpCandidateEvaluationMode::Authoritative,
        &mut terminal_projection_slot,
        &mut terminal_candidate_slot,
        &mut terminal_scratch,
    )
    .unwrap();
    let terminal = terminal_candidate_slot.as_ref().unwrap();
    let terminal_rows = hlp_stage4b2a_test_rows(terminal).unwrap();
    let terminal_signature = hlp_preposition_pair_signature(terminal);
    let identity = terminal.authoritative.is_some()
        && terminal_projection_slot.as_ref().unwrap().authoritative()
        && terminal.settlement_cash_available
        && hlp_exact_derivative_sample_is_unbound(terminal, &context)
        && terminal.structural_topology == center_topology
        && hlp_preposition_signature_class_matches(
            center_signature.base,
            terminal_signature.base,
        )
        && hlp_preposition_signature_class_matches(
            center_signature.quote,
            terminal_signature.quote,
        )
        && hlp_coordinate_within_center_trust(
            center_coordinate.0,
            terminal_coordinate.0,
            context.base_start,
        )
        && hlp_coordinate_within_center_trust(
            center_coordinate.1,
            terminal_coordinate.1,
            context.quote_start,
        );
    let safe = stage4c_candidate_is_fully_safe(terminal, &context);
    eprintln!(
        "ZERO-QUOTE-ONE-AUTH-BROYDEN {label} scalar=(in={scalar_input},out={scalar_output},end_price={scalar_end_price}) center={center_coordinate:?}/rows={center_rows:?}/topology={center_topology:?} axes=({base_axis_coordinate:?}/{:?},{quote_axis_coordinate:?}/{:?}) targets=({base_target},{quote_target}) first_step={first_step:?} first_J={first_j_coordinate:?}/rows={first_j_rows:?}/contracts={first_j_contracts} A2={terminal_coordinate:?}/rows={terminal_rows:?}/safe={safe}/identity={identity} counts=(compact={},raw={},authority={},candidate={})",
        hlp_stage4b2a_test_rows(&base_axis).unwrap(),
        hlp_stage4b2a_test_rows(&quote_axis).unwrap(),
        CONCENTRATED_PRE_SOLVE_COMPACT_EVALUATIONS.with(Cell::get),
        CONCENTRATED_PRE_SOLVE_RAW_EVALUATIONS.with(Cell::get),
        CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(Cell::get),
        CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(Cell::get),
    );
    assert!(first_j_contracts);
    assert!(identity);
    assert!(safe);
    assert_eq!(CONCENTRATED_PRE_SOLVE_COMPACT_EVALUATIONS.with(Cell::get), 4);
    assert_eq!(CONCENTRATED_PRE_SOLVE_RAW_EVALUATIONS.with(Cell::get), 0);
    assert_eq!(CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(Cell::get), 1);
    assert_eq!(snapshot.try_to_vec().unwrap(), snapshot_before);
}

#[test]
fn stage4c_zero_quote_one_authority_broyden_spot_base_diagnostic() {
    assert_stage4c_zero_quote_one_authority_broyden_spot_base(
        stage4b2a_spot_market(false),
        MarketAsset::Base,
        350_000 * 1_000_000,
        "spot-base",
    );
}
*/

fn assert_stage4c_zero_quote_exact_a1_frozen_broyden_remains_red(
    mut snapshot: Market,
    asset_in: MarketAsset,
    reserve_credit: u64,
    label: &str,
) {
    let current_slot = curve_slot(&snapshot);
    snapshot.prepare_amm_for_swap(current_slot).unwrap();
    let pre_state = snapshot.dynamic_fee_pre_state(current_slot).unwrap();
    let preliminary = snapshot
        .preliminary_swap_inputs_for_state(reserve_credit, current_slot, pre_state)
        .unwrap();
    let mut context_slot = None;
    capture_concentrated_hlp_solve_context_into(
        &snapshot,
        asset_in,
        reserve_credit,
        current_slot,
        pre_state,
        preliminary,
        SwapCashPolicy::Spot,
        &mut context_slot,
    )
    .unwrap();
    let context = context_slot.unwrap();
    let mut compact_slot = None;
    HlpCompactSolveContext::capture_into(&snapshot, &context, &mut compact_slot).unwrap();
    let compact = compact_slot.unwrap();
    let cash_floors = context.cash_policy.floors(&snapshot, asset_in, 0).unwrap();
    let (a1_coordinate, scalar_input, scalar_output, scalar_end_price) =
        stage4c_zero_quote_geometry_seed(&compact, &context, reserve_credit);
    assert_ne!(a1_coordinate, (0, 0));
    let snapshot_before = snapshot.try_to_vec().unwrap();
    reset_stage4b2a_test_state();

    let mut lifecycle_scratch = Market::default();
    let mut a1_market = snapshot.clone();
    let mut a1_projection_slot = None;
    let mut a1_candidate_slot = None;
    let mut terminal_scratch = HlpTerminalSwapScratch::default();
    evaluate_concentrated_hlp_candidate(
        &mut a1_market,
        &mut lifecycle_scratch,
        &snapshot,
        &context,
        cash_floors,
        a1_coordinate.0,
        a1_coordinate.1,
        HlpCandidateEvaluationMode::Authoritative,
        &mut a1_projection_slot,
        &mut a1_candidate_slot,
        &mut terminal_scratch,
    )
    .unwrap();
    let exact_a1 = *a1_candidate_slot.as_ref().unwrap();
    assert!(exact_a1.authoritative.is_some());
    assert!(a1_projection_slot.as_ref().unwrap().authoritative());
    assert!(exact_a1.settlement_cash_available);
    assert!(hlp_exact_derivative_sample_is_unbound(&exact_a1, &context));
    let a1_rows = hlp_stage4b2a_test_rows(&exact_a1).unwrap();
    let a1_topology = exact_a1.structural_topology;
    let a1_signature = hlp_preposition_pair_signature(&exact_a1);
    let base_row = hlp_exact_control_row(&exact_a1, MarketAsset::Base, context.base_start).unwrap();
    let quote_row = hlp_exact_control_row(&exact_a1, MarketAsset::Quote, context.quote_start).unwrap();
    let a1_base_error = hlp_exact_control_value(&exact_a1, MarketAsset::Base, base_row);
    let a1_quote_error = hlp_exact_control_value(&exact_a1, MarketAsset::Quote, quote_row);

    let inventory_changed = exact_a1.base_receipt.ylp_mint_amount != 0
        || exact_a1.base_receipt.ylp_burn_amount != 0
        || exact_a1.quote_receipt.ylp_mint_amount != 0
        || exact_a1.quote_receipt.ylp_burn_amount != 0;
    downgrade_authoritative_a1_projection(
        a1_projection_slot.as_mut().unwrap(),
        &context,
        &compact,
        a1_market.base_side.shares.ylp_supply,
        inventory_changed,
        a1_topology,
    )
    .unwrap();
    let Some(ConcentratedHlpProjection::FrozenA1(frozen_a1)) = a1_projection_slot.take() else {
        panic!("{label}: exact A1 must destructively downgrade")
    };
    a1_candidate_slot = None;
    a1_market.clone_from(&snapshot);
    lifecycle_scratch.clone_from(&snapshot);

    let mut origin_slot = None;
    evaluate_stage4c_frozen_center_bounded_axis(
        &compact,
        &context,
        cash_floors,
        a1_coordinate.0,
        a1_coordinate.1,
        &frozen_a1,
        &mut origin_slot,
    )
    .unwrap();
    let origin = *origin_slot.as_ref().unwrap();
    let origin_rows = hlp_stage4b2a_test_rows(&origin).unwrap();
    assert_eq!(origin_rows, a1_rows);
    assert_eq!(origin.structural_topology, a1_topology);
    let origin_signature = hlp_preposition_pair_signature(&origin);
    assert!(hlp_preposition_signature_class_matches(
        a1_signature.base,
        origin_signature.base,
    ));
    assert!(hlp_preposition_signature_class_matches(
        a1_signature.quote,
        origin_signature.quote,
    ));

    let base_probe_delta = stage4c_signed_round_away_divisor(
        hlp_exact_axis_probe_delta(a1_coordinate.0, context.base_start, a1_base_error).unwrap(),
        16,
    )
    .unwrap();
    let quote_probe_delta = stage4c_signed_round_away_divisor(
        hlp_exact_axis_probe_delta(a1_coordinate.1, context.quote_start, a1_quote_error).unwrap(),
        16,
    )
    .unwrap();
    let base_axis_coordinate = (a1_coordinate.0.checked_add(base_probe_delta).unwrap(), a1_coordinate.1);
    let quote_axis_coordinate = (a1_coordinate.0, a1_coordinate.1.checked_add(quote_probe_delta).unwrap());
    let mut base_axis_slot = None;
    evaluate_stage4c_frozen_center_bounded_axis(
        &compact,
        &context,
        cash_floors,
        base_axis_coordinate.0,
        base_axis_coordinate.1,
        &frozen_a1,
        &mut base_axis_slot,
    )
    .unwrap();
    let base_axis = *base_axis_slot.as_ref().unwrap();
    let mut quote_axis_slot = None;
    evaluate_stage4c_frozen_center_bounded_axis(
        &compact,
        &context,
        cash_floors,
        quote_axis_coordinate.0,
        quote_axis_coordinate.1,
        &frozen_a1,
        &mut quote_axis_slot,
    )
    .unwrap();
    let quote_axis = *quote_axis_slot.as_ref().unwrap();
    for axis in [&base_axis, &quote_axis] {
        let signature = hlp_preposition_pair_signature(axis);
        assert!(axis.settlement_cash_available);
        assert!(hlp_exact_derivative_sample_is_unbound(axis, &context));
        assert_eq!(axis.structural_topology, a1_topology);
        assert!(hlp_preposition_signature_class_matches(
            a1_signature.base,
            signature.base,
        ));
        assert!(hlp_preposition_signature_class_matches(
            a1_signature.quote,
            signature.quote,
        ));
    }

    let basis = HlpFiniteDifferenceBasis {
        origin: HlpExactSampleRows::from_candidate(&origin),
        base_probe_delta_nad: base_probe_delta,
        base_probe: HlpExactSampleRows::from_candidate(&base_axis),
        quote_probe_delta_nad: quote_probe_delta,
        quote_probe: HlpExactSampleRows::from_candidate(&quote_axis),
        base_probe_recorded: true,
        quote_probe_recorded: true,
        ..HlpFiniteDifferenceBasis::default()
    };
    let base_target =
        stage4c_opposite_half_budget_target(a1_base_error, context.base_start.tracking.loss_budget_nad).unwrap();
    let quote_target =
        stage4c_opposite_half_budget_target(a1_quote_error, context.quote_start.tracking.loss_budget_nad).unwrap();
    let first_step = basis
        .solve_step(
            &context,
            a1_coordinate.0,
            a1_coordinate.1,
            base_row,
            quote_row,
            a1_base_error.checked_sub(base_target).unwrap(),
            a1_quote_error.checked_sub(quote_target).unwrap(),
        )
        .unwrap();
    let first_j_coordinate = (
        a1_coordinate.0.checked_add(first_step.0).unwrap(),
        a1_coordinate.1.checked_add(first_step.1).unwrap(),
    );
    let mut first_j_slot = None;
    evaluate_stage4c_frozen_center_bounded_axis(
        &compact,
        &context,
        cash_floors,
        first_j_coordinate.0,
        first_j_coordinate.1,
        &frozen_a1,
        &mut first_j_slot,
    )
    .unwrap();
    let first_j = *first_j_slot.as_ref().unwrap();
    let first_j_rows = hlp_stage4b2a_test_rows(&first_j).unwrap();
    let first_j_base_error = hlp_exact_control_value(&first_j, MarketAsset::Base, base_row);
    let first_j_quote_error = hlp_exact_control_value(&first_j, MarketAsset::Quote, quote_row);
    let first_j_contracts = first_j_base_error.unsigned_abs() < a1_base_error.unsigned_abs()
        && first_j_quote_error.unsigned_abs() < a1_quote_error.unsigned_abs();
    let terminal_coordinate = basis
        .solve_broyden_candidate(
            &context,
            a1_coordinate.0,
            a1_coordinate.1,
            first_j_coordinate.0,
            first_j_coordinate.1,
            base_row,
            quote_row,
            first_j_base_error,
            first_j_quote_error,
        )
        .unwrap();
    assert!(hlp_coordinate_within_center_trust(
        a1_coordinate.0,
        terminal_coordinate.0,
        context.base_start,
    ));
    assert!(hlp_coordinate_within_center_trust(
        a1_coordinate.1,
        terminal_coordinate.1,
        context.quote_start,
    ));

    origin_slot = None;
    base_axis_slot = None;
    quote_axis_slot = None;
    first_j_slot = None;
    let mut a2_market = snapshot.clone();
    let mut a2_projection_slot = None;
    let mut a2_candidate_slot = None;
    let mut a2_terminal_scratch = HlpTerminalSwapScratch::default();
    evaluate_concentrated_hlp_candidate(
        &mut a2_market,
        &mut lifecycle_scratch,
        &snapshot,
        &context,
        cash_floors,
        terminal_coordinate.0,
        terminal_coordinate.1,
        HlpCandidateEvaluationMode::Authoritative,
        &mut a2_projection_slot,
        &mut a2_candidate_slot,
        &mut a2_terminal_scratch,
    )
    .unwrap();
    let a2 = a2_candidate_slot.as_ref().unwrap();
    let a2_rows = hlp_stage4b2a_test_rows(a2).unwrap();
    let a2_signature = hlp_preposition_pair_signature(a2);
    let identity = a2.authoritative.is_some()
        && a2_projection_slot.as_ref().unwrap().authoritative()
        && a2.settlement_cash_available
        && hlp_exact_derivative_sample_is_unbound(a2, &context)
        && a2.structural_topology == a1_topology
        && hlp_preposition_signature_class_matches(a1_signature.base, a2_signature.base)
        && hlp_preposition_signature_class_matches(a1_signature.quote, a2_signature.quote)
        && hlp_coordinate_within_center_trust(a1_coordinate.0, terminal_coordinate.0, context.base_start)
        && hlp_coordinate_within_center_trust(a1_coordinate.1, terminal_coordinate.1, context.quote_start);
    let safe = stage4c_candidate_is_fully_safe(a2, &context);
    eprintln!(
        "ZERO-QUOTE-EXACT-A1-FROZEN-BROYDEN {label} scalar=(in={scalar_input},out={scalar_output},end_price={scalar_end_price}) A1={a1_coordinate:?}/rows={a1_rows:?}/topology={a1_topology:?} origin={origin_rows:?}/equal={} axes=({base_axis_coordinate:?}/{:?},{quote_axis_coordinate:?}/{:?}) targets=({base_target},{quote_target}) first_step={first_step:?} first_J={first_j_coordinate:?}/rows={first_j_rows:?}/contracts={first_j_contracts} A2={terminal_coordinate:?}/rows={a2_rows:?}/safe={safe}/identity={identity} counts=(compact={},raw={},authority={},candidate={})",
        origin_rows == a1_rows,
        hlp_stage4b2a_test_rows(&base_axis).unwrap(),
        hlp_stage4b2a_test_rows(&quote_axis).unwrap(),
        CONCENTRATED_PRE_SOLVE_COMPACT_EVALUATIONS.with(Cell::get),
        CONCENTRATED_PRE_SOLVE_RAW_EVALUATIONS.with(Cell::get),
        CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(Cell::get),
        CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(Cell::get),
    );
    assert!(first_j_contracts);
    assert!(identity);
    assert!(!safe);
    assert_eq!(CONCENTRATED_PRE_SOLVE_COMPACT_EVALUATIONS.with(Cell::get), 4);
    assert_eq!(CONCENTRATED_PRE_SOLVE_RAW_EVALUATIONS.with(Cell::get), 0);
    assert_eq!(CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(Cell::get), 2);
    assert_eq!(snapshot.try_to_vec().unwrap(), snapshot_before);
}

#[test]
fn stage4c_zero_quote_exact_a1_frozen_broyden_spot_base_remains_red() {
    assert_stage4c_zero_quote_exact_a1_frozen_broyden_remains_red(
        stage4b2a_spot_market(false),
        MarketAsset::Base,
        350_000 * 1_000_000,
        "spot-base",
    );
}

#[test]
fn stage4c_center_fresh_spot_base_differential() {
    assert_stage4c_center_fresh_active(
        stage4b2a_spot_market(false),
        MarketAsset::Base,
        "spot-base",
        Some(true),
        false,
        1,
        None,
    );
}

#[test]
fn stage4c_center_fresh_solvent_two_point_one_base_differential() {
    let scale = 1_000_000_u64;
    let mut market = active_concentrated_hlp_market_with_decimals(6);
    market.base_side.credit_reserve(500_000 * scale, true).unwrap();
    market.quote_side.credit_reserve(1_000_000 * scale, true).unwrap();
    market.checkpoint_amm_neutral_inventory(0).unwrap();
    market.debt.base_borrow_index_nad = 21 * NAD as u128 / 10;
    market.debt.quote_borrow_index_nad = 21 * NAD as u128 / 10;
    assert_stage4c_center_fresh_active(market, MarketAsset::Base, "solvent-2.1-base", Some(true), true, 1, None);
}

#[test]
fn stage4c_center_fresh_one_active_unpaid_base_differential() {
    let scale = 1_000_000_u64;
    let mut market = seeded_market();
    market.base_side.asset_decimals = 6;
    market.quote_side.asset_decimals = 6;
    configure_market_depth(&mut market, 1_000_000 * scale, 20_000);
    market
        .deposit_single_sided(MarketAsset::Base, 100_000 * scale, 1)
        .unwrap();
    enable_concentrated_curve(&mut market);
    let curve_before = market.curve_reserves_nad().unwrap();
    let principal = 200_000 * scale;
    let unpaid_interest = 500 * scale;
    market.base_side.reserves.cash_reserve -= principal;
    market.debt.fixed_base_shares = principal as u128;
    market.debt.fixed_base_principal = principal;
    market.debt.base_borrow_index_nad =
        ((principal as u128 + unpaid_interest as u128) * NAD as u128) / principal as u128;
    market.base_side.reserves.live_reserve += unpaid_interest;
    assert_eq!(market.curve_reserves_nad().unwrap(), curve_before);

    assert_stage4c_center_fresh_active(
        market,
        MarketAsset::Base,
        "one-active-unpaid-base",
        Some(true),
        true,
        -1,
        None,
    );
}

#[test]
fn stage4c_center_fresh_rawless_spot_base_characterization() {
    assert_stage4c_center_fresh_active(
        stage4b2a_spot_market(false),
        MarketAsset::Base,
        "rawless-spot-base",
        Some(true),
        true,
        -1,
        Some(true),
    );
}

#[test]
fn stage4c_center_fresh_rawless_solvent_two_point_one_base_characterization() {
    let scale = 1_000_000_u64;
    let mut market = active_concentrated_hlp_market_with_decimals(6);
    market.base_side.credit_reserve(500_000 * scale, true).unwrap();
    market.quote_side.credit_reserve(1_000_000 * scale, true).unwrap();
    market.checkpoint_amm_neutral_inventory(0).unwrap();
    market.debt.base_borrow_index_nad = 21 * NAD as u128 / 10;
    market.debt.quote_borrow_index_nad = 21 * NAD as u128 / 10;
    assert_stage4c_center_fresh_active(
        market,
        MarketAsset::Base,
        "rawless-solvent-2.1-base",
        Some(true),
        true,
        1,
        Some(true),
    );
}

#[test]
fn stage4c_center_fresh_rawless_one_active_unpaid_base_characterization() {
    let scale = 1_000_000_u64;
    let mut market = seeded_market();
    market.base_side.asset_decimals = 6;
    market.quote_side.asset_decimals = 6;
    configure_market_depth(&mut market, 1_000_000 * scale, 20_000);
    market
        .deposit_single_sided(MarketAsset::Base, 100_000 * scale, 1)
        .unwrap();
    enable_concentrated_curve(&mut market);
    let curve_before = market.curve_reserves_nad().unwrap();
    let principal = 200_000 * scale;
    let unpaid_interest = 500 * scale;
    market.base_side.reserves.cash_reserve -= principal;
    market.debt.fixed_base_shares = principal as u128;
    market.debt.fixed_base_principal = principal;
    market.debt.base_borrow_index_nad =
        ((principal as u128 + unpaid_interest as u128) * NAD as u128) / principal as u128;
    market.base_side.reserves.live_reserve += unpaid_interest;
    assert_eq!(market.curve_reserves_nad().unwrap(), curve_before);
    assert_stage4c_center_fresh_active(
        market,
        MarketAsset::Base,
        "rawless-one-active-unpaid-base",
        Some(true),
        true,
        -1,
        Some(true),
    );
}

#[test]
fn stage4c_center_fresh_rawless_spot_quote_remains_red() {
    assert_stage4c_center_fresh_active(
        stage4b2a_spot_market(false),
        MarketAsset::Quote,
        "rawless-spot-quote",
        Some(true),
        true,
        -1,
        Some(false),
    );
}

#[test]
fn stage4c_frozen_a1_local_spot_quote_is_safe() {
    assert_stage4c_center_fresh_active_with_credit(
        stage4b2a_spot_market(false),
        MarketAsset::Quote,
        350_000 * 1_000_000,
        "frozen-a1-local-spot-quote",
        Some(true),
        true,
        1,
        None,
        Some(true),
    );
}

#[test]
fn stage4c_opposite_full_axis_active_quote_characterization() {
    assert_stage4c_center_fresh_active(
        active_concentrated_hlp_market_with_decimals(6),
        MarketAsset::Quote,
        "opposite-full-axis-active-quote",
        Some(true),
        true,
        -1,
        Some(true),
    );
}

#[test]
fn stage4c_opposite_full_axis_asymmetric_unpaid_quote_characterization() {
    let scale = 1_000_000_u64;
    let mut market = seeded_market();
    market.base_side.asset_decimals = 6;
    market.quote_side.asset_decimals = 6;
    configure_market_depth(&mut market, 1_000_000 * scale, 20_000);
    market
        .deposit_single_sided(MarketAsset::Base, 100_000 * scale, 1)
        .unwrap();
    enable_concentrated_curve(&mut market);
    let curve_before = market.curve_reserves_nad().unwrap();
    let principal = 200_000 * scale;
    let unpaid_interest = 500 * scale;
    market.base_side.reserves.cash_reserve -= principal;
    market.debt.fixed_base_shares = principal as u128;
    market.debt.fixed_base_principal = principal;
    market.debt.base_borrow_index_nad =
        ((principal as u128 + unpaid_interest as u128) * NAD as u128) / principal as u128;
    market.base_side.reserves.live_reserve += unpaid_interest;
    assert_eq!(market.curve_reserves_nad().unwrap(), curve_before);
    assert_stage4c_center_fresh_active(
        market,
        MarketAsset::Quote,
        "opposite-full-axis-asymmetric-unpaid-quote",
        Some(true),
        true,
        -1,
        Some(true),
    );
}

#[test]
fn stage4c_opposite_full_axis_funding_one_point_one_characterization() {
    for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = active_concentrated_hlp_market_with_decimals(6);
        market.debt.base_borrow_index_nad = 11 * NAD as u128 / 10;
        market.debt.quote_borrow_index_nad = 11 * NAD as u128 / 10;
        assert_stage4c_center_fresh_active(
            market,
            asset_in,
            match asset_in {
                MarketAsset::Base => "opposite-full-axis-funding-1.1-base",
                MarketAsset::Quote => "opposite-full-axis-funding-1.1-quote",
            },
            Some(true),
            true,
            -1,
            Some(true),
        );
    }
}

#[test]
fn stage4c_guidance_d_action_two_sign_joint_base_500k_characterization() {
    for axis_divisor in [1_i128, -1] {
        assert_stage4c_center_fresh_active_with_credit(
            active_concentrated_hlp_market_with_decimals(6),
            MarketAsset::Base,
            500_000 * 1_000_000,
            if axis_divisor > 0 {
                "guidance-d-joint-base-500k-inward"
            } else {
                "guidance-d-joint-base-500k-reflected"
            },
            None,
            true,
            axis_divisor,
            None,
            None,
        );
    }
}

#[test]
fn stage4c_guidance_d_action_two_sign_asymmetric_unpaid_quote_characterization() {
    let build = || {
        let scale = 1_000_000_u64;
        let mut market = seeded_market();
        market.base_side.asset_decimals = 6;
        market.quote_side.asset_decimals = 6;
        configure_market_depth(&mut market, 1_000_000 * scale, 20_000);
        market
            .deposit_single_sided(MarketAsset::Base, 100_000 * scale, 1)
            .unwrap();
        enable_concentrated_curve(&mut market);
        let curve_before = market.curve_reserves_nad().unwrap();
        let principal = 200_000 * scale;
        let unpaid_interest = 500 * scale;
        market.base_side.reserves.cash_reserve -= principal;
        market.debt.fixed_base_shares = principal as u128;
        market.debt.fixed_base_principal = principal;
        market.debt.base_borrow_index_nad =
            ((principal as u128 + unpaid_interest as u128) * NAD as u128) / principal as u128;
        market.base_side.reserves.live_reserve += unpaid_interest;
        assert_eq!(market.curve_reserves_nad().unwrap(), curve_before);
        market
    };
    for axis_divisor in [1_i128, -1] {
        assert_stage4c_center_fresh_active(
            build(),
            MarketAsset::Quote,
            if axis_divisor > 0 {
                "guidance-d-asymmetric-unpaid-quote-inward"
            } else {
                "guidance-d-asymmetric-unpaid-quote-reflected"
            },
            None,
            true,
            axis_divisor,
            None,
        );
    }
}

fn assert_stage4b2a_spot_base_success(retain_dynamic_surcharge: bool) {
    let mut market = stage4b2a_spot_market(retain_dynamic_surcharge);
    reset_stage4b2a_test_state();
    let (base, quote, swap_quote) =
        solve_concentrated_hlp_swap(&mut market, MarketAsset::Base, 350_000 * 1_000_000).unwrap();
    assert_stage4c_counts(4, 0, 2);
    assert_stage4c_rawless_success_trace(
        (-33_851_687_807_396, 102_352_277_765_190),
        (-33_857_586_522_400, 102_377_850_307_588),
        [
            -1_136_968,
            -1_478_635,
            153_280_704_813,
            371_163_871_277,
            3_732_182_583_535,
            8_582_241_134_363,
        ],
        (HlpPlanTopologyKind::DeleverageDirect, HlpPlanTopologyKind::LeverageUp),
    );
    assert_concentrated_candidate_safe(&market, base, quote, swap_quote);
    assert_market_hlp_invariants(&market);
}

#[test]
fn stage4c_rawless_spot_base_matches_exact_a2() {
    assert_stage4b2a_spot_base_success(false);
}
#[test]
fn stage4c_rawless_retained_base_matches_exact_a2() {
    assert_stage4b2a_spot_base_success(true);
}

#[test]
fn stage4c_rawless_solvent_two_point_one_base_is_safe() {
    let scale = 1_000_000_u64;
    let mut market = active_concentrated_hlp_market_with_decimals(6);
    market.base_side.credit_reserve(500_000 * scale, true).unwrap();
    market.quote_side.credit_reserve(1_000_000 * scale, true).unwrap();
    market.checkpoint_amm_neutral_inventory(0).unwrap();
    market.debt.base_borrow_index_nad = 21 * NAD as u128 / 10;
    market.debt.quote_borrow_index_nad = 21 * NAD as u128 / 10;

    reset_stage4b2a_test_state();
    let (base, quote, swap_quote) =
        solve_concentrated_hlp_swap(&mut market, MarketAsset::Base, 350_000 * scale).unwrap();
    assert_stage4c_counts(4, 0, 2);
    assert_concentrated_candidate_safe(&market, base, quote, swap_quote);
    assert_market_hlp_invariants(&market);
}

#[test]
fn stage4c_frozen_center_fingerprint_mutation_fails_closed() {
    let mut market = stage4b2a_spot_market(false);
    let before = market.try_to_vec().unwrap();
    reset_stage4b2a_test_state();
    let _mutation = MutateFrozenCenterFingerprintGuard::enable();
    assert_eq!(
        solve_concentrated_hlp_swap(&mut market, MarketAsset::Base, 350_000 * 1_000_000).unwrap_err(),
        error!(ErrorCode::HlpSettlementUnavailable)
    );
    assert_eq!(market.try_to_vec().unwrap(), before);
    assert_stage4c_counts(3, 0, 0);
    assert_eq!(HLP_STAGE4B2A_LAST_TRACE.with(|trace| *trace.borrow()), None);
}

#[test]
fn stage4c_rawless_one_active_unpaid_base_matches_exact_a2() {
    let scale = 1_000_000_u64;
    let mut market = seeded_market();
    market.base_side.asset_decimals = 6;
    market.quote_side.asset_decimals = 6;
    configure_market_depth(&mut market, 1_000_000 * scale, 20_000);
    market
        .deposit_single_sided(MarketAsset::Base, 100_000 * scale, 1)
        .unwrap();
    enable_concentrated_curve(&mut market);
    let curve_before = market.curve_reserves_nad().unwrap();
    let principal = 200_000 * scale;
    let unpaid_interest = 500 * scale;
    market.base_side.reserves.cash_reserve -= principal;
    market.debt.fixed_base_shares = principal as u128;
    market.debt.fixed_base_principal = principal;
    market.debt.base_borrow_index_nad =
        ((principal as u128 + unpaid_interest as u128) * NAD as u128) / principal as u128;
    market.base_side.reserves.live_reserve += unpaid_interest;
    assert_eq!(market.curve_reserves_nad().unwrap(), curve_before);

    reset_stage4b2a_test_state();
    let (base, quote, swap_quote) =
        solve_concentrated_hlp_swap(&mut market, MarketAsset::Base, 350_000 * scale).unwrap();
    assert_stage4c_counts(3, 0, 2);
    assert_stage4c_rawless_success_trace(
        (-37_520_473_080_830, 0),
        (-37_518_581_549_747, 0),
        [-186_511, 0, -185_140, 0, 0, 0],
        (HlpPlanTopologyKind::DeleverageDirect, HlpPlanTopologyKind::Inactive),
    );
    assert_eq!(quote, empty_hlp_rebalance_receipt(MarketAsset::Quote));
    assert_concentrated_candidate_safe(&market, base, quote, swap_quote);
    assert_market_hlp_invariants(&market);
}

#[test]
fn stage4c_rawless_spot_quote_red_restores_after_a2() {
    let mut market = stage4b2a_spot_market(false);
    let before = market.try_to_vec().unwrap();
    reset_stage4b2a_test_state();
    assert_eq!(
        solve_concentrated_hlp_swap(&mut market, MarketAsset::Quote, 350_000 * 1_000_000).unwrap_err(),
        error!(ErrorCode::HlpSettlementUnavailable)
    );
    assert_eq!(market.try_to_vec().unwrap(), before);
    assert_stage4c_counts(4, 0, 2);
    let trace = stage4b2a_last_trace();
    assert_eq!(trace.a1_coordinate, Some((16_864_563_184_013, -28_861_710_222_310)));
    assert_eq!(trace.raw_coordinate, None);
    assert_eq!(
        trace.a2_rows,
        Some([
            -1_542_153_621,
            -2_652_297_016,
            -20_580_792,
            -40_995_302,
            1_954_132_041_226,
            3_352_692_979_496,
        ])
    );
    assert_eq!(trace.raw_rows, None);
    assert_eq!(trace.raw_robust, None);
    assert_eq!(trace.a2_coordinate, Some((16_865_824_197_463, -28_858_824_833_913)));
    assert_eq!(trace.a1_topology, trace.a2_topology);
    let a1_signature = trace.a1_signature.unwrap();
    let a2_signature = trace.a2_signature.unwrap();
    assert!(hlp_preposition_signature_class_matches(
        a1_signature.base,
        a2_signature.base
    ));
    assert!(hlp_preposition_signature_class_matches(
        a1_signature.quote,
        a2_signature.quote
    ));
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
fn cpmm_predictor_keeps_four_compact_samples_and_one_authoritative_gate() {
    let scale = 1_000_000_u64;
    for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = seeded_market();
        market.base_side.asset_decimals = 6;
        market.quote_side.asset_decimals = 6;
        configure_market_depth(&mut market, 1_000_000 * scale, 20_000);
        market
            .deposit_single_sided(MarketAsset::Base, 100_000 * scale, 1)
            .unwrap();
        market
            .deposit_single_sided(MarketAsset::Quote, 200_000 * scale, 1)
            .unwrap();
        market.config.settlement_divergence_bps = BPS_DENOMINATOR;
        market.ensure_amm_initialized(0).unwrap();
        assert!(market.current_curve_parameters(0).is_cpmm());

        CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(|count| count.set(0));
        CONCENTRATED_PRE_SOLVE_COMPACT_EVALUATIONS.with(|count| count.set(0));
        CONCENTRATED_PRE_SOLVE_RAW_EVALUATIONS.with(|count| count.set(0));
        CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(|count| count.set(0));
        HLP_COMPACT_GUIDANCE_EXACT_OUT_PROBES.with(|count| count.set(0));
        HLP_COMPACT_GUIDANCE_EXACT_OUT_CANONICAL_FALLBACKS.with(|count| count.set(0));
        let (base, quote, swap_quote) = solve_concentrated_hlp_swap(&mut market, asset_in, 100_000 * scale).unwrap();

        assert_eq!(CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(Cell::get), 5);
        assert_eq!(CONCENTRATED_PRE_SOLVE_COMPACT_EVALUATIONS.with(Cell::get), 4);
        assert_eq!(CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(Cell::get), 1);
        assert_eq!(HLP_COMPACT_GUIDANCE_EXACT_OUT_PROBES.with(Cell::get), 0);
        assert_eq!(HLP_COMPACT_GUIDANCE_EXACT_OUT_CANONICAL_FALLBACKS.with(Cell::get), 0);
        assert_concentrated_candidate_safe(&market, base, quote, swap_quote);
        assert_market_hlp_invariants(&market);
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
        CONCENTRATED_PRE_SOLVE_COMPACT_EVALUATIONS.with(|count| count.set(0));
        CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(|count| count.set(0));
        assert_eq!(
            solve_concentrated_hlp_swap(&mut capped, asset_in, 500_000).unwrap_err(),
            error!(ErrorCode::HlpSettlementUnavailable)
        );
        let evaluations = CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(Cell::get);
        let compact_evaluations = CONCENTRATED_PRE_SOLVE_COMPACT_EVALUATIONS.with(Cell::get);
        let authoritative_evaluations = CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(Cell::get);
        assert!(evaluations > 1 && evaluations <= HLP_CONCENTRATED_MAX_CANDIDATE_EVALUATIONS);
        assert!(compact_evaluations <= HLP_CONCENTRATED_MAX_COMPACT_EVALUATIONS);
        assert!(authoritative_evaluations <= 1);
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
        CONCENTRATED_PRE_SOLVE_COMPACT_EVALUATIONS.with(|count| count.set(0));
        CONCENTRATED_PRE_SOLVE_RAW_EVALUATIONS.with(|count| count.set(0));
        CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(|count| count.set(0));
        let prepared = crate::instructions::SwapRequest {
            current_slot: 0,
            asset_in,
            reserve_credit: 350_000 * scale,
        }
        .prepare(&mut market);
        if asset_in == MarketAsset::Quote {
            assert_eq!(prepared.unwrap_err(), error!(ErrorCode::HlpSettlementUnavailable));
            assert_stage4c_counts(4, 0, 2);
            continue;
        }
        let prepared = prepared.unwrap();
        let quote = prepared.quote;
        let evaluations = CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(Cell::get);
        let compact_evaluations = CONCENTRATED_PRE_SOLVE_COMPACT_EVALUATIONS.with(Cell::get);
        let raw_evaluations = CONCENTRATED_PRE_SOLVE_RAW_EVALUATIONS.with(Cell::get);
        let authoritative_evaluations = CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(Cell::get);
        assert_eq!(
            (
                evaluations,
                compact_evaluations,
                raw_evaluations,
                authoritative_evaluations
            ),
            (6, 4, 0, 2),
            "asset_in={asset_in:?}"
        );
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
        market.config.amm.adjustment_threshold_nad = 0;
        market.config.amm.adjustment_step_nad = 0;
        market.config.amm.min_adjustment_interval_slots = 0;
        market.amm.retain_dynamic_surcharge = false;
        CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(|count| count.set(0));
        CONCENTRATED_PRE_SOLVE_COMPACT_EVALUATIONS.with(|count| count.set(0));
        CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(|count| count.set(0));
        let (base, quote_receipt, quote) = solve_concentrated_hlp_swap(&mut market, asset_in, 350_000 * scale)
            .unwrap_or_else(|error| {
                panic!(
                    "asset_in={asset_in:?} candidates={} compact={} authority={}: {error:?}",
                    CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(Cell::get),
                    CONCENTRATED_PRE_SOLVE_COMPACT_EVALUATIONS.with(Cell::get),
                    CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(Cell::get),
                )
            });
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
fn two_active_concentrated_hlps_with_funding_interest_settle_both_directions_within_stage4b1_caps() {
    let scale = 1_000_000_u64;
    for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = active_concentrated_hlp_market_with_decimals(6);
        market.debt.base_borrow_index_nad = 11 * NAD as u128 / 10;
        market.debt.quote_borrow_index_nad = 11 * NAD as u128 / 10;
        assert_eq!(market.unrealized_interest(MarketAsset::Base).unwrap(), 0);
        assert_eq!(market.unrealized_interest(MarketAsset::Quote).unwrap(), 0);
        CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(|count| count.set(0));
        CONCENTRATED_PRE_SOLVE_COMPACT_EVALUATIONS.with(|count| count.set(0));
        CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(|count| count.set(0));
        HLP_COMPACT_GUIDANCE_EXACT_OUT_PROBES.with(|count| count.set(0));
        HLP_COMPACT_GUIDANCE_EXACT_OUT_CANONICAL_FALLBACKS.with(|count| count.set(0));

        let prepared = crate::instructions::SwapRequest {
            current_slot: 0,
            asset_in,
            reserve_credit: 350_000 * scale,
        }
        .prepare(&mut market)
        .unwrap_or_else(|error| panic!("asset_in={asset_in:?}: {error:?}"));
        let evaluations = CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(Cell::get);
        let compact_evaluations = CONCENTRATED_PRE_SOLVE_COMPACT_EVALUATIONS.with(Cell::get);
        let authoritative_evaluations = CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(Cell::get);
        assert!(evaluations <= HLP_CONCENTRATED_MAX_CANDIDATE_EVALUATIONS);
        assert!(compact_evaluations <= 4, "asset_in={asset_in:?}");
        assert!(authoritative_evaluations <= HLP_CONCENTRATED_MAX_AUTHORITATIVE_EVALUATIONS);
        let expected_authoritative_evaluations = match asset_in {
            MarketAsset::Base => 2,
            MarketAsset::Quote => 1,
        };
        assert_eq!(
            authoritative_evaluations, expected_authoritative_evaluations,
            "asset_in={asset_in:?}"
        );
        let exact_out_probes = HLP_COMPACT_GUIDANCE_EXACT_OUT_PROBES.with(Cell::get);
        assert!(
            exact_out_probes
                <= compact_evaluations
                    .saturating_mul(4)
                    .saturating_mul(MAX_RESIDUAL_PROBES_PER_BOUNDED_EXACT_OUT_LEG as u32),
            "asset_in={asset_in:?} exact_out_probes={exact_out_probes}"
        );
        assert_eq!(
            HLP_COMPACT_GUIDANCE_EXACT_OUT_CANONICAL_FALLBACKS.with(Cell::get),
            0,
            "asset_in={asset_in:?}"
        );
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
    CONCENTRATED_PRE_SOLVE_COMPACT_EVALUATIONS.with(|count| count.set(0));
    CONCENTRATED_PRE_SOLVE_RAW_EVALUATIONS.with(|count| count.set(0));
    CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(|count| count.set(0));
    HLP_COMPACT_GUIDANCE_EXACT_IN_PROBES.with(|count| count.set(0));
    HLP_COMPACT_GUIDANCE_EXACT_OUT_PROBES.with(|count| count.set(0));
    HLP_COMPACT_GUIDANCE_EXACT_OUT_CANONICAL_FALLBACKS.with(|count| count.set(0));
    HLP_RAW_CANONICAL_SCALAR_CALLS.with(|count| count.set(0));
    HLP_RAW_CANONICAL_SCALAR_RESIDUALS.with(|count| count.set(0));
    let prepared = request.prepare(&mut first).unwrap();
    let first_evaluations = CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(Cell::get);
    let first_compact_evaluations = CONCENTRATED_PRE_SOLVE_COMPACT_EVALUATIONS.with(Cell::get);
    let first_raw_evaluations = CONCENTRATED_PRE_SOLVE_RAW_EVALUATIONS.with(Cell::get);
    let first_authoritative_evaluations = CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(Cell::get);
    let first_exact_in_probes = HLP_COMPACT_GUIDANCE_EXACT_IN_PROBES.with(Cell::get);
    let first_exact_out_probes = HLP_COMPACT_GUIDANCE_EXACT_OUT_PROBES.with(Cell::get);
    let first_exact_out_fallbacks = HLP_COMPACT_GUIDANCE_EXACT_OUT_CANONICAL_FALLBACKS.with(Cell::get);
    let first_raw_scalar_calls = HLP_RAW_CANONICAL_SCALAR_CALLS.with(Cell::get);
    let first_raw_scalar_residuals = HLP_RAW_CANONICAL_SCALAR_RESIDUALS.with(Cell::get);
    CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(|count| count.set(0));
    CONCENTRATED_PRE_SOLVE_COMPACT_EVALUATIONS.with(|count| count.set(0));
    CONCENTRATED_PRE_SOLVE_RAW_EVALUATIONS.with(|count| count.set(0));
    CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(|count| count.set(0));
    HLP_COMPACT_GUIDANCE_EXACT_IN_PROBES.with(|count| count.set(0));
    HLP_RAW_CANONICAL_SCALAR_CALLS.with(|count| count.set(0));
    HLP_RAW_CANONICAL_SCALAR_RESIDUALS.with(|count| count.set(0));
    let replay_prepared = request.prepare(&mut replay).unwrap();
    let replay_evaluations = CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(Cell::get);
    let replay_compact_evaluations = CONCENTRATED_PRE_SOLVE_COMPACT_EVALUATIONS.with(Cell::get);
    let replay_raw_evaluations = CONCENTRATED_PRE_SOLVE_RAW_EVALUATIONS.with(Cell::get);
    let replay_authoritative_evaluations = CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(Cell::get);
    let replay_exact_in_probes = HLP_COMPACT_GUIDANCE_EXACT_IN_PROBES.with(Cell::get);
    let replay_raw_scalar_calls = HLP_RAW_CANONICAL_SCALAR_CALLS.with(Cell::get);
    let replay_raw_scalar_residuals = HLP_RAW_CANONICAL_SCALAR_RESIDUALS.with(Cell::get);

    assert_eq!(first_evaluations, 6);
    assert_eq!(first_compact_evaluations, 4);
    assert_eq!(first_raw_evaluations, 0);
    assert_eq!(replay_evaluations, first_evaluations);
    assert_eq!(replay_compact_evaluations, first_compact_evaluations);
    assert_eq!(replay_raw_evaluations, first_raw_evaluations);
    assert_eq!(first_authoritative_evaluations, 2);
    assert_eq!(first_exact_in_probes, 10);
    assert_eq!(first_exact_out_probes, 30);
    assert_eq!(first_exact_out_fallbacks, 0);
    assert_eq!(first_raw_scalar_calls, 0);
    assert_eq!(first_raw_scalar_residuals, 0);
    assert_eq!(replay_authoritative_evaluations, first_authoritative_evaluations);
    assert_eq!(replay_exact_in_probes, first_exact_in_probes);
    assert_eq!(replay_raw_scalar_calls, first_raw_scalar_calls);
    assert_eq!(replay_raw_scalar_residuals, first_raw_scalar_residuals);
    assert_stage4c_rawless_success_trace(
        (-159_850_010_344_773, -205_993_557_286_745),
        (-159_848_952_677_555, -205_984_576_466_221),
        [1_625_896, 5_412_265, -46_693_392, 2_171_555_593, 0, 0],
        (HlpPlanTopologyKind::DeleverageExactOut, HlpPlanTopologyKind::LeverageUp),
    );
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
    assert_eq!(base_receipt.ylp_burn_amount, 56_417_277_415);
    assert_eq!(target_leg, 79_924_476_337);
    assert_eq!(borrowed_leg, 159_848_952_675);
    assert_eq!(interest, 167_460_807_564);
    assert!(interest > borrowed_leg);

    let shortfall = interest - borrowed_leg;
    let target_debit =
        settled_close_target_amount(&fixture, MarketAsset::Base, target_leg, borrowed_leg, interest).unwrap();
    let exact_target_input = target_leg - target_debit;
    assert_eq!(shortfall, 7_611_854_889);
    assert_eq!(exact_target_input, 3_805_971_933);
    assert_eq!(target_debit, 76_118_504_404);

    let mut post_burn_reserves = fixture.curve_reserves_nad().unwrap();
    post_burn_reserves.base -= normalize_to_nad(target_leg as u128, fixture.base_side.asset_decimals).unwrap();
    post_burn_reserves.quote -= normalize_to_nad(borrowed_leg as u128, fixture.quote_side.asset_decimals).unwrap();
    let post_base_raw = denormalize_from_nad_floor(post_burn_reserves.base, fixture.base_side.asset_decimals).unwrap();
    let post_quote_raw =
        denormalize_from_nad_floor(post_burn_reserves.quote, fixture.quote_side.asset_decimals).unwrap();
    assert_eq!(post_base_raw, 1_620_075_523_663);
    assert_eq!(post_quote_raw, 3_240_151_047_325);
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
    assert_eq!(insufficient_out, 7_611_854_887);
    assert_eq!(sufficient_out, 7_611_854_889);
    assert!(insufficient_out < shortfall && sufficient_out >= shortfall);

    let post_base_raw = u128::from(post_base_raw);
    let post_quote_raw = u128::from(post_quote_raw);
    let cpmm_denominator = post_quote_raw - u128::from(shortfall);
    let cpmm_input = (post_base_raw * u128::from(shortfall) + cpmm_denominator - 1) / cpmm_denominator;
    assert_eq!(cpmm_input, 3_814_889_492);
    assert_ne!(u128::from(exact_target_input), cpmm_input);
    assert_eq!(prepared.quote.amount_out, 679_953_441_070);

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
    assert_eq!((base_tracking_delta, quote_tracking_delta), (1_625_896, 5_412_265));
    assert!(base_tracking_delta.unsigned_abs() <= prepared.base_pre_rebalance.tracking_loss_budget_nad);
    assert!(quote_tracking_delta.unsigned_abs() <= prepared.quote_pre_rebalance.tracking_loss_budget_nad);
    let final_reserves = first.curve_reserves_nad().unwrap();
    assert_eq!(final_reserves.base, 1_921_372_047_756_000);
    assert_eq!(final_reserves.quote, 2_456_704_243_898_000);
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

/// Deleverage debits ride the live-reserve ray while lever-up legs ride the
/// curve-reserve ray, so a deleverage moves the executable curve reserves off
/// the proportional ray exactly when per-side unrealized interest is
/// asymmetric (U_b/C_b != U_q/C_q). The control arm shows the same deleverage
/// is exactly proportional (and price-preserving) without unrealized
/// interest. This pins the counterexample in
/// design/hlp-analytic-solver-phase1.md §3.2: joint hLP prepositions do not
/// factor through one aggregate depth coordinate. The marginal price is a
/// weak detector here because deep concentration flattens price response
/// near center, so the assertion is on the ray departure itself.
#[test]
fn deleverage_with_asymmetric_unrealized_interest_leaves_the_proportional_ray() {
    struct DeleverOutcome {
        ray_gap: u128,
        atom_tolerance: u128,
        price_moved: bool,
    }

    fn delever_base_vault(market: &mut Market) -> DeleverOutcome {
        // Grow the base vault's quote funding debt so its exposure turns
        // negative and the rebalance takes the deleverage path.
        market.debt.quote_borrow_index_nad = (NAD as u128) * 105 / 100;
        let price_before = market.curve_marginal_price_nad(0).unwrap();
        let curve_before = (
            market.curve_reserve(MarketAsset::Base).unwrap() as u128,
            market.curve_reserve(MarketAsset::Quote).unwrap() as u128,
        );
        let (base_receipt, _) = rebalance_hlp_vaults(market).unwrap();
        assert!(
            base_receipt.ideal_delta < 0,
            "fixture must take the deleverage path: {:?}",
            base_receipt
        );
        assert!(base_receipt.ylp_burn_amount > 0);
        assert_market_hlp_invariants(market);
        let curve_after = (
            market.curve_reserve(MarketAsset::Base).unwrap() as u128,
            market.curve_reserve(MarketAsset::Quote).unwrap() as u128,
        );
        // The post-deleverage reserves are on the pre-deleverage proportional
        // ray iff the cross-products match. One raw atom of leg rounding moves
        // the cross-product by at most one opposing reserve.
        let ray_gap = (curve_after.0 * curve_before.1).abs_diff(curve_after.1 * curve_before.0);
        DeleverOutcome {
            ray_gap,
            atom_tolerance: curve_before.0 + curve_before.1,
            price_moved: market.curve_marginal_price_nad(0).unwrap() != price_before,
        }
    }

    let control_outcome = delever_base_vault(&mut active_concentrated_hlp_market());
    assert!(
        control_outcome.ray_gap <= control_outcome.atom_tolerance,
        "control deleverage without unrealized interest left the proportional ray: gap {}",
        control_outcome.ray_gap
    );
    assert!(!control_outcome.price_moved);

    let mut market = active_concentrated_hlp_market();
    // Coherent accrued-unpaid fixed base interest: a borrower holds 250k of
    // principal (cash down by the borrowed amount, indexed debt up by the
    // same), and a further 300k of interest has accrued on the claim (live up,
    // matching the indexed-debt growth in the virtual reserve identity). The
    // executable curve reserve `live - unrealized` is unchanged by
    // construction, so both vault valuations and the marginal price are
    // untouched until the deleverage burns against the live basis.
    let principal = 250_000_u64;
    let interest = 300_000_u64;
    assert_eq!(market.debt.base_borrow_index_nad, NAD as u128);
    let base_curve_before = market.curve_reserve(MarketAsset::Base).unwrap();
    market.debt.fixed_base_shares = (principal + interest) as u128;
    market.debt.fixed_base_principal = principal;
    market.base_side.reserves.cash_reserve -= principal;
    market.base_side.reserves.live_reserve += interest;
    assert_eq!(
        market.unrealized_interest(MarketAsset::Base).unwrap(),
        interest as u128
    );
    assert_eq!(market.unrealized_interest(MarketAsset::Quote).unwrap(), 0);
    assert_eq!(market.curve_reserve(MarketAsset::Base).unwrap(), base_curve_before);
    assert_market_hlp_invariants(&market);

    let outcome = delever_base_vault(&mut market);
    assert!(
        outcome.ray_gap > 100 * outcome.atom_tolerance,
        "asymmetric unrealized interest must push the curve reserves off the \
         proportional ray through the live-basis deleverage debit: gap {} \
         (atom tolerance {})",
        outcome.ray_gap,
        outcome.atom_tolerance
    );
    assert!(outcome.price_moved);
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
    CONCENTRATED_PRE_SOLVE_COMPACT_EVALUATIONS.with(|count| count.set(0));
    CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(|count| count.set(0));
    let first_swap = apply_test_composite_swap(&mut market, MarketAsset::Base, 350_000);
    let first_counts = (
        CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(Cell::get),
        CONCENTRATED_PRE_SOLVE_COMPACT_EVALUATIONS.with(Cell::get),
        CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(Cell::get),
    );
    CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(|count| count.set(0));
    CONCENTRATED_PRE_SOLVE_COMPACT_EVALUATIONS.with(|count| count.set(0));
    CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(|count| count.set(0));
    let _second_swap = apply_test_composite_swap(&mut market, MarketAsset::Quote, first_swap.amount_out);
    let second_counts = (
        CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(Cell::get),
        CONCENTRATED_PRE_SOLVE_COMPACT_EVALUATIONS.with(Cell::get),
        CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(Cell::get),
    );
    assert_eq!(first_counts, (5, 4, 1));
    assert_eq!(second_counts, (7, 5, 2));
    let trace = HLP_STAGE4B2A_LAST_TRACE
        .with(|trace| *trace.borrow())
        .expect("reflected Quote-axis trace");
    assert_eq!(
        trace.rejected_quote_axis_coordinate,
        Some((28_832_969_290_939, -44_108_081_728_358))
    );
    assert_eq!(
        trace.rejected_quote_axis_rows,
        Some([
            -14_836_569_950,
            -281_792_762_527,
            -13_207_274_600,
            -279_763_734_438,
            0,
            0,
        ])
    );
    assert_eq!(
        trace.rejected_quote_axis_topology,
        Some(HlpStructuralTopologyTrace {
            pre_base: HlpPackedPlanTopology(0x004),
            pre_quote: HlpPackedPlanTopology(0x0a5),
            post_base: HlpPackedPlanTopology(0x004),
            post_quote: HlpPackedPlanTopology(0x0e5),
        })
    );
    assert_eq!(
        trace.reflected_quote_axis_coordinate,
        Some((28_832_969_290_939, -45_417_576_121_978))
    );
    assert_eq!(
        trace.reflected_quote_axis_rows,
        Some([-17_337_244_612, 199_207_237_473, -15_197_829_332, 201_473_727_060, 0, 0,])
    );
    assert_eq!(
        trace.reflected_quote_axis_topology,
        Some(HlpStructuralTopologyTrace {
            pre_base: HlpPackedPlanTopology(0x004),
            pre_quote: HlpPackedPlanTopology(0x0a5),
            post_base: HlpPackedPlanTopology(0x004),
            post_quote: HlpPackedPlanTopology(0x0a5),
        })
    );
    assert_eq!(trace.a2_coordinate, Some((28_924_261_894_461, -44_874_579_976_086)));
    assert_eq!(
        trace.a2_rows,
        Some([-836_570_322, -790_063_059, 1_275_118_882, 3_213_405_971, 0, 0,])
    );
    assert_eq!(trace.a1_topology, trace.a2_topology);
    assert_eq!(trace.a2_topology, trace.reflected_quote_axis_topology);

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

/// Test-only one-authority characterization. P0 and a fresh projected center
/// establish the full bounded quote, then frozen-center axes solve directly
/// to the opposite half-budget interior. Only the terminal point is granted
/// authoritative quote/checkpoint capability.
fn assert_stage4c_one_authority_interior_target(
    mut snapshot: Market,
    asset_in: MarketAsset,
    reserve_credit: u64,
    label: &str,
) {
    let current_slot = curve_slot(&snapshot);
    snapshot.prepare_amm_for_swap(current_slot).unwrap();
    let pre_state = snapshot.dynamic_fee_pre_state(current_slot).unwrap();
    let preliminary = snapshot
        .preliminary_swap_inputs_for_state(reserve_credit, current_slot, pre_state)
        .unwrap();
    let mut context_slot = None;
    capture_concentrated_hlp_solve_context_into(
        &snapshot,
        asset_in,
        reserve_credit,
        current_slot,
        pre_state,
        preliminary,
        SwapCashPolicy::Spot,
        &mut context_slot,
    )
    .unwrap();
    let context = context_slot.unwrap();
    let mut compact_slot = None;
    HlpCompactSolveContext::capture_into(&snapshot, &context, &mut compact_slot).unwrap();
    let compact = compact_slot.unwrap();
    let cash_floors = context.cash_policy.floors(&snapshot, asset_in, 0).unwrap();
    let active_axis_count = u32::from(context.base_start.active) + u32::from(context.quote_start.active);

    reset_stage4b2a_test_state();
    let snapshot_before = snapshot.try_to_vec().unwrap();

    let mut p0_projection_slot = None;
    let mut p0_candidate_slot = None;
    evaluate_compact_concentrated_hlp_candidate(
        &compact,
        &context,
        cash_floors,
        0,
        0,
        false,
        &mut p0_projection_slot,
        &mut p0_candidate_slot,
    )
    .unwrap();
    let p0 = p0_candidate_slot.unwrap();
    let center_coordinate = (p0.next_base_delta_nad, p0.next_quote_delta_nad);
    assert_ne!(center_coordinate, (0, 0), "{label}: P0 produced no center");

    let mut center_projection_slot = None;
    let mut center_candidate_slot = None;
    evaluate_compact_concentrated_hlp_candidate(
        &compact,
        &context,
        cash_floors,
        center_coordinate.0,
        center_coordinate.1,
        true,
        &mut center_projection_slot,
        &mut center_candidate_slot,
    )
    .unwrap();
    let center = center_candidate_slot.unwrap();
    assert!(center.settlement_cash_available, "{label}: center cash");
    assert!(hlp_exact_derivative_sample_is_unbound(&center, &context));
    let center_topology = center.structural_topology;
    let center_signature = hlp_preposition_pair_signature(&center);
    let center_mode = center.guidance_exact_in_mode;
    let center_trace = center.guidance_settlement_trace;
    let center_rows = HlpExactSampleRows::from_candidate(&center);
    let base_row = hlp_exact_control_row(&center, MarketAsset::Base, context.base_start).unwrap();
    let quote_row = hlp_exact_control_row(&center, MarketAsset::Quote, context.quote_start).unwrap();
    let center_base_error = center_rows.value(MarketAsset::Base, base_row);
    let center_quote_error = center_rows.value(MarketAsset::Quote, quote_row);
    let ConcentratedHlpProjection::FrozenCenter(frozen_center) = center_projection_slot.unwrap() else {
        panic!("{label}: center did not freeze its bounded quote")
    };

    let base_probe_delta = if context.base_start.active {
        hlp_exact_axis_probe_delta(center_coordinate.0, context.base_start, center_base_error).unwrap()
    } else {
        0
    };
    let quote_probe_delta = if context.quote_start.active {
        hlp_exact_axis_probe_delta(center_coordinate.1, context.quote_start, center_quote_error).unwrap()
    } else {
        0
    };
    let mut base_axis_slot = None;
    if context.base_start.active {
        evaluate_frozen_center_axis_concentrated_hlp_candidate(
            &compact,
            &context,
            cash_floors,
            center_coordinate.0.checked_add(base_probe_delta).unwrap(),
            center_coordinate.1,
            &frozen_center,
            &mut base_axis_slot,
        )
        .unwrap();
    } else {
        base_axis_slot = Some(center);
    }
    let mut quote_axis_slot = None;
    if context.quote_start.active {
        evaluate_frozen_center_axis_concentrated_hlp_candidate(
            &compact,
            &context,
            cash_floors,
            center_coordinate.0,
            center_coordinate.1.checked_add(quote_probe_delta).unwrap(),
            &frozen_center,
            &mut quote_axis_slot,
        )
        .unwrap();
    } else {
        quote_axis_slot = Some(center);
    }
    let base_axis = *base_axis_slot.as_ref().unwrap();
    let quote_axis = *quote_axis_slot.as_ref().unwrap();
    for (asset, axis) in [(MarketAsset::Base, &base_axis), (MarketAsset::Quote, &quote_axis)] {
        let active = match asset {
            MarketAsset::Base => context.base_start.active,
            MarketAsset::Quote => context.quote_start.active,
        };
        if !active {
            continue;
        }
        let axis_signature = hlp_preposition_pair_signature(axis);
        assert!(axis.settlement_cash_available, "{label}: {asset:?} axis cash");
        assert!(hlp_exact_derivative_sample_is_unbound(axis, &context));
        assert_eq!(
            axis.structural_topology, center_topology,
            "{label}: {asset:?} axis topology"
        );
        assert_eq!(axis.guidance_exact_in_mode, center_mode, "{label}: {asset:?} axis mode");
        assert_eq!(
            axis.guidance_settlement_trace, center_trace,
            "{label}: {asset:?} axis trace"
        );
        assert!(hlp_preposition_signature_class_matches(
            center_signature.base,
            axis_signature.base,
        ));
        assert!(hlp_preposition_signature_class_matches(
            center_signature.quote,
            axis_signature.quote,
        ));
    }

    let basis = HlpFiniteDifferenceBasis {
        origin: center_rows,
        guidance_exact_in_mode: center_mode,
        guidance_settlement_trace: center_trace,
        base_probe_delta_nad: base_probe_delta,
        base_probe: HlpExactSampleRows::from_candidate(&base_axis),
        quote_probe_delta_nad: quote_probe_delta,
        quote_probe: HlpExactSampleRows::from_candidate(&quote_axis),
        base_signature: center_signature.base,
        quote_signature: center_signature.quote,
        base_probe_recorded: context.base_start.active,
        quote_probe_recorded: context.quote_start.active,
    };
    let base_target = if context.base_start.active {
        stage4c_opposite_half_budget_target(center_base_error, context.base_start.tracking.loss_budget_nad).unwrap()
    } else {
        0
    };
    let quote_target = if context.quote_start.active {
        stage4c_opposite_half_budget_target(center_quote_error, context.quote_start.tracking.loss_budget_nad).unwrap()
    } else {
        0
    };
    let (base_step, quote_step) = basis
        .solve_step(
            &context,
            center_coordinate.0,
            center_coordinate.1,
            base_row,
            quote_row,
            center_base_error.checked_sub(base_target).unwrap(),
            center_quote_error.checked_sub(quote_target).unwrap(),
        )
        .unwrap_or_else(|| panic!("{label}: interior-target basis was singular or untrusted"));
    let terminal_coordinate = (
        center_coordinate.0.checked_add(base_step).unwrap(),
        center_coordinate.1.checked_add(quote_step).unwrap(),
    );
    let base_trusted =
        hlp_coordinate_within_center_trust(center_coordinate.0, terminal_coordinate.0, context.base_start);
    let quote_trusted =
        hlp_coordinate_within_center_trust(center_coordinate.1, terminal_coordinate.1, context.quote_start);

    let _ = frozen_center;

    let mut terminal_market = snapshot.clone();
    let mut lifecycle_scratch = Market::default();
    let mut terminal_projection_slot = None;
    let mut terminal_candidate_slot = None;
    let mut terminal_scratch = HlpTerminalSwapScratch::default();
    evaluate_concentrated_hlp_candidate(
        &mut terminal_market,
        &mut lifecycle_scratch,
        &snapshot,
        &context,
        cash_floors,
        terminal_coordinate.0,
        terminal_coordinate.1,
        HlpCandidateEvaluationMode::Authoritative,
        &mut terminal_projection_slot,
        &mut terminal_candidate_slot,
        &mut terminal_scratch,
    )
    .unwrap();
    let terminal = terminal_candidate_slot.as_ref().unwrap();
    let terminal_rows = hlp_stage4b2a_test_rows(terminal).unwrap();
    let terminal_signature = hlp_preposition_pair_signature(terminal);
    let topology_matches = terminal.structural_topology == center_topology;
    let signature_matches = hlp_preposition_signature_class_matches(center_signature.base, terminal_signature.base)
        && hlp_preposition_signature_class_matches(center_signature.quote, terminal_signature.quote);
    let inactive_rows_zero = (context.base_start.active
        || [terminal_rows[0], terminal_rows[2], terminal_rows[4]] == [0; 3])
        && (context.quote_start.active || [terminal_rows[1], terminal_rows[3], terminal_rows[5]] == [0; 3]);
    let cash = terminal.settlement_cash_available;
    let unbound = hlp_exact_derivative_sample_is_unbound(terminal, &context);
    let safe = stage4c_candidate_is_fully_safe(terminal, &context);
    eprintln!(
        "ONE-AUTHORITY {label} center={center_coordinate:?}/rows={:?}/topology={center_topology:?}/signature={center_signature:?} base_axis=({}, {})/rows={:?} quote_axis=({}, {})/rows={:?} targets=({base_target},{quote_target}) predicted={terminal_coordinate:?} exact_rows={terminal_rows:?}/topology={:?}/signature={terminal_signature:?}/cash={cash}/unbound={unbound}/trust=({base_trusted},{quote_trusted})/identity=(topology={topology_matches},signature={signature_matches},inactive={inactive_rows_zero})/safe={safe} counts=(compact={},authority={})",
        hlp_stage4b2a_test_rows(&center).unwrap(),
        center_coordinate.0.checked_add(base_probe_delta).unwrap(),
        center_coordinate.1,
        hlp_stage4b2a_test_rows(&base_axis).unwrap(),
        center_coordinate.0,
        center_coordinate.1.checked_add(quote_probe_delta).unwrap(),
        hlp_stage4b2a_test_rows(&quote_axis).unwrap(),
        terminal.structural_topology,
        CONCENTRATED_PRE_SOLVE_COMPACT_EVALUATIONS.with(Cell::get),
        CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(Cell::get),
    );
    assert!(terminal_projection_slot.as_ref().unwrap().authoritative());
    assert!(terminal.authoritative.is_some());
    assert!(cash && unbound && base_trusted && quote_trusted);
    assert!(topology_matches && signature_matches && inactive_rows_zero);
    assert!(!safe, "{label}: one-authority interior target unexpectedly became safe");
    assert_eq!(
        CONCENTRATED_PRE_SOLVE_COMPACT_EVALUATIONS.with(Cell::get),
        2 + active_axis_count
    );
    assert_eq!(CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(Cell::get), 1);
    assert_eq!(snapshot.try_to_vec().unwrap(), snapshot_before);
}

#[test]
fn stage4c_one_authority_interior_target_spot_base_remains_red() {
    assert_stage4c_one_authority_interior_target(
        stage4b2a_spot_market(false),
        MarketAsset::Base,
        350_000 * 1_000_000,
        "spot-base",
    );
}
