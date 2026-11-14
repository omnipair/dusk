fn funding_tail_market(vaults: u8, above: bool) -> Market {
    funding_market_with_shape(vaults, above, curve_parameters(Shape::TailFloor))
}

fn funding_market_with_shape(vaults: u8, above: bool, shape: ConcentratedCurveParameters) -> Market {
    let mut market = seeded_market(PAIRS[1], 1_000_000.0, Shape::TailFloor).unwrap();
    if market.config.amm.concentrated_curve_parameters().unwrap() != shape {
        market.config.amm.set_concentrated_curve_parameters(shape).unwrap();
        *market.amm = Default::default();
        market.prepare_amm_for_swap(1).unwrap();
    }
    for (mask, asset) in [(1, MarketAsset::Base), (2, MarketAsset::Quote)] {
        if vaults & mask != 0 {
            let deposit = reserve_raw(&market, asset) / 20;
            market.deposit_single_sided(asset, deposit, 1).unwrap();
        }
    }
    let asset = if above { MarketAsset::Quote } else { MarketAsset::Base };
    let outer = 10_000 + shape.core_half_width_bps as u64 + shape.fade_width_bps as u64;
    let target = if above {
        150 * NAD * outer / 10_000 * 110 / 100
    } else {
        150 * NAD * 10_000 / outer * 90 / 100
    };
    let boundaries = curve_boundaries(&market);
    let boundary = if above {
        *boundaries.iter().min_by_key(|point| point.base_reserve).unwrap()
    } else {
        *boundaries.iter().max_by_key(|point| point.base_reserve).unwrap()
    };
    let (boundary_asset, boundary_input) = input_to_reach(&market, boundary).unwrap();
    assert_eq!(boundary_asset, asset);
    let (mut low, mut high) = (1, boundary_input * 2);
    while low < high {
        let amount = low + (high - low) / 2;
        let mut candidate = market.clone();
        swap(&mut candidate, asset, amount, 2).unwrap();
        let price = candidate.current_concentrated_spot_price_nad().unwrap().unwrap();
        if if above { price >= target } else { price <= target } {
            high = amount;
        } else {
            low = amount + 1;
        }
    }
    swap(&mut market, asset, low, 2).unwrap();
    let geometry = market.current_concentrated_curve_geometry().unwrap().unwrap();
    assert_eq!(
        geometry.branch(market.ordinary_curve_point_nad().unwrap()),
        if above {
            crate::transitions::amm::ConcentratedCurveBranch::UpperTail
        } else {
            crate::transitions::amm::ConcentratedCurveBranch::LowerTail
        }
    );
    market.assert_market_invariants().unwrap();
    market
}

#[test]
fn idle_hlp_funding_outside_band_preserves_pricing_inventory() {
    for shape in [
        curve_parameters(Shape::TailFloor),
        ConcentratedCurveParameters {
            peak_amplification_nad: max_amplification(500, 0),
            core_half_width_bps: 500,
            fade_width_bps: 0,
        },
        ConcentratedCurveParameters {
            peak_amplification_nad: max_amplification(5_000, 0),
            core_half_width_bps: 5_000,
            fade_width_bps: 0,
        },
    ] {
        for vaults in [1, 2, 3] {
            for above in [false, true] {
                for hours in [1, 24, 7 * 24, 30 * 24] {
                    let mut market = funding_market_with_shape(vaults, above, shape);
                    let point = market.ordinary_curve_point_nad().unwrap();
                    let cache = market.amm.concentrated_curve_cache;
                    let price = market.current_concentrated_spot_price_nad().unwrap();
                    let reserves = market.curve_reserves_nad().unwrap();
                    let cash = (
                        market.base_side.reserves.cash_reserve,
                        market.quote_side.reserves.cash_reserve,
                    );
                    let debt = (market.debt.base_borrow_index_nad, market.debt.quote_borrow_index_nad);
                    let slot = 2 + hours * crate::constants::MS_PER_DAY / 24 / crate::constants::TARGET_MS_PER_SLOT;
                    market.accrue_interest_to_slot(slot).unwrap();
                    assert_eq!(market.curve_reserves_nad().unwrap(), reserves);
                    assert_eq!(
                        (
                            market.base_side.reserves.cash_reserve,
                            market.quote_side.reserves.cash_reserve
                        ),
                        cash
                    );
                    assert!(market.debt.base_borrow_index_nad > debt.0 || market.debt.quote_borrow_index_nad > debt.1);
                    assert_eq!(
                        market.ordinary_curve_point_nad().unwrap(),
                        point,
                        "vaults={vaults} above={above}"
                    );
                    market.update_at_slot(slot).unwrap();
                    assert_eq!(market.amm.concentrated_curve_cache, cache);
                    assert_eq!(market.current_concentrated_spot_price_nad().unwrap(), price);
                    assert_eq!(market.risk.cached_spot_base_price_nad, price.unwrap());
                    market.assert_market_invariants().unwrap();
                }
            }
        }
    }
}

