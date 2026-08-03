use anchor_lang::prelude::*;

use crate::{
    constants::*,
    events::{MarketConfigUpdateScheduled, MarketEventMetadata, MarketHealthUpdated, MarketUpdated},
    state::{Market, MarketConfig, MarketTimelockAction},
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct UpdateMarketConfigArgs {
    pub config: MarketConfig,
}

#[event_cpi]
#[derive(Accounts)]
pub struct UpdateMarketConfig<'info> {
    #[account(
        mut,
        seeds = [
            MARKET_V2_SEED_PREFIX,
            market.base_side.asset_mint.as_ref(),
            market.quote_side.asset_mint.as_ref(),
            market.params_hash.as_ref(),
        ],
        bump = market.bump
    )]
    pub market: Box<Account<'info, Market>>,

    /// Must be the market manager (checked in the handler).
    pub authority_signer: Signer<'info>,
}

impl<'info> UpdateMarketConfig<'info> {
    pub fn handle_update(ctx: Context<Self>, args: UpdateMarketConfigArgs) -> Result<()> {
        let signer = ctx.accounts.authority_signer.key();
        let current_slot = Clock::get()?.slot;
        let market = &mut ctx.accounts.market;
        match market.prepare_config_update(signer, args.config, current_slot)? {
            MarketTimelockAction::Scheduled { execute_after_slot } => {
                emit_cpi!(MarketConfigUpdateScheduled {
                    market: market.key(),
                    execute_after_slot,
                    target_hlp_leverage_bps: args.config.target_hlp_leverage_bps,
                    swap_fee_bps: args.config.swap_fee_bps,
                    manager_fee_bps: args.config.manager_fee_bps,
                    protocol_fee_bps: args.config.protocol_fee_bps,
                    config: args.config,
                    metadata: MarketEventMetadata::new(signer, market.key())?,
                });
                return Ok(());
            }
            MarketTimelockAction::Ready => {}
        }
        apply_config_update(market, args.config, current_slot)?;
        market.clear_pending_config_update();

        emit_cpi!(MarketUpdated {
            market: market.key(),
            reduce_only: market.reduce_only,
            target_hlp_leverage_bps: market.config.target_hlp_leverage_bps,
            swap_fee_bps: market.config.swap_fee_bps,
            manager_fee_bps: market.config.manager_fee_bps,
            protocol_fee_bps: market.config.protocol_fee_bps,
            config: market.config,
            metadata: MarketEventMetadata::new(signer, market.key())?,
        });
        let health = market.market_health()?;
        emit_cpi!(MarketHealthUpdated {
            market: market.key(),
            global_health_base_contribution_for_quote_debt: health.global_health_base_contribution_for_quote_debt,
            global_health_quote_contribution_for_base_debt: health.global_health_quote_contribution_for_base_debt,
            effective_base_debt_nad: health.effective_base_debt_nad,
            effective_quote_debt_nad: health.effective_quote_debt_nad,
            base_debt_health_bps: health.base_debt_health_bps,
            quote_debt_health_bps: health.quote_debt_health_bps,
            metadata: MarketEventMetadata::new(signer, market.key())?,
        });

        Ok(())
    }
}

