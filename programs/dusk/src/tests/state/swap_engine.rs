use super::*;
use crate::{
    constants::{BPS_DENOMINATOR, INTEREST_INITIAL_RATE_AT_TARGET_NAD, MARKET_LAYOUT_VERSION, MIN_HALF_LIFE_MS},
    state::{AmmConfig, AmmCurveParameters, AmmState, Debt, MarketConfig, MarketSide, ReserveShares, Reserves},
};
use proptest::prelude::*;

fn implicit_divergence_surcharge_amount(
    center_input_reserve_nad: u128,
    start_input_reserve_nad: u128,
    available: u64,
    input_decimals: u8,
    coefficient_nad: u64,
) -> Result<u64> {
    require!(available > 0, ErrorCode::AmountZero);
    require!(input_decimals <= NAD_DECIMALS, ErrorCode::UnsupportedAssetDecimals);
    const TEST_DIVERGENCE_CAP_BPS: u16 = 5_000;
    if coefficient_nad == 0 {
        return Ok(0);
    }
    let decimal_scale = 10_u128
        .checked_pow((NAD_DECIMALS - input_decimals) as u32)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    implicit_divergence_surcharge_amount_core(
        PreparedSwapDivergencePotential::new(
            ceil_div(center_input_reserve_nad, decimal_scale).ok_or(ErrorCode::MarketMathOverflow)?,
            start_input_reserve_nad / decimal_scale,
            coefficient_nad,
            fee_share_cap_to_marginal_rate_nad(TEST_DIVERGENCE_CAP_BPS)?,
            gross_fee_budget_floor(available, TEST_DIVERGENCE_CAP_BPS)?,
        )?,
        available,
    )
}

fn market_with_config(amm: AmmConfig) -> Market {
    let reserve = 1_000_000 * NAD;
    let mut market = Market {
        version: MARKET_LAYOUT_VERSION,
        base_side: MarketSide {
            asset_decimals: 9,
            reserves: Reserves {
                live_reserve: reserve,
                cash_reserve: reserve,
                ..Reserves::default()
            },
            shares: ReserveShares {
                ylp_supply: reserve,
                ..ReserveShares::default()
            },
            ..MarketSide::default()
        },
        quote_side: MarketSide {
            asset_decimals: 9,
            reserves: Reserves {
                live_reserve: reserve,
                cash_reserve: reserve,
                ..Reserves::default()
            },
            shares: ReserveShares {
                ylp_supply: reserve,
                ..ReserveShares::default()
            },
            ..MarketSide::default()
        },
        config: MarketConfig {
            swap_fee_bps: 30,
            divergence_fee_share_cap_bps: 2_000,
            volatility_fee_share_cap_bps: 2_000,
            amm,
            ..MarketConfig::default()
        },
        debt: Debt {
            base_borrow_index_nad: NAD as u128,
            quote_borrow_index_nad: NAD as u128,
            base_rate_at_target_nad: INTEREST_INITIAL_RATE_AT_TARGET_NAD,
            quote_rate_at_target_nad: INTEREST_INITIAL_RATE_AT_TARGET_NAD,
            ..Debt::default()
        },
        ..Market::default()
    };
    let q = market.evaluate_current_curve(10).unwrap().balanced_equivalent_q;
    let q_per_share = market.curve_q_per_share_nad(q).unwrap();
    market.amm = AmmState::initialize(&market.config.amm, NAD, q_per_share, 10).unwrap();
    market
}

fn market_with_config_and_decimals(amm: AmmConfig, base_decimals: u8, quote_decimals: u8) -> Market {
    let mut market = market_with_config(amm);
    let base_reserve = 1_000_000_u64 * 10_u64.pow(base_decimals as u32);
    let quote_reserve = 1_000_000_u64 * 10_u64.pow(quote_decimals as u32);
    market.base_side.asset_decimals = base_decimals;
    market.base_side.reserves.live_reserve = base_reserve;
    market.base_side.reserves.cash_reserve = base_reserve;
    market.base_side.shares.ylp_supply = base_reserve;
    market.quote_side.asset_decimals = quote_decimals;
    market.quote_side.reserves.live_reserve = quote_reserve;
    market.quote_side.reserves.cash_reserve = quote_reserve;
    market.quote_side.shares.ylp_supply = quote_reserve;

    // The normalized reserves remain exactly 1,000,000 tokens per side, so
    // the initialized curve checkpoint remains bound to the same state.
    assert_eq!(market.curve_reserves_nad().unwrap().base, 1_000_000_u128 * NAD as u128);
    assert_eq!(market.curve_reserves_nad().unwrap().quote, 1_000_000_u128 * NAD as u128);
    market
}

fn market_with_raw_reserves_and_decimals(
    amm: AmmConfig,
    base_reserve: u64,
    quote_reserve: u64,
    base_decimals: u8,
    quote_decimals: u8,
) -> Market {
    let mut market = market_with_config(amm);
    market.amm = AmmState::default();
    market.config.swap_fee_bps = 0;
    market.base_side.asset_decimals = base_decimals;
    market.base_side.reserves.live_reserve = base_reserve;
    market.base_side.reserves.cash_reserve = base_reserve;
    market.base_side.shares.ylp_supply = base_reserve;
    market.quote_side.asset_decimals = quote_decimals;
    market.quote_side.reserves.live_reserve = quote_reserve;
    market.quote_side.reserves.cash_reserve = quote_reserve;
    market.quote_side.shares.ylp_supply = base_reserve;

    let q = market.evaluate_current_curve(10).unwrap().balanced_equivalent_q;
    let q_per_share = market.curve_q_per_share_nad(q).unwrap();
    market.amm = AmmState::initialize(&market.config.amm, NAD, q_per_share, 10).unwrap();
    market
}

