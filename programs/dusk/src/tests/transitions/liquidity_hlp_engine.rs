use super::*;
use proptest::prelude::*;

#[test]
fn h_lp_entry_mints_against_current_value_even_if_cached_values_are_stale() {
    use crate::state::{AmmConfig, AmmState};

    for target in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = Market::default();
        market.add_liquidity(1_000_000, 1_000_000).unwrap();
        market.debt.base_borrow_index_nad = NAD as u128;
        market.debt.quote_borrow_index_nad = NAD as u128;
        market.deposit_single_sided(target, 20_000, 1).unwrap();
        market.config.amm = AmmConfig {
            peak_amplification_nad: 4 * NAD,
            core_half_width_bps: 100,
            fade_width_bps: 400,
            center_ema_half_life_ms: crate::constants::MIN_HALF_LIFE_MS,
            ..AmmConfig::default()
        };
        market.amm = AmmState::default();
        market.prepare_amm_for_swap(1).unwrap();

        let prices = current_hlp_curve_prices(&market).unwrap();
        let supply_before = match target {
            MarketAsset::Base => market.base_hlp_vault.hlp_supply,
            MarketAsset::Quote => market.quote_hlp_vault.hlp_supply,
        };
        let nav_before = hlp_share_nav_nad_with_prices(&market, target, prices).unwrap();
        let vault = match target {
            MarketAsset::Base => &mut market.base_hlp_vault,
            MarketAsset::Quote => &mut market.quote_hlp_vault,
        };
        vault.last_nav_nad = nav_before / 2;
        vault.cached_settlement_price_nad = prices.base_in_quote_nad() * 2;
        market.config.settlement_divergence_bps = 1;

        let receipt = market.deposit_single_sided(target, 10_000, 1).unwrap();
        let nav_after = hlp_share_nav_nad_with_prices(&market, target, prices).unwrap();
        let expected = (nav_after - nav_before) * u128::from(supply_before) / nav_before;
        assert_eq!(u128::from(receipt.hlp_amount), expected, "target={target:?}");
        assert!(
            nav_after * u128::from(supply_before) >= nav_before * u128::from(receipt.hlp_supply),
            "existing holders must not lose per-share economic NAV; target={target:?}"
        );
        market.assert_market_invariants().unwrap();
    }
}

#[test]
fn h_lp_share_value_includes_unpaid_public_interest_without_spending_it() {
    for target in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = Market::default();
        market.add_liquidity(1_000_000, 1_000_000).unwrap();
        market.debt.base_borrow_index_nad = NAD as u128;
        market.debt.quote_borrow_index_nad = NAD as u128;
        market.deposit_single_sided(target, 20_000, 1).unwrap();
        let prices = current_hlp_curve_prices(&market).unwrap();
        let economic_before = hlp_share_nav_nad_with_prices(&market, target, prices).unwrap();
        let executable_before = hlp_nav_nad_with_prices(&market, target, prices).unwrap();
        assert_eq!(economic_before, executable_before);

        // A public borrower owes 25,000 of accrued target-asset interest.
        // It belongs to the yLP share owners but is not executable AMM cash.
        match target {
            MarketAsset::Base => {
                market.debt.fixed_base_shares = 400_000;
                market.debt.fixed_base_principal = 400_000;
                market.debt.base_borrow_index_nad = (NAD as u128) * 17 / 16;
            }
            MarketAsset::Quote => {
                market.debt.fixed_quote_shares = 400_000;
                market.debt.fixed_quote_principal = 400_000;
                market.debt.quote_borrow_index_nad = (NAD as u128) * 17 / 16;
            }
        }
        market.side_mut(target).reserves.cash_reserve -= 400_000;
        market.side_mut(target).reserves.live_reserve += 25_000;
        let vault = match target {
            MarketAsset::Base => &market.base_hlp_vault,
            MarketAsset::Quote => &market.quote_hlp_vault,
        };
        let expected_interest_claim =
            25_000_u128 * u128::from(vault.ylp_shares) / u128::from(market.side(target).shares.ylp_supply);
        let expected_in_nad = market.normalize_amount(expected_interest_claim, market.side(target).asset_decimals).unwrap();
        let economic_after = hlp_share_nav_nad_with_prices(&market, target, prices).unwrap();
        assert_eq!(economic_after - economic_before, expected_in_nad);
        assert_eq!(hlp_nav_nad_with_prices(&market, target, prices).unwrap(), executable_before);
        assert_eq!(current_hlp_curve_prices(&market).unwrap(), prices);
    }
}

