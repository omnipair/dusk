use anchor_lang::prelude::*;

#[cfg(test)]
use std::cell::Cell;

use crate::{
    constants::{BPS_DENOMINATOR, MIN_LIQUIDITY, NAD, NAD_DECIMALS},
    errors::ErrorCode,
    math::{
        asymptotic_scaled_rate_nad, concentrated_hybrid_branch, concentrated_hybrid_branch_cached,
        concentrated_prepare_curve, concentrated_prepare_curve_cached, concentrated_prepare_curve_seeded_cached,
        decay_volatility_nad, denormalize_from_nad_floor, effective_rate_floor_nad, fee_share_cap_to_marginal_rate_nad,
        gross_fee_budget_floor, hard_total_fee_budget_floor, minimum_executable_input, normalize_to_nad,
        validate_fee_share_caps, volatility_after_success_nad, ConcentratedCommonNumeraire, ConcentratedEvaluation,
        ConcentratedGeometryCache, ConcentratedHybridBranch, ConcentratedInvariantSeed, ConcentratedPreparedCurve,
        ConcentratedSwapDirection, DynamicFeeConfig, DynamicFeePreState, DynamicFeeQuote,
        PreparedDivergenceStatePotential, CONCENTRATED_MATH_REVISION, MAX_COMMON_RESERVE,
    },
    shared::math::ceil_div,
    state::market::{
        AmmCurveParameters, AmmState, DeferredControllerTarget, Market, MarketAsset, PROTECTED_LIQUIDITY_COVERAGE_BPS,
        PROTECTED_LIQUIDITY_GUARD_BPS,
    },
};

#[cfg(test)]
use crate::math::{
    concentrated_evaluate, outward_divergence_fee_raw_saturating_prepared, prepare_outward_divergence_potential,
    PreparedOutwardDivergencePotential,
};

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
mod amm_engine_tests {
    include!("../tests/market/amm_engine.rs");
}

/// Executable AMM inventory. Unlike `live_reserve`, these coordinates exclude
/// accrued-but-unpaid lending interest because interest is claimable yield,
/// not compounding swap principal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CurveReservesNad {
    pub base: u128,
    pub quote: u128,
}

/// One fully evaluated executable curve state produced by the quote pipeline.
///
/// Reserve coordinates are normalized from the raw amounts that will actually
/// be credited/debited on-chain. Private identity fields prevent callers from
/// pairing the cached evaluation with different reserves, center, or curve
/// parameters merely to avoid a solve. This is an ephemeral plan value, not a
/// second persisted invariant proof hierarchy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CurveCheckpoint {
    pub(crate) reserves: CurveReservesNad,
    center_price_nad: u64,
    parameters: AmmCurveParameters,
    evaluation: ConcentratedEvaluation,
}

impl CurveCheckpoint {
    #[cfg(test)]
    pub(crate) fn evaluation_if_matches(
        self,
        market: &Market,
        current_slot: u64,
    ) -> Result<Option<ConcentratedEvaluation>> {
        Ok((self.reserves == market.curve_reserves_nad()?
            && self.center_price_nad == market.current_curve_center_price_nad()?
            && self.parameters == market.current_curve_parameters(current_slot))
        .then_some(self.evaluation))
    }

    pub(crate) fn validated_evaluation(self, market: &Market, current_slot: u64) -> Result<ConcentratedEvaluation> {
        require!(
            self.reserves == market.curve_reserves_nad()?
                && self.center_price_nad == market.current_curve_center_price_nad()?
                && self.parameters == market.current_curve_parameters(current_slot),
            ErrorCode::BrokenInvariant
        );
        Ok(self.evaluation)
    }