fn concentrated_fee_config() -> AmmConfig {
    AmmConfig {
        peak_depth_nad: 200 * NAD,
        fade_scale_nad: NAD / 10,
        center_ema_half_life_ms: MIN_HALF_LIFE_MS,
        volatility_half_life_ms: MIN_HALF_LIFE_MS,
        adjustment_threshold_nad: NAD / 100,
        adjustment_step_nad: NAD / 1_000,
        min_adjustment_interval_slots: 1,
        volatility_shock_cap_nad: NAD / 10,
        volatility_cap_nad: NAD,
        divergence_fee_coefficient_nad: 10 * NAD,
        volatility_fee_coefficient_nad: NAD / 10,
        ..AmmConfig::default()
    }
}

fn cpmm_fee_config() -> AmmConfig {
    AmmConfig {
        divergence_fee_coefficient_nad: 10 * NAD,
        volatility_fee_coefficient_nad: 0,
        ..AmmConfig::default()
    }
}

fn raw_cpmm_market(
    base_reserve: u64,
    base_decimals: u8,
    quote_reserve: u64,
    quote_decimals: u8,
    swap_fee_bps: u16,
) -> Market {
    Market {
        version: MARKET_LAYOUT_VERSION,
        base_side: MarketSide {
            asset_decimals: base_decimals,
            reserves: Reserves {
                live_reserve: base_reserve,
                cash_reserve: base_reserve,
                ..Reserves::default()
            },
            ..MarketSide::default()
        },
        quote_side: MarketSide {
            asset_decimals: quote_decimals,
            reserves: Reserves {
                live_reserve: quote_reserve,
                cash_reserve: quote_reserve,
                ..Reserves::default()
            },
            ..MarketSide::default()
        },
        config: MarketConfig {
            swap_fee_bps,
            amm: AmmConfig {
                peak_depth_nad: 0,
                fade_scale_nad: 0,
                ..AmmConfig::default()
            },
            ..MarketConfig::default()
        },
        debt: Debt {
            base_borrow_index_nad: NAD as u128,
            quote_borrow_index_nad: NAD as u128,
            base_rate_at_target_nad: INTEREST_INITIAL_RATE_AT_TARGET_NAD,
            quote_rate_at_target_nad: INTEREST_INITIAL_RATE_AT_TARGET_NAD,
            ..Debt::default()
        },
        ..Market::default()
    }
}

fn concentrated_market_at_center(center_price_nad: u64, base_reserve: u64, quote_reserve: u64) -> Market {
    let mut market = Market {
        version: MARKET_LAYOUT_VERSION,
        base_side: MarketSide {
            asset_decimals: NAD_DECIMALS,
            reserves: Reserves {
                live_reserve: base_reserve,
                cash_reserve: base_reserve,
                ..Reserves::default()
            },
            shares: ReserveShares {
                ylp_supply: NAD,
                ..ReserveShares::default()
            },
            ..MarketSide::default()
        },
        quote_side: MarketSide {
            asset_decimals: NAD_DECIMALS,
            reserves: Reserves {
                live_reserve: quote_reserve,
                cash_reserve: quote_reserve,
                ..Reserves::default()
            },
            shares: ReserveShares {
                ylp_supply: NAD,
                ..ReserveShares::default()
            },
            ..MarketSide::default()
        },
        config: MarketConfig {
            swap_fee_bps: 0,
            divergence_fee_share_cap_bps: 2_000,
            volatility_fee_share_cap_bps: 2_000,
            amm: concentrated_fee_config(),
            ..MarketConfig::default()
        },
        debt: Debt {
            base_borrow_index_nad: NAD as u128,
            quote_borrow_index_nad: NAD as u128,
            base_rate_at_target_nad: INTEREST_INITIAL_RATE_AT_TARGET_NAD,
            quote_rate_at_target_nad: INTEREST_INITIAL_RATE_AT_TARGET_NAD,
            ..Debt::default()
        },
        ..Market::default()
    };
    assert_eq!(market.current_curve_center_price_nad().unwrap(), center_price_nad);
    let q = market.evaluate_current_curve(10).unwrap().balanced_equivalent_q;
    let q_per_share = market.curve_q_per_share_nad(q).unwrap();
    market.amm = AmmState::initialize(&market.config.amm, center_price_nad, q_per_share, 10).unwrap();
    market
}

fn apply_swap_quote(market: &mut Market, quote: &AmmSwapQuote, slot: u64) {
    let input_side = market.side_mut(quote.asset_in);
    input_side.reserves.live_reserve += quote.fee.reserve_input_credit;
    input_side.reserves.cash_reserve += quote.fee.reserve_input_credit;
    let output_side = market.side_mut(quote.asset_in.opposite());
    output_side.reserves.live_reserve -= quote.amount_out;
    output_side.reserves.cash_reserve -= quote.amount_out;
    market.amm.volatility_accumulator_nad = quote.post_success_volatility_nad;
    market.amm.last_observation_slot = slot;
}

