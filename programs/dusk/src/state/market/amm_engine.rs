use anchor_lang::prelude::*;

use super::{
    AmmCurveParameters, AmmState, CurveStateCertificate, Market, MarketAsset, PROTECTED_LIQUIDITY_COVERAGE_BPS,
    PROTECTED_LIQUIDITY_GUARD_BPS,
};
use crate::{
    constants::{BPS_DENOMINATOR, MIN_LIQUIDITY, NAD},
    errors::ErrorCode,
    math::{
        concentrated_balanced_equivalent_q, concentrated_hybrid_branch, concentrated_prepare_curve,
        concentrated_prepare_curve_with_hint, ConcentratedHybridBranch,
    },
    shared::math::ceil_div,
};

const MAX_FUNDED_STEP_HALVINGS: u32 = 8;
const MAX_FUNDED_CANDIDATE_SOLVES: u32 = MAX_FUNDED_STEP_HALVINGS + 1;

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
struct AmmLiquidityEvaluation {
    invariant_d: u128,
    invariant_d_high: u128,
    balanced_equivalent_q: u128,
}

impl Market {
    /// Parks curve/risk observations after the last public yLP exits. The
    /// permanently burned `MIN_LIQUIDITY` shares and their proportional token
    /// dust remain in reserves, but no price-consuming action may use that
    /// sub-floor inner state. A later two-sided deposit rebuilds a fresh exact
    /// observation once supported liquidity is restored.
    pub(crate) fn park_amm_after_full_public_liquidity_exit(&mut self, current_slot: u64) -> Result<()> {
        require_eq!(
            self.base_side.shares.ylp_supply,
            self.quote_side.shares.ylp_supply,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            self.base_side.shares.ylp_supply,
            MIN_LIQUIDITY,
            ErrorCode::InvalidArgument
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

        self.amm.clear_invariant_bracket();
        self.amm.q_per_share_nad = 0;
        self.amm.protected_floor_per_share_nad = 0;
        self.amm.retention_required_nad = 0;
        self.amm.retention_stop_nad = 0;
        self.amm.retention_hard_cap_nad = 0;
        self.amm.retain_dynamic_surcharge = false;
        self.amm.retention_target_saturated = false;
        self.amm.retention_target_stale = false;
        self.amm.risk_curve_cache = Default::default();
        self.amm.exact_curve_observation = Default::default();
        self.risk = Default::default();
        self.last_update_slot = current_slot;
        Ok(())
    }

    /// Reserve mutations invalidate a previously projected center/ramp cost.
    /// Keep the last certified target sticky and retain dynamic surcharge until
    /// the permissionless maintenance instruction values the actual next move.
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
        let prepared = if !parameters.is_cpmm() && self.amm.initialized && self.amm.invariant_d_nad > 0 {
            // The stored D belongs to the preceding admitted state. It is
            // only a Newton starting point: CONCENTRATED rebuilds and certifies the
            // complete sign bracket for these new reserves/center/params.
            concentrated_prepare_curve_with_hint(
                reserves.base,
                reserves.quote,
                center_price_nad as u128,
                parameters.peak_depth_nad as u128,
                parameters.imbalance_scale_nad as u128,
                self.amm.invariant_d_nad,
            )?
        } else {
            concentrated_prepare_curve(
                reserves.base,
                reserves.quote,
                center_price_nad as u128,
                parameters.peak_depth_nad as u128,
                parameters.imbalance_scale_nad as u128,
            )?
        };
        let (invariant_d, invariant_d_high) = prepared.invariant_bracket();
        Ok(AmmLiquidityEvaluation {
            invariant_d,
            invariant_d_high,
            balanced_equivalent_q: concentrated_balanced_equivalent_q(invariant_d, center_price_nad as u128)?,
        })
    }

