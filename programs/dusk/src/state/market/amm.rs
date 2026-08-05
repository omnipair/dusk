use anchor_lang::prelude::*;

use crate::{
    constants::{BPS_DENOMINATOR, MAX_HALF_LIFE_MS, MIN_HALF_LIFE_MS, NAD},
    errors::ErrorCode,
    math::{
        decay_volatility_nad, ema_u64, volatility_after_success_nad, ConcentratedC1Geometry, ConcentratedGeometryCache,
        CONCENTRATED_MATH_REVISION, CONCENTRATED_MIN_PEAK_DEPTH_NAD,
    },
};

/// Extra marginal-depth multiplier at the balanced center, NAD-scaled.
/// A value of `200 * NAD` gives `201x` CPMM marginal depth at the center.
pub const MIN_AMM_PEAK_DEPTH_NAD: u64 = CONCENTRATED_MIN_PEAK_DEPTH_NAD as u64;
pub const MAX_AMM_PEAK_DEPTH_NAD: u64 = 2_000 * NAD;
/// NAD-scaled fade scale controlling how quickly the extra depth
/// fades away from the center.
pub const MIN_AMM_FADE_SCALE_NAD: u64 = 100;
pub const MAX_AMM_FADE_SCALE_NAD: u64 = 199_000_000;
pub const MIN_AMM_ADJUSTMENT_NAD: u64 = NAD / 1_000_000;
pub const MAX_AMM_ADJUSTMENT_NAD: u64 = NAD / 10;
pub const MAX_AMM_VOLATILITY_NAD: u64 = 10 * NAD;
/// Governance/arithmetic bound on signal sensitivity, not a fee-rate cap.
/// The divergence potential remains unbounded in marginal rate; volatility
/// uses a separate asymptotic mapping over the bounded pressure.
pub const MAX_AMM_FEE_COEFFICIENT_NAD: u64 = 100 * NAD;
pub const MIN_AMM_RAMP_DURATION_SLOTS: u64 = 9_000;
pub const MAX_AMM_RAMP_DURATION_SLOTS: u64 = 6_480_000;
pub const MAX_AMM_ADJUSTMENT_INTERVAL_SLOTS: u64 = 216_000;
/// Fixed configuration extension room. `AmmConfig` is embedded in both the
/// active config and the pending timelocked config, so reserving it before
/// launch is what lets later AMM controls ship without resizing `Market`.
///
/// Keep this wire-carried reserve compact: the complete `MarketConfig` is also
/// an initialize/update instruction argument, and 64 bytes made the
/// initialize transaction exceed Solana's 1,232-byte limit. Layout v2 keeps
/// this 33-byte wire reserve for future configuration fields. One reserve byte
/// stores the account-only curve-math revision below; because `AmmConfig` is
/// embedded twice, this keeps `Market` from growing.
pub const AMM_CONFIG_RESERVED_BYTES: usize = 33;
/// Parameter-bound finite-C1 geometry. The expensive Q80 square roots are
/// paid only when the applied shape changes; swaps reconstruct Q64/Q48
/// projections from this program-owned cache.
pub const AMM_CONCENTRATED_GEOMETRY_CACHE_BYTES: usize =
    core::mem::size_of::<u8>() + 2 * core::mem::size_of::<u64>() + 7 * core::mem::size_of::<u128>();
/// One serialized byte records that retained principal changed the exact
/// reserves after the last forward-target solve.
pub const AMM_RETENTION_TARGET_STALE_BYTES: usize = core::mem::size_of::<bool>();
/// A controller target is frozen when its full scheduled move is not yet
/// funded. Real swap-like operations retry this exact target; there is no
/// auxiliary instruction or keeper dependency.
pub const AMM_DEFERRED_CONTROLLER_TARGET_BYTES: usize = core::mem::size_of::<u8>()
    + 2 * core::mem::size_of::<u64>()
    + 4 * core::mem::size_of::<u128>()
    + core::mem::size_of::<bool>();