fn sequential_divergence_fee(mut market: Market, asset_in: MarketAsset, total_input: u64, parts: u64) -> u64 {
    assert_eq!(total_input % parts, 0);
    let part = total_input / parts;
    let mut total_fee = 0_u64;
    for _ in 0..parts {
        let quote = market.quote_amm_swap(asset_in, part, 10).unwrap();
        total_fee += quote.fee.divergence_surcharge_debit;
        apply_swap_quote(&mut market, &quote, 10);
    }
    total_fee
}

const SPLIT_DISCOUNT_BOUND_PPM: u128 = 1;

fn uneven_split_parts(total_input: u64) -> [u64; 5] {
    const WEIGHTS: [u128; 4] = [1, 7, 19, 31];
    let mut parts = [0_u64; 5];
    let mut allocated = 0_u64;
    for (index, weight) in WEIGHTS.into_iter().enumerate() {
        let part = u64::try_from((total_input as u128) * weight / 100).unwrap();
        assert!(part > 0);
        parts[index] = part;
        allocated = allocated.checked_add(part).unwrap();
    }
    parts[4] = total_input.checked_sub(allocated).unwrap();
    assert!(parts[4] > 0);
    assert_eq!(parts.iter().copied().sum::<u64>(), total_input);
    parts
}

fn quote_and_apply_uneven_path(mut market: Market, asset_in: MarketAsset, parts: &[u64]) -> (Market, u64) {
    let mut total_divergence_fee = 0_u64;
    for amount_in in parts {
        let quote = market.quote_amm_swap(asset_in, *amount_in, 10).unwrap();
        assert!(quote.amount_out > 0);
        assert!(quote.end_price_nad > 0);
        assert!(quote.reserve_end_price_nad > 0);
        assert!(quote.trade_endpoint().is_ok());
        assert!(quote.reserve_endpoint().is_ok());
        total_divergence_fee = total_divergence_fee
            .checked_add(quote.fee.divergence_surcharge_debit)
            .unwrap();
        apply_swap_quote(&mut market, &quote, 10);
    }

    let endpoint = market.evaluate_current_curve(10).unwrap();
    assert!(endpoint.invariant_d > 0);
    assert!(endpoint.marginal_price_nad > 0);
    (market, total_divergence_fee)
}

fn assert_uneven_split_has_no_material_discount(market: Market, asset_in: MarketAsset, total_input: u64) {
    let parts = uneven_split_parts(total_input);
    let (_, one_shot_fee) = quote_and_apply_uneven_path(market.clone(), asset_in, &[total_input]);
    let (split_endpoint, split_fee) = quote_and_apply_uneven_path(market, asset_in, &parts);
    assert!(one_shot_fee > 0);
    assert!(split_fee > 0);
    let discount = one_shot_fee.saturating_sub(split_fee) as u128;

    // One ppm is 0.01 bps. The additional raw atom per split boundary covers
    // independent integer endpoint rounding without hiding a proportional
    // discount behind a large absolute-token allowance.
    let proportional_bound = ((total_input as u128) * SPLIT_DISCOUNT_BOUND_PPM).div_ceil(1_000_000);
    let raw_atom_bound = (parts.len() - 1) as u128;
    let rounding_bound = proportional_bound + raw_atom_bound;
    assert!(
        discount <= rounding_bound,
        "asset={asset_in:?}, retained={}, one-shot={one_shot_fee}, split={split_fee}, discount={discount}, bound={rounding_bound}",
        split_endpoint.amm.retain_dynamic_surcharge,
    );

    // The final split endpoint must remain executable in the restoring
    // direction, not merely deserialize or pass an accounting identity.
    let restoring_input = 10_u64.pow(split_endpoint.side(asset_in.opposite()).asset_decimals as u32);
    let restoring = split_endpoint
        .quote_amm_swap(asset_in.opposite(), restoring_input, 10)
        .unwrap();
    assert!(restoring.amount_out > 0);
    assert!(restoring.fee.divergence_surcharge_debit == 0);
}

fn move_to_concentrated_tail(mut market: Market, asset_in: MarketAsset) -> Market {
    market.config.swap_fee_bps = 0;
    market.config.amm.divergence_fee_coefficient_nad = 0;
    market.config.amm.volatility_fee_coefficient_nad = 0;
    let seed_input = 500_000_u64 * 10_u64.pow(market.side(asset_in).asset_decimals as u32);
    let seed = market.quote_amm_swap(asset_in, seed_input, 10).unwrap();
    apply_swap_quote(&mut market, &seed, 10);

    let reserves = market.curve_reserves_nad().unwrap();
    let parameters = market.current_curve_parameters(10);
    let branch = crate::math::concentrated_hybrid_branch(
        reserves.base,
        reserves.quote,
        market.current_curve_center_price_nad().unwrap() as u128,
        parameters.peak_depth_nad as u128,
        parameters.fade_scale_nad as u128,
    )
    .unwrap();
    assert_ne!(branch, crate::math::ConcentratedHybridBranch::Inner);

    market.config.amm.divergence_fee_coefficient_nad = 10 * NAD;
    market
}

