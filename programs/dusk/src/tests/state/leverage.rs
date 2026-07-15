use super::*;
use crate::{
    constants::{INTEREST_INITIAL_RATE_AT_TARGET_NAD, NAD},
    state::{
        Debt, HlpVault, Insurance, MarketConfig, MarketSide, PendingAuthorityChange,
        PendingConfigChange, ProtocolAuctionSplit, ReserveShares, Reserves, Risk,
    },
};

fn test_market(base_cash: u64, quote_cash: u64) -> Market {
    let mut base_side = MarketSide::default();
    base_side.reserves = Reserves {
        live_reserve: base_cash,
        cash_reserve: base_cash,
        reserved_liability: 0,
    };
    base_side.shares = ReserveShares {
        ylp_supply: base_cash,
    };
    let mut quote_side = MarketSide::default();
    quote_side.reserves = Reserves {
        live_reserve: quote_cash,
        cash_reserve: quote_cash,
        reserved_liability: 0,
    };
    quote_side.shares = ReserveShares {
        ylp_supply: quote_cash,
    };
    Market {
        version: 2,
        base_mint: Pubkey::new_unique(),
        quote_mint: Pubkey::new_unique(),
        ylp_mint: Pubkey::new_unique(),
        operator: Pubkey::new_unique(),
        manager: Pubkey::new_unique(),
        base_side,
        quote_side,
        config: MarketConfig {
            swap_fee_bps: 0,
            ..MarketConfig::default()
        },
        debt: Debt {
            base_borrow_index_nad: NAD as u128,
            quote_borrow_index_nad: NAD as u128,
            base_rate_at_target_nad: INTEREST_INITIAL_RATE_AT_TARGET_NAD,
            quote_rate_at_target_nad: INTEREST_INITIAL_RATE_AT_TARGET_NAD,
            ..Debt::default()
        },
        base_hlp_vault: HlpVault::default(),
        quote_hlp_vault: HlpVault::default(),
        risk: Risk::default(),
        insurance: Insurance::default(),
        pending_config: PendingConfigChange::default(),
        pending_operator: PendingAuthorityChange::default(),
        pending_manager: PendingAuthorityChange::default(),
        params_hash: [0u8; 32],
        last_update_slot: 0,
        reduce_only: false,
        bump: 255,
    }
}

fn assert_error<T>(result: Result<T>, expected: &str) {
    let error = match result {
        Ok(_) => panic!("expected {expected} error"),
        Err(error) => error,
    };
    let rendered = format!("{error:?} {error}");
    assert!(
        rendered.contains(expected),
        "expected error containing {expected}, got {rendered}"
    );
}

fn empty_position() -> LeveragePosition {
    LeveragePosition {
        owner: Pubkey::default(),
        market: Pubkey::default(),
        position_id: Pubkey::default(),
        debt_asset: 0,
        margin_mode: 0,
        collateral_amount: 0,
        margin_amount: 0,
        open_notional: 0,
        debt_principal: 0,
        debt_shares: 0,
        multiplier_bps: 0,
        opened_at: 0,
        opened_slot: 0,
        bump: 0,
    }
}

fn seeded_position(
    market: &mut Market,
    debt_asset: MarketAsset,
    debt_amount: u64,
    collateral_amount: u64,
) -> LeveragePosition {
    let debt_shares = market
        .debt
        .add_isolated_debt(debt_asset, debt_amount)
        .unwrap();
    match debt_asset {
        MarketAsset::Base => {
            market.base_side.reserves.cash_reserve -= debt_amount;
        }
        MarketAsset::Quote => {
            market.quote_side.reserves.cash_reserve -= debt_amount;
        }
    }
    let mut position = empty_position();
    position.initialize(
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        debt_asset,
        collateral_amount,
        debt_amount,
        debt_amount * 2,
        debt_amount,
        debt_shares,
        20_000,
        0,
        0,
        255,
    );
    position
}