/// Layout v2 materializes the parameter-bound concentration geometry,
/// retained-funding marker, and deferred controller target as concrete state.
/// Pessimistic lending shapes are intentionally reconstructed only by
/// risk-sensitive operations instead of being persisted in every market.
/// The account-only expansion reserve is fully allocated to keep Anchor's
/// generated SBF deserializer inside Solana's 4 KiB stack frame.
/// Future account-only fields require another explicit layout revision; the
/// 33-byte `AmmConfig` wire reserve above remains available for configuration.
pub const AMM_STATE_RESERVED_BYTES: usize = 0;

/// Protocol constants for the retained-surcharge safety budget.
pub const PROTECTED_LIQUIDITY_COVERAGE_BPS: u16 = 12_500;
pub const PROTECTED_LIQUIDITY_GUARD_BPS: u16 = 1;
pub const PROTECTED_LIQUIDITY_CAP_BPS: u16 = 100;
pub const PROTECTED_LIQUIDITY_HYSTERESIS_BPS: u16 = 1_000;

/// AMM controls. `peak_depth_nad == 0 && fade_scale_nad == 0` selects CPMM.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, InitSpace, PartialEq, Eq)]
pub struct AmmConfig {
    pub peak_depth_nad: u64,
    pub fade_scale_nad: u64,
    pub center_ema_half_life_ms: u64,
    pub volatility_half_life_ms: u64,
    pub adjustment_threshold_nad: u64,
    pub adjustment_step_nad: u64,
    pub min_adjustment_interval_slots: u64,
    pub volatility_shock_cap_nad: u64,
    pub volatility_cap_nad: u64,
    pub divergence_fee_coefficient_nad: u64,
    pub volatility_fee_coefficient_nad: u64,
    pub ramp_duration_slots: u64,
    pub reserved: [u8; AMM_CONFIG_RESERVED_BYTES],
}