fn assert_split_matrix_is_immaterial(market: Market) {
    for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
        let total_input = 100_000_u64 * 10_u64.pow(market.side(asset_in).asset_decimals as u32);
        for retain in [false, true] {
            let mut scenario = market.clone();
            scenario.config.swap_fee_bps = 0;
            scenario.config.amm.volatility_fee_coefficient_nad = 0;
            scenario.amm.retain_dynamic_surcharge = retain;
            let one_shot = scenario
                .quote_amm_swap(asset_in, total_input, 10)
                .unwrap()
                .fee
                .divergence_surcharge_debit;
            assert!(one_shot > 0);

            for parts in [2_u64, 10, 100] {
                let split_fee = sequential_divergence_fee(scenario.clone(), asset_in, total_input, parts);
                assert!(split_fee > 0);
                let discount = one_shot.saturating_sub(split_fee);

                if retain {
                    // Retained surcharge increases the next invariant. The
                    // updated balanced coordinate makes every tested split
                    // at least as expensive, so retention is not an evasion
                    // path even though it cannot telescope through a frozen D.
                    assert!(
                        split_fee >= one_shot,
                        "asset={asset_in:?}, retain={retain}, parts={parts}, one-shot={one_shot}, split={split_fee}"
                    );
                } else {
                    // A distributed-fee path telescopes to raw-token
                    // precision. Re-solving the canonical integer D between
                    // pieces can still move a divergence endpoint by atoms;
                    // keep that bounded below 0.01 bp. The former
                    // no-divergence endpoint was exploitable by 6.93 bps at
                    // only two pieces.
                    assert!(
                        (discount as u128) * 1_000_000 <= total_input as u128,
                        "asset={asset_in:?}, retain={retain}, parts={parts}, one-shot={one_shot}, split={split_fee}"
                    );
                }
            }
        }
    }
}

fn assert_implicit_endpoint_is_maximal(
    center_nad: u128,
    start_nad: u128,
    available: u64,
    decimals: u8,
    coefficient_nad: u64,
) {
    reset_divergence_endpoint_iterations();
    let surcharge =
        implicit_divergence_surcharge_amount(center_nad, start_nad, available, decimals, coefficient_nad).unwrap();
    let executable = available - surcharge;
    let maximum_surcharge = hard_total_fee_budget_floor(available);
    let charged_potential = divergence_fee_for_executable_input(
        center_nad,
        start_nad,
        executable,
        decimals,
        coefficient_nad,
        maximum_surcharge,
    )
    .unwrap();
    assert!(surcharge >= charged_potential);
    assert!(
        divergence_total_cost(
            center_nad,
            start_nad,
            executable,
            decimals,
            coefficient_nad,
            maximum_surcharge,
        )
        .unwrap()
            <= available as u128
    );
    if executable < available {
        assert!(
            divergence_total_cost(
                center_nad,
                start_nad,
                executable + 1,
                decimals,
                coefficient_nad,
                maximum_surcharge,
            )
            .unwrap()
                > available as u128,
            "center={center_nad}, start={start_nad}, available={available}, decimals={decimals}, surcharge={surcharge}"
        );
    }
    assert!(
        divergence_endpoint_iterations() <= 16,
        "implicit endpoint needed {} probes: center={center_nad}, start={start_nad}, available={available}, decimals={decimals}, coefficient={coefficient_nad}",
        divergence_endpoint_iterations(),
    );
}

fn assert_actual_restorative_and_crossing_paths(mut market: Market) {
    market.config.swap_fee_bps = 0;
    market.config.amm.volatility_fee_coefficient_nad = 0;
    let outward = market.quote_amm_swap(MarketAsset::Base, 5_000 * NAD, 10).unwrap();
    apply_swap_quote(&mut market, &outward, 10);

    let restorative = market
        .quote_amm_swap(MarketAsset::Quote, outward.amount_out / 2, 10)
        .unwrap();
    assert_eq!(restorative.fee.divergence_surcharge_debit, 0);

    let crossing = market
        .quote_amm_swap(MarketAsset::Quote, outward.amount_out * 2, 10)
        .unwrap();
    assert!(crossing.start_price_nad < NAD);
    assert!(crossing.end_price_nad > NAD);
    assert!(crossing.fee.divergence_surcharge_debit > 0);
}

#[test]
fn disabled_dynamic_fee_has_exact_legacy_fee_identity() {
    let market = market_with_config(AmmConfig::default());
    let reserve_credit = 10_000 * NAD;
    let quote = market.quote_amm_swap(MarketAsset::Base, reserve_credit, 10).unwrap();
    let base_fee = (reserve_credit as u128 * 30 / BPS_DENOMINATOR as u128) as u64;
    assert_eq!(quote.fee.base_fee_debit, base_fee);
    assert_eq!(quote.fee.dynamic_surcharge_debit, 0);
    assert_eq!(quote.fee.amount_in_for_quote, reserve_credit - base_fee);
    assert_eq!(quote.fee.reserve_input_credit, reserve_credit - base_fee);
}

