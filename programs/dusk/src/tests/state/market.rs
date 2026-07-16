use super::*;
    use proptest::prelude::*;

    fn market_with_roles(manager: Pubkey, operator: Pubkey) -> Market {
        Market {
            version: MARKET_VERSION,
            ylp_mint: Pubkey::new_unique(),
            operator,
            manager,
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

    fn valid_config() -> MarketConfig {
        MarketConfig {
            swap_fee_bps: 0,
            manager_fee_bps: 0,
            protocol_fee_bps: 0,
            target_hlp_leverage_bps: BPS_DENOMINATOR * 2,
            settlement_divergence_bps: BPS_DENOMINATOR,
            ema_half_life_ms: MIN_HALF_LIFE_MS,
            directional_ema_half_life_ms: MIN_HALF_LIFE_MS,
            k_ema_half_life_ms: MIN_HALF_LIFE_MS,
            max_daily_borrow_bps: BPS_DENOMINATOR,
            global_health_contribution_cap_bps: 15_000,
            borrow_market_health_floor_bps: BPS_DENOMINATOR,
            start_time: 0,
        }
    }

    fn invariant_market(base_cash: u64, quote_cash: u64) -> Market {
        let base_mint = Pubkey::new_unique();
        let quote_mint = Pubkey::new_unique();
        let mut base_side = MarketSide {
            asset_mint: base_mint,
            asset_decimals: 0,
            ..MarketSide::default()
        };
        base_side.reserves = Reserves {
            live_reserve: base_cash,
            cash_reserve: base_cash,
            reserved_liability: 0,
        };
        base_side.shares.ylp_supply = base_cash;
        let mut quote_side = MarketSide {
            asset_mint: quote_mint,
            asset_decimals: 0,
            ..MarketSide::default()
        };
        quote_side.reserves = Reserves {
            live_reserve: quote_cash,
            cash_reserve: quote_cash,
            reserved_liability: 0,
        };
        quote_side.shares.ylp_supply = quote_cash;
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

    fn borrow_position_for_debt(debt_asset: MarketAsset, collateral_amount: u64) -> BorrowPosition {
        let mut position = BorrowPosition {
            owner: Pubkey::new_unique(),
            market: Pubkey::new_unique(),
            position_id: Pubkey::new_unique(),
            base_collateral: 0,
            quote_collateral: 0,
            global_health_base_contribution_for_quote_debt: 0,
            global_health_quote_contribution_for_base_debt: 0,
            base_liquidation_cf_bps: 0,
            quote_liquidation_cf_bps: 0,
            fixed_base_shares: 0,
            fixed_quote_shares: 0,
            auction_start_time: 0,
            auction_start_price_nad: 0,
            auction_floor_price_nad: 0,
            bump: 255,
        };
        match debt_asset {
            MarketAsset::Base => position.quote_collateral = collateral_amount,
            MarketAsset::Quote => position.base_collateral = collateral_amount,
        }
        position
    }

    fn reserve_pair(market: &Market, asset: MarketAsset) -> (u64, u64) {
        let side = market.side(asset);
        (side.reserves.live_reserve, side.reserves.cash_reserve)
    }

    fn set_borrow_index(market: &mut Market, asset: MarketAsset, index_nad: u128) {
        match asset {
            MarketAsset::Base => market.debt.base_borrow_index_nad = index_nad,
            MarketAsset::Quote => market.debt.quote_borrow_index_nad = index_nad,
        }
    }

    fn add_accrued_cash_backed_interest_to_live_reserve(
        market: &mut Market,
        asset: MarketAsset,
        shares: u128,
        principal: u64,
    ) -> u64 {
        let index = market.debt.borrow_index(asset);
        let current_debt = Debt::shares_to_debt(shares, index).unwrap();
        let accrued_interest = current_debt.checked_sub(principal as u128).unwrap();
        let accrued_interest = u64::try_from(accrued_interest).unwrap();
        let side = market.side_mut(asset);
        side.reserves.live_reserve = side
            .reserves
            .live_reserve
            .checked_add(accrued_interest)
            .unwrap();
        accrued_interest
    }

    #[test]
    fn assert_manager_accepts_only_the_manager() {
        let manager = Pubkey::new_unique();
        let operator = Pubkey::new_unique();
        let market = market_with_roles(manager, operator);
        assert!(market.assert_manager(manager).is_ok());
        // The operator is NOT the manager for sensitive (manager-only) actions.
        assert!(market.assert_manager(operator).is_err());
        assert!(market.assert_manager(Pubkey::new_unique()).is_err());
    }

    #[test]
    fn assert_config_authority_accepts_only_manager() {
        let manager = Pubkey::new_unique();
        let operator = Pubkey::new_unique();
        let market = market_with_roles(manager, operator);
        assert!(market.assert_config_authority(manager).is_ok());
        assert!(market.assert_config_authority(operator).is_err());
        assert!(market
            .assert_config_authority(Pubkey::new_unique())
            .is_err());
    }

    #[test]
    fn operator_rotation_requires_timelock() {
        let manager = Pubkey::new_unique();
        let operator = Pubkey::new_unique();
        let new_operator = Pubkey::new_unique();
        let mut market = market_with_roles(manager, operator);

        let action = market
            .prepare_operator_update(manager, new_operator, 10)
            .unwrap();
        assert_eq!(
            action,
            MarketTimelockAction::Scheduled {
                execute_after_slot: 10 + MARKET_GOVERNANCE_DELAY_SLOTS
            }
        );
        assert_eq!(market.operator, operator);

        let err = market
            .prepare_operator_update(
                manager,
                new_operator,
                10 + MARKET_GOVERNANCE_DELAY_SLOTS - 1,
            )
            .unwrap_err();
        assert_eq!(
            err,
            anchor_lang::prelude::error!(ErrorCode::GovernanceTimelockNotReady)
        );

        let action = market
            .prepare_operator_update(manager, new_operator, 10 + MARKET_GOVERNANCE_DELAY_SLOTS)
            .unwrap();
        assert_eq!(action, MarketTimelockAction::Ready);
        market.apply_operator_update(new_operator);
        assert_eq!(market.operator, new_operator);
        assert!(!market.pending_operator.active);
    }

    #[test]
    fn config_update_requires_timelock() {
        let manager = Pubkey::new_unique();
        let operator = Pubkey::new_unique();
        let mut market = market_with_roles(manager, operator);
        let mut config = MarketConfig::default();
        config.target_hlp_leverage_bps = BPS_DENOMINATOR * 2;
        config.global_health_contribution_cap_bps = BPS_DENOMINATOR;
        config.borrow_market_health_floor_bps = BPS_DENOMINATOR;
        config.ema_half_life_ms = MIN_HALF_LIFE_MS;
        config.directional_ema_half_life_ms = MIN_HALF_LIFE_MS;
        config.k_ema_half_life_ms = MIN_HALF_LIFE_MS;
        let action = market.prepare_config_update(manager, config, 7).unwrap();
        assert_eq!(
            action,
            MarketTimelockAction::Scheduled {
                execute_after_slot: 7 + MARKET_GOVERNANCE_DELAY_SLOTS
            }
        );

        let action = market
            .prepare_config_update(manager, config, 7 + MARKET_GOVERNANCE_DELAY_SLOTS)
            .unwrap();
        assert_eq!(action, MarketTimelockAction::Ready);
    }

    #[test]
    fn borrow_preserves_virtual_reserve_as_cash_plus_debt() {
        let mut market = invariant_market(1_000_000, 1_000_000);
        let mut borrow_position = BorrowPosition {
            owner: Pubkey::new_unique(),
            market: Pubkey::new_unique(),
            position_id: Pubkey::new_unique(),
            base_collateral: 0,
            quote_collateral: 250_000,
            global_health_base_contribution_for_quote_debt: 0,
            global_health_quote_contribution_for_base_debt: 0,
            base_liquidation_cf_bps: 0,
            quote_liquidation_cf_bps: 0,
            fixed_base_shares: 0,
            fixed_quote_shares: 0,
            auction_start_time: 0,
            auction_start_price_nad: 0,
            auction_floor_price_nad: 0,
            bump: 255,
        };

        market
            .borrow(&mut borrow_position, MarketAsset::Base, 100_000, 0)
            .unwrap();

        assert_eq!(market.base_side.reserves.live_reserve, 1_000_000);
        assert_eq!(market.base_side.reserves.cash_reserve, 900_000);
        assert_eq!(market.debt.fixed_base_debt().unwrap(), 100_000);
        market
            .assert_virtual_reserve_invariant(MarketAsset::Base)
            .unwrap();
        market
            .assert_virtual_reserve_invariant(MarketAsset::Quote)
            .unwrap();
    }

    #[test]
    fn borrow_never_exceeds_cash_headroom() {
        let mut market = invariant_market(1_000_000, 1_000_000);
        market.base_side.reserves.cash_reserve = 10_000;
        let mut position = borrow_position_for_debt(MarketAsset::Base, 500_000);

        let err = market
            .borrow(&mut position, MarketAsset::Base, 10_001, 0)
            .unwrap_err();

        assert_eq!(
            err,
            anchor_lang::prelude::error!(ErrorCode::InsufficientBorrowHeadroom)
        );
        assert_eq!(market.base_side.daily_limits.borrowed_bucket, 0);
        assert_eq!(position.fixed_base_shares, 0);
    }

    #[test]
    fn liquidation_cf_slippage_protects_borrow_and_withdrawal() {
        let mut market = invariant_market(1_000_000, 1_000_000);
        let mut position = borrow_position_for_debt(MarketAsset::Base, 250_000);

        let borrow_err = market
            .borrow(&mut position, MarketAsset::Base, 100_000, 8_501)
            .unwrap_err();
        assert_eq!(
            borrow_err,
            anchor_lang::prelude::error!(ErrorCode::SlippageExceeded)
        );
        assert_eq!(position.fixed_base_shares, 0);

        market
            .borrow(&mut position, MarketAsset::Base, 100_000, 8_500)
            .unwrap();
        let withdrawal_err = market
            .withdraw_collateral(&mut position, MarketAsset::Quote, 1, 8_501)
            .unwrap_err();
        assert_eq!(
            withdrawal_err,
            anchor_lang::prelude::error!(ErrorCode::SlippageExceeded)
        );
        assert_eq!(position.quote_collateral, 250_000);
        assert_eq!(position.base_liquidation_cf_bps, 8_500);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn borrow_preserves_cash_backed_virtual_reserve_invariant_across_assets(
            borrow_base in any::<bool>(),
            base_cash in 1_000_000u64..50_000_000,
            quote_cash in 1_000_000u64..50_000_000,
            borrow_bps in 1u64..=500,
        ) {
            let borrow_asset = if borrow_base {
                MarketAsset::Base
            } else {
                MarketAsset::Quote
            };
            let collateral_asset = borrow_asset.opposite();
            let mut market = invariant_market(base_cash, quote_cash);
            let debt_cash_before = market.side(borrow_asset).reserves.cash_reserve;
            let debt_live_before = market.side(borrow_asset).reserves.live_reserve;
            let collateral_amount = market.side(collateral_asset).reserves.live_reserve / 2;
            let mut borrow_position =
                borrow_position_for_debt(borrow_asset, collateral_amount.max(1));
            let borrow_amount = debt_cash_before
                .checked_mul(borrow_bps)
                .unwrap()
                .checked_div(BPS_DENOMINATOR as u64)
                .unwrap()
                .max(1);

            let receipt = market
                .borrow(
                    &mut borrow_position,
                    borrow_asset,
                    borrow_amount,
                    0,
                )
                .unwrap();

            let (live_after, cash_after) = reserve_pair(&market, borrow_asset);
            prop_assert_eq!(receipt.interest_paid, 0);
            prop_assert_eq!(live_after, debt_live_before);
            prop_assert_eq!(cash_after, debt_cash_before - borrow_amount);
            match borrow_asset {
                MarketAsset::Base => {
                    prop_assert_eq!(borrow_position.fixed_base_debt(&market.debt).unwrap(), borrow_amount as u128);
                    prop_assert_eq!(market.debt.fixed_base_principal, borrow_amount as u128);
                }
                MarketAsset::Quote => {
                    prop_assert_eq!(borrow_position.fixed_quote_debt(&market.debt).unwrap(), borrow_amount as u128);
                    prop_assert_eq!(market.debt.fixed_quote_principal, borrow_amount as u128);
                }
            }
            market.assert_market_invariants().unwrap();
        }

        #[test]
        fn repay_preserves_cash_backed_virtual_reserve_invariant_across_principal_and_interest(
            repay_base in any::<bool>(),
            base_cash in 1_000_000u64..50_000_000,
            quote_cash in 1_000_000u64..50_000_000,
            borrow_bps in 1u64..=500,
            interest_bps in 1u128..=2_000,
            repay_bps in 1u128..=10_000,
        ) {
            let repay_asset = if repay_base {
                MarketAsset::Base
            } else {
                MarketAsset::Quote
            };
            let collateral_asset = repay_asset.opposite();
            let mut market = invariant_market(base_cash, quote_cash);
            let debt_cash_before = market.side(repay_asset).reserves.cash_reserve;
            let collateral_amount = market.side(collateral_asset).reserves.live_reserve / 2;
            let mut borrow_position =
                borrow_position_for_debt(repay_asset, collateral_amount.max(1));
            let borrow_amount = debt_cash_before
                .checked_mul(borrow_bps)
                .unwrap()
                .checked_div(BPS_DENOMINATOR as u64)
                .unwrap()
                .max(1);
            market
                .borrow(
                    &mut borrow_position,
                    repay_asset,
                    borrow_amount,
                    0,
                )
                .unwrap();

            let shares = match repay_asset {
                MarketAsset::Base => borrow_position.fixed_base_shares,
                MarketAsset::Quote => borrow_position.fixed_quote_shares,
            };
            let next_index = (NAD as u128)
                .checked_mul((BPS_DENOMINATOR as u128).checked_add(interest_bps).unwrap())
                .unwrap()
                .checked_div(BPS_DENOMINATOR as u128)
                .unwrap();
            set_borrow_index(&mut market, repay_asset, next_index);
            add_accrued_cash_backed_interest_to_live_reserve(
                &mut market,
                repay_asset,
                shares,
                borrow_amount,
            );
            market.assert_virtual_reserve_invariant(repay_asset).unwrap();

            let debt_before = match repay_asset {
                MarketAsset::Base => borrow_position.fixed_base_debt(&market.debt).unwrap(),
                MarketAsset::Quote => borrow_position.fixed_quote_debt(&market.debt).unwrap(),
            };
            let repay_credit = debt_before
                .checked_mul(repay_bps)
                .unwrap()
                .checked_div(BPS_DENOMINATOR as u128)
                .unwrap()
                .max(1)
                .min(debt_before);
            let repay_credit = u64::try_from(repay_credit).unwrap();
            let (live_before, cash_before) = reserve_pair(&market, repay_asset);

            let receipt = market
                .repay(&mut borrow_position, repay_asset, repay_credit)
                .unwrap();

            let (live_after, cash_after) = reserve_pair(&market, repay_asset);
            let principal_paid = repay_credit.checked_sub(receipt.interest_paid).unwrap();
            let debt_reduction = receipt.debt_delta.unsigned_abs();
            let live_debit = debt_reduction.checked_sub(principal_paid).unwrap();
            prop_assert_eq!(live_after, live_before - live_debit);
            prop_assert_eq!(cash_after, cash_before + principal_paid);
            prop_assert!(receipt.interest_paid <= repay_credit);
            prop_assert!(debt_reduction >= repay_credit);
            market.assert_market_invariants().unwrap();
        }
    }

    #[test]
    fn partial_repay_rounding_writeoff_preserves_virtual_reserve_invariant() {
        let repay_asset = MarketAsset::Quote;
        let mut market = invariant_market(1_000_000, 28_642_837);
        let mut borrow_position = borrow_position_for_debt(repay_asset, 500_000);
        let borrow_amount = 28_642_837 * 346 / BPS_DENOMINATOR as u64;
        market
            .borrow(
                &mut borrow_position,
                repay_asset,
                borrow_amount,
                0,
            )
            .unwrap();

        let shares = borrow_position.fixed_quote_shares;
        let next_index = (NAD as u128) * 10_413 / BPS_DENOMINATOR as u128;
        set_borrow_index(&mut market, repay_asset, next_index);
        add_accrued_cash_backed_interest_to_live_reserve(
            &mut market,
            repay_asset,
            shares,
            borrow_amount,
        );
        market.assert_virtual_reserve_invariant(repay_asset).unwrap();

        let debt_before = borrow_position.fixed_quote_debt(&market.debt).unwrap();
        let repay_credit = u64::try_from(debt_before * 205 / BPS_DENOMINATOR as u128).unwrap();
        let (live_before, cash_before) = reserve_pair(&market, repay_asset);

        let receipt = market
            .repay(&mut borrow_position, repay_asset, repay_credit)
            .unwrap();

        let (live_after, cash_after) = reserve_pair(&market, repay_asset);
        let principal_paid = repay_credit.checked_sub(receipt.interest_paid).unwrap();
        let debt_reduction = receipt.debt_delta.unsigned_abs();
        assert_eq!(debt_reduction, repay_credit + 1);
        assert_eq!(live_after, live_before - (debt_reduction - principal_paid));
        assert_eq!(cash_after, cash_before + principal_paid);
        market.assert_market_invariants().unwrap();
    }

    #[test]
    fn borrower_risk_valuation_uses_k_ema_depth_cap() {
        let mut market = invariant_market(1_000_000, 1_000_000);
        market.risk = Risk {
            base_price_ema_nad: NAD,
            quote_price_ema_nad: NAD,
            directional_base_price_ema_nad: NAD,
            directional_quote_price_ema_nad: NAD,
            k_ema: (100_000_u128 * NAD as u128).pow(2),
            ..Risk::default()
        };

        let value = market
            .collateral_value_nad(MarketAsset::Base, 50_000, &market.risk)
            .unwrap();
        let expected = crate::math::calculate_normalized_amount_out(
            100_000_u128 * NAD as u128,
            100_000_u128 * NAD as u128,
            50_000_u128 * NAD as u128,
        )
        .unwrap();
        let live_depth_value = crate::math::calculate_normalized_amount_out(
            1_000_000_u128 * NAD as u128,
            1_000_000_u128 * NAD as u128,
            50_000_u128 * NAD as u128,
        )
        .unwrap();

        assert_eq!(value, expected);
        assert!(value < live_depth_value);
    }

    #[test]
    fn daily_limits_use_conservative_k_at_current_spot_ratio() {
        let mut market = invariant_market(4_000_000, 1_000_000);
        market.risk.k_ema = (1_000_000_u128 * NAD as u128).pow(2);

        assert_eq!(
            market
                .daily_limit_for_side(MarketAsset::Base, 2_000)
                .unwrap(),
            400_000
        );
        assert_eq!(
            market
                .daily_limit_for_side(MarketAsset::Quote, 2_000)
                .unwrap(),
            100_000
        );
    }

    #[test]
    fn daily_limits_track_post_swap_reserve_ratio() {
        let mut market = invariant_market(1_000_000, 1_000_000);
        market.risk.k_ema = (1_000_000_u128 * NAD as u128).pow(2);
        let amount_in = 250_000;
        let amount_out = crate::math::calculate_raw_amount_out(1_000_000, 1_000_000, amount_in).unwrap();
        market.base_side.reserves.live_reserve += amount_in;
        market.quote_side.reserves.live_reserve -= amount_out;

        assert_eq!(market.base_side.reserves.live_reserve, 1_250_000);
        assert_eq!(market.quote_side.reserves.live_reserve, 800_000);
        assert_eq!(
            market
                .daily_limit_for_side(MarketAsset::Base, 1_000)
                .unwrap(),
            125_000
        );
        assert_eq!(
            market
                .daily_limit_for_side(MarketAsset::Quote, 1_000)
                .unwrap(),
            80_000
        );
    }

    #[test]
    fn daily_limits_use_live_depth_when_k_ema_is_empty_or_above_spot() {
        let mut market = invariant_market(800_000, 1_200_000);

        assert_eq!(
            market
                .daily_limit_for_side(MarketAsset::Base, 2_500)
                .unwrap(),
            200_000
        );
        assert_eq!(
            market
                .daily_limit_for_side(MarketAsset::Quote, 2_500)
                .unwrap(),
            300_000
        );

        market.risk.k_ema = (2_000_000_u128 * NAD as u128).pow(2);
        assert_eq!(
            market
                .daily_limit_for_side(MarketAsset::Base, 2_500)
                .unwrap(),
            200_000
        );
        assert_eq!(
            market
                .daily_limit_for_side(MarketAsset::Quote, 2_500)
                .unwrap(),
            300_000
        );
    }

    #[test]
    fn daily_limits_follow_k_drawdown_growth_and_proportional_liquidity() {
        let mut market = invariant_market(2_000_000, 2_000_000);
        market.risk.k_ema = (1_000_000_u128 * NAD as u128).pow(2);

        assert_eq!(
            market
                .daily_limit_for_side(MarketAsset::Base, 1_000)
                .unwrap(),
            100_000
        );

        market.base_side.reserves.live_reserve = 500_000;
        market.quote_side.reserves.live_reserve = 500_000;
        assert_eq!(
            market
                .daily_limit_for_side(MarketAsset::Base, 1_000)
                .unwrap(),
            50_000
        );

        market.base_side.reserves.live_reserve = 2_000_000;
        market.quote_side.reserves.live_reserve = 500_000;
        assert_eq!(
            market
                .daily_limit_for_side(MarketAsset::Base, 1_000)
                .unwrap(),
            200_000
        );
        assert_eq!(
            market
                .daily_limit_for_side(MarketAsset::Quote, 1_000)
                .unwrap(),
            50_000
        );
    }

    #[test]
    fn daily_limits_respect_mixed_token_decimals() {
        let mut market = invariant_market(1_000_000_000, 2_000_000_000_000);
        market.base_side.asset_decimals = 6;
        market.quote_side.asset_decimals = 9;

        assert_eq!(
            market
                .daily_limit_for_side(MarketAsset::Base, 1_000)
                .unwrap(),
            100_000_000
        );
        assert_eq!(
            market
                .daily_limit_for_side(MarketAsset::Quote, 1_000)
                .unwrap(),
            200_000_000_000
        );
    }

    #[test]
    fn global_health_contribution_is_debt_capped_and_rounded_down() {
        let mut market = invariant_market(1_000_000, 1_000_000);
        market.refresh_risk().unwrap();

        assert_eq!(
            market
                .debt_capped_global_health_contribution(
                    MarketAsset::Base,
                    100_000,
                    1_000_000,
                    &market.risk,
                )
                .unwrap(),
            150_000
        );
        assert_eq!(
            market
                .debt_capped_global_health_contribution(
                    MarketAsset::Base,
                    100_000,
                    120_000,
                    &market.risk,
                )
                .unwrap(),
            120_000
        );
        assert_eq!(
            market
                .debt_capped_global_health_contribution(MarketAsset::Base, 1, 10, &market.risk)
                .unwrap(),
            1
        );
    }

    #[test]
    fn global_health_cap_remains_bounded_after_collateral_appreciates() {
        let mut market = invariant_market(1_000_000, 1_000_000);
        market.debt.fixed_base_shares = 100_000;
        market.debt.fixed_base_principal = 100_000;
        market.debt.global_health_quote_contribution_for_base_debt = 150_000;
        market.risk = Risk {
            base_price_ema_nad: NAD / 2,
            quote_price_ema_nad: NAD * 2,
            directional_base_price_ema_nad: NAD / 2,
            directional_quote_price_ema_nad: NAD * 2,
            k_ema: (1_000_000_u128 * NAD as u128).pow(2),
            ..Risk::default()
        };

        let health = market.market_health().unwrap();

        assert!(health.base_debt_health_bps <= 15_000);
        assert!(health.effective_base_debt_nad > 0);
    }

    #[test]
    fn deposit_and_repay_update_contribution_without_floating_cf() {
        let mut market = invariant_market(1_000_000, 1_000_000);
        market.config.borrow_market_health_floor_bps = 11_000;
        let mut position = borrow_position_for_debt(MarketAsset::Base, 250_000);

        market.borrow(&mut position, MarketAsset::Base, 100_000, 8_000).unwrap();
        let stored_cf = position.base_liquidation_cf_bps;
        assert_eq!(stored_cf, 8_500);
        assert_eq!(position.global_health_quote_contribution_for_base_debt, 150_000);

        market
            .deposit_collateral(&mut position, MarketAsset::Quote, 100_000)
            .unwrap();
        assert_eq!(position.base_liquidation_cf_bps, stored_cf);
        assert_eq!(position.global_health_quote_contribution_for_base_debt, 150_000);

        market.repay(&mut position, MarketAsset::Base, 50_000).unwrap();
        assert_eq!(position.base_liquidation_cf_bps, stored_cf);
        assert_eq!(position.global_health_quote_contribution_for_base_debt, 75_000);

        market.repay(&mut position, MarketAsset::Base, 50_000).unwrap();
        assert_eq!(position.base_liquidation_cf_bps, 0);
        assert_eq!(position.global_health_quote_contribution_for_base_debt, 0);
        assert_eq!(market.debt.global_health_quote_contribution_for_base_debt, 0);
    }

    #[test]
    fn withdrawal_uses_stored_terms_without_enforcing_global_floor() {
        let mut market = invariant_market(1_000_000, 1_000_000);
        market.config.borrow_market_health_floor_bps = 11_000;
        let mut position = borrow_position_for_debt(MarketAsset::Base, 250_000);
        market.borrow(&mut position, MarketAsset::Base, 100_000, 0).unwrap();
        let stored_cf = position.base_liquidation_cf_bps;

        market.debt.global_health_quote_contribution_for_base_debt = 100_000;
        position.global_health_quote_contribution_for_base_debt = 100_000;
        assert!(market.market_health().unwrap().base_debt_health_bps < 11_000);

        market
            .withdraw_collateral(&mut position, MarketAsset::Quote, 100_000, stored_cf)
            .unwrap();
        assert_eq!(position.quote_collateral, 150_000);
        assert_eq!(position.base_liquidation_cf_bps, stored_cf);
    }

    #[test]
    fn interest_growth_reduces_global_health_without_changing_stored_cf() {
        let mut market = invariant_market(1_000_000, 1_000_000);
        market.config.borrow_market_health_floor_bps = 11_000;
        let mut position = borrow_position_for_debt(MarketAsset::Base, 300_000);
        market.borrow(&mut position, MarketAsset::Base, 100_000, 0).unwrap();
        let stored_cf = position.base_liquidation_cf_bps;
        let contribution = position.global_health_quote_contribution_for_base_debt;
        let health_before = market.market_health().unwrap().base_debt_health_bps;

        market.debt.base_borrow_index_nad = (NAD as u128) * 6 / 5;
        let health_after = market.market_health().unwrap().base_debt_health_bps;

        assert!(health_after < health_before);
        assert_eq!(position.base_liquidation_cf_bps, stored_cf);
        assert_eq!(position.global_health_quote_contribution_for_base_debt, contribution);
    }

    #[test]
    fn alice_exit_does_not_change_bob_terms_and_low_health_pauses_later_borrowing() {
        let mut market = invariant_market(1_000_000, 1_000_000);
        market.config.borrow_market_health_floor_bps = 11_000;
        let mut bob = borrow_position_for_debt(MarketAsset::Base, 30_000);
        let mut alice = borrow_position_for_debt(MarketAsset::Base, 300_000);

        market.borrow(&mut bob, MarketAsset::Base, 20_000, 0).unwrap();
        market.borrow(&mut alice, MarketAsset::Base, 100_000, 0).unwrap();
        let bob_cf = bob.base_liquidation_cf_bps;

        let aggregate_shares = market.debt.fixed_base_shares;
        market.debt.base_borrow_index_nad = (NAD as u128) * 3 / 2;
        add_accrued_cash_backed_interest_to_live_reserve(
            &mut market,
            MarketAsset::Base,
            aggregate_shares,
            120_000,
        );

        market.repay(&mut alice, MarketAsset::Base, 150_000).unwrap();
        market
            .withdraw_collateral(&mut alice, MarketAsset::Quote, 300_000, 0)
            .unwrap();

        assert_eq!(alice.quote_collateral, 0);
        assert_eq!(alice.global_health_quote_contribution_for_base_debt, 0);
        assert_eq!(bob.base_liquidation_cf_bps, bob_cf);
        assert!(market.market_health().unwrap().base_debt_health_bps < 11_000);

        let mut charlie = borrow_position_for_debt(MarketAsset::Base, 100_000);
        let err = market
            .borrow(&mut charlie, MarketAsset::Base, 10_000, 0)
            .unwrap_err();
        assert_eq!(err, anchor_lang::prelude::error!(ErrorCode::InsufficientMarketHealth));
        assert_eq!(bob.base_liquidation_cf_bps, bob_cf);
    }

    #[test]
    fn dynamic_terms_are_decimal_invariant() {
        let mut market = invariant_market(1_000_000_000, 1_000_000_000_000);
        market.base_side.asset_decimals = 6;
        market.quote_side.asset_decimals = 9;
        market.refresh_risk().unwrap();

        let terms = market
            .dynamic_borrow_terms(
                MarketAsset::Base,
                500_000_000_000,
                0,
                0,
                0,
                &market.risk,
            )
            .unwrap();

        assert_eq!(terms.max_debt, 269_166_666);
        assert_eq!(terms.max_cf_bps, 8_075);
        assert_eq!(terms.liquidation_cf_bps, 8_500);
    }

    proptest! {
        #[test]
        fn splitting_positions_cannot_increase_global_health_contribution(
            first_debt in 1_u64..500_000,
            second_debt in 1_u64..500_000,
            first_collateral in 0_u64..1_000_000,
            second_collateral in 0_u64..1_000_000,
        ) {
            let mut market = invariant_market(2_000_000, 2_000_000);
            market.refresh_risk().unwrap();
            let first = market
                .debt_capped_global_health_contribution(
                    MarketAsset::Base,
                    first_debt as u128,
                    first_collateral,
                    &market.risk,
                )
                .unwrap();
            let second = market
                .debt_capped_global_health_contribution(
                    MarketAsset::Base,
                    second_debt as u128,
                    second_collateral,
                    &market.risk,
                )
                .unwrap();
            let combined = market
                .debt_capped_global_health_contribution(
                    MarketAsset::Base,
                    (first_debt + second_debt) as u128,
                    first_collateral + second_collateral,
                    &market.risk,
                )
                .unwrap();

            prop_assert!(first + second <= combined);
        }

        #[test]
        fn conservative_k_depth_and_daily_limit_never_exceed_live_inventory(
            base in 1_000_u64..1_000_000_000,
            quote in 1_000_u64..1_000_000_000,
            k_scale_bps in 1_u128..20_001,
            limit_bps in 0_u16..=BPS_DENOMINATOR,
        ) {
            let mut market = invariant_market(base, quote);
            let spot_k = (base as u128 * NAD as u128)
                .checked_mul(quote as u128 * NAD as u128)
                .unwrap();
            market.risk.k_ema = (spot_k / BPS_DENOMINATOR as u128)
                .checked_mul(k_scale_bps)
                .unwrap();

            let (base_depth, quote_depth) = market
                .conservative_risk_reserve_depths(&market.risk)
                .unwrap();
            prop_assert!(base_depth <= base);
            prop_assert!(quote_depth <= quote);
            prop_assert!(market.daily_limit_for_side(MarketAsset::Base, limit_bps).unwrap() <= base);
            prop_assert!(market.daily_limit_for_side(MarketAsset::Quote, limit_bps).unwrap() <= quote);
        }
    }

    #[test]
    fn repay_routes_interest_out_without_breaking_virtual_reserve_invariant() {
        let mut market = invariant_market(900, 1_000);
        market.base_side.reserves.live_reserve = 1_010;
        market.base_side.shares.ylp_supply = 1_010;
        market.debt.base_borrow_index_nad = (NAD as u128) * 11 / 10;
        market.debt.fixed_base_shares = 100;
        market.debt.fixed_base_principal = 100;
        let mut borrow_position = BorrowPosition {
            owner: Pubkey::new_unique(),
            market: Pubkey::new_unique(),
            position_id: Pubkey::new_unique(),
            base_collateral: 0,
            quote_collateral: 0,
            global_health_base_contribution_for_quote_debt: 0,
            global_health_quote_contribution_for_base_debt: 0,
            base_liquidation_cf_bps: 0,
            quote_liquidation_cf_bps: 0,
            fixed_base_shares: 100,
            fixed_quote_shares: 0,
            auction_start_time: 0,
            auction_start_price_nad: 0,
            auction_floor_price_nad: 0,
            bump: 255,
        };

        let receipt = market
            .repay(&mut borrow_position, MarketAsset::Base, 110)
            .unwrap();

        assert_eq!(receipt.interest_paid, 10);
        assert_eq!(market.base_side.reserves.live_reserve, 1_000);
        assert_eq!(market.base_side.reserves.cash_reserve, 1_000);
        assert_eq!(market.debt.fixed_base_debt().unwrap(), 0);
        market
            .assert_virtual_reserve_invariant(MarketAsset::Base)
            .unwrap();
    }