#[test]
fn exact_output_quote_is_minimal_and_never_underfills() {
    let mut market = test_market(1_000_000, 2_000_000);
    market.config.swap_fee_bps = 30;
    let target_out = 123_456;

    let quote = market
        .quote_leverage_swap_exact_output(MarketAsset::Base, target_out)
        .unwrap();
    let achieved = calculate_raw_amount_out(
        market.base_side.reserves.live_reserve,
        market.quote_side.reserves.live_reserve,
        quote.amount_in_after_fee,
    )
    .unwrap();

    assert_eq!(quote.amount_out, target_out);
    assert!(achieved >= target_out);
    let previous_amount_in = quote.amount_in - 1;
    let previous_fee = leverage_swap_fee(previous_amount_in, market.config.swap_fee_bps).unwrap();
    let previous_after_fee = previous_amount_in - previous_fee;
    let previous_out = calculate_raw_amount_out(
        market.base_side.reserves.live_reserve,
        market.quote_side.reserves.live_reserve,
        previous_after_fee,
    )
    .unwrap();
    assert!(previous_out < target_out);
}

#[test]
fn exact_output_quote_is_minimal_across_assets_fees_and_sizes() {
    for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
        for swap_fee_bps in [0, 1, 30, 500, 9_999] {
            for target_out in [1, 2, 999, 50_000] {
                let mut market = test_market(1_000_000, 2_000_000);
                market.config.swap_fee_bps = swap_fee_bps;
                let quote = market
                    .quote_leverage_swap_exact_output(asset_in, target_out)
                    .unwrap();
                let (reserve_in, reserve_out) = match asset_in {
                    MarketAsset::Base => (1_000_000, 2_000_000),
                    MarketAsset::Quote => (2_000_000, 1_000_000),
                };
                let achieved =
                    calculate_raw_amount_out(reserve_in, reserve_out, quote.amount_in_after_fee).unwrap();

                assert_eq!(quote.amount_out, target_out);
                assert!(achieved >= target_out);
                assert_eq!(
                    quote.amount_in_after_fee,
                    quote.amount_in - quote.fee_credit
                );

                let previous_amount_in = quote.amount_in - 1;
                let previous_fee = leverage_swap_fee(previous_amount_in, swap_fee_bps).unwrap();
                let previous_after_fee = previous_amount_in - previous_fee;
                let previous_out = if previous_after_fee == 0 {
                    0
                } else {
                    calculate_raw_amount_out(reserve_in, reserve_out, previous_after_fee).unwrap()
                };
                assert!(
                    previous_out < target_out,
                    "non-minimal quote for {asset_in:?}, fee {swap_fee_bps}, target {target_out}"
                );
            }
        }
    }
}

#[test]
fn leverage_quotes_reject_zero_exhausted_liquidity_and_full_fee() {
    let mut market = test_market(1_000, 2_000);

    assert_error(
        market.quote_leverage_swap(MarketAsset::Base, 0),
        "AmountZero",
    );
    assert_error(
        market.quote_leverage_swap_exact_output(MarketAsset::Base, 0),
        "AmountZero",
    );
    assert_error(
        market.quote_leverage_swap_exact_output(MarketAsset::Base, 2_000),
        "InsufficientLiquidity",
    );

    market.config.swap_fee_bps = BPS_DENOMINATOR;
    assert_error(
        market.quote_leverage_swap(MarketAsset::Base, 1),
        "InsufficientOutputAmount",
    );
    assert_error(
        market.quote_leverage_swap_exact_output(MarketAsset::Base, 1),
        "InsufficientOutputAmount",
    );
}

#[test]
fn leverage_multiplier_math_enforces_boundaries_and_rounding() {
    assert_eq!(
        leverage_target_collateral_from_margin(10_000, 10_001).unwrap(),
        10_001
    );
    assert_eq!(leverage_debt_from_margin(10_000, 10_001).unwrap(), 1);
    assert_eq!(
        leverage_target_collateral_from_margin(10_000, LEVERAGE_MAX_MULTIPLIER_BPS).unwrap(),
        10_000 * LEVERAGE_MAX_MULTIPLIER_BPS / BPS_DENOMINATOR as u64
    );

    assert_error(
        leverage_target_collateral_from_margin(0, 20_000),
        "AmountZero",
    );
    assert_error(
        leverage_target_collateral_from_margin(1_000, 10_000),
        "InvalidArgument",
    );
    assert_error(
        leverage_target_collateral_from_margin(1_000, LEVERAGE_MAX_MULTIPLIER_BPS + 1),
        "LeverageMultiplierTooHigh",
    );
    assert_error(
        leverage_target_collateral_from_margin(1, 10_001),
        "AmountZero",
    );
    assert_error(
        leverage_target_collateral_from_margin(u64::MAX, LEVERAGE_MAX_MULTIPLIER_BPS),
        "MarketMathOverflow",
    );
}