    fn evaluate_current_amm_liquidity(&self, current_slot: u64) -> Result<AmmLiquidityEvaluation> {
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
        let current = concentrated_hybrid_branch(
            reserves.base,
            reserves.quote,
            self.amm.center_price_nad as u128,
            parameters.peak_depth_nad as u128,
            parameters.imbalance_scale_nad as u128,
        )?;
        let candidate = concentrated_hybrid_branch(
            reserves.base,
            reserves.quote,
            candidate_center_price_nad as u128,
            parameters.peak_depth_nad as u128,
            parameters.imbalance_scale_nad as u128,
        )?;
        Ok(current == candidate && current != ConcentratedHybridBranch::Inner)
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
        state.commit_invariant_bracket(evaluation.invariant_d, evaluation.invariant_d_high)?;
        // At the balanced initial center there is no requested move to fund.
        // Materialize only the scalar cap here; hypothetical center solves are
        // deferred to the bounded maintenance instruction.
        state.refresh_retention_target(q_per_share_nad, 0)?;
        self.amm = state;
        Ok(true)
    }

    /// Checkpoint a completed reserve/share mutation as economically neutral.
    /// This preserves, but never creates, the retained-surcharge budget.
    pub(crate) fn checkpoint_amm_neutral_inventory(&mut self, current_slot: u64) -> Result<()> {
        self.checkpoint_amm_neutral_inventory_raw(current_slot)?;
        self.defer_amm_retention_target()
    }

    /// Low-heap neutral checkpoint for an hLP swap. One full final-curve
    /// evaluation supplies both AMM D/Q accounting and the exact risk
    /// observation; expensive pessimistic risk shapes are left cached only
    /// when their inputs remain identical.
    pub(crate) fn checkpoint_amm_neutral_inventory_and_observe_risk(&mut self, current_slot: u64) -> Result<()> {
        self.ensure_amm_initialized(current_slot)?;
        require!(self.amm.initialized, ErrorCode::BrokenInvariant);
        let evaluation = self.evaluate_current_curve(current_slot)?;
        let q_per_share_nad = self.curve_q_per_share_nad(evaluation.balanced_equivalent_q)?;
        self.amm
            .commit_invariant_bracket(evaluation.invariant_d, evaluation.invariant_d_high)?;
        self.amm.checkpoint_neutral_liquidity(q_per_share_nad);
        self.defer_amm_retention_target()?;
        self.observe_risk_from_curve_evaluation(evaluation, current_slot)
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
        self.checkpoint_amm_neutral_from_liquidity_evaluation(evaluation)
    }

    /// Reuses a quote-time full evaluation only after proving that its raw
    /// executable reserves, center, and applied parameters are still exact.
    pub(crate) fn checkpoint_amm_neutral_inventory_from_certificate(
        &mut self,
        certificate: CurveStateCertificate,
        current_slot: u64,
    ) -> Result<()> {
        self.ensure_amm_initialized(current_slot)?;
        require!(self.amm.initialized, ErrorCode::BrokenInvariant);
        let evaluation = certificate.validated_evaluation(self, current_slot)?;
        self.checkpoint_amm_neutral_from_liquidity_evaluation(AmmLiquidityEvaluation {
            invariant_d: evaluation.invariant_d,
            invariant_d_high: evaluation.invariant_d_high,
            balanced_equivalent_q: evaluation.balanced_equivalent_q,
        })
    }