#[test]
fn both_hlp_vaults_settle_idle_funding_once_without_losing_nav_or_hedges() {
    use crate::transitions::liquidity::{current_hlp_curve_prices, current_hlp_signed_navs_with_prices};
    for above in [false, true] {
        for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
            let mut market = funding_tail_market(3, above);
            let prices = current_hlp_curve_prices(&market).unwrap();
            let price = market.current_concentrated_spot_price_nad().unwrap().unwrap() as u128;
            let before_accrual = current_hlp_signed_navs_with_prices(&market, prices).unwrap();
            let slot = 2 + crate::constants::MS_PER_DAY / crate::constants::TARGET_MS_PER_SLOT;
            market.update_at_slot(slot).unwrap();
            let after_accrual = current_hlp_signed_navs_with_prices(&market, prices).unwrap();
            assert!(after_accrual.0 < before_accrual.0 && after_accrual.1 < before_accrual.1);
            let debts = [
                Debt::shares_to_debt(market.base_hlp_vault.debt_shares, market.debt.quote_borrow_index_nad).unwrap()
                    as u64,
                Debt::shares_to_debt(market.quote_hlp_vault.debt_shares, market.debt.base_borrow_index_nad).unwrap()
                    as u64,
            ];
            let interest = [
                debts[0] - market.base_hlp_vault.debt_principal,
                debts[1] - market.quote_hlp_vault.debt_principal,
            ];
            assert!(interest[0] > 0 && interest[1] > 0);
            let amount = raw_amount(
                if asset_in == MarketAsset::Base { 0.001 } else { 0.15 },
                market.side(asset_in).asset_decimals,
            )
            .unwrap();
            let before = market.clone();
            let mut prepared = SwapRequest {
                current_slot: slot,
                current_unix_timestamp: 0,
                asset_in,
                reserve_credit: amount,
                protocol_fee_bps: 0,
            }
            .prepare(&mut market)
            .unwrap();
            let state = before.integrated_curve_state_nad().unwrap();
            let quote = prepared.quote;
            assert_eq!(quote.start_price_nad, before.risk.cached_spot_base_price_nad);
            let receipt = prepared
                .finalize_state(&mut market, slot, 0, ProtocolAuctionSplit::default())
                .unwrap();
            assert_eq!(receipt.base_rebalance.interest_paid, interest[0]);
            assert_eq!(receipt.quote_rebalance.interest_paid, interest[1]);
            for asset in [MarketAsset::Base, MarketAsset::Quote] {
                let paid = if asset == MarketAsset::Base {
                    interest[1]
                } else {
                    interest[0]
                };
                let expected = before.side(asset).reserves.cash_reserve as i128
                    + if asset == asset_in {
                        quote.fee.amount_in_for_quote as i128
                    } else {
                        -(quote.gross_amount_out as i128)
                    }
                    - paid as i128;
                assert_eq!(market.side(asset).reserves.cash_reserve as i128, expected);
            }
            let after_settlement = current_hlp_signed_navs_with_prices(&market, prices).unwrap();
            let base_atom = atom_nad(&market, MarketAsset::Base);
            let quote_atom = atom_nad(&market, MarketAsset::Quote);
            let base_bound = 4 * (base_atom + quote_atom * NAD as u128 / price);
            let quote_bound = 4 * (quote_atom + base_atom * price / NAD as u128);
            assert!(state.base_hlp_equity.abs_diff(after_accrual.0 as u128) <= base_bound);
            assert!(state.quote_hlp_equity.abs_diff(after_accrual.1 as u128) <= quote_bound);
            assert!(
                after_settlement.0.abs_diff(after_accrual.0) <= base_bound,
                "base NAV: before={after_accrual:?} after={after_settlement:?}"
            );
            assert!(
                after_settlement.1.abs_diff(after_accrual.1) <= quote_bound,
                "quote NAV: before={after_accrual:?} after={after_settlement:?}"
            );
            let supply = market.base_side.shares.srlp_supply as u128;
            for (vault, opposite, index) in [
                (
                    &market.base_hlp_vault,
                    MarketAsset::Quote,
                    market.debt.quote_borrow_index_nad,
                ),
                (
                    &market.quote_hlp_vault,
                    MarketAsset::Base,
                    market.debt.base_borrow_index_nad,
                ),
            ] {
                let claim = market.side(opposite).reserves.live_reserve * vault.srlp_shares as u128 / supply;
                let debt = Debt::shares_to_debt(vault.debt_shares, index).unwrap();
                assert!(claim.abs_diff(debt) <= 1, "claim={claim} debt={debt}");
            }
            market.assert_market_invariants().unwrap();
            assert!(
                market
                    .current_concentrated_spot_price_nad()
                    .unwrap()
                    .unwrap()
                    .abs_diff(quote.reserve_end_price_nad)
                    <= quote.reserve_end_price_nad.div_ceil(NAD)
            );
            let mut next = SwapRequest {
                current_slot: slot,
                current_unix_timestamp: 0,
                asset_in,
                reserve_credit: amount,
                protocol_fee_bps: 0,
            }
            .prepare(&mut market)
            .unwrap();
            // Reconstructing a rounded endpoint can differ by a few NAD
            // price units. Bound the discrepancy to one part per billion.
            assert!(
                next.quote
                    .start_price_nad
                    .abs_diff(market.risk.cached_spot_base_price_nad)
                    <= market.risk.cached_spot_base_price_nad.div_ceil(NAD)
            );
            let second = next
                .finalize_state(&mut market, slot, 0, ProtocolAuctionSplit::default())
                .unwrap();
            assert!(second.base_rebalance.interest_paid <= 1);
            assert!(second.quote_rebalance.interest_paid <= 1);
        }
    }
}