#[test]
fn h_lp_exit_survives_stale_reference_after_concentrated_swap() {
    use crate::state::{AmmConfig, AmmState};
    use crate::transitions::amm::SwapRequest;
    use crate::transitions::liquidity::SwapCashPolicy;
    use crate::transitions::LeverageSwapFeeCredit;

    for fee_bps in [0, 30] {
        let mut initial = Market::default();
        initial.add_liquidity(1_000_000, 1_000_000).unwrap();
        initial.debt.base_borrow_index_nad = NAD as u128;
        initial.debt.quote_borrow_index_nad = NAD as u128;
        initial.config.settlement_divergence_bps = 10_000;
        initial.deposit_single_sided(MarketAsset::Base, 20_000, 1).unwrap();
        initial.deposit_single_sided(MarketAsset::Quote, 20_000, 1).unwrap();
        initial.config.swap_fee_bps = fee_bps;
        initial.config.amm = AmmConfig {
            peak_amplification_nad: 4 * NAD,
            core_half_width_bps: 100,
            fade_width_bps: 400,
            center_ema_half_life_ms: crate::constants::MIN_HALF_LIFE_MS,
            ..AmmConfig::default()
        };
        initial.amm = AmmState::default();
        initial.prepare_amm_for_swap(1).unwrap();
        initial.config.settlement_divergence_bps = 500;

        for target in [MarketAsset::Base, MarketAsset::Quote] {
            let supply = match target {
                MarketAsset::Base => initial.base_hlp_vault.hlp_supply,
                MarketAsset::Quote => initial.quote_hlp_vault.hlp_supply,
            };
            let slice = supply / 2;
            let baseline = initial.quote_hlp_withdrawal(target, slice).unwrap();
            for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
                let mut market = initial.clone();
                let amount_in = 100_000;
                let mut swap = SwapRequest {
                    current_slot: 2,
                    current_unix_timestamp: 0,
                    asset_in,
                    reserve_credit: amount_in,
                    protocol_fee_bps: 0,
                }
                .prepare(&mut market)
                .unwrap();
                let quote = swap.leverage_quote();
                let first_out = quote.amount_out;
                let fee_credit = LeverageSwapFeeCredit::from_total_actual_credit(
                    &quote,
                    swap.quote.fee.claimable_fee_debit,
                )
                .unwrap();
                swap.apply(&mut market, SwapCashPolicy::Spot, fee_credit, 2, 0, ProtocolAuctionSplit::default(), None)
                    .unwrap();
                let reference = match target {
                    MarketAsset::Base => market.base_hlp_vault.cached_settlement_price_nad,
                    MarketAsset::Quote => market.quote_hlp_vault.cached_settlement_price_nad,
                };
                assert_ne!(current_hlp_curve_prices(&market).unwrap().base_in_quote_nad(), reference);

                let preview = market.quote_hlp_withdrawal(target, slice).unwrap();
                let exit = market.withdraw_single_sided(target, slice).unwrap();
                assert_eq!(exit.target_amount_out, preview);
                assert!(exit.target_amount_out.abs_diff(baseline) <= 4);

                // A trader who moves the price, exits their hLP, then reverses
                // the trade does not extract value in this zero/30-bps fixture.
                let mut reverse = SwapRequest {
                    current_slot: 3,
                    current_unix_timestamp: 0,
                    asset_in: asset_in.opposite(),
                    reserve_credit: first_out,
                    protocol_fee_bps: 0,
                }
                .prepare(&mut market)
                .unwrap();
                let reverse_quote = reverse.leverage_quote();
                let reverse_credit = LeverageSwapFeeCredit::from_total_actual_credit(
                    &reverse_quote,
                    reverse.quote.fee.claimable_fee_debit,
                )
                .unwrap();
                reverse
                    .apply(
                        &mut market,
                        SwapCashPolicy::Spot,
                        reverse_credit,
                        3,
                        0,
                        ProtocolAuctionSplit::default(),
                        None,
                    )
                    .unwrap();
                let gain_at_initial_price = i128::from(exit.target_amount_out) - i128::from(baseline)
                    + i128::from(reverse_quote.amount_out)
                    - i128::from(amount_in);
                assert!(gain_at_initial_price <= 0, "fee={fee_bps}, target={target:?}, input={asset_in:?}");
                market.assert_market_invariants().unwrap();
            }
        }
    }
}

