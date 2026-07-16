use super::*;

    fn valid_config() -> MarketConfig {
        MarketConfig {
            swap_fee_bps: 30,
            manager_fee_bps: 0,
            protocol_fee_bps: 0,
            target_hlp_leverage_bps: 20_000,
            settlement_divergence_bps: 500,
            ema_half_life_ms: 60_000,
            directional_ema_half_life_ms: 60_000,
            k_ema_half_life_ms: 60_000,
            max_daily_borrow_bps: 2_000,
            utilized_collateral_cap_bps: 15_000,
            market_health_min_bps: 11_000,
            start_time: 0,
        }
    }

    #[test]
    fn market_config_caps_manager_fee_at_five_percent() {
        let mut config = valid_config();
        config.manager_fee_bps = MAX_MANAGER_FEE_BPS;
        config.validate().unwrap();

        config.manager_fee_bps = MAX_MANAGER_FEE_BPS + 1;
        let err = config.validate().unwrap_err();

        assert_eq!(
            err,
            anchor_lang::prelude::error!(ErrorCode::InvalidMarketConfig)
        );
    }

    #[test]
    fn market_config_rejects_utilization_cap_below_health_floor() {
        let mut config = valid_config();
        config.utilized_collateral_cap_bps = 10_000;
        config.market_health_min_bps = 11_000;

        let err = config.validate().unwrap_err();

        assert_eq!(
            err,
            anchor_lang::prelude::error!(ErrorCode::InvalidMarketConfig)
        );
    }

    #[test]
    fn market_config_rejects_inert_ema_half_lives() {
        let mut config = valid_config();
        config.ema_half_life_ms = 0;
        assert_eq!(
            config.validate().unwrap_err(),
            anchor_lang::prelude::error!(ErrorCode::InvalidMarketConfig)
        );

        let mut config = valid_config();
        config.directional_ema_half_life_ms = MIN_HALF_LIFE_MS - 1;
        assert_eq!(
            config.validate().unwrap_err(),
            anchor_lang::prelude::error!(ErrorCode::InvalidMarketConfig)
        );

        let mut config = valid_config();
        config.k_ema_half_life_ms = MAX_HALF_LIFE_MS + 1;
        assert_eq!(
            config.validate().unwrap_err(),
            anchor_lang::prelude::error!(ErrorCode::InvalidMarketConfig)
        );
    }

    #[test]
    fn market_config_rejects_invalid_hlp_leverage() {
        let mut config = valid_config();
        config.target_hlp_leverage_bps = 19_999;

        let err = config.validate().unwrap_err();

        assert_eq!(
            err,
            anchor_lang::prelude::error!(ErrorCode::InvalidMarketConfig)
        );
    }
