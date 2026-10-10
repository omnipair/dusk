use super::*;

const START_SLOT: u64 = 2_500;

fn sol_usdc_market(hlp_dollars: u64) -> Market {
    let side = |decimals| MarketSide {
        asset_mint: Pubkey::new_unique(),
        hlp_mint: Pubkey::new_unique(),
        reserve_vault: Pubkey::new_unique(),
        collateral_vault: Pubkey::new_unique(),
        interest_vault: Pubkey::new_unique(),
        asset_decimals: decimals,
        ..MarketSide::default()
    };
    let config = MarketConfig {
        swap_fee_bps: 3,
        divergence_fee_share_cap_bps: 20,
        volatility_fee_share_cap_bps: 20,
        target_hlp_leverage_bps: 20_000,
        settlement_divergence_bps: 10_000,
        ema_half_life_ms: 60_000,
        directional_ema_half_life_ms: 60_000,
        curve_depth_ema_half_life_ms: 60_000,
        max_daily_borrow_bps: crate::state::DEFAULT_DAILY_BORROW_BPS,
        global_health_contribution_cap_bps: 15_000,
        borrow_market_health_floor_bps: 11_000,
        amm: AmmConfig {
            peak_amplification_nad: 5 * NAD,
            core_half_width_bps: 100,
            fade_width_bps: 400,
            center_ema_half_life_ms: 60_000,
            volatility_half_life_ms: 60_000,
            adjustment_threshold_nad: NAD / 100,
            adjustment_step_nad: NAD / 1_000,
            min_adjustment_interval_slots: 150,
            volatility_shock_cap_nad: NAD / 20,
            volatility_cap_nad: NAD / 10,
            divergence_fee_coefficient_nad: NAD / 10,
            volatility_fee_coefficient_nad: NAD / 10,
            ..AmmConfig::default()
        },
        ..MarketConfig::default()
    };
    let mut market = Market::default();
    market
        .initialize(
            Pubkey::new_unique(),
            side(9),
            side(6),
            config,
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            [0; 32],
            Pubkey::new_unique(),
            200 * NAD,
            0,
            START_SLOT,
            255,
        )
        .unwrap();
    let ordinary_dollars = 1_000_000 - 2 * hlp_dollars;
    market
        .add_liquidity(ordinary_dollars / 400 * NAD, ordinary_dollars / 2 * 1_000_000)
        .unwrap();
    market.finalize_amm_transition_and_observe_risk(START_SLOT).unwrap();
    market
        .deposit_single_sided(MarketAsset::Base, hlp_dollars / 200 * NAD, START_SLOT)
        .unwrap();
    market.finalize_amm_transition_and_observe_risk(START_SLOT).unwrap();
    market
        .deposit_single_sided(MarketAsset::Quote, hlp_dollars * 1_000_000, START_SLOT)
        .unwrap();
    market.finalize_amm_transition_and_observe_risk(START_SLOT).unwrap();
    market
}

