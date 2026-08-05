use anchor_lang::prelude::*;

use super::{
    AmmCurveParameters, AmmState, DeferredControllerTarget, Market, PROTECTED_LIQUIDITY_COVERAGE_BPS,
    PROTECTED_LIQUIDITY_GUARD_BPS,
};
use crate::{
    constants::{BPS_DENOMINATOR, MIN_LIQUIDITY, NAD},
    errors::ErrorCode,
    math::{
        concentrated_hybrid_branch, concentrated_hybrid_branch_cached, concentrated_prepare_curve,
        concentrated_prepare_curve_cached, concentrated_prepare_curve_seeded_cached, ConcentratedGeometryCache,
        ConcentratedInvariantSeed, CONCENTRATED_MATH_REVISION,
    },
    shared::math::ceil_div,
};

#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
thread_local! {
    static AMM_LIQUIDITY_CANDIDATE_SOLVES: Cell<u32> = const { Cell::new(0) };
}

#[cfg(test)]
fn reset_amm_liquidity_candidate_solves() {
    AMM_LIQUIDITY_CANDIDATE_SOLVES.with(|count| count.set(0));
}

#[cfg(test)]
fn amm_liquidity_candidate_solves() -> u32 {
    AMM_LIQUIDITY_CANDIDATE_SOLVES.with(Cell::get)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct AmmLiquidityEvaluation {
    pub(super) invariant_d: u128,
    pub(super) balanced_equivalent_q: u128,
    pub(super) geometry_cache: Option<ConcentratedGeometryCache>,
}

impl Market {
    fn advance_curve_revision(&mut self) -> Result<()> {
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
    fn defer_amm_retention_target(&mut self) -> Result<()> {
        if !self.amm.initialized {
            return Ok(());
        }
        let parameters = self.amm.applied_curve_parameters;
        if self.amm.ramp.active || (!parameters.is_cpmm() && self.config.amm.adjustment_step_nad > 0) {
            self.amm.mark_retention_target_stale();
        } else {
            self.amm.refresh_retention_target(self.amm.q_per_share_nad, 0)?;
        }
        Ok(())
    }

    /// The protection engine needs only D and Q. Keeping this separate from a
    /// full curve evaluation avoids paying for a marginal-price solve at every
    /// neutral checkpoint and for every recenter/ramp candidate.
    fn evaluate_amm_liquidity_candidate(
        &self,
        center_price_nad: u64,
        parameters: AmmCurveParameters,
    ) -> Result<AmmLiquidityEvaluation> {
        #[cfg(test)]
        AMM_LIQUIDITY_CANDIDATE_SOLVES.with(|count| count.set(count.get().saturating_add(1)));
        let reserves = self.curve_reserves_nad()?;
        let can_reuse_geometry = !parameters.is_cpmm()
            && self.amm.initialized
            && parameters == self.amm.applied_curve_parameters
            && self.amm.curve_math_revision == CONCENTRATED_MATH_REVISION
            && self
                .amm
                .concentrated_geometry_cache
                .matches(parameters.peak_depth_nad as u128, parameters.fade_scale_nad as u128);
        let prepared = if can_reuse_geometry && self.amm.initialized && self.amm.invariant_d_nad > 0 {
            // The stored D belongs to the preceding admitted state. It is
            // only a Newton starting point: CONCENTRATED rebuilds and verifies the
            // complete sign bracket for these new reserves/center/params.
            concentrated_prepare_curve_seeded_cached(
                reserves.base,
                reserves.quote,
                center_price_nad as u128,
                parameters.peak_depth_nad as u128,
                parameters.fade_scale_nad as u128,
                self.amm.concentrated_geometry_cache,
                ConcentratedInvariantSeed::Hint(self.amm.invariant_d_nad),
            )?
        } else if can_reuse_geometry {
            concentrated_prepare_curve_cached(
                reserves.base,
                reserves.quote,
                center_price_nad as u128,
                parameters.peak_depth_nad as u128,
                parameters.fade_scale_nad as u128,
                self.amm.concentrated_geometry_cache,
            )?
        } else {
            concentrated_prepare_curve(
                reserves.base,
                reserves.quote,
                center_price_nad as u128,
                parameters.peak_depth_nad as u128,
                parameters.fade_scale_nad as u128,
            )?
        };
        let invariant_d = prepared.invariant_d();
        let balanced_equivalent_q = prepared.balanced_equivalent_q()?;
        let geometry_cache = prepared.geometry_cache();
        Ok(AmmLiquidityEvaluation {
            invariant_d,
            balanced_equivalent_q,
            geometry_cache,
        })
    }

    pub(super) fn evaluate_current_amm_liquidity(&self, current_slot: u64) -> Result<AmmLiquidityEvaluation> {
        self.evaluate_amm_liquidity_candidate(
            self.current_curve_center_price_nad()?,
            self.current_curve_parameters(current_slot),
        )
    }

    /// A center move that leaves fixed reserves on the same outer hybrid
    /// branch changes only the fee anchor. That branch is exactly CPMM, so its
    /// economic Q is center-independent; D/Q reconstruction dust must not be
    /// charged as recenter impairment.
    fn recenter_stays_on_same_cpmm_tail(
        &self,
        candidate_center_price_nad: u64,
        parameters: AmmCurveParameters,
    ) -> Result<bool> {
        if parameters.is_cpmm() {
            return Ok(true);
        }
        let reserves = self.curve_reserves_nad()?;
        let use_cache = self.amm.initialized
            && parameters == self.amm.applied_curve_parameters
            && self.amm.curve_math_revision == CONCENTRATED_MATH_REVISION
            && self
                .amm
                .concentrated_geometry_cache
                .matches(parameters.peak_depth_nad as u128, parameters.fade_scale_nad as u128);
        let (current, candidate) = if use_cache {
            (
                concentrated_hybrid_branch_cached(
                    reserves.base,
                    reserves.quote,
                    self.amm.center_price_nad as u128,
                    parameters.peak_depth_nad as u128,
                    parameters.fade_scale_nad as u128,
                    self.amm.concentrated_geometry_cache,
                )?,
                concentrated_hybrid_branch_cached(
                    reserves.base,
                    reserves.quote,
                    candidate_center_price_nad as u128,
                    parameters.peak_depth_nad as u128,
                    parameters.fade_scale_nad as u128,
                    self.amm.concentrated_geometry_cache,
                )?,
            )
        } else {
            (
                concentrated_hybrid_branch(
                    reserves.base,
                    reserves.quote,
                    self.amm.center_price_nad as u128,
                    parameters.peak_depth_nad as u128,
                    parameters.fade_scale_nad as u128,
                )?,
                concentrated_hybrid_branch(
                    reserves.base,
                    reserves.quote,
                    candidate_center_price_nad as u128,
                    parameters.peak_depth_nad as u128,
                    parameters.fade_scale_nad as u128,
                )?,
            )
        };
        Ok(current.same_exact_tail(candidate))
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
        let evaluation = self.evaluate_amm_liquidity_candidate(center_price_nad, self.config.amm.curve_parameters())?;
        let q_per_share_nad = self.curve_q_per_share_nad(evaluation.balanced_equivalent_q)?;
        let mut state = AmmState::initialize(&self.config.amm, center_price_nad, q_per_share_nad, current_slot)?;
        state.commit_invariant(evaluation.invariant_d)?;
        // At the balanced initial center there is no requested move to fund.
        // Materialize only the scalar cap here; the next genuine operation
        // evaluates at most one controller target.
        state.refresh_retention_target(q_per_share_nad, 0)?;
        self.amm = state;
        Ok(true)
    }

    /// Checkpoint a completed reserve/share mutation as economically neutral.
    /// This preserves, but never creates, the retained-surcharge budget. The
    /// returned full evaluation lets a swap reuse the same final solve for its
    /// scalar risk observation after inline hLP settlement.
    pub(crate) fn checkpoint_amm_neutral_inventory(
        &mut self,
        current_slot: u64,
    ) -> Result<crate::math::ConcentratedEvaluation> {
        self.ensure_amm_initialized(current_slot)?;
        require!(self.amm.initialized, ErrorCode::BrokenInvariant);
        let evaluation = self.evaluate_current_curve(current_slot)?;
        let q_per_share_nad = self.curve_q_per_share_nad(evaluation.balanced_equivalent_q)?;
        self.amm.commit_invariant(evaluation.invariant_d)?;
        self.amm.checkpoint_neutral_liquidity(q_per_share_nad);
        self.defer_amm_retention_target()?;
        Ok(evaluation)
    }

    /// Same accounting checkpoint without recomputing the forward protection
    /// target. Composite transitions use this between internal legs and
    /// refresh once after their final reserve state is known.
    pub(crate) fn checkpoint_amm_neutral_inventory_raw(&mut self, current_slot: u64) -> Result<()> {
        self.ensure_amm_initialized(current_slot)?;
        if !self.amm.initialized {
            return Ok(());
        }
        let evaluation = self.evaluate_current_amm_liquidity(current_slot)?;
        let q_per_share_nad = self.curve_q_per_share_nad(evaluation.balanced_equivalent_q)?;
        self.amm.commit_invariant(evaluation.invariant_d)?;
        self.amm.checkpoint_neutral_liquidity(q_per_share_nad);
        Ok(())
    }

    /// Called only after the neutral post-trade state has been checkpointed and
    /// the retained surcharge has then been added to executable reserves.
    #[cfg(test)]
    pub(crate) fn checkpoint_amm_retained_surcharge(&mut self, current_slot: u64) -> Result<()> {
        self.checkpoint_amm_retained_surcharge_raw(current_slot)?;
        self.defer_amm_retention_target()
    }

    #[cfg(test)]
    pub(crate) fn checkpoint_amm_retained_surcharge_raw(&mut self, current_slot: u64) -> Result<()> {
        require!(self.amm.initialized, ErrorCode::BrokenInvariant);
        let evaluation = self.evaluate_current_amm_liquidity(current_slot)?;
        let q_per_share_nad = self.curve_q_per_share_nad(evaluation.balanced_equivalent_q)?;
        self.amm.commit_invariant(evaluation.invariant_d)?;
        self.amm.checkpoint_retained_surcharge(q_per_share_nad)?;
        Ok(())
    }

    /// Explicit socialized-loss checkpoint. Accrued unpaid interest has already
    /// been removed from curve reserves, so only actual executable-liquidity
    /// loss consumes protected profit.
    pub(crate) fn checkpoint_amm_socialized_loss_raw(&mut self, current_slot: u64) -> Result<()> {
        require!(self.amm.initialized, ErrorCode::BrokenInvariant);
        let evaluation = self.evaluate_current_amm_liquidity(current_slot)?;
        let q_per_share_nad = self.curve_q_per_share_nad(evaluation.balanced_equivalent_q)?;
        self.amm.commit_invariant(evaluation.invariant_d)?;
        self.amm.checkpoint_recenter_or_loss(q_per_share_nad);
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
    /// applied curve from one evaluation. Liquidity mutations used to call
    /// `finalize_amm_transition` and then solve the identical curve again for
    /// risk observation; this path shares that single canonical result.
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
            self.amm.clear_invariant();
            self.amm.q_per_share_nad = 0;
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
        let was_initialized = self.amm.initialized;
        let reserves = self.curve_reserves_nad()?;
        if reserves.base == 0 && reserves.quote == 0 {
            return Ok(());
        }
        require!(
            reserves.base > 0 && reserves.quote > 0,
            ErrorCode::InsufficientLiquidity
        );
        require!(self.base_side.shares.ylp_supply > 0, ErrorCode::SupplyUnderflow);

        let evaluation = self.evaluate_current_curve(current_slot)?;
        let q_per_share_nad = self.curve_q_per_share_nad(evaluation.balanced_equivalent_q)?;
        if was_initialized {
            self.amm.commit_invariant(evaluation.invariant_d)?;
            self.amm.checkpoint_neutral_liquidity(q_per_share_nad);
            self.defer_amm_retention_target()?;
        } else {
            let center_price_nad = self.current_curve_center_price_nad()?;
            let mut state = AmmState::initialize(&self.config.amm, center_price_nad, q_per_share_nad, current_slot)?;
            state.commit_invariant(evaluation.invariant_d)?;
            state.refresh_retention_target(q_per_share_nad, 0)?;
            self.amm = state;
        }
        self.advance_curve_revision()?;
        self.observe_exact_risk_from_curve_evaluation(evaluation, current_slot)
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

        let mut pending = self.amm.deferred_controller_target;
        if pending.is_active() {
            require!(
                pending.kind == DeferredControllerTarget::RAMP || pending.kind == DeferredControllerTarget::RECENTER,
                ErrorCode::BrokenInvariant
            );
            if pending.created_slot >= current_slot {
                return Ok(false);
            }

            if pending.kind == DeferredControllerTarget::RAMP && !self.amm.ramp.active {
                self.amm.deferred_controller_target.clear();
                pending.clear();
            } else if pending.kind == DeferredControllerTarget::RECENTER {
                let center = self.amm.center_price_nad;
                let ema = self.amm.price_ema_nad;
                let distance = symmetric_distance_nad(center, ema)?;
                let target_still_reachable = if pending.center_price_nad > center {
                    ema >= pending.center_price_nad
                } else if pending.center_price_nad < center {
                    ema <= pending.center_price_nad
                } else {
                    false
                };
                if distance < self.config.amm.adjustment_threshold_nad as u128 || !target_still_reachable {
                    self.amm.deferred_controller_target.clear();
                    self.amm.refresh_retention_target(self.amm.q_per_share_nad, 0)?;
                    pending.clear();
                }
            }

            if pending.is_active() {
                let reserves = self.curve_reserves_nad()?;
                let base_delta = reserves.base.abs_diff(pending.evaluated_base_reserve_nad);
                let quote_delta = reserves.quote.abs_diff(pending.evaluated_quote_reserve_nad);
                let base_threshold = (pending.evaluated_base_reserve_nad / BPS_DENOMINATOR as u128).max(1);
                let quote_threshold = (pending.evaluated_quote_reserve_nad / BPS_DENOMINATOR as u128).max(1);
                let reserves_changed_materially = base_delta >= base_threshold || quote_delta >= quote_threshold;
                // A cap-bound target is impossible under the admitted
                // controller request. Reserve motion must not turn every user
                // operation back into an expensive probe: only governance
                // changing that request (or the EMA reversal cancellation
                // above) clears saturation.
                if pending.saturated {
                    return Ok(false);
                }
                if !reserves_changed_materially && self.amm.spendable_protected_profit_nad() < pending.required_nad {
                    return Ok(false);
                }

                let evaluation = self.evaluate_amm_liquidity_candidate(pending.center_price_nad, pending.parameters)?;
                let candidate_q = if pending.kind == DeferredControllerTarget::RECENTER
                    && self.recenter_stays_on_same_cpmm_tail(pending.center_price_nad, pending.parameters)?
                {
                    self.amm.q_per_share_nad
                } else {
                    self.curve_q_per_share_nad(evaluation.balanced_equivalent_q)?
                };
                let covered = covered_impairment_nad(self.amm.q_per_share_nad, candidate_q)?;
                if !self.amm.recenter_is_funded(covered) {
                    let impairment = self.amm.q_per_share_nad.saturating_sub(candidate_q);
                    let target = self
                        .amm
                        .refresh_retention_target(self.amm.q_per_share_nad, impairment)?;
                    self.amm.deferred_controller_target.required_nad = target.required_nad;
                    self.amm.deferred_controller_target.evaluated_base_reserve_nad = reserves.base;
                    self.amm.deferred_controller_target.evaluated_quote_reserve_nad = reserves.quote;
                    self.amm.deferred_controller_target.saturated = target.saturated;
                    return Ok(false);
                }

                self.amm.deferred_controller_target.clear();
                if pending.kind == DeferredControllerTarget::RAMP {
                    self.amm.commit_applied_curve_parameters(
                        pending.parameters,
                        evaluation.geometry_cache,
                        current_slot,
                    )?;
                    self.amm.commit_invariant(evaluation.invariant_d)?;
                    self.amm.checkpoint_recenter_or_loss(candidate_q);
                    self.amm.settle_ramp(current_slot);
                } else {
                    self.amm.commit_recenter(
                        &self.config.amm,
                        pending.center_price_nad,
                        evaluation.invariant_d,
                        candidate_q,
                        covered,
                        current_slot,
                    )?;
                }
                self.defer_amm_retention_target()?;
                return Ok(true);
            }
        }

        if self.amm.ramp.active && current_slot > self.amm.last_ramp_update_slot {
            let applied = self.amm.applied_curve_parameters;
            let desired = self.amm.desired_curve_parameters(&self.config.amm, current_slot);
            if desired == applied {
                self.amm.last_ramp_update_slot = current_slot;
                self.amm.settle_ramp(current_slot);
                return Ok(false);
            }

            let reserves = self.curve_reserves_nad()?;
            let evaluation = self.evaluate_amm_liquidity_candidate(self.amm.center_price_nad, desired)?;
            let candidate_q = self.curve_q_per_share_nad(evaluation.balanced_equivalent_q)?;
            let covered = covered_impairment_nad(self.amm.q_per_share_nad, candidate_q)?;
            if self.amm.recenter_is_funded(covered) {
                self.amm
                    .commit_applied_curve_parameters(desired, evaluation.geometry_cache, current_slot)?;
                self.amm.commit_invariant(evaluation.invariant_d)?;
                self.amm.checkpoint_recenter_or_loss(candidate_q);
                self.amm.settle_ramp(current_slot);
                self.defer_amm_retention_target()?;
                return Ok(true);
            }

            let impairment = self.amm.q_per_share_nad.saturating_sub(candidate_q);
            let target = self
                .amm
                .refresh_retention_target(self.amm.q_per_share_nad, impairment)?;
            self.amm.last_ramp_update_slot = current_slot;
            self.amm.deferred_controller_target = DeferredControllerTarget {
                kind: DeferredControllerTarget::RAMP,
                center_price_nad: self.amm.center_price_nad,
                parameters: desired,
                required_nad: target.required_nad,
                evaluated_base_reserve_nad: reserves.base,
                evaluated_quote_reserve_nad: reserves.quote,
                created_slot: current_slot,
                saturated: target.saturated,
            };
            return Ok(false);
        }

        let parameters = self.amm.applied_curve_parameters;
        if self.config.amm.adjustment_step_nad == 0 || self.amm.ramp.active {
            return Ok(false);
        }
        // Ramp and center movement share one controller-move allowance. A
        // completed ramp may clear `ramp.active` immediately, so its committed
        // slot must still suppress a second center mutation in that slot.
        if current_slot <= self.amm.last_ramp_update_slot {
            return Ok(false);
        }
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
        let candidate_center = if target > center {
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
            center
        };
        if candidate_center == self.amm.center_price_nad {
            return Ok(false);
        }
        let reserves = self.curve_reserves_nad()?;
        let evaluation = self.evaluate_amm_liquidity_candidate(candidate_center, parameters)?;
        let candidate_q = if self.recenter_stays_on_same_cpmm_tail(candidate_center, parameters)? {
            self.amm.q_per_share_nad
        } else {
            self.curve_q_per_share_nad(evaluation.balanced_equivalent_q)?
        };
        let covered = covered_impairment_nad(self.amm.q_per_share_nad, candidate_q)?;
        if self.amm.recenter_is_funded(covered) {
            self.amm.commit_recenter(
                &self.config.amm,
                candidate_center,
                evaluation.invariant_d,
                candidate_q,
                covered,
                current_slot,
            )?;
            self.defer_amm_retention_target()?;
            return Ok(true);
        }

        let impairment = self.amm.q_per_share_nad.saturating_sub(candidate_q);
        let target = self
            .amm
            .refresh_retention_target(self.amm.q_per_share_nad, impairment)?;
        self.amm.deferred_controller_target = DeferredControllerTarget {
            kind: DeferredControllerTarget::RECENTER,
            center_price_nad: candidate_center,
            parameters,
            required_nad: target.required_nad,
            evaluated_base_reserve_nad: reserves.base,
            evaluated_quote_reserve_nad: reserves.quote,
            created_slot: current_slot,
            saturated: target.saturated,
        };
        Ok(false)
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
    /// supply, so recomputing the same D/Q here would be redundant and
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

fn covered_impairment_nad(current_q: u128, candidate_q: u128) -> Result<u128> {
    let impairment = current_q.saturating_sub(candidate_q);
    if impairment == 0 {
        return Ok(0);
    }
    let covered = mul_bps_ceil(impairment, PROTECTED_LIQUIDITY_COVERAGE_BPS)?;
    covered
        .checked_add(mul_bps_ceil(current_q, PROTECTED_LIQUIDITY_GUARD_BPS)?)
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
mod tests {
    include!("../../tests/state/amm_engine.rs");
}
