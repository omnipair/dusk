use super::*;
    use crate::state::{PendingAuthorityChange, PendingConfigChange};
    use crate::{
        constants::{BPS_DENOMINATOR, MARKET_VERSION},
        math::calculate_raw_amount_out,
        state::{Insurance, MarketConfig, MarketHealth, MarketSide, Risk},
    };

    fn valid_config() -> MarketConfig {
        MarketConfig {
            swap_fee_bps: 30,
            manager_fee_bps: 0,
            protocol_fee_bps: 0,
            target_hlp_leverage_bps: BPS_DENOMINATOR * 2,
            settlement_divergence_bps: 500,
            emergency_exit_haircut_bps: 250,
            ema_half_life_ms: 60_000,
            directional_ema_half_life_ms: 60_000,
            k_ema_half_life_ms: 60_000,
            max_daily_borrow_bps: 2_000,
            max_daily_withdraw_bps: 2_000,
            spot_ema_divergence_bps: 1_000,
            k_ema_drawdown_bps: 1_000,
            recognized_collateral_cap_bps: 15_000,
            market_health_min_bps: 11_000,
            hedged_lp_enabled: true,
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
        base_hlp_vault.initialize(MarketAsset::Base, Pubkey::new_unique(), 0);
        let mut quote_hlp_vault = HlpVault::default();
        quote_hlp_vault.initialize(MarketAsset::Quote, Pubkey::new_unique(), 0);

        Market {
            version: MARKET_VERSION,
            base_mint,
            quote_mint,
            ylp_mint: Pubkey::new_unique(),
            operator: Pubkey::new_unique(),
            manager: Pubkey::new_unique(),
            base_side,
            quote_side,
            config: valid_config(),
            debt: Debt {
                base_borrow_index_nad: NAD as u128,
                quote_borrow_index_nad: NAD as u128,
                ..Debt::default()
            },
            base_hlp_vault,
            quote_hlp_vault,
            risk: Risk::default(),
            health: MarketHealth::default(),
            insurance: Insurance::default(),
            pending_config: PendingConfigChange::default(),
            pending_operator: PendingAuthorityChange::default(),
            pending_manager: PendingAuthorityChange::default(),
            params_hash: [7; 32],
            last_update_slot: 0,
            reduce_only: false,
            bump: 255,
        }
    }

    #[test]
    fn open_hlp_keeps_leverage_debt_on_aggregate_vault() {
        let mut market = seeded_market();

        let receipt = DepositSingleSided::new(MarketAsset::Base, 100, 1)
            .apply(&mut market)
            .unwrap();

        assert_eq!(receipt.borrowed_amount, 200);
        assert_eq!(receipt.ylp_amount, 100);
        assert_eq!(receipt.hlp_amount, 100);
        assert_eq!(market.debt.fixed_quote_shares, 0);
        assert!(market.base_hlp_vault.debt_shares > 0);
        assert_eq!(market.base_hlp_vault.debt_principal, 200);
        assert_eq!(market.base_hlp_vault.ylp_shares, 100);
        assert_eq!(market.base_hlp_vault.base_hlp_live_reserve, 0);
        assert_eq!(market.base_hlp_vault.quote_hlp_live_reserve, 200);
        assert_eq!(market.base_side.reserves.cash_reserve, 1_100);
        assert_eq!(market.quote_side.reserves.cash_reserve, 2_000);
        assert_eq!(market.base_hlp_vault.last_nav_nad, 100 * NAD as u128);
        market
            .assert_virtual_reserve_invariant(MarketAsset::Base)
            .unwrap();
        market
            .assert_virtual_reserve_invariant(MarketAsset::Quote)
            .unwrap();
    }

    #[test]
    fn open_hlp_requires_borrowed_side_cash_headroom() {
        let mut market = seeded_market();
        market.quote_side.reserves.cash_reserve = 199;

        let err = DepositSingleSided::new(MarketAsset::Base, 100, 1)
            .apply(&mut market)
            .unwrap_err();

        assert_eq!(err, error!(ErrorCode::InsufficientBorrowHeadroom));
    }

    #[test]
    fn repeated_open_hlp_mints_against_delta_nav() {
        let mut market = seeded_market();

        let first = DepositSingleSided::new(MarketAsset::Base, 100, 1)
            .apply(&mut market)
            .unwrap();
        let second = DepositSingleSided::new(MarketAsset::Base, 120, 1)
            .apply(&mut market)
            .unwrap();

        assert_eq!(first.hlp_amount, 100);
        assert_eq!(second.hlp_amount, 120);
        assert_eq!(market.base_hlp_vault.hlp_supply, 220);
        assert_eq!(market.base_hlp_vault.ylp_shares, 220);
        assert_eq!(market.base_hlp_vault.last_nav_nad, 220 * NAD as u128);
    }

    #[test]
    fn h_lp_nav_values_collateral_and_debt_in_target_numeraire() {
        let mut market = seeded_market();

        DepositSingleSided::new(MarketAsset::Base, 100, 1)
            .apply(&mut market)
            .unwrap();

        assert_eq!(
            hlp_collateral_value_nad(&market, MarketAsset::Base, &market.base_hlp_vault).unwrap(),
            200 * NAD as u128
        );
        assert_eq!(
            hlp_debt_value_nad(&market, MarketAsset::Base).unwrap(),
            100 * NAD as u128
        );
        assert_eq!(
            hlp_nav_nad(&market, MarketAsset::Base).unwrap(),
            100 * NAD as u128
        );
    }

    #[test]
    fn accrued_interest_grows_hlp_debt_and_reduces_nav() {
        let mut market = seeded_market();
        DepositSingleSided::new(MarketAsset::Base, 100, 1)
            .apply(&mut market)
            .unwrap();
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
        market
            .assert_virtual_reserve_invariant(MarketAsset::Quote)
            .unwrap();
    }

    #[test]
    fn close_hlp_burns_vault_ylp_and_repays_vault_debt() {
        let mut market = seeded_market();
        let deposit_receipt = DepositSingleSided::new(MarketAsset::Base, 100, 1)
            .apply(&mut market)
            .unwrap();

        let withdraw_receipt = WithdrawSingleSided::new(MarketAsset::Base, deposit_receipt.hlp_amount)
            .apply(&mut market)
            .unwrap();

        assert_eq!(withdraw_receipt.target_amount_out, 100);
        assert_eq!(withdraw_receipt.debt_repaid, 200);
        assert_eq!(market.base_hlp_vault.hlp_supply, 0);
        assert_eq!(market.base_hlp_vault.debt_shares, 0);
        assert_eq!(market.base_hlp_vault.debt_principal, 0);
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
        market
            .assert_virtual_reserve_invariant(MarketAsset::Base)
            .unwrap();
        market
            .assert_virtual_reserve_invariant(MarketAsset::Quote)
            .unwrap();
    }

    #[test]
    fn close_hlp_realizes_interest_from_borrowed_side_cash() {
        let mut market = seeded_market();
        let deposit_receipt = DepositSingleSided::new(MarketAsset::Base, 100, 1)
            .apply(&mut market)
            .unwrap();
        market.debt.quote_borrow_index_nad = (NAD as u128) * 110 / 100;

        let withdraw_receipt = WithdrawSingleSided::new(MarketAsset::Base, deposit_receipt.hlp_amount)
            .apply(&mut market)
            .unwrap();

        assert_eq!(withdraw_receipt.debt_repaid, 220);
        assert_eq!(withdraw_receipt.interest_paid, 20);
        assert_eq!(market.base_hlp_vault.debt_principal, 0);
        assert_eq!(market.base_hlp_vault.quote_hlp_live_reserve, 0);
        assert_eq!(market.quote_side.reserves.live_reserve, 1_980);
        assert_eq!(market.quote_side.reserves.cash_reserve, 1_980);
        market
            .assert_virtual_reserve_invariant(MarketAsset::Quote)
            .unwrap();
    }

    #[test]
    fn close_hlp_converts_borrowed_side_surplus_into_target_out() {
        let mut market = seeded_market();
        let deposit_receipt = DepositSingleSided::new(MarketAsset::Base, 100, 1)
            .apply(&mut market)
            .unwrap();
        market.quote_side.reserves.live_reserve = 2_300;
        market.quote_side.reserves.cash_reserve = 2_100;
        market
            .assert_virtual_reserve_invariant(MarketAsset::Quote)
            .unwrap();

        let withdraw_receipt = WithdrawSingleSided::new(MarketAsset::Base, deposit_receipt.hlp_amount)
            .apply(&mut market)
            .unwrap();

        assert!(withdraw_receipt.target_amount_out > 100);
        assert_eq!(withdraw_receipt.debt_repaid, 200);
        assert_eq!(market.base_hlp_vault.hlp_supply, 0);
        assert_eq!(market.quote_side.reserves.live_reserve, 2_100);
        assert_eq!(market.quote_side.reserves.cash_reserve, 2_100);
        market
            .assert_virtual_reserve_invariant(MarketAsset::Quote)
            .unwrap();
    }

    #[test]
    fn close_hlp_uses_target_side_value_for_borrowed_side_shortfall() {
        let mut market = seeded_market();
        let deposit_receipt = DepositSingleSided::new(MarketAsset::Base, 100, 1)
            .apply(&mut market)
            .unwrap();
        market.quote_side.reserves.live_reserve = 2_110;
        market.quote_side.reserves.cash_reserve = 1_910;
        market
            .assert_virtual_reserve_invariant(MarketAsset::Quote)
            .unwrap();

        let withdraw_receipt = WithdrawSingleSided::new(MarketAsset::Base, deposit_receipt.hlp_amount)
            .apply(&mut market)
            .unwrap();

        assert!(withdraw_receipt.target_amount_out < 100);
        assert_eq!(withdraw_receipt.debt_repaid, 200);
        assert_eq!(market.base_hlp_vault.hlp_supply, 0);
        assert_eq!(market.quote_side.reserves.live_reserve, 1_910);
        assert_eq!(market.quote_side.reserves.cash_reserve, 1_910);
        market
            .assert_virtual_reserve_invariant(MarketAsset::Quote)
            .unwrap();
    }

    #[test]
    fn open_hlp_rejects_settlement_price_divergence() {
        let mut market = seeded_market();
        DepositSingleSided::new(MarketAsset::Base, 100, 1)
            .apply(&mut market)
            .unwrap();

        market.quote_side.reserves.live_reserve = 4_000;
        market.quote_side.reserves.cash_reserve = 3_800;
        market
            .assert_virtual_reserve_invariant(MarketAsset::Quote)
            .unwrap();
        let err = DepositSingleSided::new(MarketAsset::Base, 100, 1)
            .apply(&mut market)
            .unwrap_err();

        assert_eq!(err, error!(ErrorCode::HlpSettlementUnavailable));
    }

    #[test]
    fn close_hlp_rejects_settlement_price_divergence() {
        let mut market = seeded_market();
        let receipt = DepositSingleSided::new(MarketAsset::Base, 100, 1)
            .apply(&mut market)
            .unwrap();

        market.quote_side.reserves.live_reserve = 4_000;
        market.quote_side.reserves.cash_reserve = 3_800;
        market
            .assert_virtual_reserve_invariant(MarketAsset::Quote)
            .unwrap();
        let err = WithdrawSingleSided::new(MarketAsset::Base, receipt.hlp_amount)
            .apply(&mut market)
            .unwrap_err();

        assert_eq!(err, error!(ErrorCode::HlpSettlementUnavailable));
    }

    #[test]
    fn h_lp_checkpoint_refreshes_settlement_reference() {
        let mut market = seeded_market();
        DepositSingleSided::new(MarketAsset::Base, 100, 1)
            .apply(&mut market)
            .unwrap();
        market.quote_side.reserves.live_reserve = 2_080;
        market.quote_side.reserves.cash_reserve = 1_880;
        market
            .assert_virtual_reserve_invariant(MarketAsset::Quote)
            .unwrap();

        checkpoint_hlp_vaults(&mut market, 42).unwrap();

        assert_eq!(market.base_hlp_vault.last_rebalance_slot, 42);
        assert_eq!(
            market.base_hlp_vault.cached_settlement_price_nad,
            current_settlement_price_nad(&market, MarketAsset::Base).unwrap()
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

    #[test]
    fn rebalance_hlp_leverages_up_with_balanced_ylp() {
        let mut market = seeded_market();
        DepositSingleSided::new(MarketAsset::Base, 100, 1)
            .apply(&mut market)
            .unwrap();
        market.quote_side.reserves.live_reserve = 2_400;
        market.quote_side.reserves.cash_reserve = 2_200;
        market
            .assert_virtual_reserve_invariant(MarketAsset::Quote)
            .unwrap();
        let ylp_before = market.base_hlp_vault.ylp_shares;
        let debt_before = market.base_hlp_vault.debt_shares;
        let principal_before = market.base_hlp_vault.debt_principal;

        let (base_receipt, _) = rebalance_hlp_vaults(&mut market, 43).unwrap();

        assert!(base_receipt.ideal_delta > 0);
        assert!(base_receipt.executed_delta > 0);
        assert!(base_receipt.ylp_mint_amount > 0);
        assert_eq!(base_receipt.ylp_burn_amount, 0);
        assert!(market.base_hlp_vault.ylp_shares > ylp_before);
        assert!(market.base_hlp_vault.debt_shares > debt_before);
        assert!(market.base_hlp_vault.debt_principal > principal_before);
        assert!(market.base_hlp_vault.base_hlp_live_reserve > 0);
        assert!(market.base_hlp_vault.quote_hlp_live_reserve > 200);
        assert_eq!(market.base_hlp_vault.last_rebalance_slot, 43);
        assert_eq!(
            market.base_hlp_vault.pending_rebalance,
            base_receipt.pending_rebalance
        );
        market
            .assert_virtual_reserve_invariant(MarketAsset::Base)
            .unwrap();
        market
            .assert_virtual_reserve_invariant(MarketAsset::Quote)
            .unwrap();
        assert_hlp_near_target(&market, MarketAsset::Base, 2 * NAD as u128);
    }

    #[test]
    fn close_hlp_after_rebalance_retires_synthetic_live_reserves() {
        let mut market = seeded_market();
        DepositSingleSided::new(MarketAsset::Base, 100, 1)
            .apply(&mut market)
            .unwrap();
        market.quote_side.reserves.live_reserve = 2_400;
        market.quote_side.reserves.cash_reserve = 2_200;
        market
            .assert_virtual_reserve_invariant(MarketAsset::Quote)
            .unwrap();

        let (base_receipt, _) = rebalance_hlp_vaults(&mut market, 43).unwrap();

        assert!(base_receipt.ylp_mint_amount > 0);
        assert!(market.base_hlp_vault.base_hlp_live_reserve > 0);
        assert!(market.base_hlp_vault.quote_hlp_live_reserve > 200);

        let hlp_amount = market.base_hlp_vault.hlp_supply;
        WithdrawSingleSided::new(MarketAsset::Base, hlp_amount)
            .apply(&mut market)
            .unwrap();

        assert_eq!(market.base_hlp_vault.hlp_supply, 0);
        assert_eq!(market.base_hlp_vault.ylp_shares, 0);
        assert_eq!(market.base_hlp_vault.debt_shares, 0);
        assert_eq!(market.base_hlp_vault.debt_principal, 0);
        assert_eq!(market.base_hlp_vault.base_hlp_live_reserve, 0);
        assert_eq!(market.base_hlp_vault.quote_hlp_live_reserve, 0);
        market
            .assert_virtual_reserve_invariant(MarketAsset::Base)
            .unwrap();
        market
            .assert_virtual_reserve_invariant(MarketAsset::Quote)
            .unwrap();
    }

    #[test]
    fn rebalance_hlp_leverage_up_stores_pending_when_borrow_cash_is_constrained() {
        let mut market = seeded_market();
        DepositSingleSided::new(MarketAsset::Base, 100, 1)
            .apply(&mut market)
            .unwrap();
        market.quote_side.reserves.live_reserve = 2_400;
        market.quote_side.reserves.cash_reserve = 50;
        market.debt.fixed_quote_shares = 2_150;
        market
            .assert_virtual_reserve_invariant(MarketAsset::Quote)
            .unwrap();
        let ideal_before = current_hlp_ideal_delta(&market, MarketAsset::Base).unwrap();
        assert!(ideal_before > 0);

        let (base_receipt, _) = rebalance_hlp_vaults(&mut market, 47).unwrap();

        assert!(base_receipt.executed_delta > 0);
        assert!(base_receipt.executed_delta < ideal_before);
        assert!(base_receipt.pending_rebalance > 0);
        assert!(base_receipt.debt_delta > 0);
        assert!(base_receipt.debt_delta <= 50);
        assert_eq!(
            market.base_hlp_vault.pending_rebalance,
            base_receipt.pending_rebalance
        );
        market
            .assert_virtual_reserve_invariant(MarketAsset::Base)
            .unwrap();
        market
            .assert_virtual_reserve_invariant(MarketAsset::Quote)
            .unwrap();
    }

    #[test]
    fn rebalance_hlp_leverage_up_keeps_swap_live_without_borrow_cash() {
        let mut market = seeded_market();
        DepositSingleSided::new(MarketAsset::Base, 100, 1)
            .apply(&mut market)
            .unwrap();
        market.quote_side.reserves.live_reserve = 2_400;
        market.quote_side.reserves.cash_reserve = 0;
        market.debt.fixed_quote_shares = 2_200;
        market
            .assert_virtual_reserve_invariant(MarketAsset::Quote)
            .unwrap();
        let ideal_before = current_hlp_ideal_delta(&market, MarketAsset::Base).unwrap();
        assert!(ideal_before > 0);

        let (base_receipt, _) = rebalance_hlp_vaults(&mut market, 48).unwrap();

        assert_eq!(base_receipt.executed_delta, 0);
        assert_eq!(base_receipt.pending_rebalance, ideal_before);
        assert_eq!(base_receipt.debt_delta, 0);
        assert_eq!(market.base_hlp_vault.pending_rebalance, ideal_before);
        market
            .assert_virtual_reserve_invariant(MarketAsset::Base)
            .unwrap();
        market
            .assert_virtual_reserve_invariant(MarketAsset::Quote)
            .unwrap();
    }

    #[test]
    fn rebalance_hlp_deleverages_with_balanced_ylp() {
        let mut market = seeded_market();
        DepositSingleSided::new(MarketAsset::Base, 100, 1)
            .apply(&mut market)
            .unwrap();
        market.quote_side.reserves.live_reserve = 1_800;
        market.quote_side.reserves.cash_reserve = 1_600;
        market
            .assert_virtual_reserve_invariant(MarketAsset::Quote)
            .unwrap();
        let ylp_before = market.base_hlp_vault.ylp_shares;
        let debt_before = market.base_hlp_vault.debt_shares;
        let principal_before = market.base_hlp_vault.debt_principal;

        let (base_receipt, _) = rebalance_hlp_vaults(&mut market, 44).unwrap();

        assert!(base_receipt.ideal_delta < 0);
        assert!(base_receipt.executed_delta < 0);
        assert!(base_receipt.ylp_burn_amount > 0);
        assert_eq!(base_receipt.ylp_mint_amount, 0);
        assert!(market.base_hlp_vault.ylp_shares < ylp_before);
        assert!(market.base_hlp_vault.debt_shares < debt_before);
        assert!(market.base_hlp_vault.debt_principal < principal_before);
        assert_eq!(market.base_hlp_vault.last_rebalance_slot, 44);
        assert_eq!(
            market.base_hlp_vault.pending_rebalance,
            base_receipt.pending_rebalance
        );
        market
            .assert_virtual_reserve_invariant(MarketAsset::Base)
            .unwrap();
        market
            .assert_virtual_reserve_invariant(MarketAsset::Quote)
            .unwrap();
        assert_hlp_near_target(&market, MarketAsset::Base, 2 * NAD as u128);
    }

    #[test]
    fn rebalance_hlp_deleverage_pays_accrued_interest_from_borrowed_cash() {
        let mut market = seeded_market();
        DepositSingleSided::new(MarketAsset::Base, 100, 1)
            .apply(&mut market)
            .unwrap();
        market.quote_side.reserves.live_reserve = 1_800;
        market.quote_side.reserves.cash_reserve = 1_600;
        market.debt.quote_borrow_index_nad = (NAD as u128) * 110 / 100;
        market
            .assert_virtual_reserve_invariant(MarketAsset::Quote)
            .unwrap();
        let quote_cash_before = market.quote_side.reserves.cash_reserve;
        let principal_before = market.base_hlp_vault.debt_principal;

        let (base_receipt, _) = rebalance_hlp_vaults(&mut market, 44).unwrap();

        assert!(base_receipt.executed_delta < 0);
        let principal_repaid = principal_before
            .checked_sub(market.base_hlp_vault.debt_principal)
            .unwrap();
        let interest_paid = base_receipt
            .debt_delta
            .unsigned_abs()
            .checked_sub(principal_repaid)
            .unwrap();
        assert!(interest_paid > 0);
        assert_eq!(base_receipt.interest_paid as u128, interest_paid);
        assert!(
            quote_cash_before
                .checked_sub(market.quote_side.reserves.cash_reserve)
                .unwrap() as u128
                >= interest_paid
        );
        market
            .assert_virtual_reserve_invariant(MarketAsset::Base)
            .unwrap();
        market
            .assert_virtual_reserve_invariant(MarketAsset::Quote)
            .unwrap();
    }

    #[test]
    fn quote_hlp_rebalance_moves_both_ylp_sides() {
        let mut market = seeded_market();
        DepositSingleSided::new(MarketAsset::Quote, 200, 1)
            .apply(&mut market)
            .unwrap();
        market.base_side.reserves.live_reserve = 1_200;
        market.base_side.reserves.cash_reserve = 1_100;
        market
            .assert_virtual_reserve_invariant(MarketAsset::Base)
            .unwrap();
        let ylp_before = market.quote_hlp_vault.ylp_shares;
        let debt_before = market.quote_hlp_vault.debt_shares;
        let principal_before = market.quote_hlp_vault.debt_principal;

        let (_, quote_receipt) = rebalance_hlp_vaults(&mut market, 45).unwrap();

        assert!(quote_receipt.ideal_delta > 0);
        assert!(quote_receipt.executed_delta > 0);
        assert!(quote_receipt.ylp_mint_amount > 0);
        assert!(market.quote_hlp_vault.ylp_shares > ylp_before);
        assert!(market.quote_hlp_vault.debt_shares > debt_before);
        assert!(market.quote_hlp_vault.debt_principal > principal_before);
        assert_eq!(market.quote_hlp_vault.last_rebalance_slot, 45);
        market
            .assert_virtual_reserve_invariant(MarketAsset::Base)
            .unwrap();
        market
            .assert_virtual_reserve_invariant(MarketAsset::Quote)
            .unwrap();
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

        DepositSingleSided::new(MarketAsset::Base, 100_000, 1)
            .apply(&mut market)
            .unwrap();
        DepositSingleSided::new(MarketAsset::Quote, 200_000, 1)
            .apply(&mut market)
            .unwrap();

        let amount_in_after_fee = 50_000;
        let amount_out = calculate_raw_amount_out(
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
            0,
            crate::state::ProtocolAuctionSplit::default(),
        )
        .unwrap();

        let quoted_post_swap_price =
            market_spot_price_nad(&market.base_side, &market.quote_side).unwrap();
        let base_liquidity_before = market.base_side.reserves.live_reserve;
        let quote_liquidity_before = market.quote_side.reserves.live_reserve;

        let (base_receipt, quote_receipt) = rebalance_hlp_vaults(&mut market, 46).unwrap();

        assert!(
            base_receipt.executed_delta != 0 || quote_receipt.executed_delta != 0,
            "test must exercise an hLP rebalance"
        );
        assert_ne!(
            market.base_side.reserves.live_reserve,
            base_liquidity_before
        );
        assert_ne!(
            market.quote_side.reserves.live_reserve,
            quote_liquidity_before
        );

        let post_rebalance_price =
            market_spot_price_nad(&market.base_side, &market.quote_side).unwrap();
        let price_diff = quoted_post_swap_price.abs_diff(post_rebalance_price);
        assert!(
            price_diff <= quoted_post_swap_price / BPS_DENOMINATOR as u64 + 1,
            "hLP rebalance moved post-swap spot by more than rounding: quoted {}, final {}",
            quoted_post_swap_price,
            post_rebalance_price
        );
    }
