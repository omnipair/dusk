use super::*;

    fn market_with_roles(manager: Pubkey, operator: Pubkey) -> Market {
        Market {
            version: MARKET_VERSION,
            base_mint: Pubkey::new_unique(),
            quote_mint: Pubkey::new_unique(),
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
            health: MarketHealth::default(),
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
            emergency_exit_haircut_bps: 0,
            ema_half_life_ms: MIN_HALF_LIFE_MS,
            directional_ema_half_life_ms: MIN_HALF_LIFE_MS,
            k_ema_half_life_ms: MIN_HALF_LIFE_MS,
            max_daily_borrow_bps: BPS_DENOMINATOR,
            max_daily_withdraw_bps: BPS_DENOMINATOR,
            spot_ema_divergence_bps: BPS_DENOMINATOR,
            k_ema_drawdown_bps: BPS_DENOMINATOR,
            recognized_collateral_cap_bps: 15_000,
            market_health_min_bps: BPS_DENOMINATOR,
            liquidation_auction_duration_slots: 1_200,
            liquidation_auction_start_incentive_bps: 0,
            hedged_lp_enabled: true,
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
                base_rate_at_target_nad: INTEREST_INITIAL_RATE_AT_TARGET_NAD,
                quote_rate_at_target_nad: INTEREST_INITIAL_RATE_AT_TARGET_NAD,
                ..Debt::default()
            },
            base_hlp_vault: HlpVault::default(),
            quote_hlp_vault: HlpVault::default(),
            risk: Risk::default(),
            health: MarketHealth::default(),
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
        config.recognized_collateral_cap_bps = BPS_DENOMINATOR;
        config.market_health_min_bps = BPS_DENOMINATOR;
        config.ema_half_life_ms = MIN_HALF_LIFE_MS;
        config.directional_ema_half_life_ms = MIN_HALF_LIFE_MS;
        config.k_ema_half_life_ms = MIN_HALF_LIFE_MS;
        config.liquidation_auction_duration_slots = 1_200;
        config.liquidation_auction_start_incentive_bps = 0;

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
        let mut margin_position = MarginPosition {
            owner: Pubkey::new_unique(),
            market: Pubkey::new_unique(),
            base_collateral: 0,
            quote_collateral: 250_000,
            recognized_base_collateral_for_quote_debt: 0,
            recognized_quote_collateral_for_base_debt: 0,
            fixed_base_shares: 0,
            fixed_quote_shares: 0,
            risk_epoch: 0,
            bump: 255,
        };

        market
            .borrow(&mut margin_position, MarketAsset::Base, 100_000, BPS_DENOMINATOR as u64)
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
    fn repay_routes_interest_out_without_breaking_virtual_reserve_invariant() {
        let mut market = invariant_market(900, 1_000);
        market.base_side.reserves.live_reserve = 1_010;
        market.base_side.shares.ylp_supply = 1_010;
        market.debt.base_borrow_index_nad = (NAD as u128) * 11 / 10;
        market.debt.fixed_base_shares = 100;
        market.debt.fixed_base_principal = 100;
        let mut margin_position = MarginPosition {
            owner: Pubkey::new_unique(),
            market: Pubkey::new_unique(),
            base_collateral: 0,
            quote_collateral: 0,
            recognized_base_collateral_for_quote_debt: 0,
            recognized_quote_collateral_for_base_debt: 0,
            fixed_base_shares: 100,
            fixed_quote_shares: 0,
            risk_epoch: 0,
            bump: 255,
        };

        let receipt = market
            .repay(&mut margin_position, MarketAsset::Base, 110)
            .unwrap();

        assert_eq!(receipt.interest_paid, 10);
        assert_eq!(market.base_side.reserves.live_reserve, 1_000);
        assert_eq!(market.base_side.reserves.cash_reserve, 1_000);
        assert_eq!(market.debt.fixed_base_debt().unwrap(), 0);
        market
            .assert_virtual_reserve_invariant(MarketAsset::Base)
            .unwrap();
    }