#[test]
fn funding_payment_alone_preserves_the_pricing_reserve_point() {
    use crate::transitions::liquidity::prepare_concentrated_hlp_transition_from_end;
    for vaults in [1, 2, 3] {
        for above in [false, true] {
            let mut market = funding_tail_market(vaults, above);
            let slot = 2 + crate::constants::MS_PER_DAY / crate::constants::TARGET_MS_PER_SLOT;
            market.update_at_slot(slot).unwrap();
            let point = market.ordinary_curve_point_nad().unwrap();
            let price = market.current_concentrated_spot_price_nad().unwrap();
            let cache = market.amm.concentrated_curve_cache;
            let state = market.integrated_curve_state_nad().unwrap();
            assert_eq!(
                state.reconstructed_curve_point().unwrap().unwrap(),
                point,
                "state vaults={vaults} above={above}"
            );
            let payment = prepare_concentrated_hlp_transition_from_end(&market, state, false, true, 0, 0).unwrap();
            assert_eq!(
                payment.final_ordinary_point_nad(&market).unwrap(),
                point,
                "plan vaults={vaults} above={above}"
            );
            let (base, quote) = payment.consume(&mut market).unwrap();
            assert!(base.interest_paid > 0 || quote.interest_paid > 0);
            assert_eq!(
                market.ordinary_curve_point_nad().unwrap(),
                point,
                "vaults={vaults} above={above}"
            );
            assert_eq!(market.amm.concentrated_curve_cache, cache);
            assert_eq!(market.current_concentrated_spot_price_nad().unwrap(), price);
            market.update_at_slot(slot).unwrap();
            assert_eq!(market.risk.cached_spot_base_price_nad, price.unwrap());
            market.assert_market_invariants().unwrap();
        }
    }
}

