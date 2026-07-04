use super::*;
    use proptest::prelude::*;

    fn empty_market() -> Market {
        Market {
            version: MARKET_VERSION,
            base_mint: Pubkey::new_unique(),
            quote_mint: Pubkey::new_unique(),
            ylp_mint: Pubkey::new_unique(),
            operator: Pubkey::new_unique(),
            manager: Pubkey::new_unique(),
            base_side: MarketSide::default(),
            quote_side: MarketSide::default(),
            config: MarketConfig::default(),
            debt: Debt::default(),
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

    fn seeded_market_with_cash_backed_debt(
        base_live: u64,
        quote_live: u64,
        ylp_supply: u64,
        base_cash_bps: u64,
        quote_cash_bps: u64,
    ) -> Market {
        let mut market = empty_market();
        let base_cash = base_live
            .checked_mul(base_cash_bps)
            .unwrap()
            .checked_div(BPS_DENOMINATOR as u64)
            .unwrap();
        let quote_cash = quote_live
            .checked_mul(quote_cash_bps)
            .unwrap()
            .checked_div(BPS_DENOMINATOR as u64)
            .unwrap();
        market.base_side.reserves.live_reserve = base_live;
        market.base_side.reserves.cash_reserve = base_cash;
        market.base_side.shares.ylp_supply = ylp_supply;
        market.quote_side.reserves.live_reserve = quote_live;
        market.quote_side.reserves.cash_reserve = quote_cash;
        market.quote_side.shares.ylp_supply = ylp_supply;
        market.debt.base_borrow_index_nad = NAD as u128;
        market.debt.quote_borrow_index_nad = NAD as u128;
        market.debt.fixed_base_shares = base_live.checked_sub(base_cash).unwrap() as u128;
        market.debt.fixed_base_principal = market.debt.fixed_base_shares;
        market.debt.fixed_quote_shares = quote_live.checked_sub(quote_cash).unwrap() as u128;
        market.debt.fixed_quote_principal = market.debt.fixed_quote_shares;
        market
    }

    fn spot_diff_bps(base_before: u64, quote_before: u64, base_after: u64, quote_after: u64) -> u128 {
        let before = (quote_before as u128)
            .checked_mul(NAD as u128)
            .unwrap()
            .checked_div(base_before as u128)
            .unwrap();
        let after = (quote_after as u128)
            .checked_mul(NAD as u128)
            .unwrap()
            .checked_div(base_after as u128)
            .unwrap();
        before
            .abs_diff(after)
            .checked_mul(BPS_DENOMINATOR as u128)
            .unwrap()
            .checked_div(before)
            .unwrap()
    }

    #[test]
    fn add_liquidity_mints_locked_minimum_liquidity() {
        let mut market = empty_market();

        let receipt = market.add_liquidity(1_000_000, 2_000_000).unwrap();

        assert_eq!(receipt.ylp_amount, 1_413_213);
        assert_eq!(market.base_side.shares.ylp_supply, 1_414_213);
        assert_eq!(market.quote_side.shares.ylp_supply, 1_414_213);
        market.assert_market_invariants().unwrap();
    }

    #[test]
    fn remove_liquidity_burns_matched_proportions() {
        let mut market = empty_market();
        market.add_liquidity(1_000_000, 2_000_000).unwrap();

        let receipt = market.remove_liquidity(250_000).unwrap();

        assert_eq!(receipt.base_amount_out, 176_776);
        assert_eq!(receipt.quote_amount_out, 353_553);
        assert_eq!(receipt.ylp_supply, 1_164_213);
        assert_eq!(market.base_side.shares.ylp_supply, 1_164_213);
        assert_eq!(market.quote_side.shares.ylp_supply, 1_164_213);
        market.assert_market_invariants().unwrap();
    }

    #[test]
    fn remove_liquidity_rejects_cash_shortfall_without_mutating_state() {
        let mut market = seeded_market_with_cash_backed_debt(1_000_000, 2_000_000, 1_000_000, 1_000, 1_000);
        let base_before = market.base_side;
        let quote_before = market.quote_side;
        let debt_before = market.debt;

        let err = match market.remove_liquidity(200_000) {
            Ok(_) => panic!("cash-constrained yLP withdrawal unexpectedly succeeded"),
            Err(err) => err,
        };

        assert_eq!(err, error!(ErrorCode::InsufficientLiquidity));
        assert_eq!(market.base_side.reserves.live_reserve, base_before.reserves.live_reserve);
        assert_eq!(market.base_side.reserves.cash_reserve, base_before.reserves.cash_reserve);
        assert_eq!(market.base_side.shares.ylp_supply, base_before.shares.ylp_supply);
        assert_eq!(market.quote_side.reserves.live_reserve, quote_before.reserves.live_reserve);
        assert_eq!(market.quote_side.reserves.cash_reserve, quote_before.reserves.cash_reserve);
        assert_eq!(market.quote_side.shares.ylp_supply, quote_before.shares.ylp_supply);
        assert_eq!(market.debt.fixed_base_shares, debt_before.fixed_base_shares);
        assert_eq!(market.debt.fixed_quote_shares, debt_before.fixed_quote_shares);
        market.assert_market_invariants().unwrap();
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn remove_liquidity_is_spot_neutral_and_invariant_preserving_under_cash_backed_debt(
            base_live in 500_000u64..20_000_000,
            quote_live in 500_000u64..40_000_000,
            ylp_supply in 500_000u64..20_000_000,
            base_cash_bps in 100u64..=10_000,
            quote_cash_bps in 100u64..=10_000,
            requested_burn_bps in 1u64..=10_000,
        ) {
            let max_safe_burn_bps = requested_burn_bps
                .min(base_cash_bps)
                .min(quote_cash_bps)
                .max(1);
            let ylp_amount = ylp_supply
                .checked_mul(max_safe_burn_bps)
                .unwrap()
                .checked_div(BPS_DENOMINATOR as u64)
                .unwrap()
                .max(1)
                .min(ylp_supply - 1);
            let mut market = seeded_market_with_cash_backed_debt(
                base_live,
                quote_live,
                ylp_supply,
                base_cash_bps,
                quote_cash_bps,
            );

            // A deliberately hostile K EMA must not gate vanilla yLP exits.
            market.risk.k_ema = (base_live as u128)
                .checked_mul(quote_live as u128)
                .unwrap()
                .checked_mul(10)
                .unwrap();
            market.config.k_ema_drawdown_bps = 0;
            market.assert_market_invariants().unwrap();

            let base_before = market.base_side.reserves.live_reserve;
            let quote_before = market.quote_side.reserves.live_reserve;
            let base_cash_before = market.base_side.reserves.cash_reserve;
            let quote_cash_before = market.quote_side.reserves.cash_reserve;
            let receipt = market.remove_liquidity(ylp_amount).unwrap();

            prop_assert!(receipt.base_amount_out <= base_live);
            prop_assert!(receipt.quote_amount_out <= quote_live);
            prop_assert!(receipt.base_amount_out <= base_cash_before);
            prop_assert!(receipt.quote_amount_out <= quote_cash_before);
            prop_assert_eq!(
                market.base_side.reserves.cash_reserve,
                base_cash_before - receipt.base_amount_out
            );
            prop_assert_eq!(
                market.quote_side.reserves.cash_reserve,
                quote_cash_before - receipt.quote_amount_out
            );
            prop_assert_eq!(market.base_side.shares.ylp_supply, market.quote_side.shares.ylp_supply);
            market.assert_market_invariants().unwrap();

            let spot_move_bps = spot_diff_bps(
                base_before,
                quote_before,
                market.base_side.reserves.live_reserve,
                market.quote_side.reserves.live_reserve,
            );
            prop_assert!(
                spot_move_bps <= 2,
                "pro-rata yLP removal moved spot by more than 2 bps: before {}/{}, after {}/{}, receipt base {}, quote {}",
                quote_before,
                base_before,
                market.quote_side.reserves.live_reserve,
                market.base_side.reserves.live_reserve,
                receipt.base_amount_out,
                receipt.quote_amount_out,
            );
        }
    }