#[test]
fn zero_depth_swap_engine_matches_raw_cpmm_normalization_rounding_and_fees() {
    let market = raw_cpmm_market(1_234_567_890_123, 9, 3_210_987_654, 6, 30);
    assert_eq!(market.current_curve_parameters(10), AmmCurveParameters::cpmm());

    for (asset_in, reserve_credit) in [
        (MarketAsset::Base, 17_345_678_901_u64),
        (MarketAsset::Quote, 51_234_567_u64),
    ] {
        let quote = market.quote_amm_swap(asset_in, reserve_credit, 10).unwrap();
        let base_fee = (reserve_credit as u128 * market.config.swap_fee_bps as u128 / BPS_DENOMINATOR as u128) as u64;
        let net_input = reserve_credit - base_fee;
        let reserves = market.curve_reserves_nad().unwrap();
        let input_nad = crate::math::normalize_to_nad(net_input as u128, market.side(asset_in).asset_decimals).unwrap();
        let expected_output_nad = match asset_in {
            MarketAsset::Base => crate::math::calculate_normalized_amount_out(reserves.base, reserves.quote, input_nad),
            MarketAsset::Quote => {
                crate::math::calculate_normalized_amount_out(reserves.quote, reserves.base, input_nad)
            }
        }
        .unwrap();
        let expected_output = crate::math::denormalize_from_nad_floor(
            expected_output_nad,
            market.side(asset_in.opposite()).asset_decimals,
        )
        .unwrap();

        assert_eq!(quote.amount_out, expected_output, "asset_in={asset_in:?}");
        assert_eq!(quote.fee.base_fee_debit, base_fee);
        assert_eq!(quote.fee.dynamic_surcharge_debit, 0);
        assert_eq!(quote.fee.total_fee_debit, base_fee);
        assert_eq!(quote.fee.amount_in_for_quote, net_input);
        assert_eq!(quote.fee.reserve_input_credit, net_input);
        assert_eq!(quote.fee.claimable_fee_debit, base_fee);

        let executable_output_nad = crate::math::normalize_to_nad(
            quote.amount_out as u128,
            market.side(asset_in.opposite()).asset_decimals,
        )
        .unwrap();
        assert!(executable_output_nad <= expected_output_nad);
        if asset_in == MarketAsset::Base {
            // The six-decimal quote token makes this leg exercise flooring of
            // sub-raw-token NAD dust, rather than merely equal-unit arithmetic.
            assert!(executable_output_nad < expected_output_nad);
        }
    }
}

#[test]
fn cpmm_split_matrix_cannot_materially_reduce_divergence_fee() {
    assert_split_matrix_is_immaterial(market_with_config(cpmm_fee_config()));
}

#[test]
fn concentrated_split_matrix_cannot_materially_reduce_divergence_fee() {
    assert_split_matrix_is_immaterial(market_with_config(concentrated_fee_config()));
}

#[test]
fn unequal_decimal_split_matrices_cover_both_assets_and_retention_modes() {
    assert_split_matrix_is_immaterial(market_with_config_and_decimals(cpmm_fee_config(), 6, 9));
    assert_split_matrix_is_immaterial(market_with_config_and_decimals(concentrated_fee_config(), 6, 9));
}

#[test]
fn far_tail_uneven_splits_have_no_material_discount_and_keep_endpoints_live() {
    for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
        let tail = move_to_concentrated_tail(market_with_config(concentrated_fee_config()), asset_in);
        let total_input = 100_000_u64 * 10_u64.pow(tail.side(asset_in).asset_decimals as u32);
        for retain in [false, true] {
            let mut scenario = tail.clone();
            scenario.amm.retain_dynamic_surcharge = retain;
            assert_uneven_split_has_no_material_discount(scenario, asset_in, total_input);
        }
    }
}

#[test]
fn maximum_supported_decimals_cover_both_curves_directions_and_fee_destinations() {
    for config in [cpmm_fee_config(), concentrated_fee_config()] {
        for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
            let market = market_with_config_and_decimals(config, NAD_DECIMALS, NAD_DECIMALS);
            let total_input = 100_000_u64 * 10_u64.pow(market.side(asset_in).asset_decimals as u32);
            for retain in [false, true] {
                let mut scenario = market.clone();
                scenario.config.swap_fee_bps = 0;
                scenario.config.amm.volatility_fee_coefficient_nad = 0;
                scenario.amm.retain_dynamic_surcharge = retain;
                assert_uneven_split_has_no_material_discount(scenario, asset_in, total_input);
            }
        }
    }
}

