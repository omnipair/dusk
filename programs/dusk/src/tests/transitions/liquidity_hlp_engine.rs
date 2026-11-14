use super::*;
use proptest::prelude::*;

#[test]
fn captured_hlp_entries_settle_funding_and_preserve_existing_claims() {
    for (bytes, slot) in [
        (include_bytes!("../fixtures/hlp-entry-layout17.bin").as_slice(), 500_291_305),
        // Market 4iuoB4WsTEsv4gnwY6RZnwgaHWqYCAwCbaLswJjbwgSa,
        // devnet slot 519889239: quote hLP needs accrued-funding settlement.
        (include_bytes!("../fixtures/hlp-entry-funding-layout17.bin").as_slice(), 519_889_239),
    ] {
        for target in [MarketAsset::Base, MarketAsset::Quote] {
            for amount in [1_000_000, 10_000_000, 100_000_000] {
                let mut market = Market::try_deserialize(&mut bytes.as_ref()).unwrap();
                market.accrue_interest_to_slot(slot).unwrap();
                market.advance_amm_clock(slot).unwrap();
                market.checkpoint_hlp_vaults().unwrap();
                let before = market.clone();
                let prices = current_hlp_curve_prices(&before).unwrap();
                let supply = match target {
                    MarketAsset::Base => before.base_hlp_vault.hlp_supply,
                    MarketAsset::Quote => before.quote_hlp_vault.hlp_supply,
                };
                let old_nav = hlp_share_nav_nad_with_prices(&before, target, prices).unwrap();
                let ordinary_supply = before.base_side.shares.srlp_supply
                    - before.base_hlp_vault.srlp_shares - before.quote_hlp_vault.srlp_shares;
                let receipt = market.deposit_single_sided(target, amount, 1).unwrap();
                assert!(receipt.hlp_amount > 0);
                market.assert_hlp_entry_available(target).unwrap();
                assert_eq!(ordinary_supply, market.base_side.shares.srlp_supply
                    - market.base_hlp_vault.srlp_shares - market.quote_hlp_vault.srlp_shares);
                if supply > 0 {
                    let nav = hlp_share_nav_nad_with_prices(&market, target, prices).unwrap();
                    assert!(ratio_lte_full_width(old_nav, supply as u128, nav, receipt.hlp_supply as u128).unwrap(),
                        "existing hLP share value must not be diluted: target={target:?}, amount={amount}");
                }
                // Each independently owned coupon stays with its old vault;
                // depositing never pays for the new shares with old income.
                for vault in [MarketAsset::Base, MarketAsset::Quote] {
                    for asset in [MarketAsset::Base, MarketAsset::Quote] {
                        assert_eq!(market.hlp_unpaid_income(vault, asset).unwrap(), before.hlp_unpaid_income(vault, asset).unwrap());
                    }
                }
                market.assert_market_invariants().unwrap();
            }
        }
    }
}

