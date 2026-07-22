use super::*;
use crate::{
    constants::{INTEREST_INITIAL_RATE_AT_TARGET_NAD, MARKET_VERSION, NAD},
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
        version: MARKET_VERSION,
        ylp_mint: Pubkey::new_unique(),
        operator: Pubkey::new_unique(),
        manager: Pubkey::new_unique(),
        base_side,
        quote_side,
        config: MarketConfig {
            swap_fee_bps: 0,
            max_daily_borrow_bps: 10_000,
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

fn empty_position() -> LeveragePosition {
    LeveragePosition {
        owner: Pubkey::default(),
        market: Pubkey::default(),
        position_id: Pubkey::default(),
        referral_profile: Pubkey::default(),
        referral_interest_share_bps: 0,
        debt_asset: 0,
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
        Pubkey::default(),
        0,
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
            Pubkey::default(),
            0,
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
fn referred_leverage_records_exact_debt_and_binds_profile() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut position = empty_position();
    let referral_profile = Pubkey::new_unique();
    let open_quote = market
        .quote_leverage_swap(MarketAsset::Base, 2_000)
        .unwrap();

    let open = market
        .open_leverage(
            &mut position,
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            referral_profile,
            2_500,
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

    assert_eq!(open.borrowed_amount, 1_000);
    assert_eq!(open.debt_amount, 1_000);
    assert_eq!(open.swap.amount_in, 2_000);
    assert_eq!(position.referral_profile, referral_profile);
    assert_eq!(position.referral_interest_share_bps, 2_500);
    assert_eq!(position.debt_principal, 1_000);
    assert_eq!(market.debt.isolated_base_principal, 1_000);
    assert_eq!(market.base_side.daily_limits.borrowed_bucket, 1_000);

    let increase_quote = market.quote_leverage_swap(MarketAsset::Base, 100).unwrap();
    let increase = market
        .increase_leverage(
            &mut position,
            100,
            increase_quote.amount_out,
            0,
            0,
            ProtocolAuctionSplit::default(),
        )
        .unwrap();
    assert_eq!(increase.borrowed_amount, 100);
    assert_eq!(increase.debt_delta, 100);
    assert_eq!(position.referral_profile, referral_profile);
    assert_eq!(position.referral_interest_share_bps, 2_500);
    assert_eq!(market.base_side.daily_limits.borrowed_bucket, 1_100);
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
            Pubkey::default(),
            0,
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

    let receipt = market
        .close_leverage(&mut position, 0, 0, 0, ProtocolAuctionSplit::default())
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
        .liquidate_leverage(&mut position, 0, 0, ProtocolAuctionSplit::default())
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
fn insolvent_liquidation_socializes_unrepaid_principal() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut position = seeded_position(&mut market, MarketAsset::Base, 1_000, 500);

    let receipt = market
        .liquidate_leverage(&mut position, 0, 0, ProtocolAuctionSplit::default())
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