#[test]
fn rejected_debt_margin_open_is_side_effect_free() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut position = empty_position();
    let quote = market
        .quote_leverage_swap(MarketAsset::Base, 2_000)
        .unwrap();
    let base_live_before = market.base_side.reserves.live_reserve;
    let base_cash_before = market.base_side.reserves.cash_reserve;
    let quote_live_before = market.quote_side.reserves.live_reserve;
    let quote_cash_before = market.quote_side.reserves.cash_reserve;

    assert_error(
        market.open_leverage(
            &mut position,
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            MarketAsset::Base,
            1_000,
            20_000,
            quote.amount_out + 1,
            0,
            0,
            255,
            0,
            0,
            ProtocolAuctionSplit::default(),
        ),
        "SlippageExceeded",
    );

    assert!(!position.is_initialized());
    assert_eq!(market.debt.isolated_base_shares, 0);
    assert_eq!(market.base_side.reserves.live_reserve, base_live_before);
    assert_eq!(market.base_side.reserves.cash_reserve, base_cash_before);
    assert_eq!(market.quote_side.reserves.live_reserve, quote_live_before);
    assert_eq!(market.quote_side.reserves.cash_reserve, quote_cash_before);
}

#[test]
fn rejected_collateral_margin_open_is_side_effect_free() {
    let mut market = test_market(1_000_000, 1_000_000);
    let quote = market
        .quote_leverage_swap_exact_output(MarketAsset::Base, 1_000)
        .unwrap();
    let base_live_before = market.base_side.reserves.live_reserve;
    let base_cash_before = market.base_side.reserves.cash_reserve;

    let mut undercredited_position = empty_position();
    assert_error(
        market.open_collateral_margin_leverage(
            &mut undercredited_position,
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            MarketAsset::Base,
            1_000,
            20_000,
            1_000,
            999,
            quote.amount_in,
            0,
            0,
            255,
            0,
            0,
            ProtocolAuctionSplit::default(),
        ),
        "InsufficientOutputAmount",
    );

    let mut slipped_position = empty_position();
    assert_error(
        market.open_collateral_margin_leverage(
            &mut slipped_position,
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            MarketAsset::Base,
            1_000,
            20_000,
            1_000,
            1_000,
            quote.amount_in - 1,
            0,
            0,
            255,
            0,
            0,
            ProtocolAuctionSplit::default(),
        ),
        "SlippageExceeded",
    );

    assert!(!undercredited_position.is_initialized());
    assert!(!slipped_position.is_initialized());
    assert_eq!(market.debt.isolated_base_shares, 0);
    assert_eq!(market.base_side.reserves.live_reserve, base_live_before);
    assert_eq!(market.base_side.reserves.cash_reserve, base_cash_before);
}

#[test]
fn open_leverage_tracks_isolated_debt_and_cash() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut position = empty_position();
    let quote = market
        .quote_leverage_swap(MarketAsset::Base, 2_000)
        .unwrap();

    let receipt = market
        .open_leverage(
            &mut position,
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            MarketAsset::Base,
            1_000,
            20_000,
            quote.amount_out,
            0,
            0,
            255,
            0,
            0,
            ProtocolAuctionSplit::default(),
        )
        .unwrap();

    assert_eq!(receipt.debt_amount, 1_000);
    assert_eq!(position.debt_shares, 1_000);
    assert_eq!(market.debt.isolated_base_shares, 1_000);
    assert_eq!(market.debt.fixed_base_shares, 0);
    assert_eq!(
        market.base_side.reserves.live_reserve,
        1_000_000 + quote.amount_in_after_fee
    );
    assert_eq!(
        market.base_side.reserves.cash_reserve,
        1_000_000 - 1_000 + quote.amount_in_after_fee
    );
    assert_eq!(
        market.quote_side.reserves.live_reserve,
        1_000_000 - quote.amount_out
    );
    assert_eq!(market.quote_side.reserves.cash_reserve, 1_000_000 - quote.amount_out);
    market
        .assert_virtual_reserve_invariant(MarketAsset::Base)
        .unwrap();
    market
        .assert_virtual_reserve_invariant(MarketAsset::Quote)
        .unwrap();
}