#[test]
fn extreme_forward_price_values_both_hlp_numeraires_without_a_rounded_reciprocal() {
    let mut market = Market::default();
    market.base_side.asset_decimals = 0;
    market.quote_side.asset_decimals = 0;
    let price = (NAD as u128) * (NAD as u128) + NAD as u128;
    let prices = hlp_curve_prices_from_base_price_nad(price).unwrap();

    assert_eq!(
        asset_value_in_target_nad_with_prices(&market, prices, MarketAsset::Base, 1, MarketAsset::Quote).unwrap(),
        price,
    );
    assert_eq!(
        asset_value_in_target_nad_with_prices(&market, prices, MarketAsset::Quote, NAD + 1, MarketAsset::Base,)
            .unwrap(),
        NAD as u128,
    );
    assert_eq!(
        raw_amount_from_target_value_nad_with_prices(
            &market,
            prices,
            MarketAsset::Quote,
            MarketAsset::Base,
            NAD as u128,
        )
        .unwrap(),
        NAD + 1,
    );
}

#[test]
fn proportional_debt_checks_adjacent_shares_after_interest_accrual() {
    let index = 6_989_199_360;
    // All five old raw candidates (6,990..=6,994) round up to 1,001
    // shares and 6,996 debt atoms, two atoms above the proportional claim.
    // The preceding share count has debt 6,989 and claim 6,990.
    for quoted_debt in [None, Some(6_992)] {
        assert_eq!(
            canonical_debt_for_proportional_claim(6_992, 10_000, 20_000, index, quoted_debt).unwrap(),
            (1_000, 6_989),
        );
    }
}

#[test]
fn proportional_debt_matches_exhaustive_share_search() {
    for reserve in 0..=32_u64 {
        for hlp in 1..=8_u64 {
            for ordinary in 1..=8_u64 {
                let supply = hlp + ordinary;
                for index in [
                    NAD as u128,
                    1_500_000_000,
                    6_989_199_360,
                    7_000_000_000,
                    100_000_000_000,
                ] {
                    // Every larger share count is above the continuous root
                    // because index >= NAD. Enumerate independently of the
                    // production candidate selection and its rounding order.
                    let limit = reserve * hlp / ordinary + 2;
                    let exists = (0..=limit).any(|shares| {
                        let debt = shares as u128 * index / NAD as u128;
                        let claim = (reserve as u128 + debt) * hlp as u128 / supply as u128;
                        debt.abs_diff(claim) <= 1
                    });
                    let result = canonical_debt_for_proportional_claim(reserve, hlp, supply, index, None);
                    assert_eq!(
                        result.is_ok(),
                        exists,
                        "reserve={reserve}, h={hlp}, S={supply}, index={index}"
                    );
                    if let Ok((shares, debt)) = result {
                        assert_eq!(shares * index / NAD as u128, debt as u128);
                        assert!(debt.abs_diff((reserve + debt) * hlp / supply) <= 1);
                    }
                }
            }
        }
    }
}

#[test]
fn proportional_debt_preserves_a_valid_quote_and_rejects_an_unsatisfiable_gap() {
    // Both adjacent points satisfy the tolerance; preserve the quoted one.
    assert_eq!(
        canonical_debt_for_proportional_claim(10, 1, 2, 2 * NAD as u128, Some(11)).unwrap(),
        (6, 12)
    );
    // At a 10-atom quantum, debt 0 has error -2 and debt 10 has error +3.
    assert!(canonical_debt_for_proportional_claim(5, 1, 2, 10 * NAD as u128, None).is_err());
    assert_eq!(
        canonical_debt_for_proportional_claim(10, 0, 0, 0, None).unwrap(),
        (0, 0)
    );
    assert!(canonical_debt_for_proportional_claim(10, 1, 1, NAD as u128, None).is_err());
    assert!(canonical_debt_for_proportional_claim(10, 1, 2, 0, None).is_err());
}