#[test]
fn implicit_divergence_endpoint_is_maximal_across_rounding_domains() {
    let center = 1_000_000_u128 * NAD as u128;
    for (decimals, available) in [
        (0_u8, 100_000_u64),
        (6, 100_000_u64 * 1_000_000),
        (9, 100_000_u64 * NAD),
    ] {
        assert_implicit_endpoint_is_maximal(center, center, available, decimals, 10 * NAD);
        assert_implicit_endpoint_is_maximal(center, center + center / 2, available, decimals, 100 * NAD);
    }

    // Large raw-domain values exercise the Newton numerator and conversion
    // boundaries without relying on a binary-search-sized iteration budget.
    assert_implicit_endpoint_is_maximal(
        u64::MAX as u128,
        (u64::MAX as u128) + (u64::MAX as u128) / 3,
        u64::MAX / 4,
        9,
        100 * NAD,
    );

    for available in 1..=3 {
        assert_implicit_endpoint_is_maximal(1, 2, available, 9, 100 * NAD);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1_024))]

    #[test]
    fn configured_runtime_domain_always_returns_the_maximal_executable_endpoint(
        center_raw in 1_u64..=u64::MAX / 2,
        outward_raw in 0_u64..=u64::MAX / 2,
        available in 1_u64..=u64::MAX,
        decimals in 0_u8..=NAD_DECIMALS,
        coefficient_nad in 0_u64..=100 * NAD,
    ) {
        let start_raw = center_raw + outward_raw;
        let center_nad = crate::math::normalize_to_nad(center_raw as u128, decimals).unwrap();
        let start_nad = crate::math::normalize_to_nad(start_raw as u128, decimals).unwrap();
        prop_assume!(center_nad > 0 && start_nad >= center_nad);

        reset_divergence_endpoint_iterations();
        let surcharge = implicit_divergence_surcharge_amount(
            center_nad,
            start_nad,
            available,
            decimals,
            coefficient_nad,
        ).unwrap();
        let executable = available - surcharge;
        let maximum_surcharge = hard_total_fee_budget_floor(available);
        prop_assert!(
            divergence_total_cost(
                center_nad,
                start_nad,
                executable,
                decimals,
                coefficient_nad,
                maximum_surcharge,
            ).unwrap()
                <= available as u128
        );
        if executable < available {
            prop_assert!(
                divergence_total_cost(
                    center_nad,
                    start_nad,
                    executable + 1,
                    decimals,
                    coefficient_nad,
                    maximum_surcharge,
                ).unwrap()
                    > available as u128
            );
        }
        prop_assert!(divergence_endpoint_iterations() <= DIVERGENCE_ENDPOINT_MAX_ITERS);
    }
}

#[test]
fn huberized_divergence_toll_never_exceeds_half_of_gross() {
    let center = NAD as u128;

    for available in [1_000_000_000_u64, 1_000_000_000_000, 1_000_000_000_000_000] {
        reset_divergence_endpoint_iterations();
        let surcharge = implicit_divergence_surcharge_amount(center, center, available, 9, NAD).unwrap();
        let executable = available - surcharge;
        assert!(executable > 0);
        let share_ppm = surcharge as u128 * 1_000_000 / available as u128;
        assert!(share_ppm <= 500_000);
        assert!(executable >= minimum_executable_input(available));
        let maximum_surcharge = hard_total_fee_budget_floor(available);
        assert!(
            divergence_total_cost(center, center, executable, 9, NAD, maximum_surcharge).unwrap() <= available as u128
        );
        assert!(
            divergence_total_cost(center, center, executable + 1, 9, NAD, maximum_surcharge).unwrap()
                > available as u128
        );
        assert!(divergence_endpoint_iterations() <= 16);
    }
}

#[test]
fn sbf_stress_shape_remains_executable_at_the_hard_divergence_cap() {
    // Mirrors the six-decimal, 100-token input-side reserve used by the SBF
    // compute harness, with the maximum configured coefficient and a very
    // large but u64-safe exact input.
    let center_nad = 100_000_000_u128 * 1_000;
    let available = 7_000_000_000_000_000_u64;
    reset_divergence_endpoint_iterations();
    let surcharge = implicit_divergence_surcharge_amount(center_nad, center_nad, available, 6, 100 * NAD).unwrap();
    let share_ppm = surcharge as u128 * 1_000_000 / available as u128;
    assert!(surcharge < available);
    assert!(share_ppm <= 500_000);
    assert!(available - surcharge >= minimum_executable_input(available));
    assert!(divergence_endpoint_iterations() <= 24);
}

#[test]
fn quote_rejects_a_zero_post_retention_mark_before_preview_or_execution_can_diverge() {
    let mut config = concentrated_fee_config();
    config.divergence_fee_coefficient_nad = 100 * NAD;
    let mut market = market_with_raw_reserves_and_decimals(config, 100_000_000, 100_000_000, 6, 6);
    let gross_input = 7_000_000_000_000_000;
    let feasible_gross_input = 2_000_000_000_000;

    market.amm.retain_dynamic_surcharge = false;
    let distributed = market
        .quote_amm_swap(MarketAsset::Base, feasible_gross_input, 10)
        .unwrap();
    assert!(distributed.end_price_nad > 0);
    assert!(distributed.reserve_end_price_nad > 0);
    let distributed_error = market.quote_amm_swap(MarketAsset::Base, gross_input, 10).unwrap_err();
    assert_eq!(distributed_error, error!(ErrorCode::InvalidSettlementPrice));

    market.amm.retain_dynamic_surcharge = true;
    let retained = market
        .quote_amm_swap(MarketAsset::Base, feasible_gross_input, 10)
        .unwrap();
    assert!(retained.end_price_nad > 0);
    assert!(retained.reserve_end_price_nad > 0);
    assert!(retained.fee.divergence_surcharge_debit as u128 * 10_000 / feasible_gross_input as u128 <= 2_000);

    let error = market.quote_amm_swap(MarketAsset::Base, gross_input, 10).unwrap_err();
    assert_eq!(error, error!(ErrorCode::InvalidSettlementPrice));
}

