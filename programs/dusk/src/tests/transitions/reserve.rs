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

    fn v1_style_liquidity_for_deposit(
        base_reserve: u64,
        quote_reserve: u64,
        ylp_supply: u64,
        max_base_reserve_credit: u64,
        max_quote_reserve_credit: u64,
    ) -> u64 {
        let base_ylp = (max_base_reserve_credit as u128)
            .checked_mul(ylp_supply as u128)
            .unwrap()
            .checked_div(base_reserve as u128)
            .unwrap();
        let quote_ylp = (max_quote_reserve_credit as u128)
            .checked_mul(ylp_supply as u128)
            .unwrap()
            .checked_div(quote_reserve as u128)
            .unwrap();
        base_ylp.min(quote_ylp).try_into().unwrap()
    }

    fn v1_style_reserve_for_liquidity(
        reserve: u64,
        ylp_supply: u64,
        ylp_amount: u64,
    ) -> u64 {
        ceil_div(
            (ylp_amount as u128).checked_mul(reserve as u128).unwrap(),
            ylp_supply as u128,
        )
        .unwrap()
        .try_into()
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
    fn add_liquidity_accepts_ratio_quote_with_raw_rounding_dust() {
        let mut market = empty_market();
        market
            .add_liquidity(1_620_342_794_237, 1_579_912_601_954)
            .unwrap();

        let base_amount = 123_000_000;
        let quote_amount = ((base_amount as u128)
            .checked_mul(market.quote_side.reserves.live_reserve as u128)
            .unwrap()
            .checked_div(market.base_side.reserves.live_reserve as u128)
            .unwrap()) as u64;

        assert_eq!(quote_amount, 119_930_949);
        let receipt = market.add_liquidity(base_amount, quote_amount).unwrap();

        assert!(receipt.ylp_amount > 0);
        market.assert_market_invariants().unwrap();
    }

    #[test]
    fn add_liquidity_uses_limiting_side_without_donating_excess() {
        let mut market = empty_market();
        market.add_liquidity(1_000_000, 2_000_000).unwrap();

        let preview = market.preview_add_liquidity(100_000, 500_000).unwrap();

        assert_eq!(preview.base_reserve_credit, 100_000);
        assert_eq!(preview.quote_reserve_credit, 200_000);
        assert_eq!(preview.ylp_amount, 141_421);
        assert_eq!(preview.ylp_supply, 1_555_634);

        let receipt = market.add_liquidity(100_000, 500_000).unwrap();

        assert_eq!(receipt.base_reserve_credit, 100_000);
        assert_eq!(receipt.quote_reserve_credit, 200_000);
        assert_eq!(receipt.ylp_amount, 141_421);
        assert_eq!(market.base_side.reserves.live_reserve, 1_100_000);
        assert_eq!(market.quote_side.reserves.live_reserve, 2_200_000);
        assert_eq!(market.base_side.shares.ylp_supply, 1_555_634);
        assert_eq!(market.quote_side.shares.ylp_supply, 1_555_634);
        market.assert_market_invariants().unwrap();
    }

    #[test]
    fn add_liquidity_rounds_used_amounts_like_v1() {
        let mut market = empty_market();
        market.add_liquidity(1_000_000, 2_000_000).unwrap();

        let preview = market.preview_add_liquidity(100_001, 200_002).unwrap();

        assert_eq!(preview.base_reserve_credit, 100_001);
        assert_eq!(preview.quote_reserve_credit, 200_001);
        assert_eq!(preview.ylp_amount, 141_422);
        assert_eq!(preview.ylp_supply, 1_555_635);

        let receipt = market.add_liquidity(100_001, 200_002).unwrap();

        assert_eq!(receipt.base_reserve_credit, 100_001);
        assert_eq!(receipt.quote_reserve_credit, 200_001);
        assert_eq!(receipt.ylp_amount, 141_422);
        assert_eq!(market.base_side.reserves.live_reserve, 1_100_001);
        assert_eq!(market.quote_side.reserves.live_reserve, 2_200_001);
        assert_eq!(market.base_side.shares.ylp_supply, 1_555_635);
        assert_eq!(market.quote_side.shares.ylp_supply, 1_555_635);
        market.assert_market_invariants().unwrap();
    }

    #[test]
    fn add_liquidity_matches_v1_limiting_side_formula() {
        let mut market = empty_market();
        market.add_liquidity(1_000_000, 2_000_000).unwrap();

        let base_reserve_before = market.base_side.reserves.live_reserve;
        let quote_reserve_before = market.quote_side.reserves.live_reserve;
        let ylp_supply_before = market.base_side.shares.ylp_supply;

        let max_base_reserve_credit = 333_333;
        let max_quote_reserve_credit = 999_999;
        let expected_ylp = v1_style_liquidity_for_deposit(
            base_reserve_before,
            quote_reserve_before,
            ylp_supply_before,
            max_base_reserve_credit,
            max_quote_reserve_credit,
        );
        let expected_base_credit =
            v1_style_reserve_for_liquidity(base_reserve_before, ylp_supply_before, expected_ylp);
        let expected_quote_credit =
            v1_style_reserve_for_liquidity(quote_reserve_before, ylp_supply_before, expected_ylp);

        let preview = market
            .preview_add_liquidity(max_base_reserve_credit, max_quote_reserve_credit)
            .unwrap();
        assert_eq!(preview.ylp_amount, expected_ylp);
        assert_eq!(preview.base_reserve_credit, expected_base_credit);
        assert_eq!(preview.quote_reserve_credit, expected_quote_credit);
        assert!(preview.base_reserve_credit <= max_base_reserve_credit);
        assert!(preview.quote_reserve_credit <= max_quote_reserve_credit);

        let receipt = market
            .add_liquidity(max_base_reserve_credit, max_quote_reserve_credit)
            .unwrap();
        assert_eq!(receipt.ylp_amount, expected_ylp);
        assert_eq!(receipt.base_reserve_credit, expected_base_credit);
        assert_eq!(receipt.quote_reserve_credit, expected_quote_credit);
        assert_eq!(
            market.base_side.reserves.live_reserve,
            base_reserve_before + expected_base_credit
        );
        assert_eq!(
            market.quote_side.reserves.live_reserve,
            quote_reserve_before + expected_quote_credit
        );
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
        fn add_liquidity_matches_v1_formula_for_existing_markets(
            base_reserve in 500_000u64..20_000_000,
            quote_reserve in 500_000u64..40_000_000,
            ylp_supply in 500_000u64..20_000_000,
            max_base_reserve_credit in 1_000u64..10_000_000,
            max_quote_reserve_credit in 1_000u64..20_000_000,
        ) {
            let expected_ylp = v1_style_liquidity_for_deposit(
                base_reserve,
                quote_reserve,
                ylp_supply,
                max_base_reserve_credit,
                max_quote_reserve_credit,
            );
            prop_assume!(expected_ylp > 0);

            let expected_base_credit =
                v1_style_reserve_for_liquidity(base_reserve, ylp_supply, expected_ylp);
            let expected_quote_credit =
                v1_style_reserve_for_liquidity(quote_reserve, ylp_supply, expected_ylp);
            prop_assert!(expected_base_credit <= max_base_reserve_credit);
            prop_assert!(expected_quote_credit <= max_quote_reserve_credit);

            let mut preview_market = empty_market();
            preview_market.base_side.reserves.live_reserve = base_reserve;
            preview_market.base_side.reserves.cash_reserve = base_reserve;
            preview_market.base_side.shares.ylp_supply = ylp_supply;
            preview_market.quote_side.reserves.live_reserve = quote_reserve;
            preview_market.quote_side.reserves.cash_reserve = quote_reserve;
            preview_market.quote_side.shares.ylp_supply = ylp_supply;

            let preview = preview_market
                .preview_add_liquidity(max_base_reserve_credit, max_quote_reserve_credit)
                .unwrap();
            prop_assert_eq!(preview.ylp_amount, expected_ylp);
            prop_assert_eq!(preview.base_reserve_credit, expected_base_credit);
            prop_assert_eq!(preview.quote_reserve_credit, expected_quote_credit);
            prop_assert_eq!(preview.ylp_supply, ylp_supply + expected_ylp);

            let mut execution_market = preview_market;
            let receipt = execution_market
                .add_liquidity(max_base_reserve_credit, max_quote_reserve_credit)
                .unwrap();
            prop_assert_eq!(receipt.ylp_amount, preview.ylp_amount);
            prop_assert_eq!(receipt.base_reserve_credit, preview.base_reserve_credit);
            prop_assert_eq!(receipt.quote_reserve_credit, preview.quote_reserve_credit);
            prop_assert_eq!(
                execution_market.base_side.reserves.live_reserve,
                base_reserve + expected_base_credit
            );
            prop_assert_eq!(
                execution_market.quote_side.reserves.live_reserve,
                quote_reserve + expected_quote_credit
            );
            prop_assert_eq!(
                execution_market.base_side.shares.ylp_supply,
                ylp_supply + expected_ylp
            );
            prop_assert_eq!(
                execution_market.quote_side.shares.ylp_supply,
                ylp_supply + expected_ylp
            );
            execution_market.assert_market_invariants().unwrap();
        }

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