#[test]
fn close_leverage_clears_isolated_debt_and_residual_cash() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut position = empty_position();
    let open_quote = market
        .quote_leverage_swap(MarketAsset::Base, 2_000)
        .unwrap();
    market
        .open_leverage(
            &mut position,
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            MarketAsset::Base,
            1_000,
            20_000,
            open_quote.amount_out,
            0,
            0,
            255,
            0,
            0,
            ProtocolAuctionSplit::default(),
        )
        .unwrap();
    let base_cash_before_close = market.base_side.reserves.cash_reserve;
    let close_quote = market
        .quote_leverage_swap(MarketAsset::Quote, position.collateral_amount)
        .unwrap();
    let collateral_swap_input = position.collateral_amount;

    let receipt = market
        .close_leverage(
            &mut position,
            collateral_swap_input,
            0,
            0,
            0,
            ProtocolAuctionSplit::default(),
        )
        .unwrap();

    assert_eq!(receipt.debt_repaid, 1_000);
    assert_eq!(market.debt.isolated_base_shares, 0);
    assert_eq!(market.debt.isolated_base_principal, 0);
    assert_eq!(position.debt_shares, 0);
    assert_eq!(position.collateral_amount, 0);
    assert_eq!(
        market.base_side.reserves.cash_reserve,
        base_cash_before_close - receipt.residual
    );
    assert_eq!(receipt.closeout_value, close_quote.amount_out);
    market
        .assert_virtual_reserve_invariant(MarketAsset::Base)
        .unwrap();
    market
        .assert_virtual_reserve_invariant(MarketAsset::Quote)
        .unwrap();
}

#[test]
fn exact_input_unwinds_account_for_collateral_transfer_fees() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut decrease_position =
        seeded_position(&mut market, MarketAsset::Base, 1_000, 3_000);
    let expected_decrease_swap = market
        .quote_leverage_swap(MarketAsset::Quote, 99)
        .unwrap();
    let decrease = market
        .decrease_leverage(
            &mut decrease_position,
            100,
            99,
            0,
            0,
            0,
            ProtocolAuctionSplit::default(),
        )
        .unwrap();
    assert_eq!(
        decrease.debt_delta,
        -i64::try_from(expected_decrease_swap.amount_out).unwrap()
    );
    assert_eq!(decrease_position.collateral_amount, 2_900);

    let mut close_position = seeded_position(&mut market, MarketAsset::Base, 1_000, 3_000);
    let close = market
        .close_leverage(
            &mut close_position,
            2_990,
            0,
            0,
            0,
            ProtocolAuctionSplit::default(),
        )
        .unwrap();
    assert_eq!(close.collateral_sold, 3_000);
    assert_eq!(close.swap.amount_in, 2_990);

    let mut liquidation_position =
        seeded_position(&mut market, MarketAsset::Base, 1_000, 1_010);
    let liquidation = market
        .liquidate_leverage(
            &mut liquidation_position,
            1_000,
            0,
            0,
            ProtocolAuctionSplit::default(),
        )
        .unwrap();
    assert_eq!(liquidation.collateral_sold, 1_010);
    assert_eq!(liquidation.swap.amount_in, 1_000);

    let mut invalid_position = seeded_position(&mut market, MarketAsset::Base, 1_000, 3_000);
    assert_error(
        market.close_leverage(
            &mut invalid_position,
            3_001,
            0,
            0,
            0,
            ProtocolAuctionSplit::default(),
        ),
        "UnexpectedTokenTransferAmount",
    );
}

#[test]
fn collateral_margin_open_targets_collateral_and_preserves_health_invariants() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut position = empty_position();
    let margin_credit = 1_000;
    let target_collateral = 2_000;
    let supplemental_target = target_collateral - margin_credit;
    let quote = market
        .quote_leverage_swap_exact_output(MarketAsset::Base, supplemental_target)
        .unwrap();

    let receipt = market
        .open_collateral_margin_leverage(
            &mut position,
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            MarketAsset::Base,
            margin_credit,
            20_000,
            supplemental_target,
            supplemental_target,
            quote.amount_in,
            0,
            0,
            255,
            0,
            0,
            ProtocolAuctionSplit::default(),
        )
        .unwrap();

    assert_eq!(position.margin_mode().unwrap(), LeverageMarginMode::Collateral);
    assert_eq!(position.margin_asset().unwrap(), MarketAsset::Quote);
    assert_eq!(position.margin_amount, margin_credit);
    assert_eq!(position.open_notional, target_collateral);
    assert_eq!(position.collateral_amount, target_collateral);
    assert_eq!(position.debt_amount(&market.debt).unwrap(), quote.amount_in);
    assert_eq!(receipt.debt_amount, quote.amount_in);
    assert_eq!(receipt.collateral_amount, target_collateral);
    assert!(receipt.closeout_value > receipt.debt_amount);
    assert!(receipt.equity > 0);
    assert_eq!(market.debt.isolated_base_shares, position.debt_shares);
    market.assert_market_invariants().unwrap();
}

