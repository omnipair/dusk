pub(crate) mod curve;
pub(crate) mod fees;

pub(crate) use curve::*;
pub(crate) use fees::*;

use anchor_lang::prelude::*;

use crate::{constants::*, errors::ErrorCode, math::*, state::*};

use super::liquidity::{
    apply_compounded_ylp_fee, quote_integrated_exact_in_with_frozen_fee, IntegratedCurveState,
    IntegratedFrozenFeeQuote, IntegratedSwapDirection,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AmmPreviewMetrics {
    pub executable_base_reserve: u64,
    pub executable_quote_reserve: u64,
    pub curve_depth_nad: u128,
    pub lower_range_price_nad: u64,
    pub upper_range_price_nad: u64,
    pub concentrated_curve_branch: u8,
    pub ordinary_base_reserve_nad: u128,
    pub ordinary_quote_reserve_nad: u128,
    pub final_spot_price_nad: u64,
}

impl Market {
    pub(crate) fn amm_preview_metrics(&self, fallback_final_spot_price_nad: u64) -> Result<AmmPreviewMetrics> {
        if !self.amm.initialized {
            return Ok(AmmPreviewMetrics {
                final_spot_price_nad: fallback_final_spot_price_nad,
                ..Default::default()
            });
        }
        let geometry = self.amm.concentrated_curve_cache.geometry()?;
        let ordinary = self.integrated_curve_state_nad()?;
        let (lower, upper) = geometry.range_prices_nad()?.unwrap_or((0, 0));
        Ok(AmmPreviewMetrics {
            executable_base_reserve: self.curve_reserve(MarketAsset::Base)?,
            executable_quote_reserve: self.curve_reserve(MarketAsset::Quote)?,
            curve_depth_nad: self
                .amm
                .concentrated_curve_cache
                .tail_liquidity
                .checked_add(self.amm.concentrated_curve_cache.concentrated_liquidity)
                .ok_or(ErrorCode::InvariantOverflow)?,
            lower_range_price_nad: u64::try_from(lower).map_err(|_| ErrorCode::MarketMathOverflow)?,
            upper_range_price_nad: u64::try_from(upper).map_err(|_| ErrorCode::MarketMathOverflow)?,
            concentrated_curve_branch: geometry
                .branch(ConcentratedCurvePoint {
                    base_reserve: ordinary.ordinary_base,
                    quote_reserve: ordinary.ordinary_quote,
                })
                .code(),
            ordinary_base_reserve_nad: ordinary.ordinary_base,
            ordinary_quote_reserve_nad: ordinary.ordinary_quote,
            final_spot_price_nad: self
                .current_concentrated_spot_price_nad()?
                .unwrap_or(fallback_final_spot_price_nad),
        })
    }

    pub(crate) fn liquidity_nad(&self) -> Result<u128> {
        geometric_mean_floor(
            normalize_to_nad(
                self.base_side.reserves.live_reserve as u128,
                self.base_side.asset_decimals,
            )?,
            normalize_to_nad(
                self.quote_side.reserves.live_reserve as u128,
                self.quote_side.asset_decimals,
            )?,
        )
    }
}

impl Market {
    /// Locks retained toxicity surcharge outside executable/yLP inventory.
    /// The physical reserve vault already received these atoms from the
    /// trader; this ledger is the exclusive ownership claim until a funded
    /// recenter deploys the bucket.
    pub(crate) fn credit_protected_recenter_reserve(&mut self, asset: MarketAsset, amount: u64) -> Result<()> {
        if amount == 0 {
            return Ok(());
        }
        let protected = &mut self.side_mut(asset).reserves.protected_recenter_reserve;
        *protected = protected.checked_add(amount).ok_or(ErrorCode::ReserveOverflow)?;
        self.amm.mark_retention_target_stale();
        Ok(())
    }

    pub(crate) fn advance_curve_revision(&mut self) -> Result<()> {
        self.curve_revision = self
            .curve_revision
            .checked_add(1)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        Ok(())
    }

    /// Reserve mutations invalidate a previously projected center/ramp cost.
    /// Keep the last evaluated target sticky and retain dynamic surcharge until
    /// the next genuine user operation values the actual move.
    /// CPMM and controller-disabled pools have no concentration impairment, so
    /// their surcharge remains immediately claimable.
    pub(crate) fn defer_amm_retention_target(&mut self) -> Result<()> {
        if !self.amm.initialized {
            return Ok(());
        }
        let parameters = self.config.amm.concentrated_curve_parameters()?;
        if !parameters.is_cpmm() && self.config.amm.adjustment_step_nad > 0 {
            self.amm.mark_retention_target_stale();
        } else {
            self.amm
                .refresh_retention_target(self.amm.curve_depth_per_share_nad, 0)?;
        }
        Ok(())
    }

    /// Lazily initializes the oracle-less AMM from the first complete
    /// two-sided liquidity state. No external price is consulted.
    pub(crate) fn ensure_amm_initialized(&mut self, current_slot: u64) -> Result<bool> {
        if self.amm.initialized {
            return Ok(false);
        }
        let reserves = self.curve_reserves_nad()?;
        if reserves.base == 0 && reserves.quote == 0 {
            return Ok(false);
        }
        require!(
            reserves.base > 0 && reserves.quote > 0,
            ErrorCode::InsufficientLiquidity
        );
        require!(self.base_side.shares.ylp_supply > 0, ErrorCode::SupplyUnderflow);

        let center_price_nad = self.current_curve_center_price_nad()?;
        let parameters = self.config.amm.concentrated_curve_parameters()?;
        let ordinary = self.integrated_curve_state_nad()?;
        let concentrated_curve_cache = prepare_concentrated_cache_at_point(
            ordinary.ordinary_base,
            ordinary.ordinary_quote,
            center_price_nad,
            parameters,
        )?;
        let curve_depth_nad = concentrated_curve_cache
            .tail_liquidity
            .checked_add(concentrated_curve_cache.concentrated_liquidity)
            .ok_or(ErrorCode::InvariantOverflow)?;
        let curve_depth_per_share_nad = self.curve_depth_per_share_nad(curve_depth_nad)?;
        let launch_reference_price_nad = self.amm.launch_reference_price_nad;
        let launch_fee_progress_offset = self.amm.launch_fee_progress_offset;
        let mut state = AmmState::initialize(
            &self.config.amm,
            center_price_nad,
            curve_depth_per_share_nad,
            current_slot,
        )?;
        state.launch_reference_price_nad = launch_reference_price_nad;
        state.launch_fee_progress_offset = launch_fee_progress_offset;
        state.concentrated_curve_cache = concentrated_curve_cache;
        state.refresh_retention_target(curve_depth_per_share_nad, 0)?;
        self.amm = state;
        Ok(true)
    }

    fn refresh_concentrated_curve_cache(&mut self, current_slot: u64, loss: bool) -> Result<(u64, u128)> {
        self.ensure_amm_initialized(current_slot)?;
        require!(self.amm.initialized, ErrorCode::BrokenInvariant);
        let ordinary = self.integrated_curve_state_nad()?;
        let cache = prepare_concentrated_cache_at_point(
            ordinary.ordinary_base,
            ordinary.ordinary_quote,
            self.current_curve_center_price_nad()?,
            self.config.amm.concentrated_curve_parameters()?,
        )?;
        let curve_depth_nad = cache
            .tail_liquidity
            .checked_add(cache.concentrated_liquidity)
            .ok_or(ErrorCode::InvariantOverflow)?;
        let curve_depth_per_share_nad = self.curve_depth_per_share_nad(curve_depth_nad)?;
        self.amm.concentrated_curve_cache = cache;
        if loss {
            self.amm.checkpoint_recenter_or_loss(curve_depth_per_share_nad);
        } else {
            self.amm.checkpoint_neutral_liquidity(curve_depth_per_share_nad);
        }
        let price_nad = self
            .current_concentrated_spot_price_nad()?
            .ok_or(ErrorCode::BrokenInvariant)?;
        Ok((price_nad, curve_depth_nad))
    }

    /// Same accounting checkpoint without recomputing the forward protection
    /// target. Composite transitions use this between internal legs and
    /// refresh once after their final reserve state is known.
    pub(crate) fn checkpoint_amm_neutral_inventory_raw(&mut self, current_slot: u64) -> Result<()> {
        self.ensure_amm_initialized(current_slot)?;
        if !self.amm.initialized {
            return Ok(());
        }
        self.refresh_concentrated_curve_cache(current_slot, false)?;
        Ok(())
    }

    /// Socialized-loss checkpoint for concentrated curve state. Accrued unpaid interest has already
    /// been removed from curve reserves, so only actual executable-liquidity
    /// loss consumes protected profit.
    pub(crate) fn checkpoint_amm_socialized_loss_raw(&mut self, current_slot: u64) -> Result<(u64, u128)> {
        self.refresh_concentrated_curve_cache(current_slot, true)
    }

    /// Finalize an atomic share/reserve haircut. Unlike the raw checkpoint
    /// used inside multi-leg leverage settlement, this is a complete public
    /// transition: retention state, revision, empty-market state, and risk all
    /// advance together.
    pub(crate) fn finalize_amm_socialized_loss_and_observe_risk(&mut self, current_slot: u64) -> Result<()> {
        if self.base_side.shares.ylp_supply == MIN_LIQUIDITY {
            return self.finalize_amm_transition_and_observe_risk(current_slot);
        }
        let (price_nad, curve_depth_nad) = self.checkpoint_amm_socialized_loss_raw(current_slot)?;
        self.defer_amm_retention_target()?;
        self.advance_curve_revision()?;
        self.observe_risk_from_concentrated_curve(price_nad, curve_depth_nad, current_slot)?;
        self.risk_revision = self.curve_revision;
        Ok(())
    }

    /// Finalizes a complete non-trade transition. First liquidity is already fully
    /// checkpointed by initialization and has no stale forward target.
    pub(crate) fn finalize_amm_transition(&mut self, current_slot: u64) -> Result<()> {
        if self.ensure_amm_initialized(current_slot)? {
            self.advance_curve_revision()?;
            return Ok(());
        }
        if !self.amm.initialized {
            return Ok(());
        }
        self.checkpoint_amm_neutral_inventory_raw(current_slot)?;
        self.defer_amm_retention_target()?;
        self.advance_curve_revision()
    }

    /// Finalizes a non-trade reserve/share mutation and records the exact
    /// applied curve from one evaluation, sharing one canonical result between
    /// transition accounting and risk observation.
    pub(crate) fn finalize_amm_transition_and_observe_risk(&mut self, current_slot: u64) -> Result<()> {
        if self.base_side.shares.ylp_supply == MIN_LIQUIDITY {
            require_eq!(
                self.base_side.shares.ylp_supply,
                self.quote_side.shares.ylp_supply,
                ErrorCode::BrokenInvariant
            );
            require!(
                self.debt.fixed_base_shares == 0
                    && self.debt.fixed_quote_shares == 0
                    && self.debt.isolated_base_shares == 0
                    && self.debt.isolated_quote_shares == 0
                    && self.debt.fixed_base_principal == 0
                    && self.debt.fixed_quote_principal == 0
                    && self.debt.isolated_base_principal == 0
                    && self.debt.isolated_quote_principal == 0
                    && self.base_side.reserves.live_reserve == self.base_side.reserves.cash_reserve
                    && self.quote_side.reserves.live_reserve == self.quote_side.reserves.cash_reserve
                    && !self.has_active_hlp(),
                ErrorCode::InsufficientLiquidity
            );
            self.amm.concentrated_curve_cache = Default::default();
            self.amm.curve_depth_per_share_nad = 0;
            self.amm.protected_floor_per_share_nad = 0;
            self.amm.retention_required_nad = 0;
            self.amm.retention_stop_nad = 0;
            self.amm.retention_hard_cap_nad = 0;
            self.amm.retain_dynamic_surcharge = false;
            self.amm.retention_target_saturated = false;
            self.amm.retention_target_stale = false;
            self.amm.deferred_controller_target.clear();
            self.risk = Default::default();
            self.advance_curve_revision()?;
            self.risk_revision = self.curve_revision;
            self.last_update_slot = current_slot;
            return Ok(());
        }
        let reserves = self.curve_reserves_nad()?;
        if reserves.base == 0 && reserves.quote == 0 {
            return Ok(());
        }
        require!(
            reserves.base > 0 && reserves.quote > 0,
            ErrorCode::InsufficientLiquidity
        );
        require!(self.base_side.shares.ylp_supply > 0, ErrorCode::SupplyUnderflow);

        if !self.amm.initialized {
            self.ensure_amm_initialized(current_slot)?;
        } else {
            self.refresh_concentrated_curve_cache(current_slot, false)?;
            self.defer_amm_retention_target()?;
        }
        self.advance_curve_revision()?;
        let curve_depth_nad = self
            .amm
            .concentrated_curve_cache
            .tail_liquidity
            .checked_add(self.amm.concentrated_curve_cache.concentrated_liquidity)
            .ok_or(ErrorCode::InvariantOverflow)?;
        let price_nad = self
            .current_concentrated_spot_price_nad()?
            .ok_or(ErrorCode::BrokenInvariant)?;
        self.observe_risk_from_concentrated_curve(price_nad, curve_depth_nad, current_slot)?;
        self.risk_revision = self.curve_revision;
        Ok(())
    }

    /// Initializes clock-driven AMM state. Parameter ramps and center moves are
    /// evaluated lazily by genuine user operations.
    pub(crate) fn advance_amm_clock(&mut self, current_slot: u64) -> Result<()> {
        self.ensure_amm_initialized(current_slot)?;
        if self.amm.initialized {
            self.amm
                .observe_clock_from_validated_config(&self.config.amm, current_slot)?;
        }
        Ok(())
    }

    /// Advances clock-only signals before a swap-like operation. The caller
    /// invokes `advance_one_amm_controller_target` before hLP pre-positioning;
    /// hLP residual safety therefore covers the complete controller-plus-trade
    /// path and positioning observes the applied curve.
    pub(crate) fn prepare_amm_for_swap(&mut self, current_slot: u64) -> Result<()> {
        self.advance_amm_clock(current_slot)
    }

    /// Lazily advances at most one already-authorized parameter-ramp or center
    /// target. No transition depends on a keeper or auxiliary instruction.
    pub(crate) fn advance_one_amm_controller_target(&mut self, current_slot: u64) -> Result<bool> {
        if !self.amm.initialized {
            return Ok(false);
        }
        let parameters = self.config.amm.concentrated_curve_parameters()?;
        self.advance_one_concentrated_controller_target(current_slot, parameters)
    }

    /// Applies at most one sticky-center move for the concentrated curve. The
    /// candidate geometry is reconstructed from unchanged ordinary reserves
    /// by a closed-form branch quadratic, then admitted through the existing
    /// protected-profit budget. It affects only later swaps.
    fn advance_one_concentrated_controller_target(
        &mut self,
        current_slot: u64,
        parameters: ConcentratedCurveParameters,
    ) -> Result<bool> {
        if parameters.is_cpmm() || self.config.amm.adjustment_step_nad == 0 {
            self.amm.deferred_controller_target.clear();
            self.amm
                .refresh_retention_target(self.amm.curve_depth_per_share_nad, 0)?;
            return Ok(false);
        }

        let ordinary = self.integrated_curve_state_nad()?;
        let mut candidate_center = 0_u64;
        let mut pending = self.amm.deferred_controller_target;
        if pending.is_active() {
            require_eq!(
                pending.kind,
                DeferredControllerTarget::RECENTER,
                ErrorCode::BrokenInvariant
            );
            if pending.created_slot >= current_slot {
                return Ok(false);
            }
            let center = self.amm.center_price_nad;
            let ema = self.amm.price_ema_nad;
            let distance = symmetric_distance_nad(center, ema)?;
            let reachable = if pending.center_price_nad > center {
                ema >= pending.center_price_nad
            } else {
                ema <= pending.center_price_nad
            };
            if distance < self.config.amm.adjustment_threshold_nad as u128 || !reachable {
                self.amm.deferred_controller_target.clear();
                self.amm
                    .refresh_retention_target(self.amm.curve_depth_per_share_nad, 0)?;
                pending.clear();
            } else if pending.saturated
                && self.base_side.reserves.protected_recenter_reserve == 0
                && self.quote_side.reserves.protected_recenter_reserve == 0
            {
                return Ok(false);
            } else {
                candidate_center = pending.center_price_nad;
            }
        }

        if candidate_center == 0 {
            let earliest = self
                .amm
                .last_adjustment_slot
                .checked_add(self.config.amm.min_adjustment_interval_slots)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            if current_slot < earliest
                || symmetric_distance_nad(self.amm.center_price_nad, self.amm.price_ema_nad)?
                    < self.config.amm.adjustment_threshold_nad as u128
            {
                return Ok(false);
            }
            let center = self.amm.center_price_nad;
            let target = self.amm.price_ema_nad;
            let step_nad = self.config.amm.adjustment_step_nad;
            candidate_center = if target > center {
                let stepped = ceil_div(
                    (center as u128)
                        .checked_mul(NAD.checked_add(step_nad).ok_or(ErrorCode::MarketMathOverflow)? as u128)
                        .ok_or(ErrorCode::MarketMathOverflow)?,
                    NAD as u128,
                )
                .ok_or(ErrorCode::MarketMathOverflow)?;
                u64::try_from(stepped)
                    .map_err(|_| ErrorCode::MarketMathOverflow)?
                    .min(target)
            } else if target < center {
                let down = (center as u128)
                    .checked_mul(NAD as u128)
                    .and_then(|value| value.checked_div((NAD + step_nad) as u128))
                    .ok_or(ErrorCode::MarketMathOverflow)?
                    .max(1);
                u64::try_from(down)
                    .map_err(|_| ErrorCode::MarketMathOverflow)?
                    .max(target)
            } else {
                return Ok(false);
            };
        }

        let protected_base = self.base_side.reserves.protected_recenter_reserve;
        let protected_quote = self.quote_side.reserves.protected_recenter_reserve;
        let deploying_protected = protected_base > 0 || protected_quote > 0;
        let mut candidate_point = ordinary;
        if deploying_protected {
            candidate_point.ordinary_base = candidate_point
                .ordinary_base
                .checked_add(normalize_to_nad(protected_base as u128, self.base_side.asset_decimals)?)
                .ok_or(ErrorCode::ReserveOverflow)?;
            candidate_point.ordinary_quote = candidate_point
                .ordinary_quote
                .checked_add(normalize_to_nad(
                    protected_quote as u128,
                    self.quote_side.asset_decimals,
                )?)
                .ok_or(ErrorCode::ReserveOverflow)?;
        }
        let candidate_cache = prepare_concentrated_cache_at_point(
            candidate_point.ordinary_base,
            candidate_point.ordinary_quote,
            candidate_center,
            parameters,
        )?;
        let candidate_curve_depth = candidate_cache
            .tail_liquidity
            .checked_add(candidate_cache.concentrated_liquidity)
            .ok_or(ErrorCode::InvariantOverflow)?;
        let candidate_curve_depth_per_share = self.curve_depth_per_share_nad(candidate_curve_depth)?;
        let covered = covered_impairment_nad(self.amm.curve_depth_per_share_nad, candidate_curve_depth_per_share)?;
        // A protected bucket is sufficient only when deploying every locked
        // atom leaves yLP curve principal no lower than before the center
        // move. This avoids assigning a fungible token-value conversion to
        // the curve-depth metric and makes the funding proof exact.
        let protected_funds_move =
            deploying_protected && candidate_curve_depth_per_share >= self.amm.curve_depth_per_share_nad;
        if protected_funds_move || self.amm.recenter_is_funded(covered) {
            // Validate the fallible commit domain before making the physical
            // bucket executable. On-chain rollback is still the outer atomic
            // boundary; these checks also keep native callers disciplined.
            self.config.amm.validate()?;
            candidate_cache.geometry()?;
            let earliest = self
                .amm
                .last_adjustment_slot
                .checked_add(self.config.amm.min_adjustment_interval_slots)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            require_gte!(current_slot, earliest, ErrorCode::InvalidArgument);
            if protected_funds_move {
                let new_base_live = self
                    .base_side
                    .reserves
                    .live_reserve
                    .checked_add(protected_base)
                    .ok_or(ErrorCode::ReserveOverflow)?;
                let new_base_cash = self
                    .base_side
                    .reserves
                    .cash_reserve
                    .checked_add(protected_base)
                    .ok_or(ErrorCode::ReserveOverflow)?;
                let new_quote_live = self
                    .quote_side
                    .reserves
                    .live_reserve
                    .checked_add(protected_quote)
                    .ok_or(ErrorCode::ReserveOverflow)?;
                let new_quote_cash = self
                    .quote_side
                    .reserves
                    .cash_reserve
                    .checked_add(protected_quote)
                    .ok_or(ErrorCode::ReserveOverflow)?;
                self.base_side.reserves.live_reserve = new_base_live;
                self.base_side.reserves.cash_reserve = new_base_cash;
                self.quote_side.reserves.live_reserve = new_quote_live;
                self.quote_side.reserves.cash_reserve = new_quote_cash;
                self.base_side.reserves.protected_recenter_reserve = 0;
                self.quote_side.reserves.protected_recenter_reserve = 0;
            }
            self.amm.deferred_controller_target.clear();
            self.amm.commit_concentrated_recenter(
                &self.config.amm,
                candidate_center,
                candidate_cache,
                candidate_curve_depth_per_share,
                if protected_funds_move { 0 } else { covered },
                current_slot,
            )?;
            self.defer_amm_retention_target()?;
            return Ok(true);
        }

        let impairment = self
            .amm
            .curve_depth_per_share_nad
            .saturating_sub(candidate_curve_depth_per_share);
        let target = self
            .amm
            .refresh_retention_target(self.amm.curve_depth_per_share_nad, impairment)?;
        self.amm.deferred_controller_target = DeferredControllerTarget {
            kind: DeferredControllerTarget::RECENTER,
            center_price_nad: candidate_center,
            required_nad: target.required_nad,
            evaluated_base_reserve_nad: ordinary.ordinary_base,
            evaluated_quote_reserve_nad: ordinary.ordinary_quote,
            created_slot: current_slot,
            saturated: target.saturated,
        };
        Ok(false)
    }

    /// Reconstructs a governance-selected concentrated shape from unchanged
    /// ordinary reserves. Parameter changes are admitted only when the
    /// existing protected-profit budget covers any principal impairment.
    pub(crate) fn apply_concentrated_curve_parameter_update(&mut self, current_slot: u64) -> Result<()> {
        let parameters = self.config.amm.concentrated_curve_parameters()?;
        let ordinary = self.integrated_curve_state_nad()?;
        let cache = prepare_concentrated_cache_at_point(
            ordinary.ordinary_base,
            ordinary.ordinary_quote,
            self.amm.center_price_nad,
            parameters,
        )?;
        let curve_depth_nad = cache
            .tail_liquidity
            .checked_add(cache.concentrated_liquidity)
            .ok_or(ErrorCode::InvariantOverflow)?;
        let curve_depth_per_share_nad = self.curve_depth_per_share_nad(curve_depth_nad)?;
        let covered = covered_impairment_nad(self.amm.curve_depth_per_share_nad, curve_depth_per_share_nad)?;
        require!(self.amm.recenter_is_funded(covered), ErrorCode::BrokenInvariant);

        self.amm.concentrated_curve_cache = cache;
        self.amm.checkpoint_recenter_or_loss(curve_depth_per_share_nad);
        self.amm.deferred_controller_target.clear();
        self.defer_amm_retention_target()?;
        self.advance_curve_revision()?;
        let price_nad = self
            .current_concentrated_spot_price_nad()?
            .ok_or(ErrorCode::BrokenInvariant)?;
        self.observe_risk_from_concentrated_curve(price_nad, curve_depth_nad, current_slot)?;
        self.risk_revision = self.curve_revision;
        Ok(())
    }

    /// Finalizes internal observations after the whole
    /// hLP-pre-solve → swap → hLP-post-solve lifecycle is complete.
    ///
    /// Only the frozen trader-visible AMM path contributes volatility.
    /// Internal hLP settlement, ramp admission, and recentering may change the
    /// pool's next marginal price, but those changes are not external flow.
    #[cfg(test)]
    pub(crate) fn finalize_amm_trade(
        &mut self,
        trade_start_price_nad: u64,
        trade_end_price_nad: u64,
        current_slot: u64,
    ) -> Result<()> {
        self.checkpoint_amm_neutral_inventory_raw(current_slot)?;
        self.finalize_amm_trade_after_inventory_checkpoint(trade_start_price_nad, trade_end_price_nad, current_slot)?;
        Ok(())
    }

    /// Finalizes a trade whose complete post-trade executable inventory was
    /// already checkpointed by the immediately preceding reserve mutation.
    ///
    /// Spot reserve application and `apply_leverage_swap` checkpoint first the
    /// invariant-preserving trade and then any retained surcharge. Their
    /// remaining fee-liability writes do not change curve reserves or yLP
    /// supply, so recomputing the same curve-depth ratio here would be redundant and
    /// prohibitively expensive on the concentrated path.
    pub(crate) fn finalize_amm_trade_after_inventory_checkpoint(
        &mut self,
        trade_start_price_nad: u64,
        trade_end_price_nad: u64,
        current_slot: u64,
    ) -> Result<()> {
        if !self.amm.initialized {
            return Ok(());
        }
        self.amm.checkpoint_trade(
            &self.config.amm,
            trade_start_price_nad,
            trade_end_price_nad,
            current_slot,
        )?;
        self.defer_amm_retention_target()?;
        self.advance_curve_revision()
    }
}

fn covered_impairment_nad(current_depth: u128, candidate_depth: u128) -> Result<u128> {
    let impairment = current_depth.saturating_sub(candidate_depth);
    if impairment == 0 {
        return Ok(0);
    }
    let covered = mul_bps_ceil(impairment, PROTECTED_LIQUIDITY_COVERAGE_BPS)?;
    covered
        .checked_add(mul_bps_ceil(current_depth, PROTECTED_LIQUIDITY_GUARD_BPS)?)
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

fn mul_bps_ceil(value: u128, bps: u16) -> Result<u128> {
    ceil_div(
        value.checked_mul(bps as u128).ok_or(ErrorCode::MarketMathOverflow)?,
        BPS_DENOMINATOR as u128,
    )
    .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

fn symmetric_distance_nad(first: u64, second: u64) -> Result<u128> {
    require!(first > 0 && second > 0, ErrorCode::InvalidSettlementPrice);
    let high = first.max(second) as u128;
    let low = first.min(second) as u128;
    ceil_div(high.checked_mul(NAD as u128).ok_or(ErrorCode::MarketMathOverflow)?, low)
        .and_then(|ratio| ratio.checked_sub(NAD as u128))
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

#[cfg(test)]
mod amm_engine_tests {
    include!("../../tests/transitions/amm.rs");
}

/// Executable AMM inventory. Unlike `live_reserve`, these coordinates exclude
/// accrued-but-unpaid lending interest because interest is claimable yield,
/// not compounding swap principal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CurveReservesNad {
    pub base: u128,
    pub quote: u128,
}

impl Market {
    /// Aggregate public fixed and isolated interest which has not yet been
    /// paid into the non-compounding interest vault. hLP funding interest is a
    /// separate debt cost and is deliberately excluded. Debt-share rounding
    /// can momentarily put tracked principal above computed debt, so principal
    /// is clamped first.
    pub(crate) fn unrealized_interest(&self, asset: MarketAsset) -> Result<u128> {
        let (fixed_debt, fixed_principal, isolated_debt, isolated_principal) = match asset {
            MarketAsset::Base => (
                self.debt.fixed_base_debt()?,
                u128::from(self.debt.fixed_base_principal),
                self.debt.isolated_debt(MarketAsset::Base)?,
                u128::from(self.debt.isolated_base_principal),
            ),
            MarketAsset::Quote => (
                self.debt.fixed_quote_debt()?,
                u128::from(self.debt.fixed_quote_principal),
                self.debt.isolated_debt(MarketAsset::Quote)?,
                u128::from(self.debt.isolated_quote_principal),
            ),
        };
        fixed_debt
            .checked_sub(fixed_principal.min(fixed_debt))
            .and_then(|fixed_interest| {
                isolated_debt
                    .checked_sub(isolated_principal.min(isolated_debt))
                    .and_then(|isolated_interest| fixed_interest.checked_add(isolated_interest))
            })
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    }

    pub(crate) fn curve_reserve(&self, asset: MarketAsset) -> Result<u64> {
        let live_reserve = self.side(asset).reserves.live_reserve as u128;
        let curve_reserve = live_reserve
            .checked_sub(self.unrealized_interest(asset)?)
            .ok_or(ErrorCode::BrokenInvariant)?;
        u64::try_from(curve_reserve).map_err(|_| ErrorCode::MarketMathOverflow.into())
    }

    pub(crate) fn curve_reserves_nad(&self) -> Result<CurveReservesNad> {
        Ok(CurveReservesNad {
            base: normalize_to_nad(
                self.curve_reserve(MarketAsset::Base)? as u128,
                self.base_side.asset_decimals,
            )?,
            quote: normalize_to_nad(
                self.curve_reserve(MarketAsset::Quote)? as u128,
                self.quote_side.asset_decimals,
            )?,
        })
    }

    pub(crate) fn integrated_curve_state_nad(&self) -> Result<IntegratedCurveState> {
        let reserves = self.curve_reserves_nad()?;
        let supply = self.base_side.shares.ylp_supply;
        require_eq!(supply, self.quote_side.shares.ylp_supply, ErrorCode::BrokenInvariant);
        require!(supply > 0, ErrorCode::SupplyUnderflow);

        let base_hlp_base_claim = mul_div_u128(reserves.base, self.base_hlp_vault.ylp_shares as u128, supply as u128)?;
        let base_hlp_quote_claim =
            mul_div_u128(reserves.quote, self.base_hlp_vault.ylp_shares as u128, supply as u128)?;
        let quote_hlp_base_claim =
            mul_div_u128(reserves.base, self.quote_hlp_vault.ylp_shares as u128, supply as u128)?;
        let quote_hlp_quote_claim =
            mul_div_u128(reserves.quote, self.quote_hlp_vault.ylp_shares as u128, supply as u128)?;
        let base_hlp_quote_debt = normalize_to_nad(
            Debt::shares_to_debt(self.base_hlp_vault.debt_shares, self.debt.quote_borrow_index_nad)?,
            self.quote_side.asset_decimals,
        )?;
        let quote_hlp_base_debt = normalize_to_nad(
            Debt::shares_to_debt(self.quote_hlp_vault.debt_shares, self.debt.base_borrow_index_nad)?,
            self.base_side.asset_decimals,
        )?;

        // Preserve each vault's actual point-in-time NAV while reconstructing
        // the zero-opposite-exposure endpoint. This is deliberately derived
        // from current claims and indexed debt rather than `last_nav_nad`,
        // which is only a settlement checkpoint and may be stale after a
        // price move or interest accrual.
        let base_opposite_net = if base_hlp_quote_claim >= base_hlp_quote_debt {
            i128::try_from(base_hlp_quote_claim - base_hlp_quote_debt).map_err(|_| ErrorCode::MarketMathOverflow)?
        } else {
            -i128::try_from(base_hlp_quote_debt - base_hlp_quote_claim).map_err(|_| ErrorCode::MarketMathOverflow)?
        };
        let quote_opposite_net = if quote_hlp_base_claim >= quote_hlp_base_debt {
            i128::try_from(quote_hlp_base_claim - quote_hlp_base_debt).map_err(|_| ErrorCode::MarketMathOverflow)?
        } else {
            -i128::try_from(quote_hlp_base_debt - quote_hlp_base_claim).map_err(|_| ErrorCode::MarketMathOverflow)?
        };
        let base_opposite_value = if base_opposite_net >= 0 {
            i128::try_from(mul_div_u128(base_opposite_net as u128, reserves.base, reserves.quote)?)
                .map_err(|_| ErrorCode::MarketMathOverflow)?
        } else {
            -i128::try_from(mul_div_u128(
                base_opposite_net.unsigned_abs(),
                reserves.base,
                reserves.quote,
            )?)
            .map_err(|_| ErrorCode::MarketMathOverflow)?
        };
        let quote_opposite_value = if quote_opposite_net >= 0 {
            i128::try_from(mul_div_u128(quote_opposite_net as u128, reserves.quote, reserves.base)?)
                .map_err(|_| ErrorCode::MarketMathOverflow)?
        } else {
            -i128::try_from(mul_div_u128(
                quote_opposite_net.unsigned_abs(),
                reserves.quote,
                reserves.base,
            )?)
            .map_err(|_| ErrorCode::MarketMathOverflow)?
        };
        let base_hlp_equity = i128::try_from(base_hlp_base_claim)
            .map_err(|_| ErrorCode::MarketMathOverflow)?
            .checked_add(base_opposite_value)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let quote_hlp_equity = i128::try_from(quote_hlp_quote_claim)
            .map_err(|_| ErrorCode::MarketMathOverflow)?
            .checked_add(quote_opposite_value)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require!(
            base_hlp_equity >= 0 && quote_hlp_equity >= 0,
            ErrorCode::HlpSettlementUnavailable
        );
        IntegratedCurveState::from_total_reserves(
            reserves.base,
            reserves.quote,
            if self.base_hlp_vault.hlp_supply == 0 {
                0
            } else {
                base_hlp_equity as u128
            },
            if self.quote_hlp_vault.hlp_supply == 0 {
                0
            } else {
                quote_hlp_equity as u128
            },
        )
    }

    pub(crate) fn current_concentrated_curve_geometry(&self) -> Result<Option<ConcentratedCurveGeometry>> {
        let parameters = self.config.amm.concentrated_curve_parameters()?;
        if !self.amm.initialized {
            let ordinary = self.integrated_curve_state_nad()?;
            let cache = prepare_concentrated_cache_at_point(
                ordinary.ordinary_base,
                ordinary.ordinary_quote,
                self.current_curve_center_price_nad()?,
                parameters,
            )?;
            return Ok(Some(cache.geometry()?));
        }
        require!(
            self.amm.concentrated_curve_cache.parameters() == parameters,
            ErrorCode::BrokenInvariant
        );
        Ok(Some(self.amm.concentrated_curve_cache.geometry()?))
    }

    pub(crate) fn current_concentrated_spot_price_nad(&self) -> Result<Option<u64>> {
        let Some(geometry) = self.current_concentrated_curve_geometry()? else {
            return Ok(None);
        };
        let state = self.integrated_curve_state_nad()?;
        u64::try_from(geometry.spot_price_nad_prevalidated(ConcentratedCurvePoint {
            base_reserve: state.ordinary_base,
            quote_reserve: state.ordinary_quote,
        })?)
        .map(Some)
        .map_err(|_| ErrorCode::MarketMathOverflow.into())
    }

    #[cfg(test)]
    pub(crate) fn quote_integrated_concentrated_exact_in_nad(
        &self,
        gross_amount_in_nad: u128,
        frozen_total_fee_nad: u128,
        asset_in: MarketAsset,
    ) -> Result<Option<IntegratedFrozenFeeQuote>> {
        let Some(geometry) = self.current_concentrated_curve_geometry()? else {
            return Ok(None);
        };
        quote_integrated_exact_in_with_frozen_fee(
            self.integrated_curve_state_nad()?,
            geometry,
            gross_amount_in_nad,
            frozen_total_fee_nad,
            match asset_in {
                MarketAsset::Base => IntegratedSwapDirection::BaseToQuote,
                MarketAsset::Quote => IntegratedSwapDirection::QuoteToBase,
            },
        )
        .map(Some)
    }

    pub(crate) fn quote_concentrated_integrated_with_fee(
        &self,
        asset_in: MarketAsset,
        reserve_credit: u64,
        preliminary: PreliminarySwapInputs,
        protocol_fee_bps: u16,
    ) -> Result<Option<ConcentratedIntegratedAmmQuote>> {
        self.quote_concentrated_integrated_with_fee_from_state(
            asset_in,
            reserve_credit,
            preliminary,
            self.integrated_curve_state_nad()?,
            protocol_fee_bps,
        )
    }

    pub(crate) fn quote_concentrated_integrated_with_fee_from_state(
        &self,
        asset_in: MarketAsset,
        reserve_credit: u64,
        preliminary: PreliminarySwapInputs,
        state: IntegratedCurveState,
        protocol_fee_bps: u16,
    ) -> Result<Option<ConcentratedIntegratedAmmQuote>> {
        let Some(geometry) = self.current_concentrated_curve_geometry()? else {
            return Ok(None);
        };
        let fee_asset = self.config.swap_fee_asset(asset_in)?;
        let fees_on_input = fee_asset == asset_in;
        let input_decimals = self.side(asset_in).asset_decimals;
        let curve_input_raw = if fees_on_input {
            preliminary.amount_in_for_quote
        } else {
            reserve_credit
        };
        let gross_curve_input_nad = normalize_to_nad(curve_input_raw as u128, input_decimals)?;
        let center = self
            .amm
            .concentrated_curve_cache
            .center_point_with_geometry(self.current_curve_center_price_nad()?, geometry)?;
        let (start_input_nad, center_input_nad) = match asset_in {
            MarketAsset::Base => (state.ordinary_base, center.base_reserve),
            MarketAsset::Quote => (state.ordinary_quote, center.quote_reserve),
        };
        let start_input_raw = denormalize_from_nad_floor(start_input_nad, input_decimals)?;
        let center_input_raw = denormalize_from_nad_floor(center_input_nad, input_decimals)?;
        require!(center_input_raw > 0, ErrorCode::InvalidMarketConfig);
        let gross_end_input_raw = start_input_raw
            .checked_add(curve_input_raw)
            .ok_or(ErrorCode::ReserveOverflow)?;
        let config = self.dynamic_fee_config()?;
        let (uncapped_divergence, saturated) = gross_path_divergence_fee_raw(
            center_input_raw,
            start_input_raw,
            gross_end_input_raw,
            config.divergence_coefficient_nad,
            config.divergence_fee_share_cap_bps,
        )?;
        let component_budget = gross_fee_budget_floor(reserve_credit, config.divergence_fee_share_cap_bps)?;
        let remaining_total_budget = hard_total_fee_budget_floor(reserve_credit)
            .checked_sub(preliminary.fee.total_fee_amount)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        let divergence_surcharge = if saturated {
            component_budget.min(remaining_total_budget)
        } else {
            u64::try_from(uncapped_divergence.min(u64::MAX as u128))
                .map_err(|_| ErrorCode::FeeMathOverflow)?
                .min(component_budget)
                .min(remaining_total_budget)
        };
        require!(divergence_surcharge < curve_input_raw, ErrorCode::InvalidSwapFeeBps);
        let divergence_nad = if fees_on_input {
            normalize_to_nad(divergence_surcharge as u128, input_decimals)?
        } else {
            0
        };
        let mut integrated = quote_integrated_exact_in_with_frozen_fee(
            state,
            geometry,
            gross_curve_input_nad,
            divergence_nad,
            match asset_in {
                MarketAsset::Base => IntegratedSwapDirection::BaseToQuote,
                MarketAsset::Quote => IntegratedSwapDirection::QuoteToBase,
            },
        )?;
        let gross_amount_out = denormalize_from_nad_floor(
            integrated.executable.amount_out,
            self.side(asset_in.opposite()).asset_decimals,
        )?;
        require!(gross_amount_out > 0, ErrorCode::InsufficientOutputAmount);
        let start_price_nad = u64::try_from(geometry.spot_price_nad_prevalidated(ConcentratedCurvePoint {
            base_reserve: state.ordinary_base,
            quote_reserve: state.ordinary_quote,
        })?)
        .map_err(|_| ErrorCode::MarketMathOverflow)?;
        let end_price_nad = u64::try_from(geometry.spot_price_nad_prevalidated(integrated.executable.curve.end)?)
            .map_err(|_| ErrorCode::MarketMathOverflow)?;
        require!(
            start_price_nad > 0 && end_price_nad > 0,
            ErrorCode::InvalidSettlementPrice
        );

        let dynamic = if fees_on_input {
            let mut dynamic = preliminary.fee;
            dynamic.divergence_surcharge_amount = divergence_surcharge;
            dynamic.dynamic_surcharge_amount = dynamic
                .volatility_surcharge_amount
                .checked_add(divergence_surcharge)
                .ok_or(ErrorCode::FeeMathOverflow)?;
            dynamic.total_fee_amount = dynamic
                .base_fee_amount
                .checked_add(dynamic.dynamic_surcharge_amount)
                .ok_or(ErrorCode::FeeMathOverflow)?;
            dynamic.divergence_rate_nad = effective_rate_floor_nad(divergence_surcharge, reserve_credit)?;
            dynamic.total_rate_nad = dynamic
                .base_rate_nad
                .checked_add(dynamic.volatility_rate_nad)
                .and_then(|value| value.checked_add(dynamic.divergence_rate_nad))
                .ok_or(ErrorCode::FeeMathOverflow)?;
            dynamic
        } else {
            output_denominated_dynamic_fee(
                gross_amount_out,
                reserve_credit,
                divergence_surcharge,
                preliminary.fee.decayed_volatility_nad,
                preliminary.base_fee_bps,
                config,
            )?
        };
        let (retained_surcharge, distributed_surcharge_debit) = if self.amm.retain_dynamic_surcharge {
            (dynamic.dynamic_surcharge_amount, 0)
        } else {
            (0, dynamic.dynamic_surcharge_amount)
        };
        let amount_in_for_quote = if fees_on_input {
            preliminary
                .amount_in_for_quote
                .checked_sub(divergence_surcharge)
                .ok_or(ErrorCode::FeeMathOverflow)?
        } else {
            reserve_credit
        };
        let fee_allocation = split_compounded_swap_fee(
            dynamic.base_fee_amount,
            distributed_surcharge_debit,
            protocol_fee_bps,
            self.config.amm.compounding_fee_bps,
        )?;
        let claimable_fee_debit = fee_allocation
            .claimable_base_fee
            .checked_add(fee_allocation.claimable_dynamic_surcharge)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        let compounded_fee_debit = fee_allocation
            .compounded_base_fee
            .checked_add(fee_allocation.compounded_dynamic_surcharge)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        if compounded_fee_debit > 0 {
            apply_compounded_ylp_fee(
                state,
                &mut integrated,
                fee_asset == MarketAsset::Base,
                normalize_to_nad(compounded_fee_debit as u128, self.side(fee_asset).asset_decimals)?,
                self.side(fee_asset).shares.ylp_supply,
                self.base_hlp_vault.ylp_shares,
                self.quote_hlp_vault.ylp_shares,
            )?;
        }
        let reserve_input_credit = if fees_on_input {
            amount_in_for_quote
                .checked_add(retained_surcharge)
                .and_then(|value| value.checked_add(compounded_fee_debit))
                .ok_or(ErrorCode::ReserveOverflow)?
        } else {
            reserve_credit
        };
        let amount_out = if fees_on_input {
            gross_amount_out
        } else {
            gross_amount_out
                .checked_sub(dynamic.total_fee_amount)
                .ok_or(ErrorCode::FeeMathOverflow)?
        };
        require!(amount_out > 0, ErrorCode::InsufficientOutputAmount);
        if fees_on_input {
            require_eq!(
                reserve_input_credit
                    .checked_add(claimable_fee_debit)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
                reserve_credit,
                ErrorCode::BrokenInvariant
            );
        } else {
            require_eq!(
                amount_out
                    .checked_add(claimable_fee_debit)
                    .and_then(|value| value.checked_add(retained_surcharge))
                    .and_then(|value| value.checked_add(compounded_fee_debit))
                    .ok_or(ErrorCode::MarketMathOverflow)?,
                gross_amount_out,
                ErrorCode::BrokenInvariant
            );
        }
        // Retained surcharge remains outside executable reserves. Compounded
        // LP fees are instead ordinary principal, so they rebase the same
        // tail+band parameters through the post-fee reserve point.
        let reserve_end_price_nad = if compounded_fee_debit == 0 {
            end_price_nad
        } else {
            let compounded_cache = prepare_concentrated_cache_at_point(
                integrated.executable.end.ordinary_base,
                integrated.executable.end.ordinary_quote,
                self.current_curve_center_price_nad()?,
                self.config.amm.concentrated_curve_parameters()?,
            )?;
            u64::try_from(
                compounded_cache
                    .geometry()?
                    .spot_price_nad_prevalidated(ConcentratedCurvePoint {
                        base_reserve: integrated.executable.end.ordinary_base,
                        quote_reserve: integrated.executable.end.ordinary_quote,
                    })?,
            )
            .map_err(|_| ErrorCode::MarketMathOverflow)?
        };
        let post_success_volatility_nad = volatility_after_success_nad(
            dynamic.decayed_volatility_nad,
            start_price_nad,
            end_price_nad,
            self.config.amm.volatility_shock_cap_nad,
            self.config.amm.volatility_cap_nad,
        )?;
        Ok(Some(ConcentratedIntegratedAmmQuote {
            integrated,
            amount_out,
            gross_amount_out,
            start_price_nad,
            end_price_nad,
            reserve_end_price_nad,
            decayed_volatility_nad: dynamic.decayed_volatility_nad,
            post_success_volatility_nad,
            fee: SwapFeeBreakdown {
                fee_asset: fee_asset.code(),
                reserve_credit,
                gross_amount_out,
                base_fee_debit: dynamic.base_fee_amount,
                divergence_surcharge_debit: dynamic.divergence_surcharge_amount,
                volatility_surcharge_debit: dynamic.volatility_surcharge_amount,
                dynamic_surcharge_debit: dynamic.dynamic_surcharge_amount,
                total_fee_debit: dynamic.total_fee_amount,
                retained_surcharge,
                distributed_surcharge_debit,
                compounded_base_fee_debit: fee_allocation.compounded_base_fee,
                compounded_dynamic_surcharge_debit: fee_allocation.compounded_dynamic_surcharge,
                compounded_fee_debit,
                amount_in_for_quote,
                reserve_input_credit,
                claimable_fee_debit,
                protocol_fee_bps,
                base_fee_rate_nad: dynamic.base_rate_nad,
                divergence_fee_rate_nad: dynamic.divergence_rate_nad,
                volatility_fee_rate_nad: dynamic.volatility_rate_nad,
                total_fee_rate_nad: dynamic.total_rate_nad,
            },
            recovery: HlpRecoveryBreakdown {
                target_asset: 0,
                funding_gap: 0,
                matched_input: 0,
                bonus_output: 0,
                discount_bps: 0,
                critical: false,
            },
        }))
    }

    /// Until first liquidity initializes AMM state, the reserve ratio is the
    /// only meaningful center. This makes a configured concentrated pool begin
    /// balanced without any external price input.
    pub(crate) fn current_curve_center_price_nad(&self) -> Result<u64> {
        if self.amm.initialized {
            require!(self.amm.center_price_nad > 0, ErrorCode::BrokenInvariant);
            return Ok(self.amm.center_price_nad);
        }
        let reserves = self.curve_reserves_nad()?;
        require!(
            reserves.base > 0 && reserves.quote > 0,
            ErrorCode::InsufficientLiquidity
        );
        let center = reserves
            .quote
            .checked_mul(NAD as u128)
            .and_then(|value| value.checked_div(reserves.base))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        u64::try_from(center).map_err(|_| ErrorCode::MarketMathOverflow.into())
    }

    pub(crate) fn curve_depth_per_share_nad(&self, curve_depth_nad: u128) -> Result<u128> {
        let supply = self.base_side.shares.ylp_supply;
        require_eq!(supply, self.quote_side.shares.ylp_supply, ErrorCode::BrokenInvariant);
        require!(supply > 0, ErrorCode::SupplyUnderflow);
        let supply_nad = normalize_to_nad(supply as u128, self.base_side.asset_decimals)?;
        curve_depth_nad
            .checked_mul(NAD as u128)
            .and_then(|value| value.checked_div(supply_nad))
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    }
}

fn output_denominated_dynamic_fee(
    gross_amount_out: u64,
    gross_amount_in: u64,
    input_divergence_signal: u64,
    decayed_volatility_nad: u64,
    base_fee_bps: u16,
    config: DynamicFeeConfig,
) -> Result<DynamicFeeQuote> {
    let hard_total_budget = hard_total_fee_budget_floor(gross_amount_out);
    let base_fee_amount = gross_fee_budget_floor(gross_amount_out, base_fee_bps)?;
    let after_base = gross_amount_out
        .checked_sub(base_fee_amount)
        .ok_or(ErrorCode::FeeMathOverflow)?;
    require!(after_base > 0, ErrorCode::InsufficientOutputAmount);

    let volatility_signal_rate_nad =
        asymptotic_scaled_rate_nad(u128::from(decayed_volatility_nad), config.volatility_coefficient_nad)?;
    let uncapped_volatility = u64::try_from(
        u128::from(after_base)
            .checked_mul(u128::from(volatility_signal_rate_nad))
            .ok_or(ErrorCode::MarketMathOverflow)?
            / u128::from(NAD),
    )
    .map_err(|_| ErrorCode::MarketMathOverflow)?;
    let volatility_surcharge_amount = uncapped_volatility
        .min(gross_fee_budget_floor(
            gross_amount_out,
            config.volatility_fee_share_cap_bps,
        )?)
        .min(
            hard_total_budget
                .checked_sub(base_fee_amount)
                .ok_or(ErrorCode::FeeMathOverflow)?,
        );
    let after_volatility = after_base
        .checked_sub(volatility_surcharge_amount)
        .ok_or(ErrorCode::FeeMathOverflow)?;
    require!(after_volatility > 0, ErrorCode::InsufficientOutputAmount);

    let divergence_signal_rate_nad = effective_rate_floor_nad(input_divergence_signal, gross_amount_in)?;
    let uncapped_divergence = u64::try_from(
        u128::from(after_volatility)
            .checked_mul(u128::from(divergence_signal_rate_nad))
            .ok_or(ErrorCode::MarketMathOverflow)?
            / u128::from(NAD),
    )
    .map_err(|_| ErrorCode::MarketMathOverflow)?;
    let divergence_surcharge_amount = uncapped_divergence
        .min(gross_fee_budget_floor(
            gross_amount_out,
            config.divergence_fee_share_cap_bps,
        )?)
        .min(
            hard_total_budget
                .checked_sub(base_fee_amount)
                .and_then(|value| value.checked_sub(volatility_surcharge_amount))
                .ok_or(ErrorCode::FeeMathOverflow)?,
        );
    let dynamic_surcharge_amount = volatility_surcharge_amount
        .checked_add(divergence_surcharge_amount)
        .ok_or(ErrorCode::FeeMathOverflow)?;
    let total_fee_amount = base_fee_amount
        .checked_add(dynamic_surcharge_amount)
        .ok_or(ErrorCode::FeeMathOverflow)?;
    require!(total_fee_amount < gross_amount_out, ErrorCode::InvalidSwapFeeBps);
    let base_rate_nad = effective_rate_floor_nad(base_fee_amount, gross_amount_out)?;
    let volatility_rate_nad = effective_rate_floor_nad(volatility_surcharge_amount, gross_amount_out)?;
    let divergence_rate_nad = effective_rate_floor_nad(divergence_surcharge_amount, gross_amount_out)?;
    let total_rate_nad = base_rate_nad
        .checked_add(volatility_rate_nad)
        .and_then(|value| value.checked_add(divergence_rate_nad))
        .ok_or(ErrorCode::FeeMathOverflow)?;
    Ok(DynamicFeeQuote {
        base_rate_nad,
        divergence_rate_nad,
        volatility_rate_nad,
        total_rate_nad,
        base_fee_amount,
        divergence_surcharge_amount,
        volatility_surcharge_amount,
        dynamic_surcharge_amount,
        total_fee_amount,
        decayed_volatility_nad,
        post_success_volatility_nad: decayed_volatility_nad,
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SwapFeeBreakdown {
    pub fee_asset: u8,
    pub reserve_credit: u64,
    pub gross_amount_out: u64,
    pub base_fee_debit: u64,
    pub divergence_surcharge_debit: u64,
    pub volatility_surcharge_debit: u64,
    pub dynamic_surcharge_debit: u64,
    pub total_fee_debit: u64,
    pub retained_surcharge: u64,
    pub distributed_surcharge_debit: u64,
    /// LP-owned base-fee atoms converted into ordinary reserve principal.
    pub compounded_base_fee_debit: u64,
    /// LP-owned distributed-surcharge atoms converted into principal.
    pub compounded_dynamic_surcharge_debit: u64,
    pub compounded_fee_debit: u64,
    pub amount_in_for_quote: u64,
    pub reserve_input_credit: u64,
    pub claimable_fee_debit: u64,
    /// Global protocol share frozen into this quote before compounding.
    pub protocol_fee_bps: u16,
    pub base_fee_rate_nad: u64,
    pub divergence_fee_rate_nad: u64,
    pub volatility_fee_rate_nad: u64,
    pub total_fee_rate_nad: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CompoundedSwapFeeAllocation {
    claimable_base_fee: u64,
    claimable_dynamic_surcharge: u64,
    compounded_base_fee: u64,
    compounded_dynamic_surcharge: u64,
}

fn fee_bps_floor(amount: u64, bps: u16) -> Result<u64> {
    require_gte!(BPS_DENOMINATOR, bps, ErrorCode::InvalidMarketConfig);
    u64::try_from(
        (amount as u128)
            .checked_mul(bps as u128)
            .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
            .ok_or(ErrorCode::FeeMathOverflow)?,
    )
    .map_err(|_| ErrorCode::FeeMathOverflow.into())
}

/// Protocol revenue remains claimable. Only the LP-owned remainder of the
/// base fee and the distributed dynamic surcharge may become pool principal.
fn split_compounded_swap_fee(
    base_fee: u64,
    distributed_dynamic_surcharge: u64,
    protocol_fee_bps: u16,
    compounding_fee_bps: u16,
) -> Result<CompoundedSwapFeeAllocation> {
    let protocol_fee = fee_bps_floor(base_fee, protocol_fee_bps)?;
    let base_lp_fee = base_fee.checked_sub(protocol_fee).ok_or(ErrorCode::FeeMathOverflow)?;
    let compounded_base_fee = fee_bps_floor(base_lp_fee, compounding_fee_bps)?;
    let compounded_dynamic_surcharge = fee_bps_floor(distributed_dynamic_surcharge, compounding_fee_bps)?;
    Ok(CompoundedSwapFeeAllocation {
        claimable_base_fee: base_fee
            .checked_sub(compounded_base_fee)
            .ok_or(ErrorCode::FeeMathOverflow)?,
        claimable_dynamic_surcharge: distributed_dynamic_surcharge
            .checked_sub(compounded_dynamic_surcharge)
            .ok_or(ErrorCode::FeeMathOverflow)?,
        compounded_base_fee,
        compounded_dynamic_surcharge,
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HlpRecoveryBreakdown {
    pub target_asset: u8,
    pub funding_gap: u64,
    pub matched_input: u64,
    pub bonus_output: u64,
    pub discount_bps: u16,
    pub critical: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AmmSwapQuote {
    pub asset_in: MarketAsset,
    /// Net output transferred or credited to the trader/position.
    pub amount_out: u64,
    /// Curve output before an output-denominated fee is withheld.
    pub gross_amount_out: u64,
    pub start_price_nad: u64,
    /// Marginal price at the invariant-preserving trader endpoint. Retained
    /// surcharge is excluded because it is principal funding, not traded flow.
    pub end_price_nad: u64,
    /// Marginal price after retained surcharge, if any, has been added to the
    /// executable reserve. This is the state used by the next quote and risk.
    pub reserve_end_price_nad: u64,
    pub decayed_volatility_nad: u64,
    pub post_success_volatility_nad: u64,
    pub fee: SwapFeeBreakdown,
    pub recovery: HlpRecoveryBreakdown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ConcentratedIntegratedAmmQuote {
    pub integrated: IntegratedFrozenFeeQuote,
    pub amount_out: u64,
    pub gross_amount_out: u64,
    pub start_price_nad: u64,
    pub end_price_nad: u64,
    pub reserve_end_price_nad: u64,
    pub decayed_volatility_nad: u64,
    pub post_success_volatility_nad: u64,
    pub fee: SwapFeeBreakdown,
    pub recovery: HlpRecoveryBreakdown,
}

impl ConcentratedIntegratedAmmQuote {
    pub(crate) fn as_swap_quote(self, asset_in: MarketAsset) -> AmmSwapQuote {
        AmmSwapQuote {
            asset_in,
            amount_out: self.amount_out,
            gross_amount_out: self.gross_amount_out,
            start_price_nad: self.start_price_nad,
            end_price_nad: self.end_price_nad,
            reserve_end_price_nad: self.reserve_end_price_nad,
            decayed_volatility_nad: self.decayed_volatility_nad,
            post_success_volatility_nad: self.post_success_volatility_nad,
            fee: self.fee,
            recovery: self.recovery,
        }
    }
}

/// Conservative first-pass coordinates for hLP pre-positioning.
///
/// The quote coordinate excludes every fee. The reserve coordinate adds back
/// dynamic surcharge that will remain as AMM principal. Divergence is omitted
/// from the first pass, so both coordinates are at least as outward as the
/// final executable path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PreliminarySwapInputs {
    pub amount_in_for_quote: u64,
    pub reserve_input_credit: u64,
    base_fee_bps: u16,
    fee: DynamicFeeQuote,
}

impl Market {
    #[cfg(test)]
    pub(crate) fn preliminary_swap_inputs_for_state(
        &self,
        asset_in: MarketAsset,
        reserve_credit: u64,
        current_slot: u64,
        pre_state: DynamicFeePreState,
    ) -> Result<PreliminarySwapInputs> {
        self.preliminary_swap_inputs_for_state_at_time(
            asset_in,
            reserve_credit,
            current_slot,
            self.config.start_time,
            pre_state,
        )
    }

    pub(crate) fn preliminary_swap_inputs_for_state_at_time(
        &self,
        asset_in: MarketAsset,
        reserve_credit: u64,
        current_slot: u64,
        current_unix_timestamp: i64,
        pre_state: DynamicFeePreState,
    ) -> Result<PreliminarySwapInputs> {
        require!(reserve_credit > 0, ErrorCode::AmountZero);
        let mut config = self.dynamic_fee_config()?;
        let gross_input_nad = normalize_to_nad(reserve_credit as u128, self.side(asset_in).asset_decimals)?;
        let current_base_price_nad = self
            .current_concentrated_spot_price_nad()?
            .ok_or(ErrorCode::InsufficientLiquidity)?;
        config.base_fee_bps = self.config.effective_base_fee_bps_for_swap_at(
            asset_in,
            gross_input_nad,
            current_unix_timestamp,
            current_base_price_nad,
            self.amm.launch_reference_price_nad,
            self.amm.launch_fee_progress_offset,
        )?;
        validate_fee_share_caps(
            config.base_fee_bps,
            config.divergence_fee_share_cap_bps,
            config.volatility_fee_share_cap_bps,
        )?;
        require!(
            config.volatility_shock_cap_nad <= config.volatility_accumulator_cap_nad,
            ErrorCode::InvalidArgument
        );
        require!(
            config.volatility_coefficient_nad == 0 || config.volatility_half_life_ms > 0,
            ErrorCode::InvalidHalfLife
        );
        let decayed_volatility_nad = decay_volatility_nad(
            pre_state
                .volatility_accumulator_nad
                .min(config.volatility_accumulator_cap_nad),
            pre_state.volatility_last_update_slot,
            current_slot,
            config.volatility_half_life_ms,
        )?;
        let hard_total_budget = hard_total_fee_budget_floor(reserve_credit);
        let base_fee_amount = gross_fee_budget_floor(reserve_credit, config.base_fee_bps)?;
        let after_base = reserve_credit
            .checked_sub(base_fee_amount)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        require!(after_base > 0, ErrorCode::InsufficientOutputAmount);

        let signal_nad = decayed_volatility_nad as u128;
        let coefficient_nad = config.volatility_coefficient_nad;
        let volatility_rate_nad = asymptotic_scaled_rate_nad(signal_nad, coefficient_nad)?;
        require!(volatility_rate_nad < NAD, ErrorCode::InvalidSwapFeeBps);
        let uncapped_volatility_surcharge = u64::try_from(
            (after_base as u128)
                .checked_mul(volatility_rate_nad as u128)
                .ok_or(ErrorCode::MarketMathOverflow)?
                / NAD as u128,
        )
        .map_err(|_| ErrorCode::MarketMathOverflow)?;
        let volatility_component_budget = gross_fee_budget_floor(reserve_credit, config.volatility_fee_share_cap_bps)?;
        let remaining_total_budget = hard_total_budget
            .checked_sub(base_fee_amount)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        let volatility_surcharge_amount = uncapped_volatility_surcharge
            .min(volatility_component_budget)
            .min(remaining_total_budget);
        require!(volatility_surcharge_amount < after_base, ErrorCode::BrokenInvariant);
        let amount_in_for_quote = after_base
            .checked_sub(volatility_surcharge_amount)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        require!(amount_in_for_quote > 0, ErrorCode::InsufficientOutputAmount);
        let total_fee_amount = base_fee_amount
            .checked_add(volatility_surcharge_amount)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        require!(total_fee_amount <= hard_total_budget, ErrorCode::BrokenInvariant);
        let base_rate_nad = effective_rate_floor_nad(base_fee_amount, reserve_credit)?;
        let volatility_rate_nad = effective_rate_floor_nad(volatility_surcharge_amount, reserve_credit)?;
        let total_rate_nad = base_rate_nad
            .checked_add(volatility_rate_nad)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        require!(total_rate_nad < NAD, ErrorCode::BrokenInvariant);
        let preliminary = DynamicFeeQuote {
            base_rate_nad,
            divergence_rate_nad: 0,
            volatility_rate_nad,
            total_rate_nad,
            base_fee_amount,
            divergence_surcharge_amount: 0,
            volatility_surcharge_amount,
            dynamic_surcharge_amount: volatility_surcharge_amount,
            total_fee_amount,
            decayed_volatility_nad,
            post_success_volatility_nad: decayed_volatility_nad,
        };
        let amount = reserve_credit
            .checked_sub(preliminary.total_fee_amount)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        require!(amount > 0, ErrorCode::InsufficientOutputAmount);
        require_gte!(
            amount,
            minimum_executable_input(reserve_credit),
            ErrorCode::BrokenInvariant
        );
        let reserve_input_credit = if self.amm.retain_dynamic_surcharge {
            // Every dynamic surcharge, including the divergence omitted from
            // this first pass, remains in the reserve. Therefore only the base
            // fee leaves the reserve coordinate.
            reserve_credit
                .checked_sub(preliminary.base_fee_amount)
                .ok_or(ErrorCode::FeeMathOverflow)?
        } else {
            // Without retention, omitted divergence can only lower the final
            // quote and reserve inputs, so the no-divergence amount is the
            // conservative outward endpoint.
            amount
        };
        require_gte!(reserve_input_credit, amount, ErrorCode::BrokenInvariant);
        Ok(PreliminarySwapInputs {
            amount_in_for_quote: amount,
            reserve_input_credit,
            base_fee_bps: config.base_fee_bps,
            fee: preliminary,
        })
    }

    pub(super) fn dynamic_fee_config(&self) -> Result<DynamicFeeConfig> {
        let profile = self.config.fee_profile();
        Ok(DynamicFeeConfig {
            base_fee_bps: profile.base_fee_bps,
            divergence_fee_share_cap_bps: profile.divergence_fee_share_cap_bps,
            volatility_fee_share_cap_bps: profile.volatility_fee_share_cap_bps,
            divergence_coefficient_nad: profile.divergence_fee_coefficient_nad,
            volatility_coefficient_nad: profile.volatility_fee_coefficient_nad,
            volatility_half_life_ms: profile.volatility_half_life_ms,
            volatility_shock_cap_nad: profile.volatility_shock_cap_nad,
            volatility_accumulator_cap_nad: profile.volatility_accumulator_cap_nad,
        })
    }

    pub(crate) fn dynamic_fee_pre_state(&self, current_slot: u64) -> Result<DynamicFeePreState> {
        Ok(DynamicFeePreState {
            center_price_nad: self.current_curve_center_price_nad()?,
            volatility_accumulator_nad: if self.amm.initialized {
                self.amm.volatility_accumulator_nad
            } else {
                0
            },
            volatility_last_update_slot: if self.amm.initialized {
                self.amm.last_observation_slot
            } else {
                current_slot
            },
        })
    }
}

impl AmmConfig {
    pub const fn is_cpmm(&self) -> bool {
        self.peak_amplification_nad == NAD
    }

    /// Typed view of the one production curve configuration. CPMM is the
    /// concentrated curve at one-times peak amplification with zero widths.
    pub fn concentrated_curve_parameters(&self) -> Result<ConcentratedCurveParameters> {
        require!(
            self.reserved.iter().all(|byte| *byte == 0),
            ErrorCode::InvalidMarketConfig
        );
        let parameters = ConcentratedCurveParameters {
            peak_amplification_nad: self.peak_amplification_nad,
            core_half_width_bps: self.core_half_width_bps,
            fade_width_bps: self.fade_width_bps,
        };
        parameters.validate(MAX_AMM_AMPLIFICATION_NAD)?;
        Ok(parameters)
    }

    pub fn set_concentrated_curve_parameters(&mut self, parameters: ConcentratedCurveParameters) -> Result<()> {
        parameters.validate(MAX_AMM_AMPLIFICATION_NAD)?;
        self.peak_amplification_nad = parameters.peak_amplification_nad;
        self.core_half_width_bps = parameters.core_half_width_bps;
        self.fade_width_bps = parameters.fade_width_bps;
        self.reserved = [0; AMM_CONFIG_RESERVED_BYTES];
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        self.concentrated_curve_parameters()?;
        require!(
            matches!(
                self.swap_fee_collect_mode,
                SWAP_FEE_COLLECT_INPUT_ASSET | SWAP_FEE_COLLECT_BASE_ONLY | SWAP_FEE_COLLECT_QUOTE_ONLY
            ),
            ErrorCode::InvalidMarketConfig
        );
        require_gte!(
            BPS_DENOMINATOR,
            self.compounding_fee_bps,
            ErrorCode::InvalidMarketConfig
        );
        require!(
            (MIN_HALF_LIFE_MS..=MAX_HALF_LIFE_MS).contains(&self.center_ema_half_life_ms)
                && (MIN_HALF_LIFE_MS..=MAX_HALF_LIFE_MS).contains(&self.volatility_half_life_ms),
            ErrorCode::InvalidHalfLife
        );

        let adjustment_disabled = self.adjustment_threshold_nad == 0
            && self.adjustment_step_nad == 0
            && self.min_adjustment_interval_slots == 0;
        if !adjustment_disabled {
            require!(
                (MIN_AMM_ADJUSTMENT_NAD..=MAX_AMM_ADJUSTMENT_NAD).contains(&self.adjustment_step_nad)
                    && (self.adjustment_step_nad..=MAX_AMM_ADJUSTMENT_NAD).contains(&self.adjustment_threshold_nad)
                    && (1..=MAX_AMM_ADJUSTMENT_INTERVAL_SLOTS).contains(&self.min_adjustment_interval_slots),
                ErrorCode::InvalidMarketConfig
            );
        }

        let volatility_signal_disabled = self.volatility_shock_cap_nad == 0 && self.volatility_cap_nad == 0;
        let volatility_signal_valid = self.volatility_shock_cap_nad > 0
            && self.volatility_shock_cap_nad <= self.volatility_cap_nad
            && self.volatility_cap_nad <= MAX_AMM_VOLATILITY_NAD;
        require!(
            volatility_signal_disabled || volatility_signal_valid,
            ErrorCode::InvalidMarketConfig
        );
        require!(
            self.volatility_fee_coefficient_nad == 0 || volatility_signal_valid,
            ErrorCode::InvalidMarketConfig
        );
        require!(
            self.divergence_fee_coefficient_nad <= MAX_AMM_FEE_COEFFICIENT_NAD
                && self.volatility_fee_coefficient_nad <= MAX_AMM_FEE_COEFFICIENT_NAD,
            ErrorCode::InvalidMarketConfig
        );
        Ok(())
    }
}

impl DeferredControllerTarget {
    pub const NONE: u8 = 0;
    pub const RECENTER: u8 = 2;

    pub const fn is_active(self) -> bool {
        self.kind != Self::NONE
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

impl AmmState {
    pub fn initialize(
        config: &AmmConfig,
        initial_price_nad: u64,
        initial_curve_depth_per_share_nad: u128,
        current_slot: u64,
    ) -> Result<Self> {
        config.validate()?;
        require!(initial_price_nad > 0, ErrorCode::InvalidSettlementPrice);
        Ok(Self {
            initialized: true,
            concentrated_curve_cache: ConcentratedCurveCache::default(),
            center_price_nad: initial_price_nad,
            price_ema_nad: initial_price_nad,
            last_trade_price_nad: initial_price_nad,
            last_observation_slot: current_slot,
            last_adjustment_slot: current_slot,
            launch_reference_price_nad: 0,
            launch_fee_progress_offset: 0,
            volatility_accumulator_nad: 0,
            curve_depth_per_share_nad: initial_curve_depth_per_share_nad,
            protected_floor_per_share_nad: initial_curve_depth_per_share_nad,
            retention_required_nad: 0,
            retention_stop_nad: 0,
            retention_hard_cap_nad: 0,
            retain_dynamic_surcharge: false,
            retention_target_saturated: false,
            retention_target_stale: false,
            deferred_controller_target: DeferredControllerTarget::default(),
            _reserved: [0; AMM_STATE_RESERVED_BYTES],
        })
    }

    /// Governance changed the controller request, so a cost cached for the
    /// preceding request is no longer admissible. The next genuine operation
    /// evaluates one fresh target against current reserves.
    pub(crate) fn invalidate_deferred_controller_target(&mut self) {
        self.deferred_controller_target.clear();
        self.retention_target_saturated = false;
        self.mark_retention_target_stale();
    }

    /// Advances clock-driven signals without fabricating an external trade.
    ///
    /// The last successful trade remains the EMA input until another trade
    /// replaces it. This lets the next genuine operation decay the EMA and
    /// volatility after a trade followed by silence.
    pub fn observe_clock(&mut self, config: &AmmConfig, current_slot: u64) -> Result<()> {
        config.validate()?;
        self.observe_clock_from_validated_config(config, current_slot)
    }

    /// Market config is validated on admission and initialization. Hot paths
    /// use this entry point to avoid repeating the full config validation both
    /// before and after one swap.
    pub(crate) fn observe_clock_from_validated_config(&mut self, config: &AmmConfig, current_slot: u64) -> Result<()> {
        require!(self.initialized, ErrorCode::InvalidMarketConfig);
        require_gte!(current_slot, self.last_observation_slot, ErrorCode::InvalidArgument);

        if current_slot == self.last_observation_slot {
            return Ok(());
        }

        let price_ema_nad = ema_u64(
            self.price_ema_nad,
            self.last_trade_price_nad,
            self.last_observation_slot,
            current_slot,
            config.center_ema_half_life_ms,
        );
        let volatility_accumulator_nad = decay_volatility_nad(
            self.volatility_accumulator_nad,
            self.last_observation_slot,
            current_slot,
            config.volatility_half_life_ms,
        )?;
        self.price_ema_nad = price_ema_nad;
        self.volatility_accumulator_nad = volatility_accumulator_nad;
        self.last_observation_slot = current_slot;
        Ok(())
    }

    /// Commits one successful external AMM flow path.
    ///
    /// Volatility measures only the executable path quoted to the trader
    /// (`start_price_nad -> end_price_nad`). Gaps caused by hLP settlement,
    /// parameter ramps, or recentering are deliberately excluded. The next
    /// trade supplies its own rebased start price, while `last_trade_price_nad`
    /// remains the prior successful trade signal observed by the EMA.
    ///
    /// On a new slot, the EMA observes only the prior slot's final trade.
    /// Same-slot swaps leave the EMA unchanged but still add their own bounded
    /// path movement.
    pub fn checkpoint_trade(
        &mut self,
        config: &AmmConfig,
        start_price_nad: u64,
        end_price_nad: u64,
        current_slot: u64,
    ) -> Result<()> {
        config.validate()?;
        require!(
            self.initialized && start_price_nad > 0 && end_price_nad > 0,
            ErrorCode::InvalidSettlementPrice
        );
        self.observe_clock_from_validated_config(config, current_slot)?;

        self.volatility_accumulator_nad = volatility_after_success_nad(
            self.volatility_accumulator_nad,
            start_price_nad,
            end_price_nad,
            config.volatility_shock_cap_nad,
            config.volatility_cap_nad,
        )?;
        self.last_trade_price_nad = end_price_nad;
        Ok(())
    }

    pub fn decayed_volatility(&self, config: &AmmConfig, current_slot: u64) -> Result<u64> {
        require_gte!(current_slot, self.last_observation_slot, ErrorCode::InvalidArgument);
        decay_volatility_nad(
            self.volatility_accumulator_nad,
            self.last_observation_slot,
            current_slot,
            config.volatility_half_life_ms,
        )
    }

    pub fn spendable_protected_profit_nad(&self) -> u128 {
        self.curve_depth_per_share_nad
            .saturating_sub(self.protected_floor_per_share_nad)
    }

    /// Retained surcharge is the only mutation allowed to increase spendable
    /// protected profit.
    pub fn checkpoint_retained_surcharge(&mut self, new_curve_depth_per_share_nad: u128) -> Result<()> {
        require_gte!(
            new_curve_depth_per_share_nad,
            self.curve_depth_per_share_nad,
            ErrorCode::BrokenInvariant
        );
        self.curve_depth_per_share_nad = new_curve_depth_per_share_nad;
        self.mark_retention_target_stale();
        Ok(())
    }

    pub(crate) fn mark_retention_target_stale(&mut self) {
        self.retention_target_stale = true;
        self.sync_stale_retention_cap();
        self.refresh_retention_gate();
    }

    /// Neutral mutations move the floor so the existing spendable budget is
    /// preserved. This includes deposits, withdrawals, interest, and hLP depth.
    pub fn checkpoint_neutral_liquidity(&mut self, new_curve_depth_per_share_nad: u128) {
        let prior_buffer = self.spendable_protected_profit_nad();
        self.curve_depth_per_share_nad = new_curve_depth_per_share_nad;
        self.protected_floor_per_share_nad = new_curve_depth_per_share_nad.saturating_sub(prior_buffer);
        self.sync_stale_retention_cap();
        self.refresh_retention_gate();
    }

    /// Recenter cost or socialized principal loss consumes buffer on a decrease.
    /// An improvement raises the floor equally and cannot manufacture buffer.
    pub fn checkpoint_recenter_or_loss(&mut self, new_curve_depth_per_share_nad: u128) {
        if new_curve_depth_per_share_nad > self.curve_depth_per_share_nad {
            self.protected_floor_per_share_nad = self
                .protected_floor_per_share_nad
                .saturating_add(new_curve_depth_per_share_nad - self.curve_depth_per_share_nad);
        }
        self.curve_depth_per_share_nad = new_curve_depth_per_share_nad;
        self.sync_stale_retention_cap();
        self.refresh_retention_gate();
    }

    pub fn refresh_retention_target(
        &mut self,
        curve_depth_per_share_nad: u128,
        worst_next_step_impairment_nad: u128,
    ) -> Result<RetentionTarget> {
        let target = retention_target(curve_depth_per_share_nad, worst_next_step_impairment_nad)?;
        self.retention_required_nad = target.required_nad;
        self.retention_stop_nad = target.stop_nad;
        self.retention_hard_cap_nad = target.hard_cap_nad;
        self.retention_target_saturated = target.saturated;
        self.retention_target_stale = false;
        self.refresh_retention_gate();
        Ok(target)
    }

    pub fn recenter_is_funded(&self, covered_actual_impairment_nad: u128) -> bool {
        covered_actual_impairment_nad <= self.retention_hard_cap_nad
            && covered_actual_impairment_nad <= self.spendable_protected_profit_nad()
    }

    fn sync_stale_retention_cap(&mut self) {
        if !self.retention_target_stale {
            return;
        }
        let denominator = BPS_DENOMINATOR as u128;
        let cap_bps = PROTECTED_LIQUIDITY_CAP_BPS as u128;
        let hard_cap_nad = (self.curve_depth_per_share_nad / denominator) * cap_bps
            + ((self.curve_depth_per_share_nad % denominator) * cap_bps).div_ceil(denominator);
        if self.retention_required_nad > hard_cap_nad {
            self.retention_required_nad = hard_cap_nad;
            self.retention_target_saturated = true;
        }
        self.retention_stop_nad = self.retention_stop_nad.min(hard_cap_nad);
        self.retention_hard_cap_nad = hard_cap_nad;
    }

    pub(crate) fn commit_concentrated_recenter(
        &mut self,
        config: &AmmConfig,
        new_center_price_nad: u64,
        new_cache: ConcentratedCurveCache,
        new_curve_depth_per_share_nad: u128,
        covered_actual_impairment_nad: u128,
        current_slot: u64,
    ) -> Result<()> {
        config.validate()?;
        require!(new_center_price_nad > 0, ErrorCode::InvalidSettlementPrice);
        new_cache.geometry()?;
        require!(
            self.recenter_is_funded(covered_actual_impairment_nad),
            ErrorCode::BrokenInvariant
        );
        let actual_impairment_nad = self
            .curve_depth_per_share_nad
            .saturating_sub(new_curve_depth_per_share_nad);
        require_gte!(
            covered_actual_impairment_nad,
            actual_impairment_nad,
            ErrorCode::BrokenInvariant
        );
        let earliest_adjustment_slot = self
            .last_adjustment_slot
            .checked_add(config.min_adjustment_interval_slots)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require_gte!(current_slot, earliest_adjustment_slot, ErrorCode::InvalidArgument);
        self.center_price_nad = new_center_price_nad;
        self.concentrated_curve_cache = new_cache;
        self.last_adjustment_slot = current_slot;
        self.checkpoint_recenter_or_loss(new_curve_depth_per_share_nad);
        Ok(())
    }

    fn refresh_retention_gate(&mut self) {
        if self.retention_target_stale {
            self.retain_dynamic_surcharge = true;
            return;
        }
        if self.retention_required_nad == 0 {
            self.retain_dynamic_surcharge = false;
            return;
        }
        let buffer = self.spendable_protected_profit_nad();
        if self.retain_dynamic_surcharge {
            if buffer >= self.retention_stop_nad {
                self.retain_dynamic_surcharge = false;
            }
        } else if buffer < self.retention_required_nad {
            self.retain_dynamic_surcharge = true;
        }
    }
}

pub fn retention_target(
    curve_depth_per_share_nad: u128,
    worst_next_step_impairment_nad: u128,
) -> Result<RetentionTarget> {
    let hard_cap_nad = mul_bps_ceil(curve_depth_per_share_nad, PROTECTED_LIQUIDITY_CAP_BPS)?;
    if curve_depth_per_share_nad == 0 || worst_next_step_impairment_nad == 0 {
        return Ok(RetentionTarget {
            hard_cap_nad,
            ..RetentionTarget::default()
        });
    }

    let covered_impairment = mul_bps_ceil(worst_next_step_impairment_nad, PROTECTED_LIQUIDITY_COVERAGE_BPS)?;
    let guard_nad = mul_bps_ceil(curve_depth_per_share_nad, PROTECTED_LIQUIDITY_GUARD_BPS)?;
    let raw_required_nad = covered_impairment
        .checked_add(guard_nad)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let required_nad = raw_required_nad.min(hard_cap_nad);
    let hysteresis_nad = mul_bps_ceil(required_nad, PROTECTED_LIQUIDITY_HYSTERESIS_BPS)?.max(guard_nad);
    let stop_nad = required_nad.saturating_add(hysteresis_nad).min(hard_cap_nad);

    Ok(RetentionTarget {
        required_nad,
        stop_nad,
        hard_cap_nad,
        saturated: raw_required_nad > hard_cap_nad,
    })
}

impl Risk {
    pub const fn pessimistic_depth_nad(&self) -> u128 {
        if self.curve_depth_ema_nad == 0 {
            self.observed_curve_depth_nad
        } else if self.observed_curve_depth_nad == 0 {
            self.curve_depth_ema_nad
        } else if self.observed_curve_depth_nad < self.curve_depth_ema_nad {
            self.observed_curve_depth_nad
        } else {
            self.curve_depth_ema_nad
        }
    }

    pub fn refreshed(
        &self,
        current_base_price_nad: u64,
        current_quote_price_nad: u64,
        current_curve_depth_nad: u128,
        config: &MarketConfig,
        current_slot: u64,
    ) -> Result<Self> {
        require!(
            current_base_price_nad > 0 && current_quote_price_nad > 0 && current_curve_depth_nad > 0,
            ErrorCode::InsufficientLiquidity
        );

        let cached_spot_base_price_nad =
            observed_or_current_u64(self.cached_spot_base_price_nad, current_base_price_nad);
        let cached_spot_quote_price_nad =
            observed_or_current_u64(self.cached_spot_quote_price_nad, current_quote_price_nad);
        let observed_curve_depth_nad = if self.observed_curve_depth_nad == 0 {
            current_curve_depth_nad
        } else {
            self.observed_curve_depth_nad
        };

        let base_price_ema_nad = ema_u64(
            self.base_price_ema_nad,
            cached_spot_base_price_nad,
            self.last_snapshot_slot,
            current_slot,
            config.ema_half_life_ms,
        );
        let quote_price_ema_nad = ema_u64(
            self.quote_price_ema_nad,
            cached_spot_quote_price_nad,
            self.last_snapshot_slot,
            current_slot,
            config.ema_half_life_ms,
        );
        let directional_base_price_ema_nad = directional_ema_u64(
            self.directional_base_price_ema_nad,
            cached_spot_base_price_nad,
            self.last_snapshot_slot,
            current_slot,
            config.directional_ema_half_life_ms,
        );
        let directional_quote_price_ema_nad = directional_ema_u64(
            self.directional_quote_price_ema_nad,
            cached_spot_quote_price_nad,
            self.last_snapshot_slot,
            current_slot,
            config.directional_ema_half_life_ms,
        );
        let curve_depth_ema_nad = ema_u128(
            self.curve_depth_ema_nad,
            observed_curve_depth_nad,
            self.last_snapshot_slot,
            current_slot,
            config.curve_depth_ema_half_life_ms,
        );

        Ok(Self {
            base_price_ema_nad,
            quote_price_ema_nad,
            directional_base_price_ema_nad,
            directional_quote_price_ema_nad,
            cached_spot_base_price_nad: current_base_price_nad,
            cached_spot_quote_price_nad: current_quote_price_nad,
            observed_curve_depth_nad: current_curve_depth_nad,
            curve_depth_ema_nad,
            last_snapshot_slot: current_slot,
        })
    }
}

impl Market {
    pub fn spot_value_in_opposite(&self, asset: MarketAsset, amount: u64) -> Result<u64> {
        require!(amount > 0, ErrorCode::AmountZero);
        let (from_reserve, to_reserve) = match asset {
            MarketAsset::Base => (
                self.base_side.reserves.live_reserve,
                self.quote_side.reserves.live_reserve,
            ),
            MarketAsset::Quote => (
                self.quote_side.reserves.live_reserve,
                self.base_side.reserves.live_reserve,
            ),
        };
        require!(from_reserve > 0 && to_reserve > 0, ErrorCode::InsufficientLiquidity);
        let value = (amount as u128)
            .checked_mul(to_reserve as u128)
            .and_then(|value| value.checked_div(from_reserve as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        u64::try_from(value).map_err(|_| ErrorCode::MarketMathOverflow.into())
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct RetentionTarget {
    pub required_nad: u128,
    pub stop_nad: u128,
    pub hard_cap_nad: u128,
    pub saturated: bool,
}