fn apply_config_update(market: &mut Market, config: MarketConfig, current_slot: u64) -> Result<()> {
    config.validate()?;
    let previous_config = market.config;
    let previous_amm = market.amm;
    let previous_risk = market.risk;
    let previous_last_update_slot = market.last_update_slot;

    let result = (|| {
        let curve_changed = previous_config.amm.curve_parameters() != config.amm.curve_parameters();
        if curve_changed && market.amm.initialized {
            // This schedules the desired path only. Swap/risk integration must
            // value each candidate and commit it through the protected-profit
            // gate before it becomes effective.
            let applied = market
                .amm
                .effective_curve_parameters(&previous_config.amm, current_slot);
            market.amm.start_applied_ramp(applied, &config.amm, current_slot)?;
        }
        market.config = config;
        // Adjustment controls and a newly scheduled curve ramp can change the
        // protected-liquidity requirement even before executable reserves
        // move. Refresh it atomically with the timelocked config so the first
        // following swap cannot use stale fee-retention routing.
        market.finalize_amm_transition(current_slot)?;
        market.refresh_risk()?;
        market.assert_market_health()
    })();
    if result.is_err() {
        market.config = previous_config;
        market.amm = previous_amm;
        market.risk = previous_risk;
        market.last_update_slot = previous_last_update_slot;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        constants::{BPS_DENOMINATOR, MIN_HALF_LIFE_MS, NAD},
        state::{AmmConfig, AmmCurveParameters, AmmState, MarketSide, ReserveShares, Reserves},
    };

    fn valid_market_config() -> MarketConfig {
        MarketConfig {
            target_hlp_leverage_bps: BPS_DENOMINATOR * 2,
            settlement_divergence_bps: 500,
            ema_half_life_ms: MIN_HALF_LIFE_MS,
            directional_ema_half_life_ms: MIN_HALF_LIFE_MS,
            q_ema_half_life_ms: MIN_HALF_LIFE_MS,
            max_daily_borrow_bps: 2_000,
            global_health_contribution_cap_bps: 15_000,
            borrow_market_health_floor_bps: 11_000,
            amm: AmmConfig::default(),
            ..MarketConfig::default()
        }
    }

    fn concentrated_amm_config() -> AmmConfig {
        AmmConfig {
            peak_depth_nad: 200 * NAD,
            imbalance_scale_nad: NAD / 100,
            center_ema_half_life_ms: MIN_HALF_LIFE_MS,
            volatility_half_life_ms: MIN_HALF_LIFE_MS,
            adjustment_threshold_nad: NAD / 50,
            adjustment_step_nad: NAD / 100,
            min_adjustment_interval_slots: 10,
            ramp_duration_slots: crate::state::MIN_AMM_RAMP_DURATION_SLOTS,
            ..AmmConfig::default()
        }
    }

    fn initialized_market() -> Market {
        let config = valid_market_config();
        let mut market = Market {
            config,
            base_side: MarketSide {
                asset_decimals: 0,
                reserves: Reserves {
                    live_reserve: 1_000_000,
                    cash_reserve: 1_000_000,
                    reserved_liability: 0,
                },
                shares: ReserveShares {
                    ylp_supply: 1_000_000,
                    ..ReserveShares::default()
                },
                ..MarketSide::default()
            },
            quote_side: MarketSide {
                asset_decimals: 0,
                reserves: Reserves {
                    live_reserve: 1_000_000,
                    cash_reserve: 1_000_000,
                    reserved_liability: 0,
                },
                shares: ReserveShares {
                    ylp_supply: 1_000_000,
                    ..ReserveShares::default()
                },
                ..MarketSide::default()
            },
            ..Market::default()
        };
        market.amm = AmmState::initialize(&config.amm, NAD, NAD as u128, 0).unwrap();
        market
    }

    #[test]
    fn applied_config_starts_desired_ramp_without_changing_effective_curve() {
        let mut market = initialized_market();
        let mut target = market.config;
        target.amm = concentrated_amm_config();
        let applied_slot = 100;

        apply_config_update(&mut market, target, applied_slot).unwrap();

        assert!(market.amm.ramp.active);
        assert_eq!(
            market.amm.effective_curve_parameters(&market.config.amm, applied_slot),
            AmmCurveParameters::cpmm()
        );
        assert_eq!(
            market
                .amm
                .desired_curve_parameters(&market.config.amm, market.amm.ramp.end_slot),
            target.amm.curve_parameters()
        );
    }

    #[test]
    fn failed_overlapping_curve_update_restores_config_and_ramp() {
        let mut market = initialized_market();
        let mut first = market.config;
        first.amm = concentrated_amm_config();
        apply_config_update(&mut market, first, 100).unwrap();
        let saved_config = market.config;
        let saved_amm = market.amm;

        let mut overlapping = first;
        overlapping.amm.peak_depth_nad = 400 * NAD;
        assert!(apply_config_update(&mut market, overlapping, 101).is_err());
        assert_eq!(market.config, saved_config);
        assert_eq!(market.amm, saved_amm);
    }

    #[test]
    fn adjustment_config_update_defers_retention_to_bounded_maintenance() {
        let mut config = valid_market_config();
        config.amm = concentrated_amm_config();
        let mut market = initialized_market();
        market.config = config;
        market.amm = AmmState::initialize(&config.amm, NAD, NAD as u128, 100).unwrap();
        // Start off-center: at a perfectly balanced reserve composition the
        // first symmetric center step improves CONCENTRATED Q in either direction and
        // correctly needs no protection. An imbalanced state exercises the
        // impairing next-step target this regression is about.
        market.base_side.reserves.live_reserve = 1_200_000;
        market.base_side.reserves.cash_reserve = 1_200_000;
        market.checkpoint_amm_neutral_inventory(100).unwrap();
        assert!(market.amm.retention_target_stale);
        assert!(market.amm.retain_dynamic_surcharge);

        let mut disabled = config;
        disabled.amm.adjustment_threshold_nad = 0;
        disabled.amm.adjustment_step_nad = 0;
        disabled.amm.min_adjustment_interval_slots = 0;
        apply_config_update(&mut market, disabled, 200).unwrap();

        assert_eq!(market.amm.retention_required_nad, 0);
        assert!(!market.amm.retain_dynamic_surcharge);
    }
}