    fn checkpoint_amm_neutral_from_liquidity_evaluation(&mut self, evaluation: AmmLiquidityEvaluation) -> Result<()> {
        let q_per_share_nad = self.curve_q_per_share_nad(evaluation.balanced_equivalent_q)?;
        self.amm
            .commit_invariant_bracket(evaluation.invariant_d, evaluation.invariant_d_high)?;
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

    pub(crate) fn checkpoint_amm_retained_surcharge_raw(&mut self, current_slot: u64) -> Result<()> {
        require!(self.amm.initialized, ErrorCode::BrokenInvariant);
        let evaluation = self.evaluate_current_amm_liquidity(current_slot)?;
        self.checkpoint_amm_retained_from_liquidity_evaluation(evaluation)
    }

    pub(crate) fn checkpoint_amm_retained_surcharge_from_certificate(
        &mut self,
        certificate: CurveStateCertificate,
        current_slot: u64,
    ) -> Result<()> {
        require!(self.amm.initialized, ErrorCode::BrokenInvariant);
        let evaluation = certificate.validated_evaluation(self, current_slot)?;
        self.checkpoint_amm_retained_from_liquidity_evaluation(AmmLiquidityEvaluation {
            invariant_d: evaluation.invariant_d,
            invariant_d_high: evaluation.invariant_d_high,
            balanced_equivalent_q: evaluation.balanced_equivalent_q,
        })
    }

    fn checkpoint_amm_retained_from_liquidity_evaluation(&mut self, evaluation: AmmLiquidityEvaluation) -> Result<()> {
        let q_per_share_nad = self.curve_q_per_share_nad(evaluation.balanced_equivalent_q)?;
        self.amm
            .commit_invariant_bracket(evaluation.invariant_d, evaluation.invariant_d_high)?;
        self.amm.checkpoint_retained_surcharge(q_per_share_nad)?;
        Ok(())
    }

    /// Explicit socialized-loss checkpoint. Accrued unpaid interest has already
    /// been removed from curve reserves, so only actual executable-liquidity
    /// loss consumes protected profit.
    pub(crate) fn checkpoint_amm_socialized_loss(&mut self, current_slot: u64) -> Result<()> {
        self.checkpoint_amm_socialized_loss_raw(current_slot)?;
        self.defer_amm_retention_target()
    }

    pub(crate) fn checkpoint_amm_socialized_loss_raw(&mut self, current_slot: u64) -> Result<()> {
        require!(self.amm.initialized, ErrorCode::BrokenInvariant);
        let evaluation = self.evaluate_current_amm_liquidity(current_slot)?;
        let q_per_share_nad = self.curve_q_per_share_nad(evaluation.balanced_equivalent_q)?;
        self.amm
            .commit_invariant_bracket(evaluation.invariant_d, evaluation.invariant_d_high)?;
        self.amm.checkpoint_recenter_or_loss(q_per_share_nad);
        Ok(())
    }

    /// Finalizes a complete non-trade transition and admits at most one funded
    /// maintenance target update. First liquidity is already fully
    /// checkpointed by initialization and has no stale forward target.
    pub(crate) fn finalize_amm_transition(&mut self, current_slot: u64) -> Result<()> {
        if self.ensure_amm_initialized(current_slot)? {
            return Ok(());
        }
        self.checkpoint_amm_neutral_inventory_raw(current_slot)?;
        self.defer_amm_retention_target()
    }

    /// Initializes clock-driven AMM state without admitting a heavy curve
    /// transition. Parameter ramps and center moves are permissionlessly
    /// advanced by `crank_amm_maintenance`, keeping swaps and debt operations
    /// below Solana's transaction compute ceiling.
    pub(crate) fn advance_amm_clock(&mut self, current_slot: u64) -> Result<()> {
        self.ensure_amm_initialized(current_slot)?;
        if self.amm.initialized {
            self.amm
                .observe_clock_from_validated_config(&self.config.amm, current_slot)?;
        }
        Ok(())
    }

    /// Opens a one-quote stale-target probe only for an instruction that is
    /// about to execute an AMM trade. Heavy ramp/recenter maintenance is kept
    /// in its own permissionless instruction.
    pub(crate) fn prepare_amm_for_swap(&mut self, current_slot: u64) -> Result<()> {
        self.advance_amm_clock(current_slot)?;
        if self.amm.initialized {
            self.amm.release_stale_retention_probe();
        }
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
    /// `swap_reserves_with_dynamic_fee_supply` and `apply_leverage_swap`
    /// checkpoint first the invariant-preserving trade and then any retained
    /// surcharge. Their remaining fee-liability writes do not change curve
    /// reserves or yLP supply, so recomputing the same D/Q here would be both
    /// redundant and prohibitively expensive on the concentrated path.
    pub(crate) fn finalize_amm_trade_after_inventory_checkpoint(
        &mut self,
        trade_start_price_nad: u64,
        trade_end_price_nad: u64,
        current_slot: u64,
    ) -> Result<Option<CurveStateCertificate>> {
        if !self.amm.initialized {
            return Ok(None);
        }
        self.amm.checkpoint_trade(
            &self.config.amm,
            trade_start_price_nad,
            trade_end_price_nad,
            current_slot,
        )?;
        self.defer_amm_retention_target()?;
        self.amm.finish_stale_retention_probe();
        Ok(None)
    }

    fn concentrated_hlp_maintenance_is_deferred(&self) -> bool {
        self.has_active_hlp()
            && (!self.amm.applied_curve_parameters.is_cpmm()
                || (self.amm.ramp.active && !self.amm.ramp.target.is_cpmm()))
    }

    /// Prevents a new hLP mint while its target vault has unsettled economic
    /// exposure or a due parameter ramp is waiting on explicit concentrated
    /// maintenance. Silently continuing would price an entrant against a stale
    /// hedge or pre-ramp NAV basis. Pending exposure and maintenance are not
    /// direct exit gates; normal settlement-divergence, cash, and solvency
    /// checks still apply.
    pub(crate) fn require_hlp_entry_maintenance_current(
        &self,
        target_asset: MarketAsset,
        current_slot: u64,
    ) -> Result<()> {
        crate::state::market::transitions::hedge::require_hlp_entry_exposure_current(self, target_asset)?;
        if self.concentrated_hlp_maintenance_is_deferred() && self.amm.ramp.active {
            let desired = self.amm.desired_curve_parameters(&self.config.amm, current_slot);
            require!(
                desired == self.amm.applied_curve_parameters,
                ErrorCode::HlpSettlementUnavailable
            );
        }
        Ok(())
    }

    #[cfg(test)]
    fn advance_funded_amm_ramp(&mut self, current_slot: u64) -> Result<()> {
        if self.concentrated_hlp_maintenance_is_deferred() {
            return Ok(());
        }
        self.advance_funded_amm_ramp_allowing_active_hlp(current_slot)
    }

    fn advance_funded_amm_ramp_allowing_active_hlp(&mut self, current_slot: u64) -> Result<()> {
        if !self.amm.initialized || !self.amm.ramp.active {
            return Ok(());
        }
        if current_slot <= self.amm.last_ramp_update_slot {
            return Ok(());
        }

        let applied = self.amm.applied_curve_parameters;
        let desired = self.amm.desired_curve_parameters(&self.config.amm, current_slot);
        if desired == applied {
            self.amm.last_ramp_update_slot = current_slot;
            self.amm.settle_ramp(current_slot);
            return Ok(());
        }

        let mut admitted = None;
        let mut full_step_impairment_nad = None;
        let minimum_covered_nad = mul_bps_ceil(self.amm.q_per_share_nad, PROTECTED_LIQUIDITY_GUARD_BPS)?;
        for halving in 0..MAX_FUNDED_CANDIDATE_SOLVES {
            let candidate = if halving == 0 {
                desired
            } else {
                interpolate_parameters(applied, desired, 1, 1_u64 << halving)
            };
            if candidate == applied {
                continue;
            }
            let evaluation = self.evaluate_amm_liquidity_candidate(self.amm.center_price_nad, candidate)?;
            let candidate_q = self.curve_q_per_share_nad(evaluation.balanced_equivalent_q)?;
            if halving == 0 {
                full_step_impairment_nad = Some(self.amm.q_per_share_nad.saturating_sub(candidate_q));
            }
            let covered = covered_impairment_nad(self.amm.q_per_share_nad, candidate_q)?;
            if self.amm.recenter_is_funded(covered) {
                admitted = Some((candidate, evaluation, candidate_q));
                break;
            }
            // Every positive impairment requires at least the fixed guard.
            // If even that floor is unfunded, no smaller candidate can pass;
            // avoid eight provably futile invariant solves.
            if self.amm.spendable_protected_profit_nad() < minimum_covered_nad {
                break;
            }
        }

        let Some((candidate, evaluation, candidate_q)) = admitted else {
            if let Some(impairment_nad) = full_step_impairment_nad {
                self.amm
                    .refresh_retention_target(self.amm.q_per_share_nad, impairment_nad)?;
            }
            self.amm.last_ramp_update_slot = current_slot;
            return Ok(());
        };
        self.amm.commit_applied_curve_parameters(candidate, current_slot)?;
        self.amm
            .commit_invariant_bracket(evaluation.invariant_d, evaluation.invariant_d_high)?;
        self.amm.checkpoint_recenter_or_loss(candidate_q);
        self.amm.settle_ramp(current_slot);
        self.defer_amm_retention_target()?;
        Ok(())
    }

    #[cfg(test)]
    fn maybe_recenter_amm(&mut self, current_slot: u64) -> Result<()> {
        if self.concentrated_hlp_maintenance_is_deferred() {
            return Ok(());
        }
        self.maybe_recenter_amm_allowing_active_hlp(current_slot)
    }

    fn maybe_recenter_amm_allowing_active_hlp(&mut self, current_slot: u64) -> Result<()> {
        let parameters = self.amm.applied_curve_parameters;
        if self.config.amm.adjustment_step_nad == 0 || self.amm.ramp.active {
            return Ok(());
        }
        let earliest = self
            .amm
            .last_adjustment_slot
            .checked_add(self.config.amm.min_adjustment_interval_slots)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        if current_slot < earliest {
            return Ok(());
        }

        let center = self.amm.center_price_nad;
        let ema = self.amm.price_ema_nad;
        if symmetric_distance_nad(center, ema)? < self.config.amm.adjustment_threshold_nad as u128 {
            self.amm.refresh_retention_target(self.amm.q_per_share_nad, 0)?;
            return Ok(());
        }
        // A retained-fee endpoint deliberately keeps the last exact target
        // sticky. Do not pay for a candidate solve until that certified target
        // says the available buffer can plausibly fund an adjustment.
        if self.amm.retention_target_stale
            && self.amm.spendable_protected_profit_nad() < self.amm.retention_required_nad
        {
            return Ok(());
        }

        let mut admitted = None;
        let mut full_step_impairment_nad = None;
        let minimum_covered_nad = mul_bps_ceil(self.amm.q_per_share_nad, PROTECTED_LIQUIDITY_GUARD_BPS)?;
        for halving in 0..MAX_FUNDED_CANDIDATE_SOLVES {
            let step = self.config.amm.adjustment_step_nad >> halving;
            if step == 0 {
                break;
            }
            let candidate_center = center_step_toward(center, ema, step)?;
            if candidate_center == center {
                break;
            }
            let evaluation = self.evaluate_amm_liquidity_candidate(candidate_center, parameters)?;
            // A zero-depth pool and a same-tail hybrid move both retain the
            // exact CPMM curve. The center changes only the fee anchor. D/Q
            // reconstruction can differ by a few integer units across those
            // equivalent centers, so keep economic Q unchanged.
            if self.recenter_stays_on_same_cpmm_tail(candidate_center, parameters)? {
                admitted = Some((candidate_center, evaluation, self.amm.q_per_share_nad, 0));
                break;
            }
            let candidate_q = self.curve_q_per_share_nad(evaluation.balanced_equivalent_q)?;
            if halving == 0 {
                full_step_impairment_nad = Some(self.amm.q_per_share_nad.saturating_sub(candidate_q));
            }
            let covered = covered_impairment_nad(self.amm.q_per_share_nad, candidate_q)?;
            if self.amm.recenter_is_funded(covered) {
                admitted = Some((candidate_center, evaluation, candidate_q, covered));
                break;
            }
            if self.amm.spendable_protected_profit_nad() < minimum_covered_nad {
                break;
            }
        }

        let Some((candidate_center, evaluation, candidate_q, covered)) = admitted else {
            if let Some(impairment_nad) = full_step_impairment_nad {
                self.amm
                    .refresh_retention_target(self.amm.q_per_share_nad, impairment_nad)?;
            }
            return Ok(());
        };
        let actual_impairment_nad = self.amm.q_per_share_nad.saturating_sub(candidate_q);
        self.amm.commit_recenter(
            &self.config.amm,
            candidate_center,
            evaluation.invariant_d,
            evaluation.invariant_d_high,
            candidate_q,
            covered,
            current_slot,
        )?;
        // The admitted step's exact realized cost is a cheap, conservative
        // feedback target for rebuilding the next buffer. Future admission
        // still values its actual candidate and cannot spend more than the
        // current hard cap or protected profit.
        self.amm.refresh_retention_target(candidate_q, actual_impairment_nad)?;
        Ok(())
    }

    /// Permissionless maintenance entry point for every AMM with an enabled
    /// center controller or active parameter ramp.
    /// Keeping the bounded curve transition separate from swap execution is
    /// what lets both instructions fit Solana's transaction compute ceiling.
    /// If hLP supply exists, actual exposure is checkpointed before and after
    /// the move; hedge settlement remains an independent permissionless crank.
    pub(crate) fn crank_concentrated_amm_with_hlp(&mut self, current_slot: u64) -> Result<bool> {
        self.advance_amm_clock(current_slot)?;
        if !self.amm.initialized {
            return Ok(false);
        }
        // Refresh actual exposure before the curve move. Pending exposure is
        // intentionally not an admission condition: it belongs to the hLP
        // controller and remains visible for a later permissionless crank.
        self.checkpoint_hlp_vaults()?;

        let before = (self.amm.center_price_nad, self.amm.applied_curve_parameters);
        // A crank that begins with an active ramp is reserved for that ramp,
        // even when it only settles the final point. This keeps the worst case
        // to one bounded candidate search (nine solves), and prevents a final
        // ramp admission from immediately composing with a center move.
        let ramp_was_active = self.amm.ramp.active;
        self.advance_funded_amm_ramp_allowing_active_hlp(current_slot)?;
        if !ramp_was_active {
            self.maybe_recenter_amm_allowing_active_hlp(current_slot)?;
        }
        let after = (self.amm.center_price_nad, self.amm.applied_curve_parameters);
        if after != before {
            let certificate = self.certify_current_curve_from_persisted_bracket(current_slot)?;
            self.observe_risk_from_curve_evaluation(certificate.certified_evaluation(), current_slot)?;
        } else {
            self.observe_current_risk(current_slot)?;
        }
        self.checkpoint_hlp_vaults()?;
        Ok(after != before)
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

fn mul_div_ceil_u64(value: u64, multiplier: u64, denominator: u64) -> Result<u64> {
    let result = ceil_div(
        (value as u128)
            .checked_mul(multiplier as u128)
            .ok_or(ErrorCode::MarketMathOverflow)?,
        denominator as u128,
    )
    .ok_or(ErrorCode::MarketMathOverflow)?;
    u64::try_from(result).map_err(|_| ErrorCode::MarketMathOverflow.into())
}

fn symmetric_distance_nad(first: u64, second: u64) -> Result<u128> {
    require!(first > 0 && second > 0, ErrorCode::InvalidSettlementPrice);
    let high = first.max(second) as u128;
    let low = first.min(second) as u128;
    ceil_div(high.checked_mul(NAD as u128).ok_or(ErrorCode::MarketMathOverflow)?, low)
        .and_then(|ratio| ratio.checked_sub(NAD as u128))
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

fn center_step_toward(center: u64, target: u64, step_nad: u64) -> Result<u64> {
    if target > center {
        Ok(mul_div_ceil_u64(
            center,
            NAD.checked_add(step_nad).ok_or(ErrorCode::MarketMathOverflow)?,
            NAD,
        )?
        .min(target))
    } else if target < center {
        let down = (center as u128)
            .checked_mul(NAD as u128)
            .and_then(|value| value.checked_div((NAD + step_nad) as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?
            .max(1);
        Ok(u64::try_from(down)
            .map_err(|_| ErrorCode::MarketMathOverflow)?
            .max(target))
    } else {
        Ok(center)
    }
}

fn interpolate_parameters(
    start: AmmCurveParameters,
    target: AmmCurveParameters,
    numerator: u64,
    denominator: u64,
) -> AmmCurveParameters {
    AmmCurveParameters {
        peak_depth_nad: interpolate_u64(start.peak_depth_nad, target.peak_depth_nad, numerator, denominator),
        imbalance_scale_nad: interpolate_u64(
            start.imbalance_scale_nad,
            target.imbalance_scale_nad,
            numerator,
            denominator,
        ),
    }
    .canonicalized_runtime()
}

fn interpolate_u64(start: u64, target: u64, numerator: u64, denominator: u64) -> u64 {
    if target >= start {
        start.saturating_add(
            ((target - start) as u128)
                .saturating_mul(numerator as u128)
                .checked_div(denominator as u128)
                .unwrap_or(0) as u64,
        )
    } else {
        start.saturating_sub(
            ((start - target) as u128)
                .saturating_mul(numerator as u128)
                .checked_div(denominator as u128)
                .unwrap_or(0) as u64,
        )
    }
}

#[cfg(test)]
mod tests {
    include!("../../tests/state/amm_engine.rs");
}