impl Default for AmmConfig {
    fn default() -> Self {
        Self {
            peak_depth_nad: 0,
            fade_scale_nad: 0,
            center_ema_half_life_ms: MIN_HALF_LIFE_MS,
            volatility_half_life_ms: MIN_HALF_LIFE_MS,
            adjustment_threshold_nad: 0,
            adjustment_step_nad: 0,
            min_adjustment_interval_slots: 0,
            volatility_shock_cap_nad: 0,
            volatility_cap_nad: 0,
            divergence_fee_coefficient_nad: 0,
            volatility_fee_coefficient_nad: 0,
            ramp_duration_slots: MIN_AMM_RAMP_DURATION_SLOTS,
            reserved: [0; AMM_CONFIG_RESERVED_BYTES],
        }
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct AmmCurveParameters {
    pub peak_depth_nad: u64,
    pub fade_scale_nad: u64,
}

impl AmmCurveParameters {
    pub const fn cpmm() -> Self {
        Self {
            peak_depth_nad: 0,
            fade_scale_nad: 0,
        }
    }

    pub const fn is_cpmm(self) -> bool {
        self.peak_depth_nad == 0
    }

    pub fn validate_endpoint(self) -> Result<()> {
        if self.peak_depth_nad == 0 {
            require_eq!(self.fade_scale_nad, 0, ErrorCode::InvalidMarketConfig);
        } else {
            require!(
                (MIN_AMM_PEAK_DEPTH_NAD..=MAX_AMM_PEAK_DEPTH_NAD).contains(&self.peak_depth_nad),
                ErrorCode::InvalidMarketConfig
            );
            require!(
                (MIN_AMM_FADE_SCALE_NAD..=MAX_AMM_FADE_SCALE_NAD).contains(&self.fade_scale_nad),
                ErrorCode::InvalidMarketConfig
            );
            require!(
                self.fade_scale_nad <= self.peak_depth_nad.saturating_mul(100),
                ErrorCode::InvalidMarketConfig
            );
        }
        Ok(())
    }

    /// Runtime points may pass below the configured endpoint minima while
    /// ramping continuously to or from CPMM. Both values must still move
    /// together and remain within the hard maxima.
    pub fn validate_runtime(self) -> Result<()> {
        if self.peak_depth_nad == 0 || self.fade_scale_nad == 0 {
            require!(
                self.peak_depth_nad == 0 && self.fade_scale_nad == 0,
                ErrorCode::InvalidMarketConfig
            );
        } else {
            require!(
                self.peak_depth_nad <= MAX_AMM_PEAK_DEPTH_NAD
                    && self.fade_scale_nad >= MIN_AMM_FADE_SCALE_NAD
                    && self.fade_scale_nad <= MAX_AMM_FADE_SCALE_NAD
                    && self.fade_scale_nad <= self.peak_depth_nad.saturating_mul(100),
                ErrorCode::InvalidMarketConfig
            );
        }
        Ok(())
    }

    /// Integer interpolation treats either half-zero concentration state as
    /// the CPMM endpoint. Peak depth and fade scale are one mode switch,
    /// so exposing either half-state would make the concentrated curve invalid.
    pub const fn canonicalized_runtime(self) -> Self {
        if self.peak_depth_nad == 0 || self.fade_scale_nad == 0 {
            Self::cpmm()
        } else {
            self
        }
    }
}

impl AmmConfig {
    pub const fn curve_parameters(&self) -> AmmCurveParameters {
        AmmCurveParameters {
            peak_depth_nad: self.peak_depth_nad,
            fade_scale_nad: self.fade_scale_nad,
        }
    }

    pub const fn is_cpmm(&self) -> bool {
        self.peak_depth_nad == 0
    }

    pub fn validate(&self) -> Result<()> {
        self.curve_parameters().validate_endpoint()?;
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
        require!(
            (MIN_AMM_RAMP_DURATION_SLOTS..=MAX_AMM_RAMP_DURATION_SLOTS).contains(&self.ramp_duration_slots),
            ErrorCode::InvalidMarketConfig
        );
        require!(
            self.reserved.iter().all(|byte| *byte == 0),
            ErrorCode::InvalidMarketConfig
        );
        Ok(())
    }
}

/// A linear ramp whose governance delay is enforced by the outer Market
/// config update. The ramp begins in the slot where that update is applied.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct AmmRamp {
    pub active: bool,
    pub start: AmmCurveParameters,
    pub target: AmmCurveParameters,
    pub start_slot: u64,
    pub end_slot: u64,
}

impl AmmRamp {
    pub fn start(
        start: AmmCurveParameters,
        target: AmmCurveParameters,
        applied_slot: u64,
        duration_slots: u64,
    ) -> Result<Self> {
        // An expired but underfunded prior ramp may leave the applied curve at
        // a sub-minimum peak. Governance must still be able to redirect that
        // safe intermediate point through a new timelocked ramp.
        start.validate_runtime()?;
        target.validate_endpoint()?;
        require!(start != target, ErrorCode::InvalidMarketConfig);
        require!(
            (MIN_AMM_RAMP_DURATION_SLOTS..=MAX_AMM_RAMP_DURATION_SLOTS).contains(&duration_slots),
            ErrorCode::InvalidMarketConfig
        );
        let end_slot = applied_slot
            .checked_add(duration_slots)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        Ok(Self {
            active: true,
            start,
            target,
            start_slot: applied_slot,
            end_slot,
        })
    }

    pub fn parameters_at(&self, fallback: AmmCurveParameters, slot: u64) -> AmmCurveParameters {
        if !self.active {
            return fallback;
        }
        if slot <= self.start_slot {
            return self.start;
        }
        if slot >= self.end_slot {
            return self.target;
        }

        let elapsed = slot - self.start_slot;
        let duration = self.end_slot - self.start_slot;
        let peak_depth_nad = interpolate_u64(self.start.peak_depth_nad, self.target.peak_depth_nad, elapsed, duration);
        // Interpolate both coordinates together across CPMM boundaries, then
        // clamp the fade while peak depth is positive. This avoids both the
        // ill-conditioned sub-100 fade region and a low-peak/broad-fade region
        // whose one-atom D rounding is too large near the inner floor.
        let fade_scale_nad = match (self.start.is_cpmm(), self.target.is_cpmm()) {
            (true, false) => interpolate_u64(0, self.target.fade_scale_nad, elapsed, duration),
            (false, true) => interpolate_u64(self.start.fade_scale_nad, 0, elapsed, duration),
            (false, false) => interpolate_u64(self.start.fade_scale_nad, self.target.fade_scale_nad, elapsed, duration),
            (true, true) => 0,
        };
        let fade_scale_nad = if peak_depth_nad > 0 {
            fade_scale_nad.max(MIN_AMM_FADE_SCALE_NAD)
        } else {
            0
        };
        AmmCurveParameters {
            peak_depth_nad,
            fade_scale_nad,
        }
        .canonicalized_runtime()
    }