#[test]
fn collateral_margin_close_repays_interest_with_exact_output_and_returns_collateral() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut position = empty_position();
    let opening_quote = market
        .quote_leverage_swap_exact_output(MarketAsset::Base, 1_000)
        .unwrap();
    market
        .open_collateral_margin_leverage(
            &mut position,
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            MarketAsset::Base,
            1_000,
            20_000,
            1_000,
            1_000,
            opening_quote.amount_in,
            0,
            0,
            255,
            0,
            0,
            ProtocolAuctionSplit::default(),
        )
        .unwrap();
    let principal = position.debt_principal as u64;
    market.debt.base_borrow_index_nad = (NAD as u128) * 11 / 10;
    let current_debt = position.debt_amount(&market.debt).unwrap();
    market.base_side.reserves.live_reserve += current_debt - principal;
    let close_quote = market
        .quote_leverage_swap_exact_output(MarketAsset::Quote, current_debt)
        .unwrap();

    let receipt = market
        .close_collateral_margin_leverage(
            &mut position,
            close_quote.amount_in,
            close_quote.amount_in,
            0,
            0,
            ProtocolAuctionSplit::default(),
        )
        .unwrap();

    assert_eq!(receipt.debt_repaid, current_debt);
    assert_eq!(receipt.interest_paid, current_debt - principal);
    assert_eq!(receipt.swap.amount_out, current_debt);
    assert_eq!(receipt.collateral_sold, close_quote.amount_in);
    assert!(receipt.residual > 0);
    assert_eq!(position.debt_shares, 0);
    assert_eq!(position.collateral_amount, 0);
    assert_eq!(market.debt.isolated_base_shares, 0);
    market.assert_market_invariants().unwrap();
}

#[test]
fn leverage_round_trips_are_symmetric_for_both_debt_assets() {
    for debt_asset in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = test_market(2_000_000, 2_000_000);
        let mut debt_margin_position = empty_position();
        let open_quote = market.quote_leverage_swap(debt_asset, 2_000).unwrap();
        market
            .open_leverage(
                &mut debt_margin_position,
                Pubkey::new_unique(),
                Pubkey::new_unique(),
                Pubkey::new_unique(),
                debt_asset,
                1_000,
                20_000,
                open_quote.amount_out,
                0,
                0,
                255,
                0,
                0,
                ProtocolAuctionSplit::default(),
            )
            .unwrap();
        let collateral_swap_input = debt_margin_position.collateral_amount;
        let debt_close = market
            .close_leverage(
                &mut debt_margin_position,
                collateral_swap_input,
                0,
                0,
                0,
                ProtocolAuctionSplit::default(),
            )
            .unwrap();
        assert_eq!(debt_close.debt_repaid, 1_000);
        assert_eq!(debt_margin_position.debt_shares, 0);

        let mut collateral_margin_position = empty_position();
        let collateral_quote = market
            .quote_leverage_swap_exact_output(debt_asset, 1_000)
            .unwrap();
        market
            .open_collateral_margin_leverage(
                &mut collateral_margin_position,
                Pubkey::new_unique(),
                Pubkey::new_unique(),
                Pubkey::new_unique(),
                debt_asset,
                1_000,
                20_000,
                1_000,
                1_000,
                collateral_quote.amount_in,
                0,
                0,
                255,
                0,
                0,
                ProtocolAuctionSplit::default(),
            )
            .unwrap();
        let current_debt = collateral_margin_position
            .debt_amount(&market.debt)
            .unwrap();
        let collateral_close_quote = market
            .quote_leverage_swap_exact_output(debt_asset.opposite(), current_debt)
            .unwrap();
        let collateral_close = market
            .close_collateral_margin_leverage(
                &mut collateral_margin_position,
                collateral_close_quote.amount_in,
                collateral_close_quote.amount_in,
                0,
                0,
                ProtocolAuctionSplit::default(),
            )
            .unwrap();
        assert_eq!(collateral_close.debt_repaid, current_debt);
        assert!(collateral_close.residual > 0);
        assert_eq!(collateral_margin_position.debt_shares, 0);
        market.assert_market_invariants().unwrap();
    }
}

