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
        let UpdateMarketConfigArgs { config } = args;
        let UpdateMarketConfig {
            market,
            authority_signer,
            ..
        } = ctx.accounts;

        let signer = authority_signer.key();
        let current_slot = Clock::get()?.slot;

        // Schedule the first request, then apply it after the timelock.
        match market.prepare_config_update(signer, config, current_slot)? {
            MarketTimelockAction::Scheduled { execute_after_slot } => {
                emit_cpi!(MarketConfigUpdateScheduled {
                    market: market.key(),
                    execute_after_slot,
                    target_hlp_leverage_bps: config.target_hlp_leverage_bps,
                    swap_fee_bps: config.swap_fee_bps,
                    manager_fee_bps: config.manager_fee_bps,
                    config,
                    metadata: MarketEventMetadata::new(signer, market.key())?,
                });
                return Ok(());
            }
            MarketTimelockAction::Ready => {}
        }

        config.validate()?;

        // Snapshot state for rollback if transition validation fails.
        let previous_config = market.config;
        let previous_base_side = market.base_side;
        let previous_quote_side = market.quote_side;
        let previous_amm = market.amm;
        let previous_debt = market.debt;
        let previous_risk = market.risk;
        let previous_last_marginal_observation_nad = market.last_marginal_observation_nad;
        let previous_curve_revision = market.curve_revision;
        let previous_risk_revision = market.risk_revision;
        let previous_last_update_slot = market.last_update_slot;
        let apply_result = (|| {
            let curve_changed = previous_config.amm.curve_parameters() != config.amm.curve_parameters();
            // Close both elapsed-time intervals under the configuration that
            // governed them. Installing the new half-lives first would
            // retroactively decay the AMM signals and integrate lending risk
            // as though the new configuration had existed since the previous
            // observation.
            market.accrue_interest_to_slot(current_slot)?;
            if market.amm.initialized {
                market
                    .amm
                    .observe_clock_from_validated_config(&previous_config.amm, current_slot)?;
            }
            market.refresh_risk_at_slot(current_slot)?;
            if curve_changed && market.amm.initialized {
                // Schedule only the desired path. A later genuine user
                // operation values and funds each effective ramp point before
                // it can change executable liquidity.
                let applied = market
                    .amm
                    .effective_curve_parameters(&previous_config.amm, current_slot);
                market.amm.start_applied_ramp(applied, &config.amm, current_slot)?;
            }
            market.config = config;
            if market.amm.initialized && !curve_changed && previous_config.amm != config.amm {
                market.amm.invalidate_deferred_controller_target();
            }
            // Adjustment controls and a newly scheduled ramp change the
            // retention requirement even before executable reserves move.
            market.finalize_amm_transition(current_slot)?;
            market.refresh_risk_at_slot(current_slot)?;
            market.assert_market_health()
        })();

        if apply_result.is_err() {
            market.config = previous_config;
            market.base_side = previous_base_side;
            market.quote_side = previous_quote_side;
            market.amm = previous_amm;
            market.debt = previous_debt;
            market.risk = previous_risk;
            market.last_marginal_observation_nad = previous_last_marginal_observation_nad;
            market.curve_revision = previous_curve_revision;
            market.risk_revision = previous_risk_revision;
            market.last_update_slot = previous_last_update_slot;
        }
        apply_result?;
        market.clear_pending_config_update();

        emit_cpi!(MarketUpdated {
            market: market.key(),
            reduce_only: market.reduce_only,
            target_hlp_leverage_bps: market.config.target_hlp_leverage_bps,
            swap_fee_bps: market.config.swap_fee_bps,
            manager_fee_bps: market.config.manager_fee_bps,
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

#[cfg(test)]
mod tests {
    include!("../../tests/instructions/market/update_config.rs");
}