#[test]
fn wide_cpmm_sbf_stress_shape_remains_live_on_bounded_u128_path() {
    // A valid zero-decimal pool can place both the selected endpoint and the
    // gross probe near the protocol's widest arithmetic domain. This mirrors
    // the permanent LiteSVM wide-CPMM compute gate.
    let center_nad = 10_000_000_000_000_000_000_000_u128;
    let available = 5_000_000_000_000_000_u64;
    reset_divergence_endpoint_iterations();
    let surcharge = implicit_divergence_surcharge_amount(center_nad, center_nad, available, 0, 100 * NAD).unwrap();
    let executable = available - surcharge;
    assert!(executable > 0);
    assert!(divergence_endpoint_iterations() <= 15);
    let maximum_surcharge = hard_total_fee_budget_floor(available);
    assert!(
        divergence_total_cost(center_nad, center_nad, executable, 0, 100 * NAD, maximum_surcharge,).unwrap()
            <= available as u128
    );
    assert!(
        divergence_total_cost(center_nad, center_nad, executable + 1, 0, 100 * NAD, maximum_surcharge,).unwrap()
            > available as u128
    );
}

#[test]
fn extreme_tail_still_leaves_the_minimum_executable_input() {
    let available = u64::MAX / 4;
    let center = NAD as u128;
    let start = (u64::MAX - available) as u128;
    reset_divergence_endpoint_iterations();
    let surcharge = implicit_divergence_surcharge_amount(center, start, available, 9, 100 * NAD).unwrap();

    assert!(
        surcharge <= hard_total_fee_budget_floor(available),
        "surcharge={surcharge}, budget={}, available={available}",
        hard_total_fee_budget_floor(available)
    );
    assert!(available - surcharge >= minimum_executable_input(available));
    assert!(divergence_endpoint_iterations() <= DIVERGENCE_ENDPOINT_MAX_ITERS);
}

#[test]
fn cpmm_actual_restorative_and_crossing_paths_charge_only_outward_leg() {
    assert_actual_restorative_and_crossing_paths(market_with_config(cpmm_fee_config()));
}

#[test]
fn concentrated_actual_restorative_and_crossing_paths_charge_only_outward_leg() {
    assert_actual_restorative_and_crossing_paths(market_with_config(concentrated_fee_config()));
}

#[test]
fn adaptive_numeraire_keeps_low_and_high_center_restoring_flow_fee_free() {
    // These centered inventories exercise both adaptive numeraires at the
    // widest useful price scales without exceeding a u64 token account.
    for market in [
        concentrated_market_at_center(1_000_000, 1_000_000_000_000_000, 1_000_000_000_000),
        concentrated_market_at_center(1_000_000_000_000_000, NAD, 1_000_000_000_000_000),
    ] {
        for outward_asset in [MarketAsset::Base, MarketAsset::Quote] {
            let mut displaced = market.clone();
            let outward_input = displaced.side(outward_asset).reserves.live_reserve / 10;
            let outward = displaced.quote_amm_swap(outward_asset, outward_input, 10).unwrap();
            assert!(outward.amount_out >= 2);
            apply_swap_quote(&mut displaced, &outward, 10);

            let restoring_asset = outward_asset.opposite();
            let restoring = displaced
                .quote_amm_swap(restoring_asset, outward.amount_out / 2, 10)
                .unwrap();
            assert_eq!(
                restoring.fee.divergence_surcharge_debit,
                0,
                "center={center}, outward={outward_asset:?}",
                center = market.amm.center_price_nad,
            );

            let crossing = displaced
                .quote_amm_swap(restoring_asset, outward.amount_out.saturating_mul(2), 10)
                .unwrap();
            assert!(
                crossing.fee.divergence_surcharge_debit > 0,
                "center={center}, outward={outward_asset:?}",
                center = market.amm.center_price_nad,
            );
        }
    }
}

#[test]
fn outward_trade_pays_quadratic_divergence_surcharge() {
    let market = market_with_config(concentrated_fee_config());
    let quote = market.quote_amm_swap(MarketAsset::Base, 50_000 * NAD, 10).unwrap();
    assert!(quote.fee.divergence_surcharge_debit > 0);
    assert_eq!(quote.fee.volatility_surcharge_debit, 0);
    assert_eq!(
        quote.fee.total_fee_debit,
        quote.fee.base_fee_debit + quote.fee.dynamic_surcharge_debit
    );
}

#[test]
fn retained_surcharge_stays_in_reserve_coordinate() {
    let mut market = market_with_config(concentrated_fee_config());
    market.amm.retain_dynamic_surcharge = true;
    let quote = market.quote_amm_swap(MarketAsset::Base, 50_000 * NAD, 10).unwrap();
    assert_eq!(quote.fee.retained_surcharge, quote.fee.dynamic_surcharge_debit);
    assert_eq!(quote.fee.distributed_surcharge_debit, 0);
    assert_eq!(quote.fee.claimable_fee_debit, quote.fee.base_fee_debit);
    assert_eq!(
        quote.fee.reserve_input_credit,
        quote.fee.amount_in_for_quote + quote.fee.retained_surcharge
    );
    let certified_reserve_end =
        u64::try_from(quote.reserve_endpoint().unwrap().evaluation().marginal_price_nad).unwrap();
    assert_eq!(quote.reserve_end_price_nad, certified_reserve_end);
    assert_ne!(quote.end_price_nad, quote.reserve_end_price_nad);
}