#[test]
fn entry_cannot_rebalance_the_other_vault_during_its_flash_session() {
    let mut market = Market::current_layout();
    market.add_liquidity(1_000_000, 1_000_000).unwrap();
    market.debt.base_borrow_index_nad = NAD as u128;
    market.debt.quote_borrow_index_nad = NAD as u128;
    market.deposit_single_sided(MarketAsset::Base, 20_000, 1).unwrap();
    market.base_hlp_vault.flash_collateral_debit = 1;
    let before = market.try_to_vec().unwrap();
    assert_eq!(market.deposit_single_sided(MarketAsset::Quote, 20_000, 1).unwrap_err(), error!(ErrorCode::LiquidationSessionActive));
    assert_eq!(market.try_to_vec().unwrap(), before);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn canonical_entry_preserves_share_value_after_indexed_funding(
        amount in 10_u64..1_000,
        elapsed in 1_u64..648_000,
        mixed_decimals in any::<bool>(),
        concentrated in any::<bool>(),
        target_base in any::<bool>(),
    ) {
        use crate::state::{AmmConfig, AmmState};
        let target = if target_base { MarketAsset::Base } else { MarketAsset::Quote };
        let mut market = Market::current_layout();
        market.base_side.asset_decimals = 6;
        market.quote_side.asset_decimals = if mixed_decimals { 9 } else { 6 };
        let base_unit = 1_000_000;
        let quote_unit = 10_u64.pow(market.quote_side.asset_decimals.into());
        market.debt.base_borrow_index_nad = NAD as u128;
        market.debt.quote_borrow_index_nad = NAD as u128;
        market.add_liquidity(1_000_000 * base_unit, 1_000_000 * quote_unit).unwrap();
        market.deposit_single_sided(MarketAsset::Base, 50 * base_unit, 1).unwrap();
        market.deposit_single_sided(MarketAsset::Quote, 50 * quote_unit, 1).unwrap();
        if concentrated {
            market.config.amm = AmmConfig {
                peak_amplification_nad: 4 * NAD,
                core_half_width_bps: 100,
                fade_width_bps: 400,
                center_ema_half_life_ms: crate::constants::MIN_HALF_LIFE_MS,
                ..AmmConfig::default()
            };
            *market.amm = AmmState::default();
            market.prepare_amm_for_swap(1).unwrap();
        }
        market.accrue_interest_to_slot(elapsed).unwrap();
        let before = market.clone();
        let prices = current_hlp_curve_prices(&before).unwrap();
        let old_nav = hlp_share_nav_nad_with_prices(&before, target, prices).unwrap();
        let old_supply = if target_base { before.base_hlp_vault.hlp_supply } else { before.quote_hlp_vault.hlp_supply };
        let deposit = amount * 10_u64.pow(market.side(target).asset_decimals.into());
        let receipt = market.deposit_single_sided(target, deposit, 1).unwrap();
        let nav = hlp_share_nav_nad_with_prices(&market, target, prices).unwrap();
        prop_assert!(ratio_lte_full_width(old_nav, old_supply as u128, nav, receipt.hlp_supply as u128).unwrap());
        let other = target.opposite();
        let other_before = hlp_share_nav_nad_with_prices(&before, other, prices).unwrap();
        let other_after = hlp_share_nav_nad_with_prices(&market, other, prices).unwrap();
        let rounding_bound = market.normalize_amount(3, market.side(other).asset_decimals).unwrap();
        prop_assert!(other_after.saturating_add(rounding_bound) >= other_before);
        market.assert_hlp_entry_available(target).unwrap();
        market.assert_market_invariants().unwrap();
        let returned = market.quote_hlp_withdrawal(target, receipt.hlp_amount).unwrap();
        // Canonical settlement already certifies at most three reserve atoms
        // on each side. Mixed-decimal exits must use that same value bound,
        // rather than pretending an opposite-mint atom is one target atom.
        let rounding_value = asset_value_in_target_nad_with_prices(&market, prices, MarketAsset::Base, 3, target).unwrap()
            + asset_value_in_target_nad_with_prices(&market, prices, MarketAsset::Quote, 3, target).unwrap();
        let rounding_atoms = market.denormalize_amount_ceil(rounding_value, market.side(target).asset_decimals).unwrap();
        prop_assert!(returned <= deposit + rounding_atoms, "entry/exit exceeds native atom reconciliation: deposited={}, returned={}, bound={}", deposit, returned, rounding_atoms);
    }
}