#[test]
fn funding_payment_on_partial_and_full_hlp_exit_keeps_pricing_principal() {
    for above in [false, true] {
        for target in [MarketAsset::Base, MarketAsset::Quote] {
            let mut market = funding_tail_market(3, above);
            let slot = 2 + crate::constants::MS_PER_DAY / crate::constants::TARGET_MS_PER_SLOT;
            market.update_at_slot(slot).unwrap();
            let point = market.ordinary_curve_point_nad().unwrap();
            let price = market.current_concentrated_spot_price_nad().unwrap();
            let supply = if target == MarketAsset::Base {
                market.base_hlp_vault.hlp_supply
            } else {
                market.quote_hlp_vault.hlp_supply
            };
            for amount in [supply / 2, supply - supply / 2] {
                let receipt = market.withdraw_single_sided(target, amount).unwrap();
                assert!(receipt.interest_paid > 0);
                assert_eq!(market.ordinary_curve_point_nad().unwrap(), point);
                assert_eq!(market.current_concentrated_spot_price_nad().unwrap(), price);
                market.assert_market_invariants().unwrap();
            }
        }
    }
}

#[test]
fn paid_funding_separation_survives_lp_round_trips_and_both_vault_closures() {
    use crate::transitions::liquidity::prepare_concentrated_hlp_transition_from_end;
    for above in [false, true] {
        for close_vaults in [false, true] {
            let mut market = funding_tail_market(3, above);
            let slot = 2 + crate::constants::MS_PER_DAY / crate::constants::TARGET_MS_PER_SLOT;
            market.update_at_slot(slot).unwrap();
            let state = market.integrated_curve_state_nad().unwrap();
            prepare_concentrated_hlp_transition_from_end(&market, state, false, true, 0, 0)
                .unwrap()
                .consume(&mut market)
                .unwrap();
            if close_vaults {
                for target in [MarketAsset::Base, MarketAsset::Quote] {
                    let supply = if target == MarketAsset::Base {
                        market.base_hlp_vault.hlp_supply
                    } else {
                        market.quote_hlp_vault.hlp_supply
                    };
                    market.withdraw_single_sided(target, supply).unwrap();
                }
                assert!(!market.has_active_hlp());
            }
            let before = market.ordinary_curve_point_nad().unwrap();
            let hlp_shares = market.base_hlp_vault.srlp_shares + market.quote_hlp_vault.srlp_shares;
            let supply = market.base_side.shares.srlp_supply - hlp_shares;
            let raw = [
                market.curve_reserve(MarketAsset::Base).unwrap(),
                market.curve_reserve(MarketAsset::Quote).unwrap(),
            ];
            // Ordinary minting retains the existing atom-rounded hLP hedge.
            // Bound both its target atom and the converted opposite atom.
            let reserves = market.curve_reserves_nad().unwrap();
            let bounds = [
                2 * (atom_nad(&market, MarketAsset::Base)
                    + atom_nad(&market, MarketAsset::Quote) * reserves.base / reserves.quote),
                2 * (atom_nad(&market, MarketAsset::Quote)
                    + atom_nad(&market, MarketAsset::Base) * reserves.quote / reserves.base),
            ];
            let added = market.add_liquidity(raw[0] / 10, raw[1] / 10).unwrap();
            let enlarged = market.ordinary_curve_point_nad().unwrap();
            for (old, new, bound) in [
                (before.base_reserve, enlarged.base_reserve, bounds[0]),
                (before.quote_reserve, enlarged.quote_reserve, bounds[1]),
            ] {
                let expected =
                    crate::math::mul_div_u128(old, u128::from(added.srlp_supply - hlp_shares), u128::from(supply))
                        .unwrap();
                assert!(
                    new.abs_diff(expected) <= bound,
                    "above={above} closed={close_vaults} old={old} new={new} expected={expected} bound={bound}"
                );
            }
            let removed = market.remove_liquidity(added.srlp_amount).unwrap();
            assert!(removed.base_amount_out <= added.base_reserve_credit);
            assert!(removed.quote_amount_out <= added.quote_reserve_credit);
            let after = market.ordinary_curve_point_nad().unwrap();
            assert!(after.base_reserve.abs_diff(before.base_reserve) <= bounds[0]);
            assert!(after.quote_reserve.abs_diff(before.quote_reserve) <= bounds[1]);
            for asset in [MarketAsset::Base, MarketAsset::Quote] {
                let amount = raw_amount(
                    if asset == MarketAsset::Base { 1.0 } else { 150.0 },
                    market.side(asset).asset_decimals,
                )
                .unwrap();
                for _ in 0..3 {
                    let output = swap(&mut market, asset, amount, slot).unwrap();
                    let returned = swap(&mut market, asset.opposite(), output, slot).unwrap();
                    assert!(returned <= amount, "funding round trip: {returned} > {amount}");
                    market.assert_market_invariants().unwrap();
                }
            }
        }
    }
}