#[test]
fn close_paths_reject_the_wrong_margin_mode_without_mutation() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut position = seeded_position(&mut market, MarketAsset::Base, 1_000, 3_000);
    let shares_before = position.debt_shares;
    let collateral_before = position.collateral_amount;

    assert_error(
        market.close_collateral_margin_leverage(
            &mut position,
            1_000,
            1_000,
            0,
            0,
            ProtocolAuctionSplit::default(),
        ),
        "InvalidLeverageMarginMode",
    );
    assert_eq!(position.debt_shares, shares_before);
    assert_eq!(position.collateral_amount, collateral_before);

    position.margin_mode = LeverageMarginMode::Collateral.code();
    let collateral_swap_input = position.collateral_amount;
    assert_error(
        market.close_leverage(
            &mut position,
            collateral_swap_input,
            0,
            0,
            0,
            ProtocolAuctionSplit::default(),
        ),
        "InvalidLeverageMarginMode",
    );
    assert_eq!(position.debt_shares, shares_before);
    assert_eq!(position.collateral_amount, collateral_before);
}

#[test]
fn close_slippage_rejections_preserve_debt_and_collateral() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut debt_position = seeded_position(&mut market, MarketAsset::Base, 1_000, 3_000);
    let debt_close_quote = market
        .quote_leverage_swap(MarketAsset::Quote, debt_position.collateral_amount)
        .unwrap();
    let minimum_too_high = debt_close_quote.amount_out - 1_000 + 1;
    let collateral_swap_input = debt_position.collateral_amount;
    assert_error(
        market.close_leverage(
            &mut debt_position,
            collateral_swap_input,
            minimum_too_high,
            0,
            0,
            ProtocolAuctionSplit::default(),
        ),
        "SlippageExceeded",
    );
    assert_eq!(debt_position.debt_shares, 1_000);
    assert_eq!(debt_position.collateral_amount, 3_000);

    let mut collateral_position = seeded_position(&mut market, MarketAsset::Quote, 1_000, 3_000);
    collateral_position.margin_mode = LeverageMarginMode::Collateral.code();
    let collateral_close_quote = market
        .quote_leverage_swap_exact_output(MarketAsset::Base, 1_000)
        .unwrap();
    assert_error(
        market.close_collateral_margin_leverage(
            &mut collateral_position,
            collateral_close_quote.amount_in,
            collateral_close_quote.amount_in - 1,
            0,
            0,
            ProtocolAuctionSplit::default(),
        ),
        "SlippageExceeded",
    );
    assert_eq!(collateral_position.debt_shares, 1_000);
    assert_eq!(collateral_position.collateral_amount, 3_000);
}

#[test]
fn collateral_margin_positions_support_existing_debt_and_leverage_updates() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut position = empty_position();
    let opening_quote = market
        .quote_leverage_swap_exact_output(MarketAsset::Base, 1_000)
        .unwrap();
    market
        .open_collateral_margin_leverage(
            &mut position,
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            MarketAsset::Base,
            1_000,
            20_000,
            1_000,
            1_000,
            opening_quote.amount_in,
            0,
            0,
            255,
            0,
            0,
            ProtocolAuctionSplit::default(),
        )
        .unwrap();

    let increase_quote = market.quote_leverage_swap(MarketAsset::Base, 100).unwrap();
    market
        .increase_leverage(
            &mut position,
            100,
            increase_quote.amount_out,
            0,
            0,
            ProtocolAuctionSplit::default(),
        )
        .unwrap();
    market
        .decrease_leverage(&mut position, 50, 50, 0, 0, 0, ProtocolAuctionSplit::default())
        .unwrap();
    market.add_leverage_margin(&mut position, 10).unwrap();
    market.remove_leverage_margin(&mut position, 10).unwrap();

    assert_eq!(position.margin_mode().unwrap(), LeverageMarginMode::Collateral);
    assert!(position.debt_amount(&market.debt).unwrap() > 0);
    assert!(position.collateral_amount > 0);
    market.assert_market_invariants().unwrap();
}