#[test]
fn senior_entry_and_cash_exit_exclude_separately_owned_unpaid_interest() {
    const UNIT: u64 = 1_000_000;
    let mut market = Market::current_layout();
    market.base_side.asset_decimals = 6;
    market.quote_side.asset_decimals = 6;
    market.debt.base_borrow_index_nad = NAD as u128;
    market.debt.quote_borrow_index_nad = NAD as u128;
    market.add_liquidity(1_000_000 * UNIT, 1_000_000 * UNIT).unwrap();
    market.debt.fixed_quote_shares = u128::from(425_000 * UNIT);
    market.debt.fixed_quote_principal = 400_000 * UNIT;
    market.quote_side.reserves.cash_reserve -= 400_000 * UNIT;
    market.quote_side.reserves.live_reserve += u128::from(25_000 * UNIT);
    let old_supply = market.base_side.shares.srlp_supply;
    let old_base_claim = market.base_side.reserves.live_reserve;
    let old_quote_claim = market.curve_reserve(MarketAsset::Quote).unwrap();
    let cash_before = market.quote_side.reserves.cash_reserve;
    let entry = market.add_liquidity(1_000 * UNIT, 1_025 * UNIT).unwrap();
    assert_eq!(entry.srlp_amount, old_supply / 1_000);
    assert_eq!(entry.quote_reserve_credit, 1_000 * UNIT);
    assert_eq!(
        srlp_live_underlying_amount(&market, MarketAsset::Base, old_supply).unwrap(),
        u64::try_from(old_base_claim).unwrap()
    );
    assert_eq!(
        market.quote_side.shares.reserve_for_burn(market.curve_reserve(MarketAsset::Quote).unwrap(), old_supply).unwrap(),
        old_quote_claim
    );
    let exit = market.remove_liquidity(entry.srlp_amount).unwrap();
    assert_eq!(exit.base_amount_out, entry.base_reserve_credit);
    assert_eq!(exit.quote_amount_out, entry.quote_reserve_credit);
    assert_eq!(market.quote_side.reserves.cash_reserve, cash_before);
    assert_eq!(
        market.unrealized_interest(MarketAsset::Quote).unwrap(),
        u128::from(25_000 * UNIT)
    );
    assert_eq!(market.quote_side.fees.interest_growth_index_q64, 0);
    market.assert_market_invariants().unwrap();
}