#[test]
fn proportional_debt_skips_unrepresentable_candidates_at_reserve_capacity() {
    let reserve = u64::MAX - 1;
    // The continuous debt is just over one atom, but only zero debt fits
    // at this two-atom share quantum. Its claim error is exactly one.
    assert_eq!(
        canonical_debt_for_proportional_claim(reserve, 1, u64::MAX - 1, 2 * NAD as u128, Some(u64::MAX)).unwrap(),
        (0, 0),
    );
    // A larger continuous solution can exceed u64 while its capacity-clipped
    // predecessor still meets the tolerance (a nearly all-hLP supply).
    assert_eq!(
        canonical_debt_for_proportional_claim(2, u64::MAX - 1, u64::MAX, NAD as u128, None).unwrap(),
        ((u64::MAX - 2) as u128, u64::MAX - 2),
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2048))]

    #[test]
    fn proportional_debt_matches_exact_feasible_interval(
        reserve in any::<u64>(),
        supply in 2..=u64::MAX,
        seed in any::<u64>(),
        index in prop_oneof![4 => NAD as u128..=10_000 * NAD as u128, 1 => NAD as u128..=u128::MAX],
        quoted in proptest::option::of(any::<u64>()),
    ) {
        let hlp = 1 + seed % (supply - 1);
        let ordinary = u128::from(supply - hlp);
        let product = u128::from(reserve) * u128::from(hlp);
        // |d - floor((R+d)h/S)| <= 1 iff -2S < d(S-h)-Rh <= S.
        // Solve that interval directly, independently of the fixed-point
        // neighborhood used by the implementation.
        let min_debt = (product + 1).saturating_sub(2 * u128::from(supply)).div_ceil(ordinary);
        let max_debt = ((product + u128::from(supply)) / ordinary).min(u128::from(u64::MAX - reserve));
        let feasible = if min_debt > max_debt {
            false
        } else {
            let first_shares = (min_debt * NAD as u128).div_ceil(index);
            first_shares * index / NAD as u128 <= max_debt
        };
        let result = canonical_debt_for_proportional_claim(reserve, hlp, supply, index, quoted);
        prop_assert_eq!(result.is_ok(), feasible);
        if let Ok((shares, debt)) = result {
            prop_assert_eq!(shares * index / NAD as u128, u128::from(debt));
            prop_assert!(u128::from(debt) >= min_debt && u128::from(debt) <= max_debt);
        }
    }
}

#[cfg(feature = "benchmark")]
fn captured_vob_market() -> Market {
    // Adapt this historical fixture only: it predates fractional index carry
    // and aggregate leverage collateral. Locate each
    // addition in the current Borsh layout using a unique sentinel, then insert
    // its neutral value in serialization order. All captured bytes stay intact.
    let mut layout = Market::default();
    layout.debt.base_borrow_index_remainder = u128::from_le_bytes([0x22; 16]);
    layout.debt.quote_borrow_index_remainder = u128::from_le_bytes([0x22; 16]);
    layout.debt.leverage_base_collateral = u64::from_le_bytes([0x33; 8]);
    layout.debt.leverage_quote_collateral = u64::from_le_bytes([0x33; 8]);
    let mut encoded_layout = Vec::new();
    layout.try_serialize(&mut encoded_layout).unwrap();
    let mut captured = include_bytes!("../fixtures/vob-market-20260929.bin").to_vec();
    for (marker, replacement) in [
        (vec![0x22; 32], vec![0; 32]),
        (vec![0x33; 16], vec![0; 16]),
    ] {
        let offsets: Vec<_> = encoded_layout
            .windows(marker.len())
            .enumerate()
            .filter_map(|(offset, window)| (window == marker).then_some(offset))
            .collect();
        assert_eq!(offsets.len(), 1, "fixture adapter sentinel must be unique");
        captured.splice(offsets[0]..offsets[0], replacement);
    }
    let mut market = Market::try_deserialize(&mut captured.as_slice()).unwrap();
    assert_eq!(market.version, 1, "the fixture must retain its original version");
    market.version = MARKET_LAYOUT_VERSION;
    market
}