    /// Returns the evaluation carried by this identity-bound checkpoint.
    /// Callers that need to apply it to mutable market state must still use
    /// `validated_evaluation`; read-only projections may consume it directly.
    pub(crate) const fn evaluation(self) -> ConcentratedEvaluation {
        self.evaluation
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CurveQuote {
    pub amount_in: u64,
    pub amount_out: u64,
    pub start_price_nad: u64,
    pub end_price_nad: u64,
    pub(crate) endpoint: CurveCheckpoint,
}

impl Market {
    /// Aggregate accrued interest which has not yet been paid into the
    /// non-compounding interest vault. Debt-share rounding can momentarily put
    /// tracked principal above computed debt, so principal is clamped first.
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

    pub(crate) fn current_curve_parameters(&self, current_slot: u64) -> AmmCurveParameters {
        self.amm.effective_curve_parameters(&self.config.amm, current_slot)
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

    pub(crate) fn evaluate_current_curve(&self, current_slot: u64) -> Result<ConcentratedEvaluation> {
        let reserves = self.curve_reserves_nad()?;
        self.prepare_curve_for_reserves_nad(reserves, self.current_curve_center_price_nad()?, current_slot)?
            .evaluation()
    }

    /// Binds a prepared point to the currently applied center and parameters.
    pub(crate) fn checkpoint_for_prepared_curve(
        &self,
        prepared: ConcentratedPreparedCurve,
        current_slot: u64,
    ) -> Result<CurveCheckpoint> {
        let center_price_nad = u64::try_from(prepared.center_price_nad).map_err(|_| ErrorCode::MarketMathOverflow)?;
        let parameters = AmmCurveParameters {
            peak_depth_nad: u64::try_from(prepared.peak_depth_nad).map_err(|_| ErrorCode::MarketMathOverflow)?,
            fade_scale_nad: u64::try_from(prepared.fade_scale_nad).map_err(|_| ErrorCode::MarketMathOverflow)?,
        };
        require_eq!(
            center_price_nad,
            self.current_curve_center_price_nad()?,
            ErrorCode::BrokenInvariant
        );
        require!(
            parameters == self.current_curve_parameters(current_slot),
            ErrorCode::BrokenInvariant
        );
        Ok(CurveCheckpoint {
            reserves: CurveReservesNad {
                base: prepared.base_reserve_nad(),
                quote: prepared.quote_reserve_nad(),
            },
            center_price_nad,
            parameters,
            evaluation: prepared.evaluation()?,
        })
    }

    #[cfg(test)]
    pub(crate) fn evaluate_curve_candidate(
        &self,
        center_price_nad: u64,
        parameters: AmmCurveParameters,
    ) -> Result<ConcentratedEvaluation> {
        let reserves = self.curve_reserves_nad()?;
        concentrated_evaluate(
            reserves.base,
            reserves.quote,
            center_price_nad as u128,
            parameters.peak_depth_nad as u128,
            parameters.fade_scale_nad as u128,
        )
    }

    pub(crate) fn curve_q_per_share_nad(&self, balanced_equivalent_q_nad: u128) -> Result<u128> {
        let supply = self.base_side.shares.ylp_supply;
        require_eq!(supply, self.quote_side.shares.ylp_supply, ErrorCode::BrokenInvariant);
        require!(supply > 0, ErrorCode::SupplyUnderflow);
        let supply_nad = normalize_to_nad(supply as u128, self.base_side.asset_decimals)?;
        balanced_equivalent_q_nad
            .checked_mul(NAD as u128)
            .and_then(|value| value.checked_div(supply_nad))
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    }

    pub(crate) fn curve_marginal_price_nad(&self, current_slot: u64) -> Result<u64> {
        let reserves = self.curve_reserves_nad()?;
        let price = self
            .prepare_curve_for_reserves_nad(reserves, self.current_curve_center_price_nad()?, current_slot)?
            .marginal_price_nad()?;
        u64::try_from(price).map_err(|_| ErrorCode::MarketMathOverflow.into())
    }

    #[cfg(test)]
    pub(crate) fn quote_curve_exact_in(
        &self,
        asset_in: MarketAsset,
        amount_in: u64,
        current_slot: u64,
    ) -> Result<CurveQuote> {
        let reserves = self.curve_reserves_nad()?;
        let prepared =
            self.prepare_curve_for_reserves_nad(reserves, self.current_curve_center_price_nad()?, current_slot)?;
        self.quote_curve_exact_in_for_prepared_nad(asset_in, amount_in, prepared, current_slot)
    }

    /// Builds the one start-state preparation shared by divergence, output,
    /// and starting-price calculations.
    pub(crate) fn prepare_curve_for_reserves_nad(
        &self,
        reserves: CurveReservesNad,
        center_price_nad: u64,
        current_slot: u64,
    ) -> Result<ConcentratedPreparedCurve> {
        let parameters = self.current_curve_parameters(current_slot);
        if !parameters.is_cpmm()
            && self.amm.initialized
            && self.amm.invariant_d_nad > 0
            && self.amm.curve_math_revision == CONCENTRATED_MATH_REVISION
        {
            // The hint is useful both for the exact live state and for
            // sequential overlay quotes. It never narrows the authoritative
            // global bracket, so stale/different overlay inventory can only
            // reduce the optimization, not change the canonical result.
            concentrated_prepare_curve_seeded_cached(
                reserves.base,
                reserves.quote,
                center_price_nad as u128,
                parameters.peak_depth_nad as u128,
                parameters.fade_scale_nad as u128,
                self.amm.concentrated_geometry_cache,
                ConcentratedInvariantSeed::Hint(self.amm.invariant_d_nad),
            )
        } else if !parameters.is_cpmm()
            && self.amm.initialized
            && self.amm.curve_math_revision == CONCENTRATED_MATH_REVISION
            && self
                .amm
                .concentrated_geometry_cache
                .matches(parameters.peak_depth_nad as u128, parameters.fade_scale_nad as u128)
        {
            concentrated_prepare_curve_cached(
                reserves.base,
                reserves.quote,
                center_price_nad as u128,
                parameters.peak_depth_nad as u128,
                parameters.fade_scale_nad as u128,
                self.amm.concentrated_geometry_cache,
            )
        } else {
            concentrated_prepare_curve(
                reserves.base,
                reserves.quote,
                center_price_nad as u128,
                parameters.peak_depth_nad as u128,
                parameters.fade_scale_nad as u128,
            )
        }
    }

    pub(crate) fn quote_curve_exact_in_for_prepared_nad(
        &self,
        asset_in: MarketAsset,
        amount_in: u64,
        prepared: ConcentratedPreparedCurve,
        current_slot: u64,
    ) -> Result<CurveQuote> {
        require!(amount_in > 0, ErrorCode::AmountZero);
        let amount_in_nad = normalize_to_nad(amount_in as u128, self.side(asset_in).asset_decimals)?;
        require!(amount_in_nad > 0, ErrorCode::AmountZero);
        let solved_amount_out_nad = prepared.quote_exact_in(
            amount_in_nad,
            match asset_in {
                MarketAsset::Base => ConcentratedSwapDirection::BaseToQuote,
                MarketAsset::Quote => ConcentratedSwapDirection::QuoteToBase,
            },
        )?;
        let amount_out =
            denormalize_from_nad_floor(solved_amount_out_nad, self.side(asset_in.opposite()).asset_decimals)?;
        require!(amount_out > 0, ErrorCode::InsufficientOutputAmount);
        // The solver coordinate may contain sub-raw-token dust. Execution
        // floors to `amount_out`, so endpoint price, D, and Q must all use the
        // normalized raw debit rather than the larger unexecutable solve.
        let executable_amount_out_nad =
            normalize_to_nad(amount_out as u128, self.side(asset_in.opposite()).asset_decimals)?;
        require!(executable_amount_out_nad > 0, ErrorCode::InsufficientOutputAmount);

        let start_price_nad =
            u64::try_from(prepared.marginal_price_nad()?).map_err(|_| ErrorCode::MarketMathOverflow)?;
        let (base_after, quote_after) = match asset_in {
            MarketAsset::Base => (
                prepared
                    .base_reserve_nad()
                    .checked_add(amount_in_nad)
                    .ok_or(ErrorCode::ReserveOverflow)?,
                prepared
                    .quote_reserve_nad()
                    .checked_sub(executable_amount_out_nad)
                    .ok_or(ErrorCode::ReserveUnderflow)?,
            ),
            MarketAsset::Quote => (
                prepared
                    .base_reserve_nad()
                    .checked_sub(executable_amount_out_nad)
                    .ok_or(ErrorCode::ReserveUnderflow)?,
                prepared
                    .quote_reserve_nad()
                    .checked_add(amount_in_nad)
                    .ok_or(ErrorCode::ReserveOverflow)?,
            ),
        };
        let successor = prepared.prepare_successor(
            base_after,
            quote_after,
            ConcentratedInvariantSeed::Hint(prepared.invariant_d()),
        )?;
        let endpoint = self.checkpoint_for_prepared_curve(successor, current_slot)?;

        Ok(CurveQuote {
            amount_in,
            amount_out,
            start_price_nad,
            end_price_nad: u64::try_from(endpoint.evaluation.marginal_price_nad)
                .map_err(|_| ErrorCode::MarketMathOverflow)?,
            endpoint,
        })
    }
}

#[cfg(test)]
const fn direction(asset_in: MarketAsset) -> ConcentratedSwapDirection {
    match asset_in {
        MarketAsset::Base => ConcentratedSwapDirection::BaseToQuote,
        MarketAsset::Quote => ConcentratedSwapDirection::QuoteToBase,
    }
}

#[cfg(test)]
mod curve_tests {
    include!("../tests/market/curve.rs");
}

/// The implicit divergence endpoint is a one-dimensional convex solve.
/// Secant and Newton probes accelerate the ordinary path, while the exact
/// feasible/infeasible cost bracket remains authoritative even when wide
/// potential arithmetic saturates an accelerator. If token
/// granularity leaves no exact root, the lower endpoint deliberately charges
/// the rounding gap as fee.
const DIVERGENCE_ENDPOINT_MAX_ITERS: usize = u64::BITS as usize;

#[cfg(test)]
thread_local! {
    static DIVERGENCE_ENDPOINT_ITERATIONS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
fn reset_divergence_endpoint_iterations() {
    DIVERGENCE_ENDPOINT_ITERATIONS.with(|iterations| iterations.set(0));
}

#[cfg(test)]
fn divergence_endpoint_iterations() -> usize {
    DIVERGENCE_ENDPOINT_ITERATIONS.with(Cell::get)
}

#[derive(Clone, Copy)]
struct PreparedSwapDivergencePotential {
    center_input_reserve_raw: u128,
    start_input_reserve_raw: u128,
    coefficient_nad: u64,
    marginal_cap_nad: u64,
    maximum_surcharge: u64,
    state_potential: Option<PreparedDivergenceStatePotential>,
}

impl PreparedSwapDivergencePotential {
    fn new(
        center_input_reserve_raw: u128,
        start_input_reserve_raw: u128,
        coefficient_nad: u64,
        marginal_cap_nad: u64,
        maximum_surcharge: u64,
    ) -> Result<Self> {
        let state_potential = u64::try_from(center_input_reserve_raw)
            .ok()
            .map(|center| PreparedDivergenceStatePotential::new(center, coefficient_nad, marginal_cap_nad))
            .transpose()?;
        Ok(Self {
            center_input_reserve_raw,
            start_input_reserve_raw,
            coefficient_nad,
            marginal_cap_nad,
            maximum_surcharge,
            state_potential,
        })
    }

    fn uncapped_fee_probe(self, executable_input: u64) -> Result<(u128, bool)> {
        if self.coefficient_nad == 0 || self.marginal_cap_nad == 0 || self.maximum_surcharge == 0 {
            return Ok((0, false));
        }
        let Some(state_potential) = self.state_potential else {
            // A center above the complete u64 token-account domain means every
            // possible endpoint is still restorative; it is not a saturated
            // outward fee.
            return Ok((0, false));
        };
        let Ok(start_raw) = u64::try_from(self.start_input_reserve_raw) else {
            return Ok((u128::MAX, true));
        };
        let Some(end_raw) = start_raw.checked_add(executable_input) else {
            return Ok((u128::MAX, true));
        };
        let center_raw = u64::try_from(self.center_input_reserve_raw).map_err(|_| ErrorCode::MarketMathOverflow)?;
        let start_outward = start_raw.saturating_sub(center_raw);
        let end_outward = end_raw.saturating_sub(center_raw);
        let (fee, fee_saturated) = if end_outward <= start_outward {
            (0, false)
        } else {
            let (start, start_saturated) = state_potential.state_potential(start_outward)?;
            let (end, end_saturated) = state_potential.state_potential(end_outward)?;
            if start_saturated || end_saturated {
                (u128::MAX, true)
            } else {
                (end.checked_sub(start).ok_or(ErrorCode::MarketMathOverflow)?, false)
            }
        };
        Ok((fee, fee_saturated))
    }

    fn total_cost_probe(self, executable_input: u64) -> Result<(u128, bool)> {
        if executable_input == 0 {
            return Ok((0, false));
        }
        let (fee, fee_saturated) = self.uncapped_fee_probe(executable_input)?;
        if fee_saturated {
            return Ok((u128::MAX, true));
        }
        match (executable_input as u128).checked_add(fee.min(self.maximum_surcharge as u128)) {
            Some(cost) => Ok((cost, false)),
            None => Ok((u128::MAX, true)),
        }
    }

    fn marginal_rate_nad(self, executable_input: u64) -> Result<u64> {
        let (fee, saturated) = self.uncapped_fee_probe(executable_input)?;
        if saturated || fee >= self.maximum_surcharge as u128 {
            // Once the per-swap gross budget binds, the budgeted potential is
            // flat even though the underlying Huber state keeps increasing.
            return Ok(0);
        }
        let Some(state_potential) = self.state_potential else {
            return Ok(0);
        };
        let start_raw = u64::try_from(self.start_input_reserve_raw).map_err(|_| ErrorCode::MarketMathOverflow)?;
        let endpoint_raw = start_raw
            .checked_add(executable_input)
            .ok_or(ErrorCode::ReserveOverflow)?;
        let center_raw = u64::try_from(self.center_input_reserve_raw).map_err(|_| ErrorCode::MarketMathOverflow)?;
        state_potential.marginal_rate_nad(endpoint_raw.saturating_sub(center_raw))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SwapFeeBreakdown {
    pub reserve_credit: u64,
    pub base_fee_debit: u64,
    pub divergence_surcharge_debit: u64,
    pub volatility_surcharge_debit: u64,
    pub dynamic_surcharge_debit: u64,
    pub total_fee_debit: u64,
    pub retained_surcharge: u64,
    pub distributed_surcharge_debit: u64,
    pub amount_in_for_quote: u64,
    pub reserve_input_credit: u64,
    pub claimable_fee_debit: u64,
    pub base_fee_rate_nad: u64,
    pub divergence_fee_rate_nad: u64,
    pub volatility_fee_rate_nad: u64,
    pub total_fee_rate_nad: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AmmSwapQuote {
    pub asset_in: MarketAsset,
    pub amount_out: u64,
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
    endpoints: Option<AmmSwapEndpoints>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AmmSwapEndpoints {
    trade: CurveCheckpoint,
    reserve: CurveCheckpoint,
}

impl AmmSwapQuote {
    pub(crate) fn trade_endpoint(&self) -> Result<CurveCheckpoint> {
        self.endpoints
            .map(|endpoints| endpoints.trade)
            .ok_or_else(|| ErrorCode::BrokenInvariant.into())
    }

    pub(crate) fn reserve_endpoint(&self) -> Result<CurveCheckpoint> {
        self.endpoints
            .map(|endpoints| endpoints.reserve)
            .ok_or_else(|| ErrorCode::BrokenInvariant.into())
    }

    /// Leverage receipts intentionally contain only ABI-visible quote fields.
    /// Reconstructed quotes are valid for reserve-overlay simulations, but may
    /// never enter an endpoint-reusing execution path.
    #[allow(clippy::too_many_arguments)]
    #[cfg(test)]
    pub(crate) const fn new_without_endpoints(
        asset_in: MarketAsset,
        amount_out: u64,
        start_price_nad: u64,
        end_price_nad: u64,
        reserve_end_price_nad: u64,
        decayed_volatility_nad: u64,
        post_success_volatility_nad: u64,
        fee: SwapFeeBreakdown,
    ) -> Self {
        Self {
            asset_in,
            amount_out,
            start_price_nad,
            end_price_nad,
            reserve_end_price_nad,
            decayed_volatility_nad,
            post_success_volatility_nad,
            fee,
            endpoints: None,
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
    fee: DynamicFeeQuote,
}

impl Market {
    /// Net input used by the hLP pre-solver. It includes base and already-known
    /// volatility fees, but intentionally omits divergence. Because the final
    /// divergence fee can only reduce input, the pre-solve endpoint is a
    /// conservative outward path for the second pass.
    #[cfg(test)]
    pub(crate) fn preliminary_swap_inputs(
        &self,
        reserve_credit: u64,
        current_slot: u64,
    ) -> Result<PreliminarySwapInputs> {
        require!(reserve_credit > 0, ErrorCode::AmountZero);
        let pre_state = self.dynamic_fee_pre_state(current_slot)?;
        self.preliminary_swap_inputs_for_state(reserve_credit, current_slot, pre_state)
    }

    pub(crate) fn preliminary_swap_inputs_for_state(
        &self,
        reserve_credit: u64,
        current_slot: u64,
        pre_state: DynamicFeePreState,
    ) -> Result<PreliminarySwapInputs> {
        require!(reserve_credit > 0, ErrorCode::AmountZero);
        let config = self.dynamic_fee_config()?;
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
            fee: preliminary,
        })
    }

    /// Deterministic two-pass quote. The caller may run the hLP pre-solver with
    /// `preliminary_swap_input` first; this method then freezes the resulting
    /// curve state, obtains a conservative no-divergence endpoint, charges the
    /// path fee, and quotes once more with the final net input.
    #[cfg(test)]
    pub(crate) fn quote_amm_swap(
        &self,
        asset_in: MarketAsset,
        reserve_credit: u64,
        current_slot: u64,
    ) -> Result<AmmSwapQuote> {
        let pre_state = self.dynamic_fee_pre_state(current_slot)?;
        let preliminary = self.preliminary_swap_inputs_for_state(reserve_credit, current_slot, pre_state)?;
        self.quote_amm_swap_for_reserves_nad(
            asset_in,
            reserve_credit,
            current_slot,
            self.curve_reserves_nad()?,
            pre_state,
            preliminary,
        )
    }

    /// Quotes a second trade against the executable reserves left by `first`
    /// without mutating EMA, protected-liquidity, or ramp state. This is used
    /// by leverage health checks to price the exact unwind that would follow a
    /// successful opening/increase/decrease trade.
    #[cfg(test)]
    pub(crate) fn quote_amm_swap_after(
        &self,
        first: &AmmSwapQuote,
        asset_in: MarketAsset,
        reserve_credit: u64,
        current_slot: u64,
    ) -> Result<AmmSwapQuote> {
        require_eq!(
            first.fee.reserve_input_credit,
            first
                .fee
                .amount_in_for_quote
                .checked_add(first.fee.retained_surcharge)
                .ok_or(ErrorCode::ReserveOverflow)?,
            ErrorCode::BrokenInvariant
        );
        let mut reserves = self.curve_reserves_nad()?;
        let input_nad = normalize_to_nad(
            first.fee.reserve_input_credit as u128,
            self.side(first.asset_in).asset_decimals,
        )?;
        let output_nad = normalize_to_nad(
            first.amount_out as u128,
            self.side(first.asset_in.opposite()).asset_decimals,
        )?;
        match first.asset_in {
            MarketAsset::Base => {
                reserves.base = reserves.base.checked_add(input_nad).ok_or(ErrorCode::ReserveOverflow)?;
                reserves.quote = reserves
                    .quote
                    .checked_sub(output_nad)
                    .ok_or(ErrorCode::ReserveUnderflow)?;
            }
            MarketAsset::Quote => {
                reserves.quote = reserves
                    .quote
                    .checked_add(input_nad)
                    .ok_or(ErrorCode::ReserveOverflow)?;
                reserves.base = reserves
                    .base
                    .checked_sub(output_nad)
                    .ok_or(ErrorCode::ReserveUnderflow)?;
            }
        }
        let pre_state = DynamicFeePreState {
            center_price_nad: self.current_curve_center_price_nad()?,
            volatility_accumulator_nad: first.post_success_volatility_nad,
            volatility_last_update_slot: current_slot,
        };
        let preliminary = self.preliminary_swap_inputs_for_state(reserve_credit, current_slot, pre_state)?;
        self.quote_amm_swap_for_reserves_nad(asset_in, reserve_credit, current_slot, reserves, pre_state, preliminary)
    }

    pub(crate) fn quote_amm_swap_for_reserves_nad(
        &self,
        asset_in: MarketAsset,
        reserve_credit: u64,
        current_slot: u64,
        reserves: CurveReservesNad,
        pre_state: DynamicFeePreState,
        preliminary: PreliminarySwapInputs,
    ) -> Result<AmmSwapQuote> {
        // Preliminary fee input depends only on the frozen accumulator. The
        // invariant-coordinate divergence potential needs starting D and the
        // input-reserve displacement, not a provisional output quote or its
        // marginal prices. Avoiding that redundant CONCENTRATED quote removes an entire
        // reserve solve plus two marginal-price proofs from every swap.
        let preliminary_input = preliminary.amount_in_for_quote;
        let prepared = self.prepare_curve_for_reserves_nad(reserves, pre_state.center_price_nad, current_slot)?;
        let direction = match asset_in {
            MarketAsset::Base => ConcentratedSwapDirection::BaseToQuote,
            MarketAsset::Quote => ConcentratedSwapDirection::QuoteToBase,
        };
        if self.amm.retain_dynamic_surcharge && prepared.peak_depth_nad != 0 {
            // Retained dynamic fees are deposited after the invariant-preserving
            // exchange. Their maximum reserve credit is already known before
            // the implicit divergence solve. Reject an endpoint that must leave
            // the bounded Q48 common-coordinate domain instead of spending the
            // full solver budget on a quote that cannot be committed.
            let input_decimals = self.side(asset_in).asset_decimals;
            let retained_endpoint_input_nad = match asset_in {
                MarketAsset::Base => reserves
                    .base
                    .checked_add(normalize_to_nad(
                        preliminary.reserve_input_credit as u128,
                        input_decimals,
                    )?)
                    .ok_or(ErrorCode::ReserveOverflow)?,
                MarketAsset::Quote => reserves
                    .quote
                    .checked_add(normalize_to_nad(
                        preliminary.reserve_input_credit as u128,
                        input_decimals,
                    )?)
                    .ok_or(ErrorCode::ReserveOverflow)?,
            };
            let retained_endpoint_common_nad = prepared
                .input_common_scale(direction)?
                .to_common_floor(retained_endpoint_input_nad)?;
            require!(
                retained_endpoint_common_nad <= MAX_COMMON_RESERVE,
                ErrorCode::InvalidSettlementPrice
            );
        }
        let config = self.dynamic_fee_config()?;
        let invariant_d_nad = prepared.invariant_d();
        let start_input_reserve_nad = match asset_in {
            MarketAsset::Base => reserves.base,
            MarketAsset::Quote => reserves.quote,
        };
        // Solve against the executable endpoint, rather than the hypothetical
        // no-divergence endpoint. If `z` is the post-base/post-volatility
        // input and `d` is executable input, this enforces
        //
        //   d + F(x + d) - F(x) = z.
        //
        // For distributed surcharge, consecutive outward trades therefore
        // telescope through the same state potential instead of receiving a
        // split discount. Retained surcharge is deposited only after the
        // invariant-preserving trade, so it is correctly excluded from the
        // charged exchange path. The resulting next-state D behavior is
        // exercised separately by the retained split matrix.
        let input_decimals = self.side(asset_in).asset_decimals;
        require!(input_decimals <= NAD_DECIMALS, ErrorCode::UnsupportedAssetDecimals);
        let divergence_component_budget = gross_fee_budget_floor(reserve_credit, config.divergence_fee_share_cap_bps)?;
        let remaining_total_budget = hard_total_fee_budget_floor(reserve_credit)
            .checked_sub(preliminary.fee.total_fee_amount)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        let maximum_divergence_surcharge = divergence_component_budget.min(remaining_total_budget);
        let divergence_marginal_cap_nad = fee_share_cap_to_marginal_rate_nad(config.divergence_fee_share_cap_bps)?;
        let divergence_surcharge_amount = if config.divergence_coefficient_nad == 0
            || divergence_marginal_cap_nad == 0
            || maximum_divergence_surcharge == 0
        {
            0
        } else {
            require!(invariant_d_nad > 0, ErrorCode::BrokenInvariant);
            let decimal_scale = 10_u128
                .checked_pow((NAD_DECIMALS - input_decimals) as u32)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            // Compute the balanced threshold directly in raw input atoms. A
            // NAD-floor followed by a raw ceil can move the threshold one atom
            // inward and charge restorative flow. The common scaling cancels
            // algebraically from F, so CPMM and concentrated pools share this
            // one physical input-reserve potential.
            let (center_raw_numerator, center_raw_denominator) = match (prepared.common_numeraire(), asset_in) {
                (ConcentratedCommonNumeraire::Quote, MarketAsset::Base) => (
                    invariant_d_nad
                        .checked_mul(NAD as u128)
                        .ok_or(ErrorCode::MarketMathOverflow)?,
                    (pre_state.center_price_nad as u128)
                        .checked_mul(2)
                        .and_then(|value| value.checked_mul(decimal_scale))
                        .ok_or(ErrorCode::MarketMathOverflow)?,
                ),
                (ConcentratedCommonNumeraire::Quote, MarketAsset::Quote)
                | (ConcentratedCommonNumeraire::Base, MarketAsset::Base) => (
                    invariant_d_nad,
                    decimal_scale.checked_mul(2).ok_or(ErrorCode::MarketMathOverflow)?,
                ),
                (ConcentratedCommonNumeraire::Base, MarketAsset::Quote) => (
                    invariant_d_nad
                        .checked_mul(pre_state.center_price_nad as u128)
                        .ok_or(ErrorCode::MarketMathOverflow)?,
                    (NAD as u128)
                        .checked_mul(2)
                        .and_then(|value| value.checked_mul(decimal_scale))
                        .ok_or(ErrorCode::MarketMathOverflow)?,
                ),
            };
            let center_input_reserve_raw =
                ceil_div(center_raw_numerator, center_raw_denominator).ok_or(ErrorCode::MarketMathOverflow)?;
            require!(center_input_reserve_raw > 0, ErrorCode::BrokenInvariant);
            require_eq!(start_input_reserve_nad % decimal_scale, 0, ErrorCode::BrokenInvariant);
            let potential = PreparedSwapDivergencePotential::new(
                center_input_reserve_raw,
                start_input_reserve_nad / decimal_scale,
                config.divergence_coefficient_nad,
                divergence_marginal_cap_nad,
                maximum_divergence_surcharge,
            )?;
            // Solve the swap endpoint beside fee composition so the exact
            // debit and rounding contract remains visible at its production
            // boundary. The test-only harness covers the raw-domain edges.
            'implicit_divergence: {
                let available = preliminary_input;
                require!(available > 0, ErrorCode::InsufficientOutputAmount);

                // Zero executable input is always feasible. Gross input is
                // either exactly fee-free or infeasible. The component budget
                // is already embedded in `potential`, so keep the conservative
                // full-domain bracket instead of subtracting an estimate here.
                let mut low = 0_u64;
                let mut low_cost = 0_u128;
                let mut high = available;
                let (mut high_cost, mut high_cost_saturated) = potential.total_cost_probe(available)?;
                if !high_cost_saturated && high_cost == available as u128 {
                    break 'implicit_divergence 0;
                }
                require!(
                    high_cost_saturated || high_cost > available as u128,
                    ErrorCode::BrokenInvariant
                );

                // A saturated gross probe can otherwise require a full
                // 64-round midpoint walk merely to discover that even the
                // minimum curve-executable input quantum is unaffordable.
                if high_cost_saturated && high > 1 {
                    let (one_cost, one_cost_saturated) = potential.total_cost_probe(1)?;
                    if one_cost_saturated || one_cost > available as u128 {
                        break 'implicit_divergence available;
                    }
                    low = 1;
                    low_cost = one_cost;
                }

                // The first probe reuses the endpoint costs. Subsequent
                // probes are safeguarded Newton steps; midpoint fallback
                // remains authoritative whenever rounding or saturation
                // makes the accelerator unsuitable.
                let mut first_probe = true;
                let mut probe_from_high = true;
                for iteration in 0..DIVERGENCE_ENDPOINT_MAX_ITERS {
                    if low_cost == available as u128 || high - low <= 1 {
                        break;
                    }
                    #[cfg(test)]
                    DIVERGENCE_ENDPOINT_ITERATIONS.with(|iterations| iterations.set(iterations.get() + 1));

                    let mut probe = if first_probe && !high_cost_saturated {
                        first_probe = false;
                        let cost_span = high_cost.checked_sub(low_cost).ok_or(ErrorCode::FeeMathOverflow)?;
                        let target_offset = (available as u128)
                            .checked_sub(low_cost)
                            .ok_or(ErrorCode::FeeMathOverflow)?;
                        let reserve_span = (high - low) as u128;
                        let interpolated_offset = target_offset
                            .checked_mul(reserve_span)
                            .and_then(|value| value.checked_div(cost_span))
                            .ok_or(ErrorCode::FeeMathOverflow)?;
                        low.checked_add(u64::try_from(interpolated_offset).map_err(|_| ErrorCode::FeeMathOverflow)?)
                            .ok_or(ErrorCode::FeeMathOverflow)?
                    } else if first_probe {
                        first_probe = false;
                        low + (high - low) / 2
                    } else {
                        let (origin, residual, add_probe) = if probe_from_high {
                            if high_cost_saturated {
                                (0, 0, false)
                            } else {
                                (
                                    high,
                                    high_cost
                                        .checked_sub(available as u128)
                                        .ok_or(ErrorCode::FeeMathOverflow)?,
                                    false,
                                )
                            }
                        } else {
                            (
                                low,
                                (available as u128)
                                    .checked_sub(low_cost)
                                    .ok_or(ErrorCode::FeeMathOverflow)?,
                                true,
                            )
                        };
                        if origin == 0 && high_cost_saturated {
                            low + (high - low) / 2
                        } else {
                            let marginal_rate_nad = potential.marginal_rate_nad(origin)?;
                            let derivative_nad = (NAD as u128)
                                .checked_add(marginal_rate_nad as u128)
                                .ok_or(ErrorCode::FeeMathOverflow)?;
                            require_gte!(derivative_nad, NAD as u128, ErrorCode::BrokenInvariant);
                            let whole = residual
                                .checked_div(derivative_nad)
                                .and_then(|value| value.checked_mul(NAD as u128))
                                .ok_or(ErrorCode::FeeMathOverflow)?;
                            let remainder = residual.checked_rem(derivative_nad).ok_or(ErrorCode::FeeMathOverflow)?;
                            let remainder_numerator =
                                remainder.checked_mul(NAD as u128).ok_or(ErrorCode::FeeMathOverflow)?;
                            let fractional = if remainder_numerator == 0 {
                                0
                            } else {
                                (remainder_numerator - 1)
                                    .checked_div(derivative_nad)
                                    .and_then(|value| value.checked_add(1))
                                    .ok_or(ErrorCode::FeeMathOverflow)?
                            };
                            let step = u64::try_from(whole.checked_add(fractional).ok_or(ErrorCode::FeeMathOverflow)?)
                                .unwrap_or(u64::MAX)
                                .max(1);
                            if add_probe {
                                origin.checked_add(step).ok_or(ErrorCode::FeeMathOverflow)?
                            } else {
                                origin.saturating_sub(step)
                            }
                        }
                    };
                    if probe <= low || probe >= high {
                        probe = low + (high - low) / 2;
                    }

                    // Preserve hard liveness independently of accelerator
                    // quality: either possible child bracket must fit the
                    // ordinary bisections remaining after this round.
                    let remaining_rounds = DIVERGENCE_ENDPOINT_MAX_ITERS - iteration - 1;
                    let maximum_next_width = 1_u128.checked_shl(remaining_rounds as u32).unwrap_or(u128::MAX);
                    let minimum_safe_probe = (high as u128).saturating_sub(maximum_next_width).max((low as u128) + 1);
                    let maximum_safe_probe = (low as u128).saturating_add(maximum_next_width).min((high as u128) - 1);
                    require!(minimum_safe_probe <= maximum_safe_probe, ErrorCode::BrokenInvariant);
                    probe = u64::try_from((probe as u128).clamp(minimum_safe_probe, maximum_safe_probe))
                        .map_err(|_| ErrorCode::FeeMathOverflow)?;

                    let (probe_cost, probe_cost_saturated) = potential.total_cost_probe(probe)?;
                    if !probe_cost_saturated && probe_cost <= available as u128 {
                        low = probe;
                        low_cost = probe_cost;
                        probe_from_high = false;

                        // fee(low)=low_cost-low and
                        // deficit=available-low_cost. Because fee is
                        // nondecreasing, low+deficit is either the exact root
                        // or a tighter infeasible endpoint.
                        let deficit = (available as u128)
                            .checked_sub(low_cost)
                            .ok_or(ErrorCode::FeeMathOverflow)?;
                        let candidate = (low as u128).checked_add(deficit).ok_or(ErrorCode::FeeMathOverflow)?;
                        if candidate > low as u128 && candidate < high as u128 {
                            let candidate = u64::try_from(candidate).map_err(|_| ErrorCode::FeeMathOverflow)?;
                            let (candidate_cost, candidate_cost_saturated) = potential.total_cost_probe(candidate)?;
                            if candidate_cost_saturated || candidate_cost > available as u128 {
                                high = candidate;
                                high_cost = candidate_cost;
                                high_cost_saturated = candidate_cost_saturated;
                            } else {
                                require_eq!(candidate_cost, available as u128, ErrorCode::BrokenInvariant);
                                low = candidate;
                                low_cost = candidate_cost;
                            }
                        }
                    } else {
                        high = probe;
                        high_cost = probe_cost;
                        high_cost_saturated = probe_cost_saturated;
                        probe_from_high = true;
                    }
                }

                // Never accept an iteration-limit approximation. The exact
                // cost or adjacent infeasible endpoint proves maximality.
                require!(
                    low_cost == available as u128 || high - low <= 1,
                    ErrorCode::FeeMathOverflow
                );
                let surcharge = available.checked_sub(low).ok_or(ErrorCode::FeeMathOverflow)?;
                require!(surcharge > 0, ErrorCode::BrokenInvariant);
                break 'implicit_divergence surcharge;
            }
        };
        // Base fee, volatility decay, and the volatility surcharge were
        // frozen once in `preliminary`. Compose only the path-dependent
        // divergence debit here; rerunning the full fee quote would duplicate
        // its most expensive state-independent work (and did so a third time
        // when hLP predictive positioning was active).
        let mut dynamic = preliminary.fee;
        require_eq!(dynamic.divergence_surcharge_amount, 0, ErrorCode::BrokenInvariant);
        require!(
            divergence_surcharge_amount <= maximum_divergence_surcharge,
            ErrorCode::BrokenInvariant
        );
        dynamic.divergence_surcharge_amount = divergence_surcharge_amount;
        dynamic.dynamic_surcharge_amount = dynamic
            .volatility_surcharge_amount
            .checked_add(divergence_surcharge_amount)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        dynamic.total_fee_amount = dynamic
            .base_fee_amount
            .checked_add(dynamic.dynamic_surcharge_amount)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        require!(
            dynamic.total_fee_amount <= hard_total_fee_budget_floor(reserve_credit),
            ErrorCode::BrokenInvariant
        );
        dynamic.divergence_rate_nad = u64::try_from(
            (divergence_surcharge_amount as u128)
                .checked_mul(NAD as u128)
                .and_then(|value| value.checked_div(reserve_credit as u128))
                .ok_or(ErrorCode::FeeMathOverflow)?,
        )
        .map_err(|_| ErrorCode::FeeMathOverflow)?;
        dynamic.total_rate_nad = dynamic
            .base_rate_nad
            .checked_add(dynamic.volatility_rate_nad)
            .and_then(|value| value.checked_add(dynamic.divergence_rate_nad))
            .ok_or(ErrorCode::FeeMathOverflow)?;
        require!(dynamic.total_rate_nad < NAD, ErrorCode::BrokenInvariant);
        let amount_in_for_quote = preliminary
            .amount_in_for_quote
            .checked_sub(divergence_surcharge_amount)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        require!(amount_in_for_quote > 0, ErrorCode::InsufficientOutputAmount);
        require_gte!(
            amount_in_for_quote,
            minimum_executable_input(reserve_credit),
            ErrorCode::BrokenInvariant
        );
        let final_curve =
            self.quote_curve_exact_in_for_prepared_nad(asset_in, amount_in_for_quote, prepared, current_slot)?;

        let divergence_surcharge_debit = dynamic.divergence_surcharge_amount;
        let volatility_surcharge_debit = dynamic.volatility_surcharge_amount;
        let (retained_surcharge, distributed_surcharge_debit) = if self.amm.retain_dynamic_surcharge {
            (dynamic.dynamic_surcharge_amount, 0)
        } else {
            (0, dynamic.dynamic_surcharge_amount)
        };
        let claimable_fee_debit = dynamic
            .base_fee_amount
            .checked_add(distributed_surcharge_debit)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        let reserve_input_credit = amount_in_for_quote
            .checked_add(retained_surcharge)
            .ok_or(ErrorCode::ReserveOverflow)?;
        require_eq!(
            reserve_input_credit
                .checked_add(claimable_fee_debit)
                .ok_or(ErrorCode::MarketMathOverflow)?,
            reserve_credit,
            ErrorCode::BrokenInvariant
        );
        let trade_endpoint = final_curve.endpoint;
        let reserve_endpoint = if retained_surcharge == 0 {
            trade_endpoint
        } else {
            let mut endpoint_reserves = trade_endpoint.reserves;
            let retained_nad = normalize_to_nad(retained_surcharge as u128, self.side(asset_in).asset_decimals)?;
            require!(retained_nad > 0, ErrorCode::BrokenInvariant);
            match asset_in {
                MarketAsset::Base => {
                    endpoint_reserves.base = endpoint_reserves
                        .base
                        .checked_add(retained_nad)
                        .ok_or(ErrorCode::ReserveOverflow)?;
                }
                MarketAsset::Quote => {
                    endpoint_reserves.quote = endpoint_reserves
                        .quote
                        .checked_add(retained_nad)
                        .ok_or(ErrorCode::ReserveOverflow)?;
                }
            }
            let center_price_nad = prepared.center_price_nad;
            let numeraire = prepared.common_numeraire();
            let endpoint_base_common = numeraire
                .base_scale(center_price_nad)?
                .to_common_floor(endpoint_reserves.base)?;
            let endpoint_quote_common = numeraire
                .quote_scale(center_price_nad)?
                .to_common_floor(endpoint_reserves.quote)?;
            let endpoint_branch = if prepared.peak_depth_nad == 0 {
                require!(
                    endpoint_reserves.base > 0 && endpoint_reserves.quote > 0,
                    ErrorCode::InvalidArgument
                );
                ConcentratedHybridBranch::Inner
            } else {
                prepared
                    .geometry
                    .ok_or(ErrorCode::BrokenInvariant)?
                    .branch(endpoint_base_common, endpoint_quote_common)?
            };
            if endpoint_branch.is_exact_tail() {
                // An exact outer tail is CPMM. Prove its marginal mark
                // directly before paying for a second invariant solve. This
                // makes an extreme retained endpoint fail deterministically
                // instead of exhausting the SBF meter on a mark that must
                // round to zero.
                let tail_price_numerator = endpoint_quote_common
                    .checked_mul(center_price_nad)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                require!(
                    tail_price_numerator >= endpoint_base_common,
                    ErrorCode::InvalidSettlementPrice
                );
            }
            let prepared_endpoint = prepared.prepare_successor(
                endpoint_reserves.base,
                endpoint_reserves.quote,
                ConcentratedInvariantSeed::Hint(trade_endpoint.evaluation().invariant_d),
            )?;
            self.checkpoint_for_prepared_curve(prepared_endpoint, current_slot)?
        };
        let reserve_end_price_nad = u64::try_from(reserve_endpoint.evaluation().marginal_price_nad)
            .map_err(|_| ErrorCode::MarketMathOverflow)?;
        // A canonical marginal mark below one NAD atom rounds to zero. Such a
        // quote cannot be consumed by the shared risk engine (or inverted for
        // the opposite-side mark), so reject it here before preview and
        // execution can disagree. Retention can make the reserve endpoint
        // materially farther out than the trader's invariant endpoint; both
        // marks therefore need the same fail-closed domain check.
        require!(
            final_curve.end_price_nad > 0 && reserve_end_price_nad > 0,
            ErrorCode::InvalidSettlementPrice
        );
        let post_success_volatility_nad = volatility_after_success_nad(
            dynamic.decayed_volatility_nad,
            final_curve.start_price_nad,
            final_curve.end_price_nad,
            self.config.amm.volatility_shock_cap_nad,
            self.config.amm.volatility_cap_nad,
        )?;

        Ok(AmmSwapQuote {
            asset_in,
            amount_out: final_curve.amount_out,
            start_price_nad: final_curve.start_price_nad,
            end_price_nad: final_curve.end_price_nad,
            reserve_end_price_nad,
            decayed_volatility_nad: dynamic.decayed_volatility_nad,
            post_success_volatility_nad,
            fee: SwapFeeBreakdown {
                reserve_credit,
                base_fee_debit: dynamic.base_fee_amount,
                divergence_surcharge_debit,
                volatility_surcharge_debit,
                dynamic_surcharge_debit: dynamic.dynamic_surcharge_amount,
                total_fee_debit: dynamic.total_fee_amount,
                retained_surcharge,
                distributed_surcharge_debit,
                amount_in_for_quote,
                reserve_input_credit,
                claimable_fee_debit,
                base_fee_rate_nad: dynamic.base_rate_nad,
                divergence_fee_rate_nad: dynamic.divergence_rate_nad,
                volatility_fee_rate_nad: dynamic.volatility_rate_nad,
                total_fee_rate_nad: dynamic.total_rate_nad,
            },
            endpoints: Some(AmmSwapEndpoints {
                trade: trade_endpoint,
                reserve: reserve_endpoint,
            }),
        })
    }

    fn dynamic_fee_config(&self) -> Result<DynamicFeeConfig> {
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

/// Finds the conservative raw-token solution of
///
/// `executable + divergence_potential(executable) = available`.
///
/// `low` always has total cost at most `available`; `high` always has total
/// cost above it. The continuous potential is convex with a Huber-capped
/// marginal rate; raw-token rounding can make its discrete approximation
/// locally uneven. Every secant or Newton probe is therefore checked against
/// the exact-cost bracket, and an ordinary midpoint is used whenever rounding
/// or wide-value saturation would fail to shrink it.
#[cfg(test)]
fn implicit_divergence_surcharge_amount_core(
    divergence_potential: PreparedSwapDivergencePotential,
    available: u64,
) -> Result<u64> {
    require!(available > 0, ErrorCode::InsufficientOutputAmount);

    // Zero executable input is always feasible. Gross input is either exactly
    // fee-free or infeasible. The component budget is already embedded in the
    // prepared potential, so keep the conservative full-domain bracket instead
    // of subtracting an estimate here.
    let mut low = 0_u64;
    let mut low_cost = 0_u128;
    let mut high = available;
    let (mut high_cost, mut high_cost_saturated) = divergence_potential.total_cost_probe(available)?;
    if !high_cost_saturated && high_cost == available as u128 {
        return Ok(0);
    }
    require!(
        high_cost_saturated || high_cost > available as u128,
        ErrorCode::BrokenInvariant
    );

    // A saturated gross probe can otherwise require a full 64-round midpoint
    // walk merely to discover that even the minimum curve-executable input
    // quantum is unaffordable. Prove that boundary once. If it is feasible it
    // becomes a stronger lower endpoint; if not, returning the full residual
    // makes the quote reject at the positive curve-input gate.
    if high_cost_saturated && high > 1 {
        let (one_cost, one_cost_saturated) = divergence_potential.total_cost_probe(1)?;
        if one_cost_saturated || one_cost > available as u128 {
            return Ok(available);
        }
        low = 1;
        low_cost = one_cost;
    }

    // The first probe uses the exact endpoint costs already paid for above.
    // Linear interpolation avoids a marginal-rate evaluation on the ordinary
    // finite-cost path. If an endpoint cost is wider than u128, midpoint
    // fallback shrinks the raw-token bracket without interpreting saturation
    // as an economic value. Subsequent probes remain safeguarded Newton steps.
    let mut first_probe = true;
    let mut probe_from_high = true;
    for iteration in 0..DIVERGENCE_ENDPOINT_MAX_ITERS {
        if low_cost == available as u128 || high - low <= 1 {
            break;
        }
        #[cfg(test)]
        DIVERGENCE_ENDPOINT_ITERATIONS.with(|iterations| iterations.set(iterations.get() + 1));

        let mut probe = if first_probe && !high_cost_saturated {
            first_probe = false;
            let cost_span = high_cost.checked_sub(low_cost).ok_or(ErrorCode::FeeMathOverflow)?;
            let target_offset = (available as u128)
                .checked_sub(low_cost)
                .ok_or(ErrorCode::FeeMathOverflow)?;
            let reserve_span = (high - low) as u128;
            let interpolated_offset = target_offset
                .checked_mul(reserve_span)
                .and_then(|value| value.checked_div(cost_span))
                .ok_or(ErrorCode::FeeMathOverflow)?;
            low.checked_add(u64::try_from(interpolated_offset).map_err(|_| ErrorCode::FeeMathOverflow)?)
                .ok_or(ErrorCode::FeeMathOverflow)?
        } else if first_probe {
            first_probe = false;
            low + (high - low) / 2
        } else {
            let (origin, residual, add_probe) = if probe_from_high {
                if high_cost_saturated {
                    (0, 0, false)
                } else {
                    (
                        high,
                        high_cost
                            .checked_sub(available as u128)
                            .ok_or(ErrorCode::FeeMathOverflow)?,
                        false,
                    )
                }
            } else {
                (
                    low,
                    (available as u128)
                        .checked_sub(low_cost)
                        .ok_or(ErrorCode::FeeMathOverflow)?,
                    true,
                )
            };
            if origin == 0 && high_cost_saturated {
                low + (high - low) / 2
            } else {
                let marginal_rate_nad = divergence_potential.marginal_rate_nad(origin)?;
                let derivative_nad = (NAD as u128)
                    .checked_add(marginal_rate_nad as u128)
                    .ok_or(ErrorCode::FeeMathOverflow)?;
                require_gte!(derivative_nad, NAD as u128, ErrorCode::BrokenInvariant);
                let whole = residual
                    .checked_div(derivative_nad)
                    .and_then(|value| value.checked_mul(NAD as u128))
                    .ok_or(ErrorCode::FeeMathOverflow)?;
                let remainder = residual.checked_rem(derivative_nad).ok_or(ErrorCode::FeeMathOverflow)?;
                let remainder_numerator = remainder.checked_mul(NAD as u128).ok_or(ErrorCode::FeeMathOverflow)?;
                let fractional = if remainder_numerator == 0 {
                    0
                } else {
                    (remainder_numerator - 1)
                        .checked_div(derivative_nad)
                        .and_then(|value| value.checked_add(1))
                        .ok_or(ErrorCode::FeeMathOverflow)?
                };
                let step = u64::try_from(whole.checked_add(fractional).ok_or(ErrorCode::FeeMathOverflow)?)
                    .unwrap_or(u64::MAX)
                    .max(1);
                if add_probe {
                    origin.checked_add(step).ok_or(ErrorCode::FeeMathOverflow)?
                } else {
                    origin.saturating_sub(step)
                }
            }
        };
        if probe <= low || probe >= high {
            probe = low + (high - low) / 2;
        }

        // Preserve a hard liveness proof independently of how accurate the
        // secant/Newton accelerator is. After this round, either possible
        // child bracket must fit the number of ordinary bisections remaining.
        // Slack earned by an earlier strong cut remains available to later
        // accelerator probes.
        let remaining_rounds = DIVERGENCE_ENDPOINT_MAX_ITERS - iteration - 1;
        let maximum_next_width = 1_u128.checked_shl(remaining_rounds as u32).unwrap_or(u128::MAX);
        let minimum_safe_probe = (high as u128).saturating_sub(maximum_next_width).max((low as u128) + 1);
        let maximum_safe_probe = (low as u128).saturating_add(maximum_next_width).min((high as u128) - 1);
        require!(minimum_safe_probe <= maximum_safe_probe, ErrorCode::BrokenInvariant);
        probe = u64::try_from((probe as u128).clamp(minimum_safe_probe, maximum_safe_probe))
            .map_err(|_| ErrorCode::FeeMathOverflow)?;

        let (probe_cost, probe_cost_saturated) = divergence_potential.total_cost_probe(probe)?;
        if !probe_cost_saturated && probe_cost <= available as u128 {
            low = probe;
            low_cost = probe_cost;
            // The fresh feasible endpoint is closest to the root in ordinary
            // cases, so start the next safeguarded Newton probe from it.
            probe_from_high = false;

            // Let fee(low) = low_cost-low and deficit = available-low_cost.
            // Because fee is nondecreasing, candidate=low+deficit has cost at
            // least `available`. Exact classification either finds the root
            // immediately or gives a tighter infeasible high endpoint. This
            // is especially effective when raw-token rounding leaves a long
            // interval with the same fee.
            let deficit = (available as u128)
                .checked_sub(low_cost)
                .ok_or(ErrorCode::FeeMathOverflow)?;
            let candidate = (low as u128).checked_add(deficit).ok_or(ErrorCode::FeeMathOverflow)?;
            if candidate > low as u128 && candidate < high as u128 {
                let candidate = u64::try_from(candidate).map_err(|_| ErrorCode::FeeMathOverflow)?;
                let (candidate_cost, candidate_cost_saturated) = divergence_potential.total_cost_probe(candidate)?;
                if candidate_cost_saturated || candidate_cost > available as u128 {
                    high = candidate;
                    high_cost = candidate_cost;
                    high_cost_saturated = candidate_cost_saturated;
                } else {
                    require_eq!(candidate_cost, available as u128, ErrorCode::BrokenInvariant);
                    low = candidate;
                    low_cost = candidate_cost;
                }
            }
        } else {
            high = probe;
            high_cost = probe_cost;
            high_cost_saturated = probe_cost_saturated;
            probe_from_high = true;
        }
    }

    // Never silently accept an iteration-limit approximation. Exact total
    // cost proves the root directly; an adjacent infeasible upper endpoint
    // proves that `low` is the maximal feasible raw-token input.
    require!(
        low_cost == available as u128 || high - low <= 1,
        ErrorCode::FeeMathOverflow
    );

    // Charging the residual as divergence surcharge is exact whenever the
    // discrete equation has a root. Across an unavoidable raw-token gap it is
    // pool-favoring by less than the gap and always leaves the selected
    // executable endpoint fully funded.
    let surcharge = available.checked_sub(low).ok_or(ErrorCode::FeeMathOverflow)?;
    require!(surcharge > 0, ErrorCode::BrokenInvariant);
    Ok(surcharge)
}

#[cfg(test)]
fn divergence_total_cost(
    center_input_reserve_nad: u128,
    start_input_reserve_nad: u128,
    executable_input: u64,
    input_decimals: u8,
    coefficient_nad: u64,
    maximum_surcharge: u64,
) -> Result<u128> {
    let prepared = prepare_outward_divergence_potential(
        center_input_reserve_nad,
        start_input_reserve_nad,
        coefficient_nad,
        5_000,
    )?;
    Ok(divergence_total_cost_probe(&prepared, executable_input, input_decimals, maximum_surcharge)?.0)
}

#[cfg(test)]
fn divergence_total_cost_probe(
    prepared: &PreparedOutwardDivergencePotential,
    executable_input: u64,
    input_decimals: u8,
    maximum_surcharge: u64,
) -> Result<(u128, bool)> {
    if executable_input == 0 {
        return Ok((executable_input as u128, false));
    }
    let executable_input_nad = normalize_to_nad(executable_input as u128, input_decimals)?;
    let end_input_reserve_nad = prepared
        .start_input_reserve_nad()
        .checked_add(executable_input_nad)
        .ok_or(ErrorCode::ReserveOverflow)?;
    let (fee, fee_saturated) =
        outward_divergence_fee_raw_saturating_prepared(prepared, end_input_reserve_nad, input_decimals)?;
    if fee_saturated {
        return Ok((u128::MAX, true));
    }
    match (executable_input as u128).checked_add(fee.min(maximum_surcharge as u128)) {
        Some(cost) => Ok((cost, false)),
        None => Ok((u128::MAX, true)),
    }
}

#[cfg(test)]
fn divergence_fee_for_executable_input(
    center_input_reserve_nad: u128,
    start_input_reserve_nad: u128,
    executable_input: u64,
    input_decimals: u8,
    coefficient_nad: u64,
    maximum_surcharge: u64,
) -> Result<u64> {
    if executable_input == 0 || coefficient_nad == 0 {
        return Ok(0);
    }
    let prepared = prepare_outward_divergence_potential(
        center_input_reserve_nad,
        start_input_reserve_nad,
        coefficient_nad,
        5_000,
    )?;
    let (cost, saturated) =
        divergence_total_cost_probe(&prepared, executable_input, input_decimals, maximum_surcharge)?;
    require!(!saturated, ErrorCode::FeeMathOverflow);
    let fee = cost
        .checked_sub(executable_input as u128)
        .ok_or(ErrorCode::FeeMathOverflow)?;
    u64::try_from(fee).map_err(|_| ErrorCode::FeeMathOverflow.into())
}

#[cfg(test)]
mod swap_engine_tests {
    include!("../tests/market/swap_engine.rs");
}