#[test]
fn announced_ramp_survives_last_lp_exit_and_reinitialization() {
    let mut market = seeded_market(PAIRS[1], 1_000_000.0, Shape::Moderate).unwrap();
    market.config.swap_fee_bps = MIN_CONCENTRATED_SWAP_FEE_BPS;
    market
        .execute_parameter_updates(
            &[MarketParameterUpdate::Concentration {
                peak_amplification_nad: 200 * NAD,
                core_half_width_bps: 25,
                fade_width_bps: 50,
                ramp_duration_slots: MIN_CONCENTRATION_RAMP_SLOTS,
            }],
            2,
        )
        .unwrap();
    let ramp = market.amm.concentration_ramp;
    let receipt = market
        .remove_liquidity(market.base_side.shares.srlp_supply - crate::constants::MIN_LIQUIDITY)
        .unwrap();
    market.finalize_amm_transition_and_observe_risk(3).unwrap();
    assert!(!market.amm.initialized);
    market
        .add_liquidity(receipt.base_amount_out, receipt.quote_amount_out)
        .unwrap();
    market.ensure_amm_initialized(4).unwrap();
    assert_eq!(market.amm.concentration_ramp, ramp);
    assert!(market.advance_concentration_ramp(ramp.end_slot).unwrap());
    assert_eq!(market.config.amm.concentrated_curve_parameters().unwrap(), ramp.target);
    assert!(!market.amm.concentration_ramp.active);
}

#[test]
fn concentration_ramp_intermediate_shapes_retain_the_required_tail() {
    use crate::state::ConcentrationRamp;
    let wide = ConcentratedCurveParameters {
        peak_amplification_nad: max_amplification(2_500, 2_500),
        core_half_width_bps: 2_500,
        fade_width_bps: 2_500,
    };
    let narrow = ConcentratedCurveParameters {
        peak_amplification_nad: max_amplification(250, 250),
        core_half_width_bps: 250,
        fade_width_bps: 250,
    };
    let target = ConcentratedCurveParameters {
        peak_amplification_nad: 200 * NAD,
        core_half_width_bps: 25,
        fade_width_bps: 50,
    };
    for (start, target) in [
        (wide, narrow),
        (narrow, wide),
        (curve_parameters(Shape::Moderate), target),
    ] {
        ConcentrationRamp::validate_target(start, target, MIN_CONCENTRATION_RAMP_SLOTS).unwrap();
        let ramp = ConcentrationRamp {
            active: true,
            start,
            target,
            start_slot: 1,
            end_slot: 1 + MIN_CONCENTRATION_RAMP_SLOTS,
            last_attempt_slot: 0,
        };
        assert_eq!(ramp.candidate(1).unwrap(), start);
        for step in 1..1_000 {
            let shape = ramp.candidate(1 + MIN_CONCENTRATION_RAMP_SLOTS * step / 1_000).unwrap();
            shape.validate(MAX_AMM_AMPLIFICATION_NAD).unwrap();
            assert!(shape.liquidity_shares_nad().unwrap().0 >= crate::state::MIN_AMM_TAIL_SHARE_NAD as u128);
        }
        assert_eq!(ramp.candidate(ramp.end_slot).unwrap(), target);
    }
}