#[test]
fn collateral_deposit_and_withdrawal_enforce_post_withdraw_health() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut position = seeded_position(&mut market, MarketAsset::Base, 1_000, 3_000);

    let deposit = market
        .deposit_leverage_collateral(&mut position, 500)
        .unwrap();
    assert_eq!(deposit.collateral_delta, 500);
    assert_eq!(position.collateral_amount, 3_500);

    let withdrawal = market
        .withdraw_leverage_collateral(&mut position, 500, 3_000)
        .unwrap();
    assert_eq!(withdrawal.collateral_delta, -500);
    assert_eq!(position.collateral_amount, 3_000);

    assert!(market
        .withdraw_leverage_collateral(&mut position, 2_000, 1_000)
        .is_err());
    assert_eq!(position.collateral_amount, 3_000);
}

#[test]
fn leverage_updates_reject_zero_overdraw_and_unhealthy_changes() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut position = seeded_position(&mut market, MarketAsset::Base, 1_000, 3_000);

    assert_error(
        market.deposit_leverage_collateral(&mut position, 0),
        "AmountZero",
    );
    assert_error(
        market.withdraw_leverage_collateral(&mut position, 0, 3_000),
        "AmountZero",
    );
    assert_error(
        market.withdraw_leverage_collateral(&mut position, 3_000, 0),
        "InsufficientAmount",
    );
    assert_error(
        market.add_leverage_margin(&mut position, 1_000),
        "InsufficientDebt",
    );
    assert_error(
        market.remove_leverage_margin(&mut position, 10_000),
        "LeverageInitialMarginTooLow",
    );
    assert_error(
        market.increase_leverage(
            &mut position,
            0,
            1,
            0,
            0,
            ProtocolAuctionSplit::default(),
        ),
        "AmountZero",
    );
    assert_error(
        market.decrease_leverage(
            &mut position,
            3_000,
            3_000,
            0,
            0,
            0,
            ProtocolAuctionSplit::default(),
        ),
        "InsufficientAmount",
    );

    assert_eq!(position.debt_shares, 1_000);
    assert_eq!(position.collateral_amount, 3_000);

    position.collateral_amount = u64::MAX;
    assert_error(
        market.deposit_leverage_collateral(&mut position, 1),
        "MarketMathOverflow",
    );
    assert_eq!(position.collateral_amount, u64::MAX);
}

#[test]
fn leverage_swap_fees_are_fully_backed_and_recorded() {
    let mut market = test_market(1_000_000, 1_000_000);
    market.config.swap_fee_bps = 30;
    let mut position = empty_position();
    let quote = market
        .quote_leverage_swap(MarketAsset::Base, 2_000)
        .unwrap();

    let receipt = market
        .open_leverage(
            &mut position,
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            MarketAsset::Base,
            1_000,
            20_000,
            quote.amount_out,
            0,
            0,
            255,
            2_000,
            3_000,
            ProtocolAuctionSplit::default(),
        )
        .unwrap();

    assert!(receipt.swap.fee_credit > 0);
    assert_eq!(
        receipt.fees.swap_fee_vault_balance,
        receipt.swap.fee_credit
    );
    assert_eq!(
        market.base_side.fees.swap_fee_vault_balance,
        receipt.swap.fee_credit
    );
    market.base_side.fees.assert_backed().unwrap();
    market.assert_market_invariants().unwrap();
}

#[test]
fn add_margin_uses_actual_rounded_isolated_debt_reduction_for_reserves() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut position = seeded_position(&mut market, MarketAsset::Base, 100, 10_000);
    market.debt.base_borrow_index_nad = (NAD as u128) * 3 / 2;
    market.base_side.reserves.live_reserve += 50;
    market
        .assert_virtual_reserve_invariant(MarketAsset::Base)
        .unwrap();
    let live_before = market.base_side.reserves.live_reserve;
    let cash_before = market.base_side.reserves.cash_reserve;

    let receipt = market
        .add_leverage_margin(&mut position, 2)
        .unwrap();

    assert_eq!(receipt.debt_delta, -3);
    assert_eq!(receipt.debt_amount, 147);
    assert_eq!(receipt.interest_paid, 1);
    assert_eq!(position.debt_shares, 98);
    assert_eq!(position.debt_principal, 98);
    assert_eq!(market.debt.isolated_base_shares, 98);
    assert_eq!(market.debt.isolated_base_principal, 98);
    assert_eq!(market.base_side.reserves.live_reserve, live_before - 2);
    assert_eq!(market.base_side.reserves.cash_reserve, cash_before + 1);
    market
        .assert_virtual_reserve_invariant(MarketAsset::Base)
        .unwrap();
    market
        .assert_virtual_reserve_invariant(MarketAsset::Quote)
        .unwrap();
}