    pub const fn is_finished(&self, slot: u64) -> bool {
        self.active && slot >= self.end_slot
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct RetentionTarget {
    pub required_nad: u128,
    pub stop_nad: u128,
    pub hard_cap_nad: u128,
    pub saturated: bool,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct DeferredControllerTarget {
    /// 0 = none, 1 = parameter ramp, 2 = center move.
    pub kind: u8,
    pub center_price_nad: u64,
    pub parameters: AmmCurveParameters,
    pub required_nad: u128,
    pub evaluated_base_reserve_nad: u128,
    pub evaluated_quote_reserve_nad: u128,
    pub created_slot: u64,
    pub saturated: bool,
}

impl DeferredControllerTarget {
    pub const NONE: u8 = 0;
    pub const RAMP: u8 = 1;
    pub const RECENTER: u8 = 2;

    pub const fn is_active(self) -> bool {
        self.kind != Self::NONE
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// Embedded mutable state for concentration, internal signals, protected
/// liquidity, and an active parameter ramp.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, InitSpace, PartialEq, Eq)]
pub struct AmmState {
    pub initialized: bool,
    /// Parameters already admitted by the protected-profit gate. Time alone
    /// never changes this field.
    pub applied_curve_parameters: AmmCurveParameters,
    /// Authoritative geometry for `applied_curve_parameters`. CPMM stores the
    /// all-zero cache. Only initialization or an admitted parameter change may
    /// replace it; center and reserve changes reuse it unchanged.
    pub concentrated_geometry_cache: ConcentratedGeometryCache,
    pub center_price_nad: u64,
    pub price_ema_nad: u64,
    pub last_trade_price_nad: u64,
    pub last_observation_slot: u64,
    pub last_adjustment_slot: u64,
    /// Prevents repeated instructions in one slot from advancing a ramp more
    /// than once.
    pub last_ramp_update_slot: u64,
    pub volatility_accumulator_nad: u64,
    pub invariant_d_nad: u128,
    /// Curve formula revision represented by `invariant_d_nad`.
    pub curve_math_revision: u8,
    pub q_per_share_nad: u128,
    /// yLP principal floor protected from funded recenter/ramp impairment.
    pub protected_floor_per_share_nad: u128,
    /// Fresh protected-profit target that arms retained surcharge routing.
    /// This is a principal-budget target, never a cap on trader fees.
    pub retention_required_nad: u128,
    /// Hysteresis threshold below which retention remains armed.
    pub retention_stop_nad: u128,
    /// Maximum protected principal one controller target may request/spend.
    /// It does not clip divergence or volatility surcharge amounts.
    pub retention_hard_cap_nad: u128,
    /// When true, dynamic surcharge is reserve principal; when false, the
    /// identical trader charge is routed to claimable yLP fee accounting.
    pub retain_dynamic_surcharge: bool,
    /// The requested protection target exceeded its principal-budget cap.
    pub retention_target_saturated: bool,
    pub ramp: AmmRamp,
    /// Retained surcharge changed executable inventory after the last exact
    /// forward-target solve. While stale, retention stays on until a decision
    /// point refreshes the target or executes a funded recenter.
    pub retention_target_stale: bool,
    /// Exact unfunded controller target retried by later real operations.
    pub deferred_controller_target: DeferredControllerTarget,
    pub _reserved: [u8; AMM_STATE_RESERVED_BYTES],
}

impl Default for AmmState {
    fn default() -> Self {
        Self {
            initialized: false,
            applied_curve_parameters: AmmCurveParameters::cpmm(),
            concentrated_geometry_cache: ConcentratedGeometryCache::default(),
            center_price_nad: 0,
            price_ema_nad: 0,
            last_trade_price_nad: 0,
            last_observation_slot: 0,
            last_adjustment_slot: 0,
            last_ramp_update_slot: 0,
            volatility_accumulator_nad: 0,
            invariant_d_nad: 0,
            curve_math_revision: CONCENTRATED_MATH_REVISION,
            q_per_share_nad: 0,
            protected_floor_per_share_nad: 0,
            retention_required_nad: 0,
            retention_stop_nad: 0,
            retention_hard_cap_nad: 0,
            retain_dynamic_surcharge: false,
            retention_target_saturated: false,
            ramp: AmmRamp::default(),
            retention_target_stale: false,
            deferred_controller_target: DeferredControllerTarget::default(),
            _reserved: [0; AMM_STATE_RESERVED_BYTES],
        }
    }
}

impl AmmState {
    pub fn initialize(
        config: &AmmConfig,
        initial_price_nad: u64,
        initial_q_per_share_nad: u128,
        current_slot: u64,
    ) -> Result<Self> {
        config.validate()?;
        require!(initial_price_nad > 0, ErrorCode::InvalidSettlementPrice);
        let applied_curve_parameters = config.curve_parameters();
        let concentrated_geometry_cache = if applied_curve_parameters.is_cpmm() {
            ConcentratedGeometryCache::default()
        } else {
            ConcentratedGeometryCache::derive(
                applied_curve_parameters.peak_depth_nad as u128,
                applied_curve_parameters.fade_scale_nad as u128,
            )?
        };

        Ok(Self {
            initialized: true,
            applied_curve_parameters,
            concentrated_geometry_cache,
            center_price_nad: initial_price_nad,
            price_ema_nad: initial_price_nad,
            last_trade_price_nad: initial_price_nad,
            last_observation_slot: current_slot,
            last_adjustment_slot: current_slot,
            last_ramp_update_slot: current_slot,
            volatility_accumulator_nad: 0,
            invariant_d_nad: 0,
            curve_math_revision: CONCENTRATED_MATH_REVISION,
            q_per_share_nad: initial_q_per_share_nad,
            protected_floor_per_share_nad: initial_q_per_share_nad,
            retention_required_nad: 0,
            retention_stop_nad: 0,
            retention_hard_cap_nad: 0,
            retain_dynamic_surcharge: false,
            retention_target_saturated: false,
            ramp: AmmRamp::default(),
            retention_target_stale: false,
            deferred_controller_target: DeferredControllerTarget::default(),
            _reserved: [0; AMM_STATE_RESERVED_BYTES],
        })
    }

    pub(crate) fn commit_invariant(&mut self, invariant_d_nad: u128) -> Result<()> {
        require!(invariant_d_nad > 0, ErrorCode::BrokenInvariant);
        if self.curve_math_revision != CONCENTRATED_MATH_REVISION {
            self.concentrated_geometry_cache = if self.applied_curve_parameters.is_cpmm() {
                ConcentratedGeometryCache::default()
            } else {
                ConcentratedGeometryCache::derive(
                    self.applied_curve_parameters.peak_depth_nad as u128,
                    self.applied_curve_parameters.fade_scale_nad as u128,
                )?
            };
        }
        self.invariant_d_nad = invariant_d_nad;
        self.curve_math_revision = CONCENTRATED_MATH_REVISION;
        Ok(())
    }

    pub(crate) fn clear_invariant(&mut self) {
        self.invariant_d_nad = 0;
        self.curve_math_revision = CONCENTRATED_MATH_REVISION;
    }

    pub fn effective_curve_parameters(&self, config: &AmmConfig, _slot: u64) -> AmmCurveParameters {
        if self.initialized {
            self.applied_curve_parameters
        } else {
            config.curve_parameters()
        }
    }

    /// Returns the clock-proposed ramp point. A caller must value this
    /// candidate on the current reserves and fund any impairment before
    /// committing it with `commit_applied_curve_parameters`.
    pub fn desired_curve_parameters(&self, config: &AmmConfig, slot: u64) -> AmmCurveParameters {
        self.ramp.parameters_at(config.curve_parameters(), slot)
    }

    /// Records a candidate only after the caller has enforced the
    /// protected-profit gate. This structural hook intentionally performs no
    /// valuation itself.
    pub fn commit_applied_curve_parameters(
        &mut self,
        candidate: AmmCurveParameters,
        geometry_cache: Option<ConcentratedGeometryCache>,
        current_slot: u64,
    ) -> Result<()> {
        require!(self.initialized && self.ramp.active, ErrorCode::InvalidMarketConfig);
        require_gt!(current_slot, self.last_ramp_update_slot, ErrorCode::InvalidArgument);
        candidate.validate_runtime()?;
        let concentrated_geometry_cache = if candidate.is_cpmm() {
            require!(geometry_cache.is_none(), ErrorCode::BrokenInvariant);
            ConcentratedGeometryCache::default()
        } else {
            let cache = geometry_cache.ok_or(ErrorCode::BrokenInvariant)?;
            ConcentratedC1Geometry::from_cache(
                cache,
                candidate.peak_depth_nad as u128,
                candidate.fade_scale_nad as u128,
            )?;
            cache
        };
        self.applied_curve_parameters = candidate;
        self.concentrated_geometry_cache = concentrated_geometry_cache;
        self.last_ramp_update_slot = current_slot;
        Ok(())
    }

    /// Starts a ramp when a timelocked outer config update is applied. The prior
    /// endpoint is supplied explicitly because `config` already holds target.
    pub fn start_applied_ramp(
        &mut self,
        old_parameters: AmmCurveParameters,
        config: &AmmConfig,
        current_slot: u64,
    ) -> Result<()> {
        require!(self.initialized, ErrorCode::InvalidMarketConfig);
        require!(
            !self.ramp.active || self.ramp.is_finished(current_slot),
            ErrorCode::InvalidMarketConfig
        );
        require!(
            self.applied_curve_parameters == old_parameters,
            ErrorCode::InvalidMarketConfig
        );
        self.ramp = AmmRamp::start(
            old_parameters,
            config.curve_parameters(),
            current_slot,
            config.ramp_duration_slots,
        )?;
        self.last_ramp_update_slot = current_slot;
        self.invalidate_deferred_controller_target();
        Ok(())
    }

    /// Governance changed the controller request, so a cost cached for the
    /// preceding request is no longer admissible. The next genuine operation
    /// evaluates one fresh target against current reserves.
    pub(crate) fn invalidate_deferred_controller_target(&mut self) {
        self.deferred_controller_target.clear();
        self.retention_target_saturated = false;
        self.mark_retention_target_stale();
    }

    /// Clears completed ramp history only after the protected-profit gate has
    /// admitted the target. Clock completion alone is insufficient.
    pub fn settle_ramp(&mut self, current_slot: u64) -> bool {
        if !self.ramp.is_finished(current_slot) || self.applied_curve_parameters != self.ramp.target {
            return false;
        }
        self.ramp = AmmRamp::default();
        true
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
        self.q_per_share_nad.saturating_sub(self.protected_floor_per_share_nad)
    }

    /// Retained surcharge is the only mutation allowed to increase spendable
    /// protected profit.
    pub fn checkpoint_retained_surcharge(&mut self, new_q_per_share_nad: u128) -> Result<()> {
        require_gte!(new_q_per_share_nad, self.q_per_share_nad, ErrorCode::BrokenInvariant);
        self.q_per_share_nad = new_q_per_share_nad;
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
    pub fn checkpoint_neutral_liquidity(&mut self, new_q_per_share_nad: u128) {
        let prior_buffer = self.spendable_protected_profit_nad();
        self.q_per_share_nad = new_q_per_share_nad;
        self.protected_floor_per_share_nad = new_q_per_share_nad.saturating_sub(prior_buffer);
        self.sync_stale_retention_cap();
        self.refresh_retention_gate();
    }

    /// Recenter cost or socialized principal loss consumes buffer on a decrease.
    /// An improvement raises the floor equally and cannot manufacture buffer.
    pub fn checkpoint_recenter_or_loss(&mut self, new_q_per_share_nad: u128) {
        if new_q_per_share_nad > self.q_per_share_nad {
            self.protected_floor_per_share_nad = self
                .protected_floor_per_share_nad
                .saturating_add(new_q_per_share_nad - self.q_per_share_nad);
        }
        self.q_per_share_nad = new_q_per_share_nad;
        self.sync_stale_retention_cap();
        self.refresh_retention_gate();
    }

    pub fn refresh_retention_target(
        &mut self,
        q_per_share_nad: u128,
        worst_next_step_impairment_nad: u128,
    ) -> Result<RetentionTarget> {
        let target = retention_target(q_per_share_nad, worst_next_step_impairment_nad)?;
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
        let hard_cap_nad = (self.q_per_share_nad / denominator) * cap_bps
            + ((self.q_per_share_nad % denominator) * cap_bps).div_ceil(denominator);
        if self.retention_required_nad > hard_cap_nad {
            self.retention_required_nad = hard_cap_nad;
            self.retention_target_saturated = true;
        }
        self.retention_stop_nad = self.retention_stop_nad.min(hard_cap_nad);
        self.retention_hard_cap_nad = hard_cap_nad;
    }

    pub fn commit_recenter(
        &mut self,
        config: &AmmConfig,
        new_center_price_nad: u64,
        new_invariant_d_nad: u128,
        new_q_per_share_nad: u128,
        covered_actual_impairment_nad: u128,
        current_slot: u64,
    ) -> Result<()> {
        config.validate()?;
        require!(new_center_price_nad > 0, ErrorCode::InvalidSettlementPrice);
        // Validate every fallible scalar before mutating any member of the
        // center/invariant/checkpoint tuple. This keeps direct/native callers
        // atomic too; on-chain rollback is not the only safety boundary.
        require!(new_invariant_d_nad > 0, ErrorCode::BrokenInvariant);
        require!(
            self.recenter_is_funded(covered_actual_impairment_nad),
            ErrorCode::BrokenInvariant
        );
        let actual_impairment_nad = self.q_per_share_nad.saturating_sub(new_q_per_share_nad);
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
        self.commit_invariant(new_invariant_d_nad)?;
        self.last_adjustment_slot = current_slot;
        self.checkpoint_recenter_or_loss(new_q_per_share_nad);
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

pub fn retention_target(q_per_share_nad: u128, worst_next_step_impairment_nad: u128) -> Result<RetentionTarget> {
    let hard_cap_nad = mul_bps_ceil(q_per_share_nad, PROTECTED_LIQUIDITY_CAP_BPS)?;
    if q_per_share_nad == 0 || worst_next_step_impairment_nad == 0 {
        return Ok(RetentionTarget {
            hard_cap_nad,
            ..RetentionTarget::default()
        });
    }

    let covered_impairment = mul_bps_ceil(worst_next_step_impairment_nad, PROTECTED_LIQUIDITY_COVERAGE_BPS)?;
    let guard_nad = mul_bps_ceil(q_per_share_nad, PROTECTED_LIQUIDITY_GUARD_BPS)?;
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

fn mul_bps_ceil(value: u128, bps: u16) -> Result<u128> {
    let numerator = value.checked_mul(bps as u128).ok_or(ErrorCode::MarketMathOverflow)?;
    Ok(numerator
        .checked_add(BPS_DENOMINATOR as u128 - 1)
        .ok_or(ErrorCode::MarketMathOverflow)?
        / BPS_DENOMINATOR as u128)
}

fn interpolate_u64(start: u64, target: u64, elapsed: u64, duration: u64) -> u64 {
    let remaining = duration - elapsed;
    let value = (start as u128)
        .saturating_mul(remaining as u128)
        .saturating_add((target as u128).saturating_mul(elapsed as u128))
        / duration as u128;
    value as u64
}

#[cfg(test)]
mod tests {
    include!("../../tests/state/amm.rs");
}