#[test]
fn h_lp_entry_mints_against_current_value_even_if_cached_values_are_stale() {
    use crate::state::{AmmConfig, AmmState};

    for target in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = Market::current_layout();
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
        *market.amm = AmmState::default();
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
        let mut market = Market::current_layout();
        market.add_liquidity(1_000_000, 1_000_000).unwrap();
        market.debt.base_borrow_index_nad = NAD as u128;
        market.debt.quote_borrow_index_nad = NAD as u128;
        market.deposit_single_sided(target, 20_000, 1).unwrap();
        let prices = current_hlp_curve_prices(&market).unwrap();
        let economic_before = hlp_share_nav_nad_with_prices(&market, target, prices).unwrap();
        let executable_before = hlp_nav_nad_with_prices(&market, target, prices).unwrap();
        assert_eq!(economic_before, executable_before);

        // A public borrower owes 25,000 of accrued target-asset interest.
        // It belongs to the srLP share owners but is not executable AMM cash.
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
        market.sync_income_receivable(target, false, 0).unwrap();
        let vault = match target {
            MarketAsset::Base => &market.base_hlp_vault,
            MarketAsset::Quote => &market.quote_hlp_vault,
        };
        let expected_interest_claim =
            12_500_u128 * u128::from(vault.srlp_shares) / u128::from(market.tranches.senior_reserve_shares(market.side(target).shares.srlp_supply).unwrap());
        let expected_in_nad = market.normalize_amount(expected_interest_claim, market.side(target).asset_decimals).unwrap();
        let economic_after = hlp_share_nav_nad_with_prices(&market, target, prices).unwrap();
        assert_eq!(economic_after - economic_before, expected_in_nad);
        assert_eq!(hlp_nav_nad_with_prices(&market, target, prices).unwrap(), economic_after);
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
        let mut initial = Market::current_layout();
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
        *initial.amm = AmmState::default();
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
    let mut market = Market::current_layout();
    market.base_side.asset_decimals = 0;
    market.quote_side.asset_decimals = 0;
    let price = (NAD as u128) * (NAD as u128) + NAD as u128;
    let prices = hlp_curve_prices_from_base_price_nad(price).unwrap();
    // One whole token of either asset in the market's internal amount units.
    let unit = market.normalize_amount(1, 0).unwrap();

    assert_eq!(
        asset_value_in_target_nad_with_prices(&market, prices, MarketAsset::Base, 1, MarketAsset::Quote).unwrap(),
        unit * (NAD as u128 + 1),
    );
    assert_eq!(
        asset_value_in_target_nad_with_prices(&market, prices, MarketAsset::Quote, NAD + 1, MarketAsset::Base,)
            .unwrap(),
        unit,
    );
    assert_eq!(
        raw_amount_from_target_value_nad_with_prices(
            &market,
            prices,
            MarketAsset::Quote,
            MarketAsset::Base,
            unit,
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
                    let result = canonical_debt_for_proportional_claim(u128::from(reserve), hlp, supply, index, None);
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
        canonical_debt_for_proportional_claim(u128::from(reserve), 1, u64::MAX - 1, 2 * NAD as u128, Some(u64::MAX)).unwrap(),
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
        let result = canonical_debt_for_proportional_claim(u128::from(reserve), hlp, supply, index, quoted);
        prop_assert_eq!(result.is_ok(), feasible);
        if let Ok((shares, debt)) = result {
            prop_assert_eq!(shares * index / NAD as u128, u128::from(debt));
            prop_assert!(u128::from(debt) >= min_debt && u128::from(debt) <= max_debt);
        }
    }
}

#[cfg(feature = "benchmark")]
fn captured_vob_market() -> Market {
    let bytes = include_bytes!("../fixtures/vob-market-layout17.bin");
    Market::try_deserialize(&mut bytes.as_slice()).unwrap()
}

#[cfg(feature = "benchmark")]
#[test]
fn captured_vob_bids_survive_fractional_output_rounding() {
    use crate::benchmark_api::{BenchmarkClock, BenchmarkMarket, BenchmarkSwapRequest};
    use crate::state::FutarchyAuthority;

    let authority =
        FutarchyAuthority::try_deserialize(&mut include_bytes!("../fixtures/vob-authority-layout17.bin").as_slice())
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
        FutarchyAuthority::try_deserialize(&mut include_bytes!("../fixtures/vob-authority-layout17.bin").as_slice())
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
        FutarchyAuthority::try_deserialize(&mut include_bytes!("../fixtures/vob-authority-layout17.bin").as_slice())
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

mod interest {
    use super::*;
    include!("liquidity_hlp_interest.rs");
}

/// A first hLP deposit whose ownership rounds away must not create receipts
/// backed by zero value. Such receipts price every later entry against zero
/// NAV, so one dust deposit would close hLP entry on that side.
#[test]
fn first_hlp_deposit_must_create_positive_value() {
    let mut market = Market::current_layout();
    market.debt.base_borrow_index_nad = NAD as u128;
    market.debt.quote_borrow_index_nad = NAD as u128;
    market.add_liquidity(1_000_000_000, 1_000_000).unwrap();

    let mut dust = market.clone();
    let error = dust.deposit_single_sided(MarketAsset::Base, 1, 1).unwrap_err();
    assert!(error.to_string().contains("AmountZero"), "{error}");

    let first = market.deposit_single_sided(MarketAsset::Base, 1_000_000, 1).unwrap();
    assert_eq!(first.hlp_amount, 1_000_000);
    let prices = current_hlp_curve_prices(&market).unwrap();
    assert!(hlp_share_nav_nad_with_prices(&market, MarketAsset::Base, prices).unwrap() > 0);
    market.assert_market_invariants().unwrap();
    let next = market.deposit_single_sided(MarketAsset::Base, 1_000_000, 1).unwrap();
    assert!(next.hlp_amount > 0);
    market.assert_market_invariants().unwrap();
}

#[test]
fn consecutive_partial_hlp_exits_keep_live_reserve_on_funded_debt() {
    use crate::state::{AmmConfig, AmmState};
    use crate::transitions::amm::SwapRequest;
    use crate::transitions::liquidity::SwapCashPolicy;
    use crate::transitions::LeverageSwapFeeCredit;
    fn swap(market: &mut Market, asset_in: MarketAsset, amount: u64, slot: u64) -> Result<()> {
        let mut prepared = SwapRequest {
            current_slot: slot,
            current_unix_timestamp: 0,
            asset_in,
            reserve_credit: amount,
            protocol_fee_bps: 0,
        }
        .prepare(market)?;
        let quote = prepared.leverage_quote();
        let credit = LeverageSwapFeeCredit::from_total_actual_credit(&quote, prepared.quote.fee.claimable_fee_debit)?;
        prepared
            .apply(market, SwapCashPolicy::Spot, credit, slot, 0, ProtocolAuctionSplit::default(), None)
            .map(|_| ())?;
        market.assert_market_invariants()
    }
    for concentrated in [false, true] {
        let mut market = Market::current_layout();
        market.add_liquidity(1_000_000, 1_000_000).unwrap();
        market.debt.base_borrow_index_nad = NAD as u128;
        market.debt.quote_borrow_index_nad = NAD as u128;
        market.config.settlement_divergence_bps = 10_000;
        market.deposit_single_sided(MarketAsset::Base, 200_000, 1).unwrap();
        market.deposit_single_sided(MarketAsset::Quote, 200_000, 1).unwrap();
        market.config.swap_fee_bps = 30;
        if concentrated {
            market.config.amm = AmmConfig {
                peak_amplification_nad: 4 * NAD,
                core_half_width_bps: 100,
                fade_width_bps: 400,
                center_ema_half_life_ms: crate::constants::MIN_HALF_LIFE_MS,
                ..AmmConfig::default()
            };
            *market.amm = AmmState::default();
            market.prepare_amm_for_swap(1).unwrap();
        }
        swap(&mut market, MarketAsset::Quote, 30_000, 2).unwrap();
        // Five consecutive partial exits without an intervening swap used to
        // leave the borrowed-side live reserve above the funded debt by more
        // than the three-atom identity tolerance, and every swap reverted.
        for target in [MarketAsset::Base, MarketAsset::Quote] {
            let supply = match target {
                MarketAsset::Base => market.base_hlp_vault.hlp_supply,
                MarketAsset::Quote => market.quote_hlp_vault.hlp_supply,
            };
            for k in 0..30_u64 {
                market.withdraw_single_sided(target, supply / 37 + k % 3).unwrap();
                let vault = match target {
                    MarketAsset::Base => market.base_hlp_vault,
                    MarketAsset::Quote => market.quote_hlp_vault,
                };
                assert_eq!(vault.hlp_live_reserve(target.opposite()), vault.debt_principal);
            }
        }
        swap(&mut market.clone(), MarketAsset::Base, 1_000, 3).unwrap();
        swap(&mut market.clone(), MarketAsset::Quote, 1_000, 3).unwrap();
        market.clone().deposit_single_sided(MarketAsset::Base, 10_000, 1).unwrap();
    }
}

#[test]
fn hlp_exit_imbalance_is_capped_by_the_bounded_market_price() {
    use crate::oracle::{OracleObservation, OraclePolicy, RuntimePricing};
    use crate::state::{AmmConfig, AmmState};
    let mut native = Market::current_layout();
    native.add_liquidity(1_000_000, 1_000_000).unwrap();
    native.config.amm = AmmConfig {
        peak_amplification_nad: 4 * NAD,
        core_half_width_bps: 100,
        fade_width_bps: 400,
        center_ema_half_life_ms: crate::constants::MIN_HALF_LIFE_MS,
        ..AmmConfig::default()
    };
    *native.amm = AmmState::default();
    native.prepare_amm_for_swap(1).unwrap();
    // Selling a 10,000 base surplus for quote, and covering a 10,000 quote
    // shortfall with base, on the native curve.
    let native_surplus = settled_close_target_amount(&native, MarketAsset::Quote, 0, 10_000, 0).unwrap();
    let native_retained = 50_000 - settled_close_target_amount(&native, MarketAsset::Base, 50_000, 0, 10_000).unwrap();
    for reference in [NAD / 2, 2 * NAD] {
        let mut market = native.clone();
        market.oracle.policy = OraclePolicy {
            lambda_bps: 9_000,
            adjustment_cap_nad: 2 * NAD,
            ..OraclePolicy::default()
        };
        market.oracle.runtime = RuntimePricing::bind(
            market.oracle.policy,
            OracleObservation {
                price_nad: reference,
                ..OracleObservation::default()
            },
        );
        market.checkpoint_oracle_risk_depth().unwrap();
        let curve = market.bounded_curve().unwrap().unwrap();
        let (low, high) = curve.price_bounds_nad(market.ordinary_curve_point_nad().unwrap()).unwrap();
        let surplus = settled_close_target_amount(&market, MarketAsset::Quote, 0, 10_000, 0).unwrap();
        assert!(surplus <= native_surplus);
        assert!(u128::from(surplus) <= 10_000 * u128::from(low) / u128::from(NAD));
        let retained = 50_000 - settled_close_target_amount(&market, MarketAsset::Base, 50_000, 0, 10_000).unwrap();
        assert!(retained >= native_retained);
        assert!(u128::from(retained) >= 10_000 * u128::from(NAD) / u128::from(high));
    }
}