#[test]
fn solvent_liquidation_closes_position_and_pays_residual_incentive() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut position = seeded_position(&mut market, MarketAsset::Base, 1_000, 1_010);

    let receipt = market
        .liquidate_leverage(&mut position, 1_010, 0, 0, ProtocolAuctionSplit::default())
        .unwrap();

    assert_eq!(market.debt.isolated_base_shares, 0);
    assert_eq!(position.debt_shares, 0);
    assert_eq!(position.collateral_amount, 0);
    assert_eq!(receipt.debt_repaid, 1_000);
    assert_eq!(receipt.principal_written_off, 0);
    assert!(receipt.liquidator_amount > 0);
    market
        .assert_virtual_reserve_invariant(MarketAsset::Base)
        .unwrap();
    market
        .assert_virtual_reserve_invariant(MarketAsset::Quote)
        .unwrap();
}

#[test]
fn healthy_position_cannot_be_liquidated_and_remains_unchanged() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut position = seeded_position(&mut market, MarketAsset::Base, 1_000, 3_000);
    let base_live_before = market.base_side.reserves.live_reserve;
    let quote_live_before = market.quote_side.reserves.live_reserve;

    assert_error(
        market.liquidate_leverage(
            &mut position,
            3_000,
            0,
            0,
            ProtocolAuctionSplit::default(),
        ),
        "LeveragePositionNotLiquidatable",
    );

    assert_eq!(position.debt_shares, 1_000);
    assert_eq!(position.collateral_amount, 3_000);
    assert_eq!(market.base_side.reserves.live_reserve, base_live_before);
    assert_eq!(market.quote_side.reserves.live_reserve, quote_live_before);
}

#[test]
fn collateral_margin_liquidation_deliberately_uses_debt_settled_full_unwind() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut position = seeded_position(&mut market, MarketAsset::Base, 1_000, 1_010);
    position.margin_mode = LeverageMarginMode::Collateral.code();
    position.margin_amount = 1_010;
    let expected_swap = market
        .quote_leverage_swap(MarketAsset::Quote, position.collateral_amount)
        .unwrap();

    let receipt = market
        .liquidate_leverage(&mut position, 1_010, 0, 0, ProtocolAuctionSplit::default())
        .unwrap();

    let debt_settled_residual = receipt
        .liquidator_amount
        .checked_add(receipt.owner_residual)
        .unwrap();
    assert_eq!(receipt.swap, expected_swap);
    assert_eq!(receipt.collateral_sold, 1_010);
    assert_eq!(receipt.closeout_value, expected_swap.amount_out);
    assert_eq!(
        debt_settled_residual,
        expected_swap.amount_out.saturating_sub(receipt.debt_repaid)
    );
    assert_eq!(position.margin_mode, LeverageMarginMode::Collateral.code());
    assert_eq!(position.debt_shares, 0);
    assert_eq!(position.collateral_amount, 0);
}

#[test]
fn insolvent_liquidation_socializes_unrepaid_principal() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut position = seeded_position(&mut market, MarketAsset::Base, 1_000, 500);

    let receipt = market
        .liquidate_leverage(&mut position, 500, 0, 0, ProtocolAuctionSplit::default())
        .unwrap();

    assert_eq!(market.debt.isolated_base_shares, 0);
    assert_eq!(position.debt_shares, 0);
    assert_eq!(position.collateral_amount, 0);
    assert!(receipt.debt_repaid < 1_000);
    assert!(receipt.principal_written_off > 0);
    assert_eq!(receipt.liquidator_amount, 0);
    market
        .assert_virtual_reserve_invariant(MarketAsset::Base)
        .unwrap();
    market
        .assert_virtual_reserve_invariant(MarketAsset::Quote)
        .unwrap();
}