#[test]
fn concentration_ramps_complete_off_center_with_both_hlp_vaults() {
    for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = seeded_market(PAIRS[1], 1_000_000.0, Shape::Moderate).unwrap();
        for asset in [MarketAsset::Base, MarketAsset::Quote] {
            let deposit = reserve_raw(&market, asset) / 20;
            market.deposit_single_sided(asset, deposit, 1).unwrap();
        }
        let amount = reserve_raw(&market, asset_in) / 1_000;
        swap(&mut market, asset_in, amount, 2).unwrap();
        let center = market.amm.center_price_nad;
        let target = ConcentratedCurveParameters {
            peak_amplification_nad: 200 * NAD,
            core_half_width_bps: 25,
            fade_width_bps: 50,
        };
        market.config.swap_fee_bps = MIN_CONCENTRATED_SWAP_FEE_BPS;
        market
            .execute_parameter_updates(
                &[MarketParameterUpdate::Concentration {
                    peak_amplification_nad: target.peak_amplification_nad,
                    core_half_width_bps: target.core_half_width_bps,
                    fade_width_bps: target.fade_width_bps,
                    ramp_duration_slots: MIN_CONCENTRATION_RAMP_SLOTS,
                }],
                3,
            )
            .unwrap();
        let before = market.clone();
        for step in 1..=40 {
            assert!(
                market
                    .advance_concentration_ramp(3 + MIN_CONCENTRATION_RAMP_SLOTS * step / 40)
                    .unwrap(),
                "asset={asset_in:?} step={step}"
            );
            assert_eq!(market.amm.center_price_nad, center);
            assert_eq!(
                market.base_side.reserves.try_to_vec().unwrap(),
                before.base_side.reserves.try_to_vec().unwrap()
            );
            assert_eq!(
                market.quote_side.reserves.try_to_vec().unwrap(),
                before.quote_side.reserves.try_to_vec().unwrap()
            );
            assert_eq!(market.base_hlp_vault.debt_shares, before.base_hlp_vault.debt_shares);
            assert_eq!(market.quote_hlp_vault.debt_shares, before.quote_hlp_vault.debt_shares);
            market.assert_market_invariants().unwrap();
        }
        assert_eq!(market.config.amm.concentrated_curve_parameters().unwrap(), target);
        assert!(!market.amm.concentration_ramp.active);
    }
}

#[test]
fn frozen_hlp_funding_remains_a_liability_without_moving_pricing_inventory() {
    use crate::transitions::liquidity::{current_hlp_curve_prices, current_hlp_signed_navs_with_prices};
    for above in [false, true] {
        let mut market = funding_tail_market(3, above);
        let point = market.ordinary_curve_point_nad().unwrap();
        let cache = market.amm.concentrated_curve_cache;
        market.debt.base_borrow_index_nad *= 100;
        market.debt.quote_borrow_index_nad *= 100;
        let liabilities = (market.base_hlp_vault.debt_shares, market.quote_hlp_vault.debt_shares);
        let nav = current_hlp_signed_navs_with_prices(&market, current_hlp_curve_prices(&market).unwrap()).unwrap();
        market.freeze_negative_hlp_vaults().unwrap();
        assert_eq!(market.hlp_frozen(MarketAsset::Base), nav.0 < 0);
        assert_eq!(market.hlp_frozen(MarketAsset::Quote), nav.1 < 0);
        assert_ne!(market.amm.frozen_hlp_mask, 0);
        assert_eq!(market.ordinary_curve_point_nad().unwrap(), point);
        assert_eq!(market.amm.concentrated_curve_cache, cache);
        assert_eq!(
            (market.base_hlp_vault.debt_shares, market.quote_hlp_vault.debt_shares),
            liabilities
        );
        market.debt.base_borrow_index_nad *= 2;
        market.debt.quote_borrow_index_nad *= 2;
        assert_eq!(market.ordinary_curve_point_nad().unwrap(), point);
        market.assert_market_invariants().unwrap();
    }
}

