use super::*;
    use proptest::prelude::*;
    use crate::state::{PendingAuthorityChange, PendingConfigChange};
    use crate::{
        constants::{BPS_DENOMINATOR, MARKET_VERSION},
        math::calculate_raw_amount_out,
        state::{Insurance, MarketConfig, MarketSide, Risk},
    };

    fn valid_config() -> MarketConfig {
        MarketConfig {
            swap_fee_bps: 30,
            manager_fee_bps: 0,
            protocol_fee_bps: 0,
            target_hlp_leverage_bps: BPS_DENOMINATOR * 2,
            settlement_divergence_bps: 500,
            ema_half_life_ms: 60_000,
            directional_ema_half_life_ms: 60_000,
            k_ema_half_life_ms: 60_000,
            max_daily_borrow_bps: 2_000,
            spot_ema_divergence_bps: 1_000,
            k_ema_drawdown_bps: 1_000,
            utilized_collateral_cap_bps: 15_000,
            market_health_min_bps: 11_000,
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
            version: MARKET_VERSION,
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

        checkpoint_hlp_vaults(&mut market).unwrap();
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

    fn assert_market_hlp_invariants(market: &Market) {
        market.base_side.assert_share_backing().unwrap();
        market.quote_side.assert_share_backing().unwrap();
        market
            .assert_virtual_reserve_invariant(MarketAsset::Base)
            .unwrap();
        market
            .assert_virtual_reserve_invariant(MarketAsset::Quote)
            .unwrap();
    }

    fn price_diff_bps(before_nad: u64, after_nad: u64) -> u64 {
        if before_nad == 0 {
            return 0;
        }
        before_nad
            .abs_diff(after_nad)
            .saturating_mul(BPS_DENOMINATOR as u64)
            / before_nad
    }

    fn set_side_live_preserving_hlp_invariant(
        market: &mut Market,
        asset: MarketAsset,
        live_reserve: u64,
    ) {
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

    fn constrain_side_cash_preserving_hlp_invariant(
        market: &mut Market,
        asset: MarketAsset,
        cash_bps: u64,
    ) {
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
                market.debt.fixed_base_principal = cash_backed_debt as u128;
            }
            MarketAsset::Quote => {
                market.debt.fixed_quote_shares = cash_backed_debt as u128;
                market.debt.fixed_quote_principal = cash_backed_debt as u128;
            }
        }
    }

    #[derive(Debug)]
    struct TestCompositeSwapReceipt {
        amount_out: u64,
        base_pre_rebalance: HlpRebalanceReceipt,
        quote_pre_rebalance: HlpRebalanceReceipt,
        base_post_rebalance: HlpRebalanceReceipt,
        quote_post_rebalance: HlpRebalanceReceipt,
    }

    fn active_hlp_market() -> Market {
        let mut market = seeded_market();
        configure_market_depth(&mut market, 1_000_000, 20_000);
        DepositSingleSided::new(MarketAsset::Base, 100_000, 1)
            .apply(&mut market)
            .unwrap();
        DepositSingleSided::new(MarketAsset::Quote, 200_000, 1)
            .apply(&mut market)
            .unwrap();
        assert_market_hlp_invariants(&market);
        market
    }

    fn checkpoint_test_pre_solve_fee_eligibility(
        market: &mut Market,
        receipt: &HlpRebalanceReceipt,
    ) {
        if receipt.ylp_mint_amount == 0 {
            return;
        }
        checkpoint_hlp_yield_from_ylp_shares(
            market,
            receipt.target_asset,
            receipt.current_swap_fee_eligible_ylp_shares,
        )
        .unwrap();
    }

    fn apply_test_composite_swap(
        market: &mut Market,
        asset_in: MarketAsset,
        amount_in_after_fee: u64,
    ) -> TestCompositeSwapReceipt {
        let (base_pre_rebalance, quote_pre_rebalance) =
            pre_solve_hlp_vaults_for_swap(market, asset_in, amount_in_after_fee)
                .unwrap();
        let pre_solve_ylp_mint_amount = base_pre_rebalance
            .ylp_mint_amount
            .checked_add(quote_pre_rebalance.ylp_mint_amount)
            .unwrap();
        let fee_eligible_ylp_supply = market
            .side(asset_in)
            .shares
            .ylp_supply
            .checked_sub(pre_solve_ylp_mint_amount)
            .unwrap();
        let (market_side_in, market_side_out) = market.swap_sides(asset_in);
        let amount_out = calculate_raw_amount_out(
            market_side_in.reserves.live_reserve,
            market_side_out.reserves.live_reserve,
            amount_in_after_fee,
        )
        .unwrap();
        market
            .swap_reserves_with_fee_supply(
                asset_in,
                amount_in_after_fee,
                amount_out,
                0,
                0,
                0,
                crate::state::ProtocolAuctionSplit::default(),
                Some(fee_eligible_ylp_supply),
            )
            .unwrap();
        checkpoint_test_pre_solve_fee_eligibility(market, &base_pre_rebalance);
        checkpoint_test_pre_solve_fee_eligibility(market, &quote_pre_rebalance);
        let (base_post_rebalance, quote_post_rebalance) =
            rebalance_hlp_vaults(market).unwrap();
        assert_market_hlp_invariants(market);
        TestCompositeSwapReceipt {
            amount_out,
            base_pre_rebalance,
            quote_pre_rebalance,
            base_post_rebalance,
            quote_post_rebalance,
        }
    }

    fn assert_no_hlp_residuals(market: &Market) {
        assert_eq!(market.base_hlp_vault.hlp_supply, 0);
        assert_eq!(market.base_hlp_vault.ylp_shares, 0);
        assert_eq!(market.base_hlp_vault.debt_shares, 0);
        assert_eq!(market.base_hlp_vault.debt_principal, 0);
        assert_eq!(market.base_hlp_vault.base_hlp_live_reserve, 0);
        assert_eq!(market.base_hlp_vault.quote_hlp_live_reserve, 0);
        assert_eq!(market.quote_hlp_vault.hlp_supply, 0);
        assert_eq!(market.quote_hlp_vault.ylp_shares, 0);
        assert_eq!(market.quote_hlp_vault.debt_shares, 0);
        assert_eq!(market.quote_hlp_vault.debt_principal, 0);
        assert_eq!(market.quote_hlp_vault.base_hlp_live_reserve, 0);
        assert_eq!(market.quote_hlp_vault.quote_hlp_live_reserve, 0);
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
            DepositSingleSided::new(target_asset, deposit_amount, 1)
                .apply(&mut market)
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
                base_receipt.pending_rebalance,
                base_receipt.ideal_delta - base_receipt.executed_delta
            );
            prop_assert_eq!(
                quote_receipt.pending_rebalance,
                quote_receipt.ideal_delta - quote_receipt.executed_delta
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

        let (base_receipt, _) = rebalance_hlp_vaults(&mut market).unwrap();

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

        let (base_receipt, _) = rebalance_hlp_vaults(&mut market).unwrap();

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

        let (base_receipt, _) = rebalance_hlp_vaults(&mut market).unwrap();

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

        let (base_receipt, _) = rebalance_hlp_vaults(&mut market).unwrap();

        assert!(base_receipt.ideal_delta < 0);
        assert!(base_receipt.executed_delta < 0);
        assert!(base_receipt.ylp_burn_amount > 0);
        assert_eq!(base_receipt.ylp_mint_amount, 0);
        assert!(market.base_hlp_vault.ylp_shares < ylp_before);
        assert!(market.base_hlp_vault.debt_shares < debt_before);
        assert!(market.base_hlp_vault.debt_principal < principal_before);
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

        let (base_receipt, _) = rebalance_hlp_vaults(&mut market).unwrap();

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

        let (_, quote_receipt) = rebalance_hlp_vaults(&mut market).unwrap();

        assert!(quote_receipt.ideal_delta > 0);
        assert!(quote_receipt.executed_delta > 0);
        assert!(quote_receipt.ylp_mint_amount > 0);
        assert!(market.quote_hlp_vault.ylp_shares > ylp_before);
        assert!(market.quote_hlp_vault.debt_shares > debt_before);
        assert!(market.quote_hlp_vault.debt_principal > principal_before);
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

        let (base_receipt, quote_receipt) = rebalance_hlp_vaults(&mut market).unwrap();

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

    #[test]
    fn small_swap_skips_hlp_pre_solve() {
        let mut market = seeded_market();
        configure_market_depth(&mut market, 1_000_000, 20_000);
        DepositSingleSided::new(MarketAsset::Base, 100_000, 1)
            .apply(&mut market)
            .unwrap();

        let (base_receipt, quote_receipt) =
            pre_solve_hlp_vaults_for_swap(&mut market, MarketAsset::Base, 1).unwrap();

        assert_eq!(base_receipt.executed_delta, 0);
        assert_eq!(quote_receipt.executed_delta, 0);
        assert_eq!(base_receipt.ylp_mint_amount, 0);
        assert_eq!(quote_receipt.ylp_mint_amount, 0);
        assert_market_hlp_invariants(&market);
    }

    #[test]
    fn large_swap_pre_solve_changes_quote_visible_depth() {
        let mut market = seeded_market();
        configure_market_depth(&mut market, 1_000_000, 20_000);
        DepositSingleSided::new(MarketAsset::Base, 100_000, 1)
            .apply(&mut market)
            .unwrap();
        DepositSingleSided::new(MarketAsset::Quote, 200_000, 1)
            .apply(&mut market)
            .unwrap();

        let amount_in_after_fee = 350_000;
        let user_only_out = calculate_raw_amount_out(
            market.base_side.reserves.live_reserve,
            market.quote_side.reserves.live_reserve,
            amount_in_after_fee,
        )
        .unwrap();
        let price_before =
            market_spot_price_nad(&market.base_side, &market.quote_side).unwrap();

        let (base_receipt, quote_receipt) = pre_solve_hlp_vaults_for_swap(
            &mut market,
            MarketAsset::Base,
            amount_in_after_fee,
        )
        .unwrap();

        assert!(
            base_receipt.executed_delta != 0 || quote_receipt.executed_delta != 0,
            "large swap should execute a quote-visible hLP pre-adjustment"
        );
        assert!(
            base_receipt.executed_delta != 0 && quote_receipt.executed_delta != 0,
            "both active hLP vaults should be eligible for pre-adjustment"
        );
        let pre_solved_out = calculate_raw_amount_out(
            market.base_side.reserves.live_reserve,
            market.quote_side.reserves.live_reserve,
            amount_in_after_fee,
        )
        .unwrap();
        assert_ne!(pre_solved_out, user_only_out);

        let price_after =
            market_spot_price_nad(&market.base_side, &market.quote_side).unwrap();
        assert!(
            price_diff_bps(price_before, price_after) <= 2,
            "pre-adjustment must preserve marginal spot within rounding"
        );
        assert_market_hlp_invariants(&market);
    }

    #[test]
    fn pre_solve_handles_opposing_hlp_flows_without_order_asymmetry() {
        let mut market = active_hlp_market();
        let amount_in_after_fee = 350_000;

        let (base_receipt, quote_receipt) = pre_solve_hlp_vaults_for_swap(
            &mut market,
            MarketAsset::Base,
            amount_in_after_fee,
        )
        .unwrap();

        assert!(
            base_receipt.executed_delta < 0,
            "base hLP should deleverage when a base-in swap moves base down"
        );
        assert!(
            quote_receipt.executed_delta > 0,
            "quote hLP should lever up when a base-in swap moves quote up"
        );
        assert_eq!(
            base_receipt.pending_rebalance,
            base_receipt.ideal_delta - base_receipt.executed_delta
        );
        assert_eq!(
            quote_receipt.pending_rebalance,
            quote_receipt.ideal_delta - quote_receipt.executed_delta
        );
        assert_market_hlp_invariants(&market);
    }

    #[test]
    fn quote_simulation_matches_composite_swap_execution() {
        let mut quoted_market = active_hlp_market();
        let mut executed_market = active_hlp_market();
        let amount_in_after_fee = 350_000;

        let quoted = apply_test_composite_swap(
            &mut quoted_market,
            MarketAsset::Base,
            amount_in_after_fee,
        );
        let executed = apply_test_composite_swap(
            &mut executed_market,
            MarketAsset::Base,
            amount_in_after_fee,
        );

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
            quoted.base_post_rebalance.executed_delta != 0
                || quoted.quote_post_rebalance.executed_delta != 0,
            "test must exercise the post-swap hLP phase too"
        );
    }

    #[test]
    fn swap_round_trip_then_hlp_close_leaves_no_synthetic_residuals() {
        let mut market = active_hlp_market();
        let base_hlp_deposit = 100_000;
        let quote_hlp_deposit = 200_000;

        let first_swap =
            apply_test_composite_swap(&mut market, MarketAsset::Base, 350_000);
        let _second_swap =
            apply_test_composite_swap(&mut market, MarketAsset::Quote, first_swap.amount_out);

        let base_hlp_supply = market.base_hlp_vault.hlp_supply;
        let quote_hlp_supply = market.quote_hlp_vault.hlp_supply;
        let base_close = WithdrawSingleSided::new(MarketAsset::Base, base_hlp_supply)
            .apply(&mut market)
            .unwrap();
        let quote_close = WithdrawSingleSided::new(MarketAsset::Quote, quote_hlp_supply)
            .apply(&mut market)
            .unwrap();

        let initial_value_at_final_spot = market
            .spot_value_in_opposite(MarketAsset::Base, base_hlp_deposit)
            .unwrap()
            .checked_add(quote_hlp_deposit)
            .unwrap();
        let realized_value_at_final_spot = market
            .spot_value_in_opposite(MarketAsset::Base, base_close.target_amount_out)
            .unwrap()
            .checked_add(quote_close.target_amount_out)
            .unwrap();
        assert!(
            realized_value_at_final_spot <= initial_value_at_final_spot + 8,
            "round-trip hLP close should not extract combined value: base {:?}, quote {:?}, realized {}, initial {}",
            base_close,
            quote_close,
            realized_value_at_final_spot,
            initial_value_at_final_spot
        );
        assert_no_hlp_residuals(&market);
    }

    #[test]
    fn mass_unwind_is_order_independent_when_cash_is_available() {
        let mut close_first = active_hlp_market();
        let mut ylp_first = active_hlp_market();
        let public_ylp_supply = 1_000_000;

        WithdrawSingleSided::new(MarketAsset::Base, close_first.base_hlp_vault.hlp_supply)
            .apply(&mut close_first)
            .unwrap();
        WithdrawSingleSided::new(MarketAsset::Quote, close_first.quote_hlp_vault.hlp_supply)
            .apply(&mut close_first)
            .unwrap();
        assert_no_hlp_residuals(&close_first);
        close_first.remove_liquidity(public_ylp_supply).unwrap();
        assert_eq!(close_first.base_side.reserves.live_reserve, 0);
        assert_eq!(close_first.quote_side.reserves.live_reserve, 0);
        assert_eq!(close_first.base_side.shares.ylp_supply, 0);
        assert_eq!(close_first.quote_side.shares.ylp_supply, 0);

        ylp_first.remove_liquidity(public_ylp_supply).unwrap();
        WithdrawSingleSided::new(MarketAsset::Base, ylp_first.base_hlp_vault.hlp_supply)
            .apply(&mut ylp_first)
            .unwrap();
        WithdrawSingleSided::new(MarketAsset::Quote, ylp_first.quote_hlp_vault.hlp_supply)
            .apply(&mut ylp_first)
            .unwrap();
        assert_no_hlp_residuals(&ylp_first);
        assert_eq!(ylp_first.base_side.reserves.live_reserve, 0);
        assert_eq!(ylp_first.quote_side.reserves.live_reserve, 0);
        assert_eq!(ylp_first.base_side.shares.ylp_supply, 0);
        assert_eq!(ylp_first.quote_side.shares.ylp_supply, 0);
    }

    #[test]
    fn pre_solved_hlp_mints_start_earning_after_current_swap_fee() {
        let mut market = seeded_market();
        configure_market_depth(&mut market, 1_000_000, 20_000);
        DepositSingleSided::new(MarketAsset::Quote, 200_000, 1)
            .apply(&mut market)
            .unwrap();
        let quote_hlp_ylp_before = market.quote_hlp_vault.ylp_shares;

        let amount_in_after_fee = 350_000;
        let (base_receipt, quote_receipt) = pre_solve_hlp_vaults_for_swap(
            &mut market,
            MarketAsset::Base,
            amount_in_after_fee,
        )
        .unwrap();
        assert_eq!(base_receipt.ylp_mint_amount, 0);
        assert!(quote_receipt.ylp_mint_amount > 0);
        assert_eq!(
            quote_receipt.current_swap_fee_eligible_ylp_shares,
            quote_hlp_ylp_before
        );
        assert!(
            quote_receipt.current_swap_fee_eligible_ylp_shares
                < market.quote_hlp_vault.ylp_shares
        );

        let pre_solve_minted = base_receipt
            .ylp_mint_amount
            .checked_add(quote_receipt.ylp_mint_amount)
            .unwrap();
        let fee_eligible_supply = market
            .base_side
            .shares
            .ylp_supply
            .checked_sub(pre_solve_minted)
            .unwrap();
        let amount_out = calculate_raw_amount_out(
            market.base_side.reserves.live_reserve,
            market.quote_side.reserves.live_reserve,
            amount_in_after_fee,
        )
        .unwrap();
        market
            .swap_reserves_with_fee_supply(
                MarketAsset::Base,
                amount_in_after_fee,
                amount_out,
                10_000,
                0,
                0,
                crate::state::ProtocolAuctionSplit::default(),
                Some(fee_eligible_supply),
            )
            .unwrap();
        checkpoint_hlp_yield_from_ylp_shares(
            &mut market,
            MarketAsset::Quote,
            quote_receipt.current_swap_fee_eligible_ylp_shares,
        )
        .unwrap();
        let growth_after_eligible_checkpoint =
            market.quote_hlp_vault.base_swap_fee_growth_index_nad;

        checkpoint_hlp_yield_from_ylp(&mut market, MarketAsset::Quote).unwrap();

        assert_eq!(
            market.quote_hlp_vault.base_swap_fee_growth_index_nad,
            growth_after_eligible_checkpoint
        );
    }