#[cfg(feature = "benchmark")]
#[test]
fn captured_vob_bids_survive_fractional_output_rounding() {
    use crate::benchmark_api::{BenchmarkClock, BenchmarkMarket, BenchmarkSwapRequest};
    use crate::state::FutarchyAuthority;

    let authority =
        FutarchyAuthority::try_deserialize(&mut include_bytes!("../fixtures/vob-authority-20260929.bin").as_slice())
            .unwrap();
    let clock = BenchmarkClock {
        slot: 505_496_119,
        unix_timestamp: 1_790_677_156,
    };
    // These are the twelve candidate bid levels for the captured devnet
    // market. Before the carry certificate, six failed BrokenInvariant;
    // the quote-side raw reconciliation was four atoms instead of three.
    // Using recorded start debt changes the fractional ordinary coordinate;
    // all twelve bids now require exactly one certified output atom.
    for (amount, expected_carry) in [
        (540_606, 1),
        (1_082_025, 1),
        (1_624_259, 1),
        (2_167_308, 1),
        (2_711_176, 1),
        (3_255_864, 1),
        (3_801_374, 1),
        (4_347_710, 1),
        (4_894_872, 1),
        (5_442_864, 1),
        (5_991_686, 1),
        (6_541_340, 1),
    ] {
        let market = captured_vob_market();
        let benchmark = BenchmarkMarket::from_market_state(market, clock).unwrap();
        let request = BenchmarkSwapRequest {
            asset_in: MarketAsset::Base,
            reserve_credit: amount,
            protocol_fee_bps: authority.revenue_share.swap_bps,
            protocol_auction_split: authority.protocol_auction_split,
        };
        let result = benchmark
            .preview_swap(request)
            .unwrap_or_else(|error| panic!("bid {amount}: {error:?}"));
        assert!(result.quote.amount_out > 0, "bid {amount}");
        let mut before = captured_vob_market();
        let prepared = crate::transitions::amm::SwapRequest {
            current_slot: clock.slot,
            current_unix_timestamp: clock.unix_timestamp,
            asset_in: MarketAsset::Base,
            reserve_credit: amount,
            protocol_fee_bps: authority.revenue_share.swap_bps,
        }
        .prepare(&mut before)
        .unwrap();
        let transition = prepared.concentrated_transition.as_ref().unwrap();
        assert_eq!(transition.base_output_rounding_carry, 0, "bid {amount}");
        assert_eq!(transition.quote_output_rounding_carry, expected_carry, "bid {amount}");
    }
}

#[cfg(feature = "benchmark")]
#[test]
fn captured_vob_bid_rejects_unexplained_reserve_drift() {
    use crate::state::FutarchyAuthority;

    let authority =
        FutarchyAuthority::try_deserialize(&mut include_bytes!("../fixtures/vob-authority-20260929.bin").as_slice())
            .unwrap();
    let mut market = captured_vob_market();
    let mut prepared = crate::transitions::amm::SwapRequest {
        current_slot: 505_496_119,
        current_unix_timestamp: 1_790_677_156,
        asset_in: MarketAsset::Base,
        reserve_credit: 1_082_025,
        protocol_fee_bps: authority.revenue_share.swap_bps,
    }
    .prepare(&mut market)
    .unwrap();
    // The certified carry is one atom. An additional four unexplained atoms
    // must still fail the original three-atom reconciliation bound.
    market.quote_side.reserves.live_reserve += 4;
    let error = prepared
        .finalize_state(
            &mut market,
            505_496_119,
            authority.revenue_share.swap_bps,
            authority.protocol_auction_split,
        )
        .unwrap_err();
    assert_eq!(error, error!(ErrorCode::BrokenInvariant));
}

#[cfg(feature = "benchmark")]
#[test]
fn captured_vob_asks_certify_only_base_output_rounding() {
    use crate::benchmark_api::{BenchmarkClock, BenchmarkMarket, BenchmarkSwapRequest};
    use crate::state::FutarchyAuthority;

    let authority =
        FutarchyAuthority::try_deserialize(&mut include_bytes!("../fixtures/vob-authority-20260929.bin").as_slice())
            .unwrap();
    let clock = BenchmarkClock {
        slot: 505_496_119,
        unix_timestamp: 1_790_677_156,
    };
    for amount in [500_000, 1_000_000, 2_000_000, 4_000_000] {
        let market = captured_vob_market();
        let benchmark = BenchmarkMarket::from_market_state(market, clock).unwrap();
        let request = BenchmarkSwapRequest {
            asset_in: MarketAsset::Quote,
            reserve_credit: amount,
            protocol_fee_bps: authority.revenue_share.swap_bps,
            protocol_auction_split: authority.protocol_auction_split,
        };
        let result = benchmark
            .preview_swap(request)
            .unwrap_or_else(|error| panic!("ask {amount}: {error:?}"));
        assert!(result.quote.amount_out > 0, "ask {amount}");
        let mut before = captured_vob_market();
        let prepared = crate::transitions::amm::SwapRequest {
            current_slot: clock.slot,
            current_unix_timestamp: clock.unix_timestamp,
            asset_in: MarketAsset::Quote,
            reserve_credit: amount,
            protocol_fee_bps: authority.revenue_share.swap_bps,
        }
        .prepare(&mut before)
        .unwrap();
        let transition = prepared.concentrated_transition.as_ref().unwrap();
        assert_eq!(transition.quote_output_rounding_carry, 0, "ask {amount}");
        assert!(transition.base_output_rounding_carry <= 1, "ask {amount}");
    }
}