#[test]
fn cash_constrained_both_hlp_vaults_retain_unpaid_funding_and_nav() {
    use crate::transitions::liquidity::{current_hlp_curve_prices, current_hlp_signed_navs_with_prices};
    for above in [false, true] {
        let mut market = funding_tail_market(3, above);
        // Seed outstanding performing public loans to isolate the settlement
        // cash constraint. This fixture does not assert borrowing admission.
        for asset in [MarketAsset::Base, MarketAsset::Quote] {
            let retained_cash = 10_u64.pow(market.side(asset).asset_decimals.into());
            let borrowed = market.side(asset).reserves.cash_reserve - retained_cash;
            let shares = Debt::debt_to_shares(borrowed, market.debt.borrow_index(asset)).unwrap();
            let actual = Debt::shares_to_debt(shares, market.debt.borrow_index(asset)).unwrap();
            market.side_mut(asset).reserves.cash_reserve = retained_cash;
            market.side_mut(asset).reserves.live_reserve += actual - borrowed as u128;
            if asset == MarketAsset::Base {
                market.debt.fixed_base_shares = shares;
                market.debt.fixed_base_principal = borrowed;
            } else {
                market.debt.fixed_quote_shares = shares;
                market.debt.fixed_quote_principal = borrowed;
            }
        }
        let slot = 2 + crate::constants::MS_PER_DAY / crate::constants::TARGET_MS_PER_SLOT;
        market.update_at_slot(slot).unwrap();
        let prices = current_hlp_curve_prices(&market).unwrap();
        let nav = current_hlp_signed_navs_with_prices(&market, prices).unwrap();
        let carry_before = [
            Debt::shares_to_debt(market.base_hlp_vault.debt_shares, market.debt.quote_borrow_index_nad).unwrap()
                - market.base_hlp_vault.debt_principal as u128,
            Debt::shares_to_debt(market.quote_hlp_vault.debt_shares, market.debt.base_borrow_index_nad).unwrap()
                - market.quote_hlp_vault.debt_principal as u128,
        ];
        let mut prepared = SwapRequest {
            current_slot: slot,
            current_unix_timestamp: 0,
            asset_in: MarketAsset::Quote,
            reserve_credit: 150_000,
            protocol_fee_bps: 0,
        }
        .prepare(&mut market)
        .unwrap();
        let before = market.clone();
        let quote = prepared.quote;
        let receipt = prepared
            .finalize_state(&mut market, slot, 0, ProtocolAuctionSplit::default())
            .unwrap();
        assert_eq!(receipt.base_rebalance.interest_paid, 0);
        assert_eq!(receipt.quote_rebalance.interest_paid, 0);
        assert_eq!(
            market.base_side.reserves.cash_reserve,
            before.base_side.reserves.cash_reserve - quote.gross_amount_out
        );
        assert_eq!(
            market.quote_side.reserves.cash_reserve,
            before.quote_side.reserves.cash_reserve + quote.fee.amount_in_for_quote
        );
        for (i, vault, opposite, index) in [
            (
                0,
                &market.base_hlp_vault,
                MarketAsset::Quote,
                market.debt.quote_borrow_index_nad,
            ),
            (
                1,
                &market.quote_hlp_vault,
                MarketAsset::Base,
                market.debt.base_borrow_index_nad,
            ),
        ] {
            let debt = Debt::shares_to_debt(vault.debt_shares, index).unwrap();
            let carry = debt - vault.debt_principal as u128;
            assert!(carry > 0 && carry.abs_diff(carry_before[i]) <= 1);
            let (reserve, coupon) = if market.side(opposite).fees.junior.initialized {
                (
                    market.curve_reserve(opposite).unwrap() as u128,
                    market
                        .hlp_unpaid_income(if i == 0 { MarketAsset::Base } else { MarketAsset::Quote }, opposite)
                        .unwrap() as u128,
                )
            } else {
                (market.side(opposite).reserves.live_reserve, 0)
            };
            let claim = reserve * vault.srlp_shares as u128 / market.base_side.shares.srlp_supply as u128 + coupon;
            assert!(
                claim.abs_diff(debt - carry) <= 1,
                "vault={i} claim={claim} debt={debt} carry={carry} principal={}",
                vault.debt_principal
            );
        }
        let after = current_hlp_signed_navs_with_prices(&market, prices).unwrap();
        let price = before.current_concentrated_spot_price_nad().unwrap().unwrap() as u128;
        let base_bound =
            4 * (atom_nad(&market, MarketAsset::Base) + atom_nad(&market, MarketAsset::Quote) * NAD as u128 / price);
        let quote_bound =
            4 * (atom_nad(&market, MarketAsset::Quote) + atom_nad(&market, MarketAsset::Base) * price / NAD as u128);
        assert!(after.0.abs_diff(nav.0) <= base_bound, "base: {nav:?} -> {after:?}");
        assert!(after.1.abs_diff(nav.1) <= quote_bound, "quote: {nav:?} -> {after:?}");
        market.assert_market_invariants().unwrap();
        let returned = swap(&mut market, MarketAsset::Base, quote.amount_out, slot).unwrap();
        assert!(returned <= 150_000, "carried-funding round trip returned {returned}");
        for _ in 0..2 {
            let bought = swap(&mut market, MarketAsset::Quote, 150_000, slot).unwrap();
            let returned = swap(&mut market, MarketAsset::Base, bought, slot).unwrap();
            assert!(
                returned <= 150_000,
                "repeated carried-funding round trip returned {returned}"
            );
            market.assert_market_invariants().unwrap();
        }
    }
}