fn settle_and_check(market: &mut Market, asset: MarketAsset, slot: u64) {
    let mut prepared = SwapRequest {
        current_slot: slot,
        current_unix_timestamp: (slot * 2 / 5) as i64,
        asset_in: asset,
        reserve_credit: if asset == MarketAsset::Base {
            NAD / 2
        } else {
            100_000_000
        },
        protocol_fee_bps: 2_000,
    }
    .prepare(market)
    .unwrap();
    let quote = prepared.quote;
    let before = market.clone();
    let finalized = prepared
        .finalize_state(market, slot, 2_000, ProtocolAuctionSplit::default())
        .unwrap();
    for side in [MarketAsset::Base, MarketAsset::Quote] {
        let (vault, receipt) = if side == MarketAsset::Base {
            (&before.quote_hlp_vault, finalized.quote_rebalance)
        } else {
            (&before.base_hlp_vault, finalized.base_rebalance)
        };
        let owed = Debt::shares_to_debt(vault.debt_shares, before.debt.borrow_index(side)).unwrap() as u64;
        let interest = owed - vault.debt_principal;
        assert_eq!(receipt.interest_paid, interest);
        let trader_flow = if side == asset {
            quote.fee.amount_in_for_quote as i128
        } else {
            -(quote.gross_amount_out as i128)
        };
        let compounded = if side.code() == quote.fee.fee_asset {
            quote.fee.compounded_fee_debit
        } else {
            0
        };
        assert_eq!(
            market.side(side).reserves.cash_reserve as i128,
            before.side(side).reserves.cash_reserve as i128 + trader_flow + compounded as i128 - interest as i128
        );
        let settled_vault = if side == MarketAsset::Base {
            &market.quote_hlp_vault
        } else {
            &market.base_hlp_vault
        };
        let claim = market.curve_reserve(side).unwrap() as u128 * settled_vault.ylp_shares as u128
            / market.side(side).shares.ylp_supply as u128;
        let debt = Debt::shares_to_debt(settled_vault.debt_shares, market.debt.borrow_index(side)).unwrap();
        assert!(claim.abs_diff(debt) <= 3, "side={side:?}, claim={claim}, debt={debt}");
        assert_eq!(debt, settled_vault.debt_principal as u128);
    }
    market.assert_market_invariants().unwrap();
}

#[test]
fn joint_shock_sol_usdc_preserves_recorded_starting_debt() {
    for hlp_dollars in [25_000, 100_000] {
        for asset in [MarketAsset::Base, MarketAsset::Quote] {
            let mut market = sol_usdc_market(hlp_dollars);
            let state = market.integrated_curve_state_nad().unwrap();
            let reserve = market.curve_reserves_nad().unwrap();
            // With no accrued interest the starting ledger must agree exactly,
            // even when the two mints and the yLP shares round differently.
            assert_eq!(
                state.ordinary_base + state.base_hlp_equity,
                reserve.base
                    - market
                        .normalize_amount(market.hlp_live_reserve(MarketAsset::Base).unwrap(), 9)
                        .unwrap()
            );
            assert_eq!(
                state.ordinary_quote + state.quote_hlp_equity,
                reserve.quote
                    - market
                        .normalize_amount(market.hlp_live_reserve(MarketAsset::Quote).unwrap(), 6)
                        .unwrap()
            );
            settle_and_check(&mut market, asset, START_SLOT);
        }
    }
}

#[test]
fn joint_shock_sol_usdc_pays_funding_once_and_preserves_opposite_claims() {
    for hlp_dollars in [25_000, 100_000] {
        for age_seconds in [30, 900, 86_400] {
            for asset in [MarketAsset::Base, MarketAsset::Quote] {
                let mut market = sol_usdc_market(hlp_dollars);
                let slot = START_SLOT + age_seconds * 5 / 2;
                market.accrue_interest_to_slot(slot).unwrap();
                market.advance_amm_clock(slot).unwrap();
                market.checkpoint_hlp_vaults().unwrap();
                market.refresh_risk_at_slot(slot).unwrap();
                let debt = Debt::shares_to_debt(market.base_hlp_vault.debt_shares, market.debt.quote_borrow_index_nad)
                    .unwrap();
                assert!(debt > market.base_hlp_vault.debt_principal as u128 + 3);
                let state = market.integrated_curve_state_nad().unwrap();
                let reserve = market.curve_reserves_nad().unwrap();
                assert_eq!(
                    state.ordinary_base + state.base_hlp_equity + state.quote_hlp_base_debt,
                    reserve.base
                );
                assert_eq!(
                    state.ordinary_quote + state.quote_hlp_equity + state.base_hlp_quote_debt,
                    reserve.quote
                );
                settle_and_check(&mut market, asset, slot);
                // A second same-slot swap must not pay the cleared interest again.
                settle_and_check(&mut market, asset.opposite(), slot);
            }
        }
    }
}