#[test]
fn retained_and_distributed_routing_quote_the_identical_trader_charge() {
    let mut distributed_market = market_with_config(concentrated_fee_config());
    distributed_market.amm.volatility_accumulator_nad = NAD / 5;
    distributed_market.amm.last_observation_slot = 10;
    distributed_market.amm.retain_dynamic_surcharge = false;
    let mut retained_market = distributed_market.clone();
    retained_market.amm.retain_dynamic_surcharge = true;
    let gross_input = 50_000 * NAD;

    let distributed = distributed_market
        .quote_amm_swap(MarketAsset::Base, gross_input, 10)
        .unwrap();
    let retained = retained_market
        .quote_amm_swap(MarketAsset::Base, gross_input, 10)
        .unwrap();

    assert_eq!(retained.amount_out, distributed.amount_out);
    assert_eq!(retained.start_price_nad, distributed.start_price_nad);
    assert_eq!(retained.end_price_nad, distributed.end_price_nad);
    assert_eq!(retained.decayed_volatility_nad, distributed.decayed_volatility_nad);
    assert_eq!(
        retained.post_success_volatility_nad,
        distributed.post_success_volatility_nad
    );
    assert_eq!(retained.fee.amount_in_for_quote, distributed.fee.amount_in_for_quote);
    assert_eq!(retained.fee.total_fee_debit, distributed.fee.total_fee_debit);
    assert_eq!(
        retained.fee.dynamic_surcharge_debit,
        distributed.fee.dynamic_surcharge_debit
    );
    assert_eq!(
        distributed.fee.distributed_surcharge_debit,
        distributed.fee.dynamic_surcharge_debit
    );
    assert_eq!(retained.fee.retained_surcharge, retained.fee.dynamic_surcharge_debit);
    assert_eq!(
        distributed.fee.reserve_input_credit + distributed.fee.claimable_fee_debit,
        gross_input
    );
    assert_eq!(
        retained.fee.reserve_input_credit + retained.fee.claimable_fee_debit,
        gross_input
    );
}

#[test]
fn preliminary_hlp_inputs_include_retained_surcharge_in_reserve_only() {
    let mut market = market_with_config(concentrated_fee_config());
    market.amm.retain_dynamic_surcharge = true;
    market.amm.volatility_accumulator_nad = NAD / 5;
    market.amm.last_observation_slot = 10;
    let reserve_credit = 50_000 * NAD;

    let preliminary = market.preliminary_swap_inputs(reserve_credit, 10).unwrap();
    let final_quote = market.quote_amm_swap(MarketAsset::Base, reserve_credit, 10).unwrap();

    assert!(preliminary.reserve_input_credit > preliminary.amount_in_for_quote);
    assert_eq!(
        preliminary.reserve_input_credit,
        reserve_credit - final_quote.fee.base_fee_debit
    );
    assert_eq!(preliminary.reserve_input_credit, final_quote.fee.reserve_input_credit);
    assert_eq!(
        preliminary.amount_in_for_quote,
        final_quote.fee.amount_in_for_quote + final_quote.fee.divergence_surcharge_debit
    );
    assert!(preliminary.amount_in_for_quote >= final_quote.fee.amount_in_for_quote);
}

#[test]
fn preliminary_hlp_inputs_are_one_coordinate_without_retention() {
    let mut market = market_with_config(concentrated_fee_config());
    market.amm.retain_dynamic_surcharge = false;
    market.amm.volatility_accumulator_nad = NAD / 5;
    market.amm.last_observation_slot = 10;
    let reserve_credit = 50_000 * NAD;

    let preliminary = market.preliminary_swap_inputs(reserve_credit, 10).unwrap();
    let final_quote = market.quote_amm_swap(MarketAsset::Base, reserve_credit, 10).unwrap();

    assert_eq!(preliminary.reserve_input_credit, preliminary.amount_in_for_quote);
    assert_eq!(
        preliminary.amount_in_for_quote,
        final_quote.fee.amount_in_for_quote + final_quote.fee.divergence_surcharge_debit
    );
    assert!(preliminary.amount_in_for_quote >= final_quote.fee.amount_in_for_quote);
    assert!(preliminary.reserve_input_credit >= final_quote.fee.reserve_input_credit);
}

#[test]
fn decayed_chop_signal_is_charged_before_current_move() {
    let mut market = market_with_config(concentrated_fee_config());
    market.amm.volatility_accumulator_nad = NAD / 5;
    market.amm.last_observation_slot = 10;
    let quote = market.quote_amm_swap(MarketAsset::Quote, 1_000 * NAD, 10).unwrap();
    assert!(quote.fee.volatility_surcharge_debit > 0);
    assert!(quote.post_success_volatility_nad >= quote.decayed_volatility_nad);
}

#[test]
fn reserve_overlay_matches_a_frozen_sequential_quote() {
    let mut market = market_with_config(concentrated_fee_config());
    market.amm.retain_dynamic_surcharge = true;
    let first = market.quote_amm_swap(MarketAsset::Base, 25_000 * NAD, 10).unwrap();
    let overlay = market
        .quote_amm_swap_after(&first, MarketAsset::Quote, 7_500 * NAD, 10)
        .unwrap();

    apply_swap_quote(&mut market, &first, 10);
    let sequential = market.quote_amm_swap(MarketAsset::Quote, 7_500 * NAD, 10).unwrap();

    assert_eq!(overlay, sequential);
}