#[test]
fn retained_surcharge_entry_pricing_holds_with_paid_funding_offsets() {
    use crate::transitions::liquidity::prepare_concentrated_hlp_transition_from_end;
    for above in [false, true] {
        let mut market = funding_tail_market(3, above);
        let slot = 2 + crate::constants::MS_PER_DAY / crate::constants::TARGET_MS_PER_SLOT;
        market.update_at_slot(slot).unwrap();
        // Paid funding moves the pricing offsets off zero without moving the point.
        let state = market.integrated_curve_state_nad().unwrap();
        let payment = prepare_concentrated_hlp_transition_from_end(&market, state, false, true, 0, 0).unwrap();
        payment.consume(&mut market).unwrap();
        assert_ne!(
            (market.amm.hlp_funding_base_pricing_offset_nad, market.amm.hlp_funding_quote_pricing_offset_nad),
            (0, 0),
            "above={above}"
        );
        for asset in [MarketAsset::Base, MarketAsset::Quote] {
            let bucket = market.curve_reserve(asset).unwrap() / 100;
            market.credit_protected_recenter_reserve(asset, bucket).unwrap();
        }
        let price = u128::from(market.current_concentrated_spot_price_nad().unwrap().unwrap());
        let value = |market: &Market, base: u64, quote: u64| {
            market.normalize_amount(u128::from(base), market.base_side.asset_decimals).unwrap() * price / NAD as u128
                + market.normalize_amount(u128::from(quote), market.quote_side.asset_decimals).unwrap()
        };
        // Holders of record own the curve reserves and the undeployed bucket.
        let per_share = |market: &Market| {
            let base = market.curve_reserve(MarketAsset::Base).unwrap() + market.base_side.reserves.protected_recenter_reserve;
            let quote = market.curve_reserve(MarketAsset::Quote).unwrap() + market.quote_side.reserves.protected_recenter_reserve;
            value(market, base, quote) * NAD as u128 / u128::from(market.base_side.shares.srlp_supply)
        };
        let (base_in, quote_in) = (market.curve_reserve(MarketAsset::Base).unwrap(), market.curve_reserve(MarketAsset::Quote).unwrap());

        // An entry and an immediate exit around a bucket cannot gain.
        let mut round_trip = market.clone();
        let entry = round_trip.add_liquidity(base_in, quote_in).unwrap();
        let exit = round_trip.remove_liquidity(entry.srlp_amount).unwrap();
        let paid = value(&round_trip, entry.base_reserve_credit, entry.quote_reserve_credit);
        let received = value(&round_trip, exit.base_amount_out, exit.quote_amount_out);
        assert!(received <= paid, "above={above} paid={paid} received={received}");

        // An entry ahead of the deployment pays for its slice of the bucket,
        // so it does not dilute the holders of record.
        let before = per_share(&market);
        market.add_liquidity(base_in, quote_in).unwrap();
        market.finalize_amm_transition_and_observe_risk(slot).unwrap();
        let after = per_share(&market);
        assert!(after >= before, "above={above} before={before} after={after}");
        assert!(after - before <= before / 1_000_000_000 + 4, "above={above} before={before} after={after}");
        market.assert_market_invariants().unwrap();
    }
}
