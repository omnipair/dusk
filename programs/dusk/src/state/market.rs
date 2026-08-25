use anchor_lang::prelude::*;

use crate::{
    constants::*,
    errors::ErrorCode,
    market::{lending::max_cf_bps_from_liquidation_cf, liquidity as rebalance},
    math::*,
    state::{
        borrow_position::{BorrowPosition, CollateralReceipt},
        futarchy_authority::{FutarchyAuthority, ProtocolAuctionLane, ProtocolAuctionSplit, ProtocolRevenueSource},
        MarketParameterUpdate, YieldAccount,
    },
};

use crate::market::SwapFeeBreakdown;

use rebalance::{
    checkpoint_hlp_yield_from_ylp, checkpoint_one_hlp_with_prices, current_hlp_curve_prices,
    current_hlp_entry_state_with_prices,
};

/// Maximum center amplification admitted by governance. The explicit curve
/// enforces this through the nonzero full-range tail share.
pub const MAX_AMM_AMPLIFICATION_NAD: u64 = 2_000 * NAD;
pub const MIN_AMM_ADJUSTMENT_NAD: u64 = NAD / 1_000_000;
pub const MAX_AMM_ADJUSTMENT_NAD: u64 = NAD / 10;
pub const MAX_AMM_VOLATILITY_NAD: u64 = 10 * NAD;
/// Governance/arithmetic bound on signal sensitivity. Separate fee-share caps
/// bound the Huberized divergence marginal and the volatility debit.
pub const MAX_AMM_FEE_COEFFICIENT_NAD: u64 = 100 * NAD;
pub const MAX_AMM_ADJUSTMENT_INTERVAL_SLOTS: u64 = 216_000;
/// Fixed configuration extension room retained for future typed AMM controls.
///
/// Keep this wire-carried reserve compact: the complete `MarketConfig` is also
/// an initialize/update instruction argument, and 64 bytes made the
/// initialize transaction exceed Solana's 1,232-byte limit. Layout v1 keeps
/// this wire reserve for future configuration fields. Launch-fee configuration
/// consumes eleven bytes from the original 33-byte reserve and the launch
/// buy-size limiter consumes another twenty-one. The remaining byte is used
/// by the fee-denomination mode.
pub const AMM_CONFIG_RESERVED_BYTES: usize = 0;
pub const SWAP_FEE_COLLECT_INPUT_ASSET: u8 = 0;
pub const SWAP_FEE_COLLECT_BASE_ONLY: u8 = 1;
pub const SWAP_FEE_COLLECT_QUOTE_ONLY: u8 = 2;
pub const LAUNCH_FEE_DECAY_DISABLED: u8 = 0;
pub const LAUNCH_FEE_DECAY_LINEAR: u8 = 1;
pub const LAUNCH_FEE_DECAY_EXPONENTIAL: u8 = 2;
pub const LAUNCH_FEE_EXPONENTIAL_PERIODS: u64 = 16;
pub const MAX_LAUNCH_FEE_DURATION_SECONDS: u64 = 30 * 24 * 60 * 60;
pub const LAUNCH_RATE_LIMIT_ASSET_DISABLED: u8 = 0;
pub const LAUNCH_RATE_LIMIT_ASSET_BASE: u8 = 1;
pub const LAUNCH_RATE_LIMIT_ASSET_QUOTE: u8 = 2;
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
/// Layout v2 also binds launch graduation price/progress and the one-shot
/// initial-liquidity authority alongside the explicit concentration state.
/// Pessimistic lending shapes are intentionally reconstructed only by
/// risk-sensitive operations instead of being persisted in every market.
/// The account-only expansion reserve is fully allocated to keep Anchor's
/// generated SBF deserializer inside Solana's 4 KiB stack frame.
/// Future account or configuration fields require another explicit layout
/// revision; all previously reserved bytes are now allocated.
pub const AMM_STATE_RESERVED_BYTES: usize = 0;

/// Protocol constants for the retained-surcharge safety budget.
pub const PROTECTED_LIQUIDITY_COVERAGE_BPS: u16 = 12_500;
pub const PROTECTED_LIQUIDITY_GUARD_BPS: u16 = 1;
pub const PROTECTED_LIQUIDITY_CAP_BPS: u16 = 100;
pub const PROTECTED_LIQUIDITY_HYSTERESIS_BPS: u16 = 1_000;

/// AMM controls. One-times peak amplification with zero widths selects the
/// full-range CPMM branch of the same explicit implementation.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, InitSpace, PartialEq, Eq)]
pub struct AmmConfig {
    pub peak_amplification_nad: u64,
    pub core_half_width_bps: u16,
    pub fade_width_bps: u16,
    pub center_ema_half_life_ms: u64,
    pub volatility_half_life_ms: u64,
    pub adjustment_threshold_nad: u64,
    pub adjustment_step_nad: u64,
    pub min_adjustment_interval_slots: u64,
    pub volatility_shock_cap_nad: u64,
    pub volatility_cap_nad: u64,
    pub divergence_fee_coefficient_nad: u64,
    pub volatility_fee_coefficient_nad: u64,
    /// Asset in which swap, toxicity, volatility, and retained-recenter fees
    /// are denominated. Lending and hLP funding interest remain in the
    /// borrowed asset and are not affected by this setting.
    pub swap_fee_collect_mode: u8,
    /// Share of the LP-owned swap fee which becomes ordinary reserve
    /// principal instead of a claimable fee liability. Zero disables native
    /// compounding; `BPS_DENOMINATOR` compounds the complete LP share.
    pub compounding_fee_bps: u16,
    /// Optional launch-only base fee. The premium above `swap_fee_bps`
    /// decays from `start_time` and is zero after the configured duration.
    pub launch_fee_start_bps: u16,
    pub launch_fee_duration_seconds: u64,
    pub launch_fee_decay_mode: u8,
    /// When all three values are zero, the launch fee follows the time
    /// schedule above. A fully nonzero tuple selects a price-milestone
    /// scheduler whose reference price is bound by the first liquidity seed.
    pub launch_market_price_step_bps: u16,
    pub launch_market_number_of_periods: u16,
    pub launch_market_reduction_factor_bps: u16,
    /// Optional launch buy-size limiter. The configured asset is the asset
    /// being bought, not the input asset. Each full/partial reference amount
    /// after the first adds `launch_rate_limit_increment_bps`, capped by
    /// `launch_rate_limit_max_fee_bps`.
    pub launch_rate_limit_asset: u8,
    pub launch_rate_limit_reference_nad: u64,
    pub launch_rate_limit_increment_bps: u16,
    pub launch_rate_limit_max_fee_bps: u16,
    pub launch_rate_limit_duration_seconds: u64,
    pub reserved: [u8; AMM_CONFIG_RESERVED_BYTES],
}

impl Default for AmmConfig {
    fn default() -> Self {
        Self {
            peak_amplification_nad: NAD,
            core_half_width_bps: 0,
            fade_width_bps: 0,
            center_ema_half_life_ms: MIN_HALF_LIFE_MS,
            volatility_half_life_ms: MIN_HALF_LIFE_MS,
            adjustment_threshold_nad: 0,
            adjustment_step_nad: 0,
            min_adjustment_interval_slots: 0,
            volatility_shock_cap_nad: 0,
            volatility_cap_nad: 0,
            divergence_fee_coefficient_nad: 0,
            volatility_fee_coefficient_nad: 0,
            swap_fee_collect_mode: SWAP_FEE_COLLECT_INPUT_ASSET,
            compounding_fee_bps: 0,
            launch_fee_start_bps: 0,
            launch_fee_duration_seconds: 0,
            launch_fee_decay_mode: LAUNCH_FEE_DECAY_DISABLED,
            launch_market_price_step_bps: 0,
            launch_market_number_of_periods: 0,
            launch_market_reduction_factor_bps: 0,
            launch_rate_limit_asset: LAUNCH_RATE_LIMIT_ASSET_DISABLED,
            launch_rate_limit_reference_nad: 0,
            launch_rate_limit_increment_bps: 0,
            launch_rate_limit_max_fee_bps: 0,
            launch_rate_limit_duration_seconds: 0,
            reserved: [0; AMM_CONFIG_RESERVED_BYTES],
        }
    }
}

impl AmmConfig {
    pub const fn is_cpmm(&self) -> bool {
        self.peak_amplification_nad == NAD
    }

    /// Typed view of the one production curve configuration. CPMM is the
    /// explicit curve at one-times peak amplification with zero widths.
    pub fn explicit_curve_parameters(&self) -> Result<ExplicitCurveParameters> {
        require!(
            self.reserved.iter().all(|byte| *byte == 0),
            ErrorCode::InvalidMarketConfig
        );
        let parameters = ExplicitCurveParameters {
            peak_amplification_nad: self.peak_amplification_nad,
            core_half_width_bps: self.core_half_width_bps,
            fade_width_bps: self.fade_width_bps,
        };
        parameters.validate(MAX_AMM_AMPLIFICATION_NAD)?;
        Ok(parameters)
    }

    pub fn set_explicit_curve_parameters(&mut self, parameters: ExplicitCurveParameters) -> Result<()> {
        parameters.validate(MAX_AMM_AMPLIFICATION_NAD)?;
        self.peak_amplification_nad = parameters.peak_amplification_nad;
        self.core_half_width_bps = parameters.core_half_width_bps;
        self.fade_width_bps = parameters.fade_width_bps;
        self.reserved = [0; AMM_CONFIG_RESERVED_BYTES];
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        self.explicit_curve_parameters()?;
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

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct RetentionTarget {
    pub required_nad: u128,
    pub stop_nad: u128,
    pub hard_cap_nad: u128,
    pub saturated: bool,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct DeferredControllerTarget {
    /// 0 = none, 2 = center move.
    pub kind: u8,
    pub center_price_nad: u64,
    pub required_nad: u128,
    pub evaluated_base_reserve_nad: u128,
    pub evaluated_quote_reserve_nad: u128,
    pub created_slot: u64,
    pub saturated: bool,
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

/// Embedded mutable state for the explicit curve, internal signals, and
/// protected recenter liquidity.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, InitSpace, PartialEq, Eq)]
pub struct AmmState {
    pub initialized: bool,
    /// Explicit CPMM-tail/band geometry. CPMM is represented by zero
    /// concentrated liquidity in this same cache.
    pub explicit_curve_cache: ExplicitCurveCache,
    pub center_price_nad: u64,
    pub price_ema_nad: u64,
    pub last_trade_price_nad: u64,
    pub last_observation_slot: u64,
    pub last_adjustment_slot: u64,
    /// Immutable launch price reference bound by the first fully-backed
    /// liquidity seed. Zero is allowed only before that seed.
    pub launch_reference_price_nad: u64,
    /// Fee-schedule progress already completed by a bootstrap adapter before
    /// the market graduates into GAMM.
    pub launch_fee_progress_offset: u16,
    pub volatility_accumulator_nad: u64,
    pub curve_depth_per_share_nad: u128,
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
    /// When true, dynamic surcharge is locked in the non-quoteable protected
    /// recenter bucket; when false, the identical trader charge is routed to
    /// claimable yLP fee accounting.
    pub retain_dynamic_surcharge: bool,
    /// The requested protection target exceeded its principal-budget cap.
    pub retention_target_saturated: bool,
    /// The protected bucket changed after the last exact forward-target solve.
    /// While stale, retention stays on until a decision point refreshes the
    /// target or executes a funded recenter.
    pub retention_target_stale: bool,
    /// Exact unfunded controller target retried by later real operations.
    pub deferred_controller_target: DeferredControllerTarget,
    pub _reserved: [u8; AMM_STATE_RESERVED_BYTES],
}

impl Default for AmmState {
    fn default() -> Self {
        Self {
            initialized: false,
            explicit_curve_cache: ExplicitCurveCache::default(),
            center_price_nad: 0,
            price_ema_nad: 0,
            last_trade_price_nad: 0,
            last_observation_slot: 0,
            last_adjustment_slot: 0,
            launch_reference_price_nad: 0,
            launch_fee_progress_offset: 0,
            volatility_accumulator_nad: 0,
            curve_depth_per_share_nad: 0,
            protected_floor_per_share_nad: 0,
            retention_required_nad: 0,
            retention_stop_nad: 0,
            retention_hard_cap_nad: 0,
            retain_dynamic_surcharge: false,
            retention_target_saturated: false,
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
        initial_curve_depth_per_share_nad: u128,
        current_slot: u64,
    ) -> Result<Self> {
        config.validate()?;
        require!(initial_price_nad > 0, ErrorCode::InvalidSettlementPrice);
        Ok(Self {
            initialized: true,
            explicit_curve_cache: ExplicitCurveCache::default(),
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

    pub(crate) fn commit_explicit_recenter(
        &mut self,
        config: &AmmConfig,
        new_center_price_nad: u64,
        new_cache: ExplicitCurveCache,
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
        self.explicit_curve_cache = new_cache;
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

fn mul_bps_ceil(value: u128, bps: u16) -> Result<u128> {
    let numerator = value.checked_mul(bps as u128).ok_or(ErrorCode::MarketMathOverflow)?;
    Ok(numerator
        .checked_add(BPS_DENOMINATOR as u128 - 1)
        .ok_or(ErrorCode::MarketMathOverflow)?
        / BPS_DENOMINATOR as u128)
}

pub const DEFAULT_DAILY_BORROW_BPS: u16 = 2_000;
pub const MAX_DAILY_BORROW_BPS: u16 = 3_000;
pub const MIN_IRM_TARGET_UTILIZATION_BPS: u16 = 6_000;
pub const MAX_IRM_TARGET_UTILIZATION_BPS: u16 = 7_500;
pub const DEFAULT_IRM_TARGET_UTILIZATION_BPS: u16 = 7_000;
pub const MIN_IRM_CURVE_STEEPNESS_NAD: u64 = 2 * NAD;
pub const MAX_IRM_CURVE_STEEPNESS_NAD: u64 = 8 * NAD;
pub const DEFAULT_IRM_CURVE_STEEPNESS_NAD: u64 = 4 * NAD;
pub const MIN_IRM_ADJUSTMENT_SPEED_PER_YEAR: u64 = 1;
pub const MAX_IRM_ADJUSTMENT_SPEED_PER_YEAR: u64 = 50;
pub const DEFAULT_IRM_ADJUSTMENT_SPEED_PER_YEAR: u64 = 20;

/// Complete mutable fee surface. The fields remain embedded in their existing
/// `MarketConfig`/`AmmConfig` locations so this view can be used by typed
/// governance without duplicating fee state.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct FeeProfile {
    pub base_fee_bps: u16,
    pub divergence_fee_share_cap_bps: u16,
    pub volatility_fee_share_cap_bps: u16,
    pub divergence_fee_coefficient_nad: u64,
    pub volatility_fee_coefficient_nad: u64,
    pub volatility_half_life_ms: u64,
    pub volatility_shock_cap_nad: u64,
    pub volatility_accumulator_cap_nad: u64,
    pub swap_fee_collect_mode: u8,
    pub compounding_fee_bps: u16,
    pub launch_fee_start_bps: u16,
    pub launch_fee_duration_seconds: u64,
    pub launch_fee_decay_mode: u8,
    pub launch_market_price_step_bps: u16,
    pub launch_market_number_of_periods: u16,
    pub launch_market_reduction_factor_bps: u16,
    pub launch_rate_limit_asset: u8,
    pub launch_rate_limit_reference_nad: u64,
    pub launch_rate_limit_increment_bps: u16,
    pub launch_rate_limit_max_fee_bps: u16,
    pub launch_rate_limit_duration_seconds: u64,
}

impl FeeProfile {
    pub fn validate(&self) -> Result<()> {
        validate_fee_share_caps(
            self.base_fee_bps,
            self.divergence_fee_share_cap_bps,
            self.volatility_fee_share_cap_bps,
        )?;
        // Validate both derived runtime rates when admitting a profile. The
        // swap solver recomputes them from the live path, while governance
        // must reject any profile whose configured extrema are not safely
        // representable.
        fee_share_cap_to_marginal_rate_nad(self.divergence_fee_share_cap_bps)?;
        let maximum_volatility_rate = asymptotic_scaled_rate_nad(
            self.volatility_accumulator_cap_nad as u128,
            self.volatility_fee_coefficient_nad,
        )?;
        require!(maximum_volatility_rate < NAD, ErrorCode::InvalidSwapFeeBps);
        require!(
            self.divergence_fee_coefficient_nad <= MAX_AMM_FEE_COEFFICIENT_NAD
                && self.volatility_fee_coefficient_nad <= MAX_AMM_FEE_COEFFICIENT_NAD,
            ErrorCode::InvalidMarketConfig
        );
        require!(
            (MIN_HALF_LIFE_MS..=MAX_HALF_LIFE_MS).contains(&self.volatility_half_life_ms),
            ErrorCode::InvalidHalfLife
        );
        let volatility_signal_disabled = self.volatility_shock_cap_nad == 0 && self.volatility_accumulator_cap_nad == 0;
        let volatility_signal_valid = self.volatility_shock_cap_nad > 0
            && self.volatility_shock_cap_nad <= self.volatility_accumulator_cap_nad
            && self.volatility_accumulator_cap_nad <= MAX_AMM_VOLATILITY_NAD;
        require!(
            volatility_signal_disabled || volatility_signal_valid,
            ErrorCode::InvalidMarketConfig
        );
        require!(
            self.volatility_fee_coefficient_nad == 0 || volatility_signal_valid,
            ErrorCode::InvalidMarketConfig
        );
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

        let launch_fee_disabled = self.launch_fee_start_bps == 0
            && self.launch_fee_duration_seconds == 0
            && self.launch_fee_decay_mode == LAUNCH_FEE_DECAY_DISABLED
            && self.launch_market_price_step_bps == 0
            && self.launch_market_number_of_periods == 0
            && self.launch_market_reduction_factor_bps == 0;
        if !launch_fee_disabled {
            require!(
                self.launch_fee_start_bps > self.base_fee_bps
                    && (1..=MAX_LAUNCH_FEE_DURATION_SECONDS).contains(&self.launch_fee_duration_seconds)
                    && matches!(
                        self.launch_fee_decay_mode,
                        LAUNCH_FEE_DECAY_LINEAR | LAUNCH_FEE_DECAY_EXPONENTIAL
                    ),
                ErrorCode::InvalidMarketConfig
            );
            validate_fee_share_caps(
                self.launch_fee_start_bps,
                self.divergence_fee_share_cap_bps,
                self.volatility_fee_share_cap_bps,
            )?;
            let market_schedule_disabled = self.launch_market_price_step_bps == 0
                && self.launch_market_number_of_periods == 0
                && self.launch_market_reduction_factor_bps == 0;
            let market_schedule_enabled = self.launch_market_price_step_bps > 0
                && self.launch_market_number_of_periods > 0
                && self.launch_market_number_of_periods <= 64
                && self.launch_market_reduction_factor_bps > 0
                && self.launch_market_reduction_factor_bps < BPS_DENOMINATOR;
            require!(
                market_schedule_disabled || market_schedule_enabled,
                ErrorCode::InvalidMarketConfig
            );
            if market_schedule_enabled {
                require!(
                    matches!(
                        self.swap_fee_collect_mode,
                        SWAP_FEE_COLLECT_BASE_ONLY | SWAP_FEE_COLLECT_QUOTE_ONLY
                    ),
                    ErrorCode::InvalidMarketConfig
                );
            }
        }

        let rate_limit_disabled = self.launch_rate_limit_asset == LAUNCH_RATE_LIMIT_ASSET_DISABLED
            && self.launch_rate_limit_reference_nad == 0
            && self.launch_rate_limit_increment_bps == 0
            && self.launch_rate_limit_max_fee_bps == 0
            && self.launch_rate_limit_duration_seconds == 0;
        if !rate_limit_disabled {
            let scheduled_peak = if launch_fee_disabled {
                self.base_fee_bps
            } else {
                self.launch_fee_start_bps
            };
            require!(
                matches!(
                    self.launch_rate_limit_asset,
                    LAUNCH_RATE_LIMIT_ASSET_BASE | LAUNCH_RATE_LIMIT_ASSET_QUOTE
                ) && self.launch_rate_limit_reference_nad > 0
                    && self.launch_rate_limit_increment_bps > 0
                    && self.launch_rate_limit_max_fee_bps > self.base_fee_bps
                    && self.launch_rate_limit_max_fee_bps >= scheduled_peak
                    && (1..=MAX_LAUNCH_FEE_DURATION_SECONDS).contains(&self.launch_rate_limit_duration_seconds),
                ErrorCode::InvalidMarketConfig
            );
            validate_fee_share_caps(
                self.launch_rate_limit_max_fee_bps,
                self.divergence_fee_share_cap_bps,
                self.volatility_fee_share_cap_bps,
            )?;
        }
        Ok(())
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, InitSpace, PartialEq, Eq)]
pub struct IrmConfig {
    pub target_utilization_bps: u16,
    pub curve_steepness_nad: u64,
    pub adjustment_speed_per_year: u64,
}

impl Default for IrmConfig {
    fn default() -> Self {
        Self {
            target_utilization_bps: DEFAULT_IRM_TARGET_UTILIZATION_BPS,
            curve_steepness_nad: DEFAULT_IRM_CURVE_STEEPNESS_NAD,
            adjustment_speed_per_year: DEFAULT_IRM_ADJUSTMENT_SPEED_PER_YEAR,
        }
    }
}

impl IrmConfig {
    pub fn validate(&self) -> Result<()> {
        require!(
            (MIN_IRM_TARGET_UTILIZATION_BPS..=MAX_IRM_TARGET_UTILIZATION_BPS).contains(&self.target_utilization_bps)
                && (MIN_IRM_CURVE_STEEPNESS_NAD..=MAX_IRM_CURVE_STEEPNESS_NAD).contains(&self.curve_steepness_nad)
                && (MIN_IRM_ADJUSTMENT_SPEED_PER_YEAR..=MAX_IRM_ADJUSTMENT_SPEED_PER_YEAR)
                    .contains(&self.adjustment_speed_per_year),
            ErrorCode::InvalidMarketConfig
        );
        Ok(())
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct MarketConfig {
    pub swap_fee_bps: u16,
    pub divergence_fee_share_cap_bps: u16,
    pub volatility_fee_share_cap_bps: u16,
    pub target_hlp_leverage_bps: u16,
    pub settlement_divergence_bps: u16,
    pub ema_half_life_ms: u64,
    pub directional_ema_half_life_ms: u64,
    pub curve_depth_ema_half_life_ms: u64,
    pub max_daily_borrow_bps: u16,
    pub global_health_contribution_cap_bps: u16,
    pub borrow_market_health_floor_bps: u16,
    pub amm: AmmConfig,
    pub irm: IrmConfig,
    pub start_time: i64,
}

impl MarketConfig {
    pub fn swap_fee_asset(&self, asset_in: MarketAsset) -> Result<MarketAsset> {
        match self.amm.swap_fee_collect_mode {
            SWAP_FEE_COLLECT_INPUT_ASSET => Ok(asset_in),
            SWAP_FEE_COLLECT_BASE_ONLY => Ok(MarketAsset::Base),
            SWAP_FEE_COLLECT_QUOTE_ONLY => Ok(MarketAsset::Quote),
            _ => err!(ErrorCode::InvalidMarketConfig),
        }
    }

    pub const fn fee_profile(&self) -> FeeProfile {
        FeeProfile {
            base_fee_bps: self.swap_fee_bps,
            divergence_fee_share_cap_bps: self.divergence_fee_share_cap_bps,
            volatility_fee_share_cap_bps: self.volatility_fee_share_cap_bps,
            divergence_fee_coefficient_nad: self.amm.divergence_fee_coefficient_nad,
            volatility_fee_coefficient_nad: self.amm.volatility_fee_coefficient_nad,
            volatility_half_life_ms: self.amm.volatility_half_life_ms,
            volatility_shock_cap_nad: self.amm.volatility_shock_cap_nad,
            volatility_accumulator_cap_nad: self.amm.volatility_cap_nad,
            swap_fee_collect_mode: self.amm.swap_fee_collect_mode,
            compounding_fee_bps: self.amm.compounding_fee_bps,
            launch_fee_start_bps: self.amm.launch_fee_start_bps,
            launch_fee_duration_seconds: self.amm.launch_fee_duration_seconds,
            launch_fee_decay_mode: self.amm.launch_fee_decay_mode,
            launch_market_price_step_bps: self.amm.launch_market_price_step_bps,
            launch_market_number_of_periods: self.amm.launch_market_number_of_periods,
            launch_market_reduction_factor_bps: self.amm.launch_market_reduction_factor_bps,
            launch_rate_limit_asset: self.amm.launch_rate_limit_asset,
            launch_rate_limit_reference_nad: self.amm.launch_rate_limit_reference_nad,
            launch_rate_limit_increment_bps: self.amm.launch_rate_limit_increment_bps,
            launch_rate_limit_max_fee_bps: self.amm.launch_rate_limit_max_fee_bps,
            launch_rate_limit_duration_seconds: self.amm.launch_rate_limit_duration_seconds,
        }
    }

    /// Applies one validated fee profile atomically; an invalid profile leaves
    /// the market configuration unchanged.
    pub fn apply_fee_profile(&mut self, profile: FeeProfile) -> Result<()> {
        profile.validate()?;
        let mut next = *self;
        next.swap_fee_bps = profile.base_fee_bps;
        next.divergence_fee_share_cap_bps = profile.divergence_fee_share_cap_bps;
        next.volatility_fee_share_cap_bps = profile.volatility_fee_share_cap_bps;
        next.amm.divergence_fee_coefficient_nad = profile.divergence_fee_coefficient_nad;
        next.amm.volatility_fee_coefficient_nad = profile.volatility_fee_coefficient_nad;
        next.amm.volatility_half_life_ms = profile.volatility_half_life_ms;
        next.amm.volatility_shock_cap_nad = profile.volatility_shock_cap_nad;
        next.amm.volatility_cap_nad = profile.volatility_accumulator_cap_nad;
        next.amm.swap_fee_collect_mode = profile.swap_fee_collect_mode;
        next.amm.compounding_fee_bps = profile.compounding_fee_bps;
        next.amm.launch_fee_start_bps = profile.launch_fee_start_bps;
        next.amm.launch_fee_duration_seconds = profile.launch_fee_duration_seconds;
        next.amm.launch_fee_decay_mode = profile.launch_fee_decay_mode;
        next.amm.launch_market_price_step_bps = profile.launch_market_price_step_bps;
        next.amm.launch_market_number_of_periods = profile.launch_market_number_of_periods;
        next.amm.launch_market_reduction_factor_bps = profile.launch_market_reduction_factor_bps;
        next.amm.launch_rate_limit_asset = profile.launch_rate_limit_asset;
        next.amm.launch_rate_limit_reference_nad = profile.launch_rate_limit_reference_nad;
        next.amm.launch_rate_limit_increment_bps = profile.launch_rate_limit_increment_bps;
        next.amm.launch_rate_limit_max_fee_bps = profile.launch_rate_limit_max_fee_bps;
        next.amm.launch_rate_limit_duration_seconds = profile.launch_rate_limit_duration_seconds;
        next.validate()?;
        *self = next;
        Ok(())
    }

    /// Effective base fee for one swap. Launch protection is a bounded,
    /// clock-derived premium and therefore needs no keeper or mutable ramp
    /// state. Linear mode decays continuously by integer seconds;
    /// exponential mode uses sixteen deterministic halving periods.
    pub fn effective_base_fee_bps_at(&self, unix_timestamp: i64) -> Result<u16> {
        let amm = self.amm;
        if amm.launch_fee_decay_mode == LAUNCH_FEE_DECAY_DISABLED {
            return Ok(self.swap_fee_bps);
        }
        require!(
            amm.launch_fee_start_bps >= self.swap_fee_bps && amm.launch_fee_duration_seconds > 0,
            ErrorCode::InvalidMarketConfig
        );
        let elapsed = unix_timestamp.saturating_sub(self.start_time).max(0) as u64;
        if elapsed >= amm.launch_fee_duration_seconds {
            return Ok(self.swap_fee_bps);
        }
        let premium = u64::from(
            amm.launch_fee_start_bps
                .checked_sub(self.swap_fee_bps)
                .ok_or(ErrorCode::InvalidMarketConfig)?,
        );
        let remaining_premium = match amm.launch_fee_decay_mode {
            LAUNCH_FEE_DECAY_LINEAR => {
                let remaining = amm
                    .launch_fee_duration_seconds
                    .checked_sub(elapsed)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                premium
                    .checked_mul(remaining)
                    .and_then(|value| value.checked_add(amm.launch_fee_duration_seconds - 1))
                    .and_then(|value| value.checked_div(amm.launch_fee_duration_seconds))
                    .ok_or(ErrorCode::MarketMathOverflow)?
            }
            LAUNCH_FEE_DECAY_EXPONENTIAL => {
                let period = elapsed
                    .checked_mul(LAUNCH_FEE_EXPONENTIAL_PERIODS)
                    .and_then(|value| value.checked_div(amm.launch_fee_duration_seconds))
                    .ok_or(ErrorCode::MarketMathOverflow)?
                    .min(LAUNCH_FEE_EXPONENTIAL_PERIODS - 1);
                let divisor = 1_u64.checked_shl(period as u32).ok_or(ErrorCode::MarketMathOverflow)?;
                premium
                    .checked_add(divisor - 1)
                    .and_then(|value| value.checked_div(divisor))
                    .ok_or(ErrorCode::MarketMathOverflow)?
            }
            _ => return err!(ErrorCode::InvalidMarketConfig),
        };
        u16::try_from(
            u64::from(self.swap_fee_bps)
                .checked_add(remaining_premium)
                .ok_or(ErrorCode::MarketMathOverflow)?,
        )
        .map_err(|_| ErrorCode::MarketMathOverflow.into())
    }

    fn effective_market_cap_fee_bps_at(
        &self,
        unix_timestamp: i64,
        current_base_price_nad: u64,
        reference_base_price_nad: u64,
        progress_offset: u16,
    ) -> Result<u16> {
        let amm = self.amm;
        require!(
            amm.launch_market_price_step_bps > 0
                && amm.launch_market_number_of_periods > 0
                && amm.launch_market_reduction_factor_bps > 0
                && reference_base_price_nad > 0
                && current_base_price_nad > 0,
            ErrorCode::InvalidMarketConfig
        );
        let elapsed = unix_timestamp.saturating_sub(self.start_time).max(0) as u64;
        if elapsed >= amm.launch_fee_duration_seconds {
            return Ok(self.swap_fee_bps);
        }
        let (current, reference) = match amm.swap_fee_collect_mode {
            // Quote-only fees identify Base as the launch asset. Its price is
            // already expressed as Quote per Base.
            SWAP_FEE_COLLECT_QUOTE_ONLY => (u128::from(current_base_price_nad), u128::from(reference_base_price_nad)),
            // Base-only fees identify Quote as the launch asset, so compare
            // the reciprocal price without materializing a rounded inverse.
            SWAP_FEE_COLLECT_BASE_ONLY => (u128::from(reference_base_price_nad), u128::from(current_base_price_nad)),
            _ => return err!(ErrorCode::InvalidMarketConfig),
        };
        let price_steps = if current <= reference {
            0
        } else {
            let numerator = current
                .checked_sub(reference)
                .and_then(|value| value.checked_mul(BPS_DENOMINATOR as u128))
                .ok_or(ErrorCode::MarketMathOverflow)?;
            let denominator = reference
                .checked_mul(u128::from(amm.launch_market_price_step_bps))
                .ok_or(ErrorCode::MarketMathOverflow)?;
            numerator
                .checked_div(denominator)
                .ok_or(ErrorCode::MarketMathOverflow)?
        };
        let periods = u128::from(progress_offset)
            .checked_add(price_steps)
            .ok_or(ErrorCode::MarketMathOverflow)?
            .min(u128::from(amm.launch_market_number_of_periods));
        let mut fee = u128::from(amm.launch_fee_start_bps);
        match amm.launch_fee_decay_mode {
            LAUNCH_FEE_DECAY_LINEAR => {
                fee = fee.saturating_sub(
                    periods
                        .checked_mul(u128::from(amm.launch_market_reduction_factor_bps))
                        .ok_or(ErrorCode::MarketMathOverflow)?,
                );
            }
            LAUNCH_FEE_DECAY_EXPONENTIAL => {
                let remaining_bps = u128::from(
                    BPS_DENOMINATOR
                        .checked_sub(amm.launch_market_reduction_factor_bps)
                        .ok_or(ErrorCode::InvalidMarketConfig)?,
                );
                for _ in 0..periods {
                    fee = fee
                        .checked_mul(remaining_bps)
                        .and_then(|value| value.checked_add(BPS_DENOMINATOR as u128 - 1))
                        .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
                        .ok_or(ErrorCode::MarketMathOverflow)?;
                }
            }
            _ => return err!(ErrorCode::InvalidMarketConfig),
        }
        u16::try_from(fee.max(u128::from(self.swap_fee_bps))).map_err(|_| ErrorCode::MarketMathOverflow.into())
    }

    /// Compose the time scheduler with the launch buy-size limiter. The time
    /// schedule protects *when* a swap executes and applies in both
    /// directions. The rate limiter protects *how much of the configured
    /// launch asset is bought* in one swap. It is stateless and therefore does
    /// not require an Alpha Vault, keeper, counter, or extra account.
    pub fn effective_base_fee_bps_for_swap_at(
        &self,
        asset_in: MarketAsset,
        gross_input_nad: u128,
        unix_timestamp: i64,
        current_base_price_nad: u64,
        launch_reference_price_nad: u64,
        launch_fee_progress_offset: u16,
    ) -> Result<u16> {
        let amm = self.amm;
        let market_cap_schedule = amm.launch_market_price_step_bps > 0;
        let scheduled_fee_bps = if market_cap_schedule {
            self.effective_market_cap_fee_bps_at(
                unix_timestamp,
                current_base_price_nad,
                launch_reference_price_nad,
                launch_fee_progress_offset,
            )?
        } else {
            self.effective_base_fee_bps_at(unix_timestamp)?
        };
        if amm.launch_rate_limit_asset == LAUNCH_RATE_LIMIT_ASSET_DISABLED {
            return Ok(scheduled_fee_bps);
        }

        let elapsed = unix_timestamp.saturating_sub(self.start_time).max(0) as u64;
        if elapsed >= amm.launch_rate_limit_duration_seconds {
            return Ok(scheduled_fee_bps);
        }
        let launch_asset = match amm.launch_rate_limit_asset {
            LAUNCH_RATE_LIMIT_ASSET_BASE => MarketAsset::Base,
            LAUNCH_RATE_LIMIT_ASSET_QUOTE => MarketAsset::Quote,
            _ => return err!(ErrorCode::InvalidMarketConfig),
        };
        if asset_in.opposite() != launch_asset || gross_input_nad == 0 {
            return Ok(scheduled_fee_bps);
        }

        let reference = u128::from(amm.launch_rate_limit_reference_nad);
        require!(reference > 0, ErrorCode::InvalidMarketConfig);
        let reference_units = gross_input_nad
            .checked_add(reference - 1)
            .and_then(|value| value.checked_div(reference))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let premium_steps = reference_units.saturating_sub(1);
        let size_premium_bps = premium_steps
            .checked_mul(u128::from(amm.launch_rate_limit_increment_bps))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let effective = u128::from(scheduled_fee_bps)
            .checked_add(size_premium_bps)
            .ok_or(ErrorCode::MarketMathOverflow)?
            .min(u128::from(amm.launch_rate_limit_max_fee_bps));
        u16::try_from(effective).map_err(|_| ErrorCode::MarketMathOverflow.into())
    }

    pub fn launch_rate_limit_active_for_swap(&self, asset_in: MarketAsset, unix_timestamp: i64) -> bool {
        let protected_asset = match self.amm.launch_rate_limit_asset {
            LAUNCH_RATE_LIMIT_ASSET_BASE => MarketAsset::Base,
            LAUNCH_RATE_LIMIT_ASSET_QUOTE => MarketAsset::Quote,
            _ => return false,
        };
        if asset_in.opposite() != protected_asset || unix_timestamp < self.start_time {
            return false;
        }
        let elapsed = unix_timestamp.saturating_sub(self.start_time) as u64;
        elapsed < self.amm.launch_rate_limit_duration_seconds
    }

    pub fn validate(&self) -> Result<()> {
        self.fee_profile().validate()?;
        require_eq!(
            self.target_hlp_leverage_bps,
            BPS_DENOMINATOR.checked_mul(2).ok_or(ErrorCode::InvalidMarketConfig)?,
            ErrorCode::InvalidMarketConfig
        );
        require!(
            self.max_daily_borrow_bps <= MAX_DAILY_BORROW_BPS && self.settlement_divergence_bps <= BPS_DENOMINATOR,
            ErrorCode::InvalidMarketConfig
        );
        require!(
            half_life_in_bounds(self.ema_half_life_ms)
                && half_life_in_bounds(self.directional_ema_half_life_ms)
                && half_life_in_bounds(self.curve_depth_ema_half_life_ms),
            ErrorCode::InvalidMarketConfig
        );
        require!(
            self.global_health_contribution_cap_bps >= BPS_DENOMINATOR
                && self.borrow_market_health_floor_bps >= BPS_DENOMINATOR
                && self.global_health_contribution_cap_bps >= self.borrow_market_health_floor_bps,
            ErrorCode::InvalidMarketConfig
        );
        self.irm.validate()?;
        self.amm.validate()?;
        Ok(())
    }
}

fn half_life_in_bounds(half_life_ms: u64) -> bool {
    (MIN_HALF_LIFE_MS..=MAX_HALF_LIFE_MS).contains(&half_life_ms)
}

#[cfg(test)]
std::thread_local! {
    static SHARES_TO_DEBT_CALL_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace)]
pub struct Debt {
    pub fixed_base_shares: u128,
    pub fixed_quote_shares: u128,
    pub base_borrow_index_nad: u128,
    pub quote_borrow_index_nad: u128,
    pub base_rate_at_target_nad: u128,
    pub quote_rate_at_target_nad: u128,
    pub global_health_base_contribution_for_quote_debt: u64,
    pub global_health_quote_contribution_for_base_debt: u64,
    pub base_last_accrual_slot: u64,
    pub quote_last_accrual_slot: u64,
    // Debt tracking (r_debt)
    /// Aggregate outstanding *principal* (borrowed token amount, excluding
    /// accrued interest) backing fixed margin debt on each side. Accrued
    /// interest is `fixed_*_debt - fixed_*_principal`; tracked so interest can
    /// be routed to the interest vault (non-compounding) instead of
    /// compounding into reserves. Principal is a raw token-atom balance and is
    /// therefore bounded by the corresponding `u64` reserve custody domain.
    pub fixed_base_principal: u64,
    pub fixed_quote_principal: u64,
    /// Aggregate isolated leverage debt. This debt contributes to utilization
    /// and interest, but is intentionally not utilized as normal margin debt.
    /// Shares remain `u128`; raw principal remains in the token account's
    /// `u64` amount domain.
    pub isolated_base_shares: u128,
    pub isolated_quote_shares: u128,
    pub isolated_base_principal: u64,
    pub isolated_quote_principal: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DebtClearance {
    pub shares_burned: u128,
    /// Cash actually accepted for this clearance. This is the canonical
    /// aggregate debt delta, not the caller's maximum input.
    pub cash_repaid: u64,
    pub debt_reduced: u64,
    pub aggregate_debt_reduced: u64,
    pub principal_paid: u64,
    pub interest_paid: u64,
    pub remaining_debt: u64,
    pub(crate) position_principal_reduced: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DebtRepaymentQuote {
    pub shares_to_burn: u128,
    pub cash_repaid: u64,
    pub position_debt_reduced: u64,
    pub remaining_position_debt: u64,
}

impl DebtClearance {
    pub fn live_debit_for_cash_repay(&self) -> Result<u64> {
        self.aggregate_debt_reduced
            .checked_sub(self.principal_paid)
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DebtWriteoff {
    pub shares_written_off: u128,
    pub debt_written_off: u64,
    pub aggregate_debt_written_off: u64,
    pub principal_written_off: u64,
}

impl Debt {
    pub fn debt_to_shares(amount: u64, borrow_index_nad: u128) -> Result<u128> {
        require!(amount > 0, ErrorCode::AmountZero);
        ceil_div(
            (amount as u128)
                .checked_mul(NAD as u128)
                .ok_or(ErrorCode::MarketMathOverflow)?,
            borrow_index_nad,
        )
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    }

    pub fn shares_to_debt(shares: u128, borrow_index_nad: u128) -> Result<u128> {
        if shares == 0 {
            return Ok(0);
        }
        #[cfg(test)]
        SHARES_TO_DEBT_CALL_COUNT.with(|count| count.set(count.get().saturating_add(1)));
        shares
            .checked_mul(borrow_index_nad)
            .and_then(|value| value.checked_div(NAD as u128))
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    }

    /// Returns the maximum share burn whose canonical aggregate debt delta is
    /// no greater than `max_repay_amount`.
    ///
    /// Debt is stored as one aggregate share bucket while positions own subsets
    /// of those shares. Therefore the only split-resistant cash charge is:
    ///
    /// `floor(aggregate_before * index) - floor(aggregate_after * index)`.
    ///
    /// Selecting shares with a ceil conversion and charging the caller's input
    /// can erase up to one indexed share of debt on every split repayment. The
    /// floor candidate below, plus the only possible adjacent candidate, makes
    /// the aggregate delta telescope exactly across any split sequence.
    pub fn repayment_for_max(
        position_shares: u128,
        aggregate_shares: u128,
        borrow_index_nad: u128,
        max_repay_amount: u64,
    ) -> Result<DebtRepaymentQuote> {
        require!(max_repay_amount > 0, ErrorCode::AmountZero);
        require!(borrow_index_nad >= NAD as u128, ErrorCode::DebtShareDivisionOverflow);
        require!(position_shares > 0, ErrorCode::InsufficientDebt);
        require_gte!(aggregate_shares, position_shares, ErrorCode::DebtShareMathOverflow);

        let aggregate_debt_before = Self::shares_to_debt(aggregate_shares, borrow_index_nad)?;
        let position_debt_before = Self::shares_to_debt(position_shares, borrow_index_nad)?;
        require!(position_debt_before > 0, ErrorCode::InsufficientDebt);

        let mut shares_to_burn = (max_repay_amount as u128)
            .checked_mul(NAD as u128)
            .and_then(|value| value.checked_div(borrow_index_nad))
            .ok_or(ErrorCode::DebtShareMathOverflow)?
            .min(position_shares);

        // Aggregate floor phases can make one additional share fit the same
        // raw-token maximum. Since index >= 1.0, no second adjacent share can
        // fit once the mathematical floor candidate has been tested.
        if shares_to_burn < position_shares {
            let adjacent = shares_to_burn.checked_add(1).ok_or(ErrorCode::DebtShareMathOverflow)?;
            let adjacent_delta = aggregate_debt_before
                .checked_sub(Self::shares_to_debt(
                    aggregate_shares
                        .checked_sub(adjacent)
                        .ok_or(ErrorCode::DebtShareMathOverflow)?,
                    borrow_index_nad,
                )?)
                .ok_or(ErrorCode::DebtMathOverflow)?;
            if adjacent_delta <= max_repay_amount as u128 {
                shares_to_burn = adjacent;
            }
        }
        require!(shares_to_burn > 0, ErrorCode::DebtShareDivisionOverflow);

        let aggregate_debt_after = Self::shares_to_debt(
            aggregate_shares
                .checked_sub(shares_to_burn)
                .ok_or(ErrorCode::DebtShareMathOverflow)?,
            borrow_index_nad,
        )?;
        let position_debt_after = Self::shares_to_debt(
            position_shares
                .checked_sub(shares_to_burn)
                .ok_or(ErrorCode::DebtShareMathOverflow)?,
            borrow_index_nad,
        )?;
        let cash_repaid = aggregate_debt_before
            .checked_sub(aggregate_debt_after)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        require!(
            cash_repaid > 0 && cash_repaid <= max_repay_amount as u128,
            ErrorCode::DebtMathOverflow
        );
        let position_debt_reduced = position_debt_before
            .checked_sub(position_debt_after)
            .ok_or(ErrorCode::DebtMathOverflow)?;

        Ok(DebtRepaymentQuote {
            shares_to_burn,
            cash_repaid: u64::try_from(cash_repaid).map_err(|_| ErrorCode::DebtMathOverflow)?,
            position_debt_reduced: u64::try_from(position_debt_reduced).map_err(|_| ErrorCode::DebtMathOverflow)?,
            remaining_position_debt: u64::try_from(position_debt_after).map_err(|_| ErrorCode::DebtMathOverflow)?,
        })
    }

    pub fn aggregate_debt_reduction_for_shares(
        aggregate_shares: u128,
        shares_to_burn: u128,
        borrow_index_nad: u128,
    ) -> Result<u64> {
        require!(shares_to_burn > 0, ErrorCode::DebtShareDivisionOverflow);
        require_gte!(aggregate_shares, shares_to_burn, ErrorCode::DebtShareMathOverflow);
        let debt_before = Self::shares_to_debt(aggregate_shares, borrow_index_nad)?;
        let debt_after = Self::shares_to_debt(
            aggregate_shares
                .checked_sub(shares_to_burn)
                .ok_or(ErrorCode::DebtShareMathOverflow)?,
            borrow_index_nad,
        )?;
        u64::try_from(debt_before.checked_sub(debt_after).ok_or(ErrorCode::DebtMathOverflow)?)
            .map_err(|_| ErrorCode::DebtMathOverflow.into())
    }

    pub fn isolated_repayment_for_max(
        &self,
        asset: MarketAsset,
        position_shares: u128,
        max_repay_amount: u64,
    ) -> Result<DebtRepaymentQuote> {
        let aggregate_shares = match asset {
            MarketAsset::Base => self.isolated_base_shares,
            MarketAsset::Quote => self.isolated_quote_shares,
        };
        Self::repayment_for_max(
            position_shares,
            aggregate_shares,
            self.borrow_index(asset),
            max_repay_amount,
        )
    }

    #[cfg(test)]
    pub(crate) fn reset_shares_to_debt_call_count() {
        SHARES_TO_DEBT_CALL_COUNT.with(|count| count.set(0));
    }

    #[cfg(test)]
    pub(crate) fn shares_to_debt_call_count() -> usize {
        SHARES_TO_DEBT_CALL_COUNT.with(std::cell::Cell::get)
    }

    /// Increase tracked margin principal when new fixed margin debt is taken on.
    pub fn add_margin_principal(&mut self, asset: MarketAsset, amount: u64) -> Result<()> {
        let principal = match asset {
            MarketAsset::Base => &mut self.fixed_base_principal,
            MarketAsset::Quote => &mut self.fixed_quote_principal,
        };
        *principal = principal.checked_add(amount).ok_or(ErrorCode::DebtMathOverflow)?;
        Ok(())
    }

    pub fn add_isolated_debt(&mut self, asset: MarketAsset, amount: u64) -> Result<u128> {
        let borrow_index_nad = self.borrow_index(asset);
        let shares = Self::debt_to_shares(amount, borrow_index_nad)?;
        let (aggregate_shares, principal) = match asset {
            MarketAsset::Base => (&mut self.isolated_base_shares, &mut self.isolated_base_principal),
            MarketAsset::Quote => (&mut self.isolated_quote_shares, &mut self.isolated_quote_principal),
        };
        let next_aggregate_shares = aggregate_shares
            .checked_add(shares)
            .ok_or(ErrorCode::DebtShareMathOverflow)?;
        let next_principal = principal.checked_add(amount).ok_or(ErrorCode::DebtMathOverflow)?;
        *aggregate_shares = next_aggregate_shares;
        *principal = next_principal;
        Ok(shares)
    }

    pub fn isolated_debt(&self, asset: MarketAsset) -> Result<u128> {
        let (shares, index) = match asset {
            MarketAsset::Base => (self.isolated_base_shares, self.base_borrow_index_nad),
            MarketAsset::Quote => (self.isolated_quote_shares, self.quote_borrow_index_nad),
        };
        Self::shares_to_debt(shares, index)
    }

    pub fn borrow_index(&self, asset: MarketAsset) -> u128 {
        match asset {
            MarketAsset::Base => self.base_borrow_index_nad,
            MarketAsset::Quote => self.quote_borrow_index_nad,
        }
    }

    pub fn clear_isolated_debt(
        &mut self,
        asset: MarketAsset,
        position_shares: &mut u128,
        position_principal: &mut u128,
        max_repay_amount: u64,
    ) -> Result<DebtClearance> {
        let clearance =
            self.isolated_clearance_for_max(asset, *position_shares, *position_principal, max_repay_amount)?;
        let remaining_shares = position_shares
            .checked_sub(clearance.shares_burned)
            .ok_or(ErrorCode::DebtShareMathOverflow)?;
        let (aggregate_shares, aggregate_principal) = match asset {
            MarketAsset::Base => (&mut self.isolated_base_shares, &mut self.isolated_base_principal),
            MarketAsset::Quote => (&mut self.isolated_quote_shares, &mut self.isolated_quote_principal),
        };
        *position_shares = remaining_shares;
        *aggregate_shares = aggregate_shares
            .checked_sub(clearance.shares_burned)
            .ok_or(ErrorCode::DebtShareMathOverflow)?;
        *position_principal = position_principal
            .checked_sub(clearance.position_principal_reduced as u128)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        *aggregate_principal = aggregate_principal
            .checked_sub(clearance.position_principal_reduced)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        if *position_shares == 0 {
            require_eq!(*position_principal, 0, ErrorCode::BrokenInvariant);
            *position_principal = 0;
        }
        if *aggregate_shares == 0 {
            require_eq!(*aggregate_principal, 0, ErrorCode::BrokenInvariant);
            *aggregate_principal = 0;
        }
        Ok(clearance)
    }

    pub(crate) fn isolated_clearance_for_max(
        &self,
        asset: MarketAsset,
        position_shares: u128,
        position_principal: u128,
        max_repay_amount: u64,
    ) -> Result<DebtClearance> {
        let repayment = self.isolated_repayment_for_max(asset, position_shares, max_repay_amount)?;
        let borrow_index_nad = self.borrow_index(asset);
        let position_debt = Self::shares_to_debt(position_shares, borrow_index_nad)?;
        let aggregate_shares = match asset {
            MarketAsset::Base => self.isolated_base_shares,
            MarketAsset::Quote => self.isolated_quote_shares,
        };
        let aggregate_principal = match asset {
            MarketAsset::Base => self.isolated_base_principal,
            MarketAsset::Quote => self.isolated_quote_principal,
        };
        let position_principal_raw = u64::try_from(position_principal).map_err(|_| ErrorCode::DebtMathOverflow)?;
        require_gte!(aggregate_principal, position_principal_raw, ErrorCode::DebtMathOverflow);
        let (position_principal_reduced, _) = crate::math::realized_interest_split(
            repayment.position_debt_reduced,
            position_debt,
            position_principal.min(position_debt),
        )?;
        let aggregate_debt_before = Self::shares_to_debt(aggregate_shares, borrow_index_nad)?;
        let aggregate_debt_after = aggregate_debt_before
            .checked_sub(repayment.cash_repaid as u128)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        let aggregate_principal_after = aggregate_principal
            .checked_sub(position_principal_reduced)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        let aggregate_interest_before = aggregate_debt_before
            .checked_sub(u128::from(aggregate_principal).min(aggregate_debt_before))
            .ok_or(ErrorCode::DebtMathOverflow)?;
        let aggregate_interest_after = aggregate_debt_after
            .checked_sub(u128::from(aggregate_principal_after).min(aggregate_debt_after))
            .ok_or(ErrorCode::DebtMathOverflow)?;
        // Only a positive reduction in the exact aggregate unrealized-interest
        // coordinate leaves executable principal unchanged. A one-atom floor
        // phase may instead increase that coordinate; classify that atom as
        // principal so split repayments keep the curve delta conservative.
        require!(
            aggregate_interest_after
                <= aggregate_interest_before
                    .checked_add(1)
                    .ok_or(ErrorCode::DebtMathOverflow)?,
            ErrorCode::BrokenInvariant
        );
        let interest_paid = u64::try_from(aggregate_interest_before.saturating_sub(aggregate_interest_after))
            .map_err(|_| ErrorCode::DebtMathOverflow)?;
        let principal_paid = repayment
            .cash_repaid
            .checked_sub(interest_paid)
            .ok_or(ErrorCode::DebtMathOverflow)?;

        Ok(DebtClearance {
            shares_burned: repayment.shares_to_burn,
            cash_repaid: repayment.cash_repaid,
            debt_reduced: repayment.position_debt_reduced,
            aggregate_debt_reduced: repayment.cash_repaid,
            principal_paid,
            interest_paid,
            remaining_debt: repayment.remaining_position_debt,
            position_principal_reduced,
        })
    }

    pub fn writeoff_isolated_position(
        &mut self,
        asset: MarketAsset,
        position_shares: &mut u128,
        position_principal: &mut u128,
    ) -> Result<DebtWriteoff> {
        require!(*position_shares > 0, ErrorCode::DebtShareDivisionOverflow);
        let borrow_index_nad = self.borrow_index(asset);
        let debt_written_off = u64::try_from(Self::shares_to_debt(*position_shares, borrow_index_nad)?)
            .map_err(|_| ErrorCode::DebtMathOverflow)?;
        let (aggregate_shares, aggregate_principal) = match asset {
            MarketAsset::Base => (&mut self.isolated_base_shares, &mut self.isolated_base_principal),
            MarketAsset::Quote => (&mut self.isolated_quote_shares, &mut self.isolated_quote_principal),
        };
        require_gte!(*aggregate_shares, *position_shares, ErrorCode::DebtShareMathOverflow);
        let aggregate_debt_before = Self::shares_to_debt(*aggregate_shares, borrow_index_nad)?;
        let aggregate_debt_after = Self::shares_to_debt(
            aggregate_shares
                .checked_sub(*position_shares)
                .ok_or(ErrorCode::DebtShareMathOverflow)?,
            borrow_index_nad,
        )?;
        let aggregate_debt_written_off = u64::try_from(
            aggregate_debt_before
                .checked_sub(aggregate_debt_after)
                .ok_or(ErrorCode::DebtMathOverflow)?,
        )
        .map_err(|_| ErrorCode::DebtMathOverflow)?;
        let principal_written_off = u64::try_from(*position_principal).map_err(|_| ErrorCode::DebtMathOverflow)?;
        require_gte!(*aggregate_principal, principal_written_off, ErrorCode::DebtMathOverflow);
        *aggregate_shares = aggregate_shares
            .checked_sub(*position_shares)
            .ok_or(ErrorCode::DebtShareMathOverflow)?;
        *aggregate_principal = aggregate_principal
            .checked_sub(principal_written_off)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        let shares_written_off = *position_shares;
        *position_shares = 0;
        *position_principal = 0;
        if *aggregate_shares == 0 {
            *aggregate_principal = 0;
        }
        Ok(DebtWriteoff {
            shares_written_off,
            debt_written_off,
            aggregate_debt_written_off,
            principal_written_off,
        })
    }

    /// Reduce tracked margin principal for a cash-backed fixed-debt repayment,
    /// returning the realized *interest* portion (the non-compounding interest
    /// the caller should route to the interest vault). Uses the side's blended
    /// principal/debt ratio, which is aggregate-conservative across positions.
    pub fn realize_margin_repay(&mut self, asset: MarketAsset, repaid: u64) -> Result<u64> {
        self.realize_margin_clearance(asset, repaid, repaid)
    }

    /// Reduce tracked margin principal for a liquidation where only part of the
    /// cleared debt may be cash-backed. The returned interest is only the portion
    /// backed by `cash_repaid`; written-off interest is never treated as received.
    pub fn realize_margin_liquidation(
        &mut self,
        asset: MarketAsset,
        cash_repaid: u64,
        debt_reduction: u64,
    ) -> Result<u64> {
        self.realize_margin_clearance(asset, cash_repaid, debt_reduction)
    }

    fn realize_margin_clearance(&mut self, asset: MarketAsset, cash_repaid: u64, debt_reduction: u64) -> Result<u64> {
        require!(
            (cash_repaid as u128) <= debt_reduction as u128,
            ErrorCode::MarketMathOverflow
        );
        let fixed_debt = match asset {
            MarketAsset::Base => self.fixed_base_debt()?,
            MarketAsset::Quote => self.fixed_quote_debt()?,
        };
        let principal = match asset {
            MarketAsset::Base => u128::from(self.fixed_base_principal),
            MarketAsset::Quote => u128::from(self.fixed_quote_principal),
        }
        // Clamp guards against rounding making principal momentarily exceed debt.
        .min(fixed_debt);
        let (_, interest_paid) = crate::math::realized_interest_split(cash_repaid, fixed_debt, principal)?;
        let (principal_reduced, _) = crate::math::realized_interest_split(debt_reduction, fixed_debt, principal)?;
        let principal_slot = match asset {
            MarketAsset::Base => &mut self.fixed_base_principal,
            MarketAsset::Quote => &mut self.fixed_quote_principal,
        };
        *principal_slot = principal_slot.saturating_sub(principal_reduced);
        Ok(interest_paid)
    }

    pub fn fixed_base_debt(&self) -> Result<u128> {
        Self::shares_to_debt(self.fixed_base_shares, self.base_borrow_index_nad)
    }

    pub fn fixed_quote_debt(&self) -> Result<u128> {
        Self::shares_to_debt(self.fixed_quote_shares, self.quote_borrow_index_nad)
    }

    pub fn fixed_debt_increase_for_shares(&self, asset: MarketAsset, shares_added: u128) -> Result<u64> {
        let (shares_before, index_nad) = match asset {
            MarketAsset::Base => (self.fixed_base_shares, self.base_borrow_index_nad),
            MarketAsset::Quote => (self.fixed_quote_shares, self.quote_borrow_index_nad),
        };
        let debt_before = Self::shares_to_debt(shares_before, index_nad)?;
        let debt_after = Self::shares_to_debt(
            shares_before
                .checked_add(shares_added)
                .ok_or(ErrorCode::DebtShareMathOverflow)?,
            index_nad,
        )?;
        u64::try_from(debt_after.checked_sub(debt_before).ok_or(ErrorCode::DebtMathOverflow)?)
            .map_err(|_| ErrorCode::DebtMathOverflow.into())
    }

    pub fn fixed_debt_reduction_for_shares(&self, asset: MarketAsset, shares_burned: u128) -> Result<u64> {
        let (shares_before, index_nad) = match asset {
            MarketAsset::Base => (self.fixed_base_shares, self.base_borrow_index_nad),
            MarketAsset::Quote => (self.fixed_quote_shares, self.quote_borrow_index_nad),
        };
        Self::aggregate_debt_reduction_for_shares(shares_before, shares_burned, index_nad)
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace)]
pub struct Fees {
    pub swap_fee_growth_index_q64: u128,
    pub interest_growth_index_q64: u128,
    /// Scaled fee entitlement not yet representable by the integer growth
    /// index. The corresponding whole-token backing already sits in
    /// `swap_fee_liability`; it must never be redistributed as unallocated
    /// revenue.
    pub swap_fee_growth_remainder_scaled: u64,
    /// Interest counterpart of `swap_fee_growth_remainder_scaled`.
    pub interest_growth_remainder_scaled: u64,
    /// Source-scoped Q64 carry for interest paid by hLP funding debt. Funding
    /// uses a non-hLP denominator, while public interest uses total yLP
    /// supply; sharing one carry across those denominators would eventually
    /// leak rounding entitlement between the two populations.
    pub hlp_funding_interest_growth_remainder_scaled: u64,
    /// Claimable swap fees physically held in the reserve vault but excluded
    /// from executable cash and live reserves.
    pub swap_fee_custody_balance: u64,
    pub interest_vault_balance: u64,
    pub swap_fee_liability: u64,
    pub interest_liability: u64,
    pub unallocated_swap_fee_liability: u64,
    pub unallocated_interest_liability: u64,
    pub swap_protocol_fee_liability: u64,
    pub swap_buyback_fee_liability: u64,
    pub interest_protocol_fee_liability: u64,
    pub interest_buyback_fee_liability: u64,
    pub referral_interest_liability: u64,
    /// Governance-approved reference market for fee-lane auctions. A default
    /// key permits only the sold market itself when it directly pairs the sold
    /// and accepted mints.
    pub fee_auction_reference_market: Pubkey,
    /// Governance-approved reference market for buyback-lane auctions. A
    /// default key has the same direct-market-only meaning as above.
    pub buyback_auction_reference_market: Pubkey,
    pub fee_swap_auction_epoch: ProtocolAuctionEpoch,
    pub fee_interest_auction_epoch: ProtocolAuctionEpoch,
    pub buyback_swap_auction_epoch: ProtocolAuctionEpoch,
    pub buyback_interest_auction_epoch: ProtocolAuctionEpoch,
}

/// Convert newly backed whole atoms plus an earlier sub-index remainder into
/// an integer per-share growth increment. The returned remainder stays in
/// scaled token-atom units and is carried into the next distribution. It is
/// always smaller than the active `u64` supply, so its persisted form is also
/// `u64` even though the numerator is evaluated in `u128`.
///
/// Moving the complete `amount` into the allocated liability is essential:
/// `growth_delta * supply` already belongs fractionally to holders even when
/// individual accounts cannot claim a whole atom yet. Leaving the rounded
/// difference in an unallocated bucket would promise it a second time.
/// Every successful distribution preserves the exact identity
/// `amount * 2^64 + prior_remainder = growth_delta * supply + remainder`.
/// The remainder is backed but neither directly claimable nor eligible for a
/// second allocation; only a later call can fold it into another index delta.
pub(crate) fn distribute_growth_q64(amount: u64, supply: u64, prior_remainder_scaled: u64) -> Result<(u128, u64)> {
    require!(supply > 0, ErrorCode::SupplyUnderflow);
    // Maximum numerator is `(2^64 - 1) * 2^64 + (2^64 - 1)`, exactly
    // `u128::MAX`. No wider production integer is required.
    let scaled = (amount as u128)
        .checked_mul(YIELD_GROWTH_SCALE_Q64)
        .and_then(|value| value.checked_add(prior_remainder_scaled as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let remainder = u64::try_from(scaled % supply as u128).map_err(|_| ErrorCode::MarketMathOverflow)?;
    Ok((scaled / supply as u128, remainder))
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq, InitSpace)]
pub struct ProtocolAuctionEpoch {
    pub start_slot: u64,
    /// Liability remaining immediately after the preceding fill. A larger
    /// current liability proves that new inventory arrived and starts a new
    /// epoch instead of inheriting an old floor price.
    pub tracked_inventory: u64,
}

pub fn accrue_fee_liability(shares: u64, fee_growth_index_q64: u128, fee_growth_checkpoint_q64: u128) -> Result<u64> {
    accrue_fee_liability_with_remainder(shares, fee_growth_index_q64, fee_growth_checkpoint_q64, 0)
        .map(|(amount, _)| amount)
}

pub fn accrue_fee_liability_with_remainder(
    shares: u64,
    fee_growth_index_q64: u128,
    fee_growth_checkpoint_q64: u128,
    prior_remainder_q64: u64,
) -> Result<(u64, u64)> {
    if shares == 0 || fee_growth_index_q64 <= fee_growth_checkpoint_q64 {
        return Ok((0, prior_remainder_q64));
    }
    let delta = fee_growth_index_q64
        .checked_sub(fee_growth_checkpoint_q64)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    // Never multiply a u64 balance by the accumulated u128 index directly.
    // Split the delta into whole and fractional Q64 limbs. Each product is at
    // most `(2^64 - 1)^2`; the fractional product plus its prior remainder is
    // at most `u128::MAX`.
    let whole_per_share = u64::try_from(delta >> 64).map_err(|_| ErrorCode::MarketMathOverflow)?;
    let whole_accrual = (shares as u128)
        .checked_mul(whole_per_share as u128)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let fractional_scaled = (shares as u128)
        .checked_mul(delta & YIELD_GROWTH_FRACTION_MASK_Q64)
        .and_then(|value| value.checked_add(prior_remainder_q64 as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let accrued = whole_accrual
        .checked_add(fractional_scaled >> 64)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let remainder =
        u64::try_from(fractional_scaled & YIELD_GROWTH_FRACTION_MASK_Q64).map_err(|_| ErrorCode::MarketMathOverflow)?;
    Ok((
        u64::try_from(accrued).map_err(|_| ErrorCode::MarketMathOverflow)?,
        remainder,
    ))
}

impl Fees {
    pub fn protocol_auction_reference_market(&self, lane: ProtocolAuctionLane) -> Pubkey {
        match lane {
            ProtocolAuctionLane::Fee => self.fee_auction_reference_market,
            ProtocolAuctionLane::Buyback => self.buyback_auction_reference_market,
        }
    }

    pub fn set_protocol_auction_reference_market(&mut self, lane: ProtocolAuctionLane, reference_market: Pubkey) {
        match lane {
            ProtocolAuctionLane::Fee => self.fee_auction_reference_market = reference_market,
            ProtocolAuctionLane::Buyback => self.buyback_auction_reference_market = reference_market,
        }
        self.reset_protocol_auction_epochs(lane);
    }

    pub fn protocol_auction_epoch(
        &self,
        lane: ProtocolAuctionLane,
        source: ProtocolRevenueSource,
        current_slot: u64,
    ) -> ProtocolAuctionEpoch {
        let liability = self.protocol_auction_liability(lane, source);
        let stored = match (lane, source) {
            (ProtocolAuctionLane::Fee, ProtocolRevenueSource::Swap) => self.fee_swap_auction_epoch,
            (ProtocolAuctionLane::Fee, ProtocolRevenueSource::Interest) => self.fee_interest_auction_epoch,
            (ProtocolAuctionLane::Buyback, ProtocolRevenueSource::Swap) => self.buyback_swap_auction_epoch,
            (ProtocolAuctionLane::Buyback, ProtocolRevenueSource::Interest) => self.buyback_interest_auction_epoch,
        };
        if stored.start_slot == 0 || liability > stored.tracked_inventory {
            ProtocolAuctionEpoch {
                start_slot: current_slot,
                tracked_inventory: liability,
            }
        } else {
            stored
        }
    }

    pub fn reset_protocol_auction_epochs(&mut self, lane: ProtocolAuctionLane) {
        match lane {
            ProtocolAuctionLane::Fee => {
                self.fee_swap_auction_epoch = ProtocolAuctionEpoch::default();
                self.fee_interest_auction_epoch = ProtocolAuctionEpoch::default();
            }
            ProtocolAuctionLane::Buyback => {
                self.buyback_swap_auction_epoch = ProtocolAuctionEpoch::default();
                self.buyback_interest_auction_epoch = ProtocolAuctionEpoch::default();
            }
        }
    }

    pub fn total_liability(&self) -> Result<u64> {
        self.swap_fee_liability
            .checked_add(self.interest_liability)
            .and_then(|value| value.checked_add(self.unallocated_swap_fee_liability))
            .and_then(|value| value.checked_add(self.unallocated_interest_liability))
            .and_then(|value| value.checked_add(self.swap_protocol_fee_liability))
            .and_then(|value| value.checked_add(self.swap_buyback_fee_liability))
            .and_then(|value| value.checked_add(self.interest_protocol_fee_liability))
            .and_then(|value| value.checked_add(self.interest_buyback_fee_liability))
            .and_then(|value| value.checked_add(self.referral_interest_liability))
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    }

    pub fn assert_backed(&self) -> Result<()> {
        let swap_liability = self
            .swap_fee_liability
            .checked_add(self.unallocated_swap_fee_liability)
            .and_then(|value| value.checked_add(self.swap_protocol_fee_liability))
            .and_then(|value| value.checked_add(self.swap_buyback_fee_liability))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require_gte!(
            self.swap_fee_custody_balance,
            swap_liability,
            ErrorCode::UnbackedFeeLiability
        );
        let interest_liability = self
            .interest_liability
            .checked_add(self.unallocated_interest_liability)
            .and_then(|value| value.checked_add(self.referral_interest_liability))
            .and_then(|value| value.checked_add(self.interest_protocol_fee_liability))
            .and_then(|value| value.checked_add(self.interest_buyback_fee_liability))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require_gte!(
            self.interest_vault_balance,
            interest_liability,
            ErrorCode::UnbackedFeeLiability
        );
        Ok(())
    }

    pub fn protocol_auction_liability(&self, lane: ProtocolAuctionLane, source: ProtocolRevenueSource) -> u64 {
        match (lane, source) {
            (ProtocolAuctionLane::Fee, ProtocolRevenueSource::Swap) => self.swap_protocol_fee_liability,
            (ProtocolAuctionLane::Buyback, ProtocolRevenueSource::Swap) => self.swap_buyback_fee_liability,
            (ProtocolAuctionLane::Fee, ProtocolRevenueSource::Interest) => self.interest_protocol_fee_liability,
            (ProtocolAuctionLane::Buyback, ProtocolRevenueSource::Interest) => self.interest_buyback_fee_liability,
        }
    }

    pub fn settle_protocol_auction_liability(
        &mut self,
        lane: ProtocolAuctionLane,
        source: ProtocolRevenueSource,
        amount: u64,
        epoch_start_slot: u64,
    ) -> Result<()> {
        require!(amount > 0, ErrorCode::AmountZero);
        let liability = match (lane, source) {
            (ProtocolAuctionLane::Fee, ProtocolRevenueSource::Swap) => &mut self.swap_protocol_fee_liability,
            (ProtocolAuctionLane::Buyback, ProtocolRevenueSource::Swap) => &mut self.swap_buyback_fee_liability,
            (ProtocolAuctionLane::Fee, ProtocolRevenueSource::Interest) => &mut self.interest_protocol_fee_liability,
            (ProtocolAuctionLane::Buyback, ProtocolRevenueSource::Interest) => &mut self.interest_buyback_fee_liability,
        };
        *liability = liability.checked_sub(amount).ok_or(ErrorCode::MarketMathOverflow)?;
        match source {
            ProtocolRevenueSource::Swap => {
                self.swap_fee_custody_balance = self
                    .swap_fee_custody_balance
                    .checked_sub(amount)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
            }
            ProtocolRevenueSource::Interest => {
                self.interest_vault_balance = self
                    .interest_vault_balance
                    .checked_sub(amount)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
            }
        }
        let remaining = self.protocol_auction_liability(lane, source);
        let next_epoch = if remaining == 0 {
            ProtocolAuctionEpoch::default()
        } else {
            ProtocolAuctionEpoch {
                start_slot: epoch_start_slot,
                tracked_inventory: remaining,
            }
        };
        match (lane, source) {
            (ProtocolAuctionLane::Fee, ProtocolRevenueSource::Swap) => self.fee_swap_auction_epoch = next_epoch,
            (ProtocolAuctionLane::Fee, ProtocolRevenueSource::Interest) => self.fee_interest_auction_epoch = next_epoch,
            (ProtocolAuctionLane::Buyback, ProtocolRevenueSource::Swap) => self.buyback_swap_auction_epoch = next_epoch,
            (ProtocolAuctionLane::Buyback, ProtocolRevenueSource::Interest) => {
                self.buyback_interest_auction_epoch = next_epoch
            }
        }
        Ok(())
    }

    pub fn protocol_fee_liability(&self) -> Result<u64> {
        self.swap_protocol_fee_liability
            .checked_add(self.interest_protocol_fee_liability)
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    }

    pub fn buyback_fee_liability(&self) -> Result<u64> {
        self.swap_buyback_fee_liability
            .checked_add(self.interest_buyback_fee_liability)
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    }
}

/// LP ownership frozen before an operation may mint or burn vault-owned yLP.
/// Funding entitlement is deliberately payment-time: ordinary yLP present at
/// this snapshot participates even if the debt accrued earlier, while a holder
/// that exited before the snapshot does not. Newly borrowed principal at the
/// same debt index cannot create interest. Inline settlement uses this
/// partition to prove that ordinary-plus-burned-MIN supply was conserved
/// across hLP rebalancing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HlpYieldEligibility {
    pub ylp_supply: u64,
    pub base_hlp_ylp_shares: u64,
    pub quote_hlp_ylp_shares: u64,
}

impl HlpYieldEligibility {
    /// yLP supply eligible for interest paid by an hLP funding position.
    ///
    /// Both hLP vault balances are excluded. The permanently burned
    /// `MIN_LIQUIDITY` shares remain in this denominator, so funding interest
    /// has a deterministic sink when no ordinary yLP holder exists and cannot
    /// be captured by a later deposit.
    pub fn non_hlp_ylp_supply(self) -> Result<u64> {
        let hlp_ylp_shares = self
            .base_hlp_ylp_shares
            .checked_add(self.quote_hlp_ylp_shares)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let supply = self
            .ylp_supply
            .checked_sub(hlp_ylp_shares)
            .ok_or(ErrorCode::BrokenInvariant)?;
        require_gte!(supply, MIN_LIQUIDITY, ErrorCode::BrokenInvariant);
        Ok(supply)
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace)]
pub struct HlpVault {
    pub ylp_vault: Pubkey,
    pub ylp_shares: u64,
    /// hLP-owned live reserve depth that is not backed by reserve cash or
    /// normal cash-backed debt. This is the explicit synthetic live component
    /// in `r_virtual = r_cash + r_cash_backed_debt + r_hlp_live`.
    pub base_hlp_live_reserve: u64,
    pub quote_hlp_live_reserve: u64,
    /// Funding debt used by the hLP vault. It accrues interest and counts
    /// toward utilization, but is not same-side cash-backed reserve debt.
    pub debt_shares: u128,
    /// Raw borrowed token atoms; products and indexed shares stay `u128`.
    pub debt_principal: u64,
    pub hlp_supply: u64,
    pub residual_exposure: i128,
    pub base_swap_fee_growth_index_q64: u128,
    pub base_interest_growth_index_q64: u128,
    pub quote_swap_fee_growth_index_q64: u128,
    pub quote_interest_growth_index_q64: u128,
    pub base_swap_fee_checkpoint_q64: u128,
    pub base_interest_checkpoint_q64: u128,
    pub quote_swap_fee_checkpoint_q64: u128,
    pub quote_interest_checkpoint_q64: u128,
    /// Aggregate sub-atom yLP entitlement carried across hLP checkpoints.
    /// These are distinct from each holder YieldAccount remainder: this layer
    /// converts vault-owned yLP growth into hLP growth without double-flooring.
    pub base_swap_fee_remainder_q64: u64,
    pub base_interest_remainder_q64: u64,
    pub quote_swap_fee_remainder_q64: u64,
    pub quote_interest_remainder_q64: u64,
    /// Sub-index distribution carry for the second, yLP-to-hLP allocation
    /// layer. Whole-token backing represented here has already left the
    /// corresponding `unallocated_*` bucket.
    pub base_swap_fee_growth_remainder_scaled: u64,
    pub base_interest_growth_remainder_scaled: u64,
    pub quote_swap_fee_growth_remainder_scaled: u64,
    pub quote_interest_growth_remainder_scaled: u64,
    pub unallocated_base_swap_fee_amount: u64,
    pub unallocated_base_interest_amount: u64,
    pub unallocated_quote_swap_fee_amount: u64,
    pub unallocated_quote_interest_amount: u64,
    pub last_nav_nad: u128,
    pub cached_settlement_price_nad: u128,
    /// Smoothed APR of the opposite asset borrowed by this target-asset hLP.
    /// The fixed twelve-hour half-life gives Stop Rate orders stable semantics.
    pub funding_apr_ema_nad: u128,
    pub funding_apr_ema_last_slot: u64,
}

impl HlpVault {
    pub fn initialize(&mut self, ylp_vault: Pubkey) {
        self.ylp_vault = ylp_vault;
    }

    pub fn mint_hlp(&mut self, amount: u64) -> Result<()> {
        require!(amount > 0, ErrorCode::AmountZero);
        self.hlp_supply = self.hlp_supply.checked_add(amount).ok_or(ErrorCode::SupplyOverflow)?;
        Ok(())
    }

    pub fn burn_hlp(&mut self, amount: u64) -> Result<()> {
        require!(amount > 0, ErrorCode::AmountZero);
        self.hlp_supply = self.hlp_supply.checked_sub(amount).ok_or(ErrorCode::SupplyUnderflow)?;
        if self.hlp_supply == 0 {
            require_eq!(self.ylp_shares, 0, ErrorCode::BrokenInvariant);
            require_eq!(self.base_hlp_live_reserve, 0, ErrorCode::BrokenInvariant);
            require_eq!(self.quote_hlp_live_reserve, 0, ErrorCode::BrokenInvariant);
            require_eq!(self.debt_shares, 0, ErrorCode::BrokenInvariant);
            require_eq!(self.debt_principal, 0, ErrorCode::BrokenInvariant);
            // A fully closed vault has no economic exposure. Do not leave a
            // stale fail-closed signal that would keep the next generation of
            // deposits gated after every share and debt claim is gone.
            self.residual_exposure = 0;
            self.funding_apr_ema_nad = 0;
            self.funding_apr_ema_last_slot = 0;
        }
        Ok(())
    }

    pub fn credit_ylp(&mut self, shares: u64) -> Result<()> {
        self.ylp_shares = self.ylp_shares.checked_add(shares).ok_or(ErrorCode::SupplyOverflow)?;
        Ok(())
    }

    pub fn debit_ylp(&mut self, shares: u64) -> Result<()> {
        self.ylp_shares = self.ylp_shares.checked_sub(shares).ok_or(ErrorCode::SupplyUnderflow)?;
        Ok(())
    }

    pub fn hlp_live_reserve(&self, asset: MarketAsset) -> u64 {
        match asset {
            MarketAsset::Base => self.base_hlp_live_reserve,
            MarketAsset::Quote => self.quote_hlp_live_reserve,
        }
    }

    pub fn credit_hlp_live_reserve(&mut self, asset: MarketAsset, amount: u64) -> Result<()> {
        let reserve = match asset {
            MarketAsset::Base => &mut self.base_hlp_live_reserve,
            MarketAsset::Quote => &mut self.quote_hlp_live_reserve,
        };
        *reserve = reserve.checked_add(amount).ok_or(ErrorCode::ReserveOverflow)?;
        Ok(())
    }

    pub fn debit_hlp_live_reserve(&mut self, asset: MarketAsset, amount: u64) -> Result<()> {
        let reserve = match asset {
            MarketAsset::Base => &mut self.base_hlp_live_reserve,
            MarketAsset::Quote => &mut self.quote_hlp_live_reserve,
        };
        *reserve = reserve.checked_sub(amount).ok_or(ErrorCode::ReserveUnderflow)?;
        Ok(())
    }

    pub fn add_debt_shares(&mut self, shares: u128) -> Result<()> {
        self.debt_shares = self
            .debt_shares
            .checked_add(shares)
            .ok_or(ErrorCode::DebtShareMathOverflow)?;
        Ok(())
    }

    pub fn add_debt_principal(&mut self, amount: u64) -> Result<()> {
        self.debt_principal = self
            .debt_principal
            .checked_add(amount)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        Ok(())
    }

    pub fn clear_debt_repay(&mut self, shares_burned: u128, borrow_index_nad: u128) -> Result<DebtClearance> {
        require!(shares_burned > 0, ErrorCode::DebtShareDivisionOverflow);
        require_gte!(self.debt_shares, shares_burned, ErrorCode::DebtShareMathOverflow);
        let total_debt = Debt::shares_to_debt(self.debt_shares, borrow_index_nad)?;
        let remaining_shares = self
            .debt_shares
            .checked_sub(shares_burned)
            .ok_or(ErrorCode::DebtShareMathOverflow)?;
        let remaining_debt = Debt::shares_to_debt(remaining_shares, borrow_index_nad)?;
        let debt_reduced_u128 = total_debt
            .checked_sub(remaining_debt)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        let debt_reduced = u64::try_from(debt_reduced_u128).map_err(|_| ErrorCode::DebtMathOverflow)?;

        let principal = u128::from(self.debt_principal).min(total_debt);
        let (principal_paid, interest_paid) =
            crate::math::realized_interest_split(debt_reduced, total_debt, principal)?;
        let remaining_debt = u64::try_from(remaining_debt).map_err(|_| ErrorCode::DebtMathOverflow)?;
        self.debt_shares = remaining_shares;
        self.debt_principal = self.debt_principal.saturating_sub(principal_paid);
        if self.debt_shares == 0 {
            self.debt_principal = 0;
        }

        Ok(DebtClearance {
            shares_burned,
            cash_repaid: debt_reduced,
            debt_reduced,
            aggregate_debt_reduced: debt_reduced,
            principal_paid,
            interest_paid,
            remaining_debt,
            position_principal_reduced: principal_paid,
        })
    }

    pub fn repayment_for_max(&self, max_repay_amount: u64, borrow_index_nad: u128) -> Result<DebtRepaymentQuote> {
        Debt::repayment_for_max(self.debt_shares, self.debt_shares, borrow_index_nad, max_repay_amount)
    }

    pub fn checkpoint_yield_from_ylp(&mut self, base_side: &MarketSide, quote_side: &MarketSide) -> Result<()> {
        self.checkpoint_yield_from_ylp_shares(base_side, quote_side, self.ylp_shares)
    }

    pub fn checkpoint_yield_from_ylp_shares(
        &mut self,
        base_side: &MarketSide,
        quote_side: &MarketSide,
        eligible_ylp_shares: u64,
    ) -> Result<()> {
        let (base_swap_fee_amount, base_swap_fee_remainder_q64) = accrue_fee_liability_with_remainder(
            eligible_ylp_shares,
            base_side.fees.swap_fee_growth_index_q64,
            self.base_swap_fee_checkpoint_q64,
            self.base_swap_fee_remainder_q64,
        )?;
        let (base_interest_amount, base_interest_remainder_q64) = accrue_fee_liability_with_remainder(
            eligible_ylp_shares,
            base_side.fees.interest_growth_index_q64,
            self.base_interest_checkpoint_q64,
            self.base_interest_remainder_q64,
        )?;
        let (quote_swap_fee_amount, quote_swap_fee_remainder_q64) = accrue_fee_liability_with_remainder(
            eligible_ylp_shares,
            quote_side.fees.swap_fee_growth_index_q64,
            self.quote_swap_fee_checkpoint_q64,
            self.quote_swap_fee_remainder_q64,
        )?;
        let (quote_interest_amount, quote_interest_remainder_q64) = accrue_fee_liability_with_remainder(
            eligible_ylp_shares,
            quote_side.fees.interest_growth_index_q64,
            self.quote_interest_checkpoint_q64,
            self.quote_interest_remainder_q64,
        )?;

        credit_hlp_growth(
            self.hlp_supply,
            &mut self.unallocated_base_swap_fee_amount,
            &mut self.base_swap_fee_growth_index_q64,
            &mut self.base_swap_fee_growth_remainder_scaled,
            base_swap_fee_amount,
        )?;
        credit_hlp_growth(
            self.hlp_supply,
            &mut self.unallocated_base_interest_amount,
            &mut self.base_interest_growth_index_q64,
            &mut self.base_interest_growth_remainder_scaled,
            base_interest_amount,
        )?;
        credit_hlp_growth(
            self.hlp_supply,
            &mut self.unallocated_quote_swap_fee_amount,
            &mut self.quote_swap_fee_growth_index_q64,
            &mut self.quote_swap_fee_growth_remainder_scaled,
            quote_swap_fee_amount,
        )?;
        credit_hlp_growth(
            self.hlp_supply,
            &mut self.unallocated_quote_interest_amount,
            &mut self.quote_interest_growth_index_q64,
            &mut self.quote_interest_growth_remainder_scaled,
            quote_interest_amount,
        )?;

        self.base_swap_fee_checkpoint_q64 = base_side.fees.swap_fee_growth_index_q64;
        self.base_interest_checkpoint_q64 = base_side.fees.interest_growth_index_q64;
        self.quote_swap_fee_checkpoint_q64 = quote_side.fees.swap_fee_growth_index_q64;
        self.quote_interest_checkpoint_q64 = quote_side.fees.interest_growth_index_q64;
        self.base_swap_fee_remainder_q64 = base_swap_fee_remainder_q64;
        self.base_interest_remainder_q64 = base_interest_remainder_q64;
        self.quote_swap_fee_remainder_q64 = quote_swap_fee_remainder_q64;
        self.quote_interest_remainder_q64 = quote_interest_remainder_q64;
        Ok(())
    }

    pub fn yield_growth_indexes(&self, revenue_asset: MarketAsset) -> (u128, u128) {
        match revenue_asset {
            MarketAsset::Base => (self.base_swap_fee_growth_index_q64, self.base_interest_growth_index_q64),
            MarketAsset::Quote => (
                self.quote_swap_fee_growth_index_q64,
                self.quote_interest_growth_index_q64,
            ),
        }
    }
}

/// Publish the yLP-owned revenue of one hLP vault into its holder index. This
/// is the same exact distributor used at the outer yLP tier:
/// `new_amount * 2^64 + old_carry = delta * hlp_supply + new_carry`.
/// Once supply is positive, every whole atom leaves `unallocated_amount`; the
/// carry is its backed, not-yet-indexable residue and cannot be allocated a
/// second time. A zero-supply vault instead retains the whole amount for its
/// final-holder drain.
fn credit_hlp_growth(
    hlp_supply: u64,
    unallocated_amount: &mut u64,
    growth_index_q64: &mut u128,
    growth_remainder_scaled: &mut u64,
    new_amount: u64,
) -> Result<()> {
    *unallocated_amount = unallocated_amount
        .checked_add(new_amount)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    if hlp_supply == 0 || (*unallocated_amount == 0 && *growth_remainder_scaled == 0) {
        return Ok(());
    }
    let allocated = *unallocated_amount;
    let (growth_delta, remainder_scaled) = distribute_growth_q64(allocated, hlp_supply, *growth_remainder_scaled)?;
    *growth_index_q64 = growth_index_q64
        .checked_add(growth_delta)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    *unallocated_amount = unallocated_amount
        .checked_sub(allocated)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    *growth_remainder_scaled = remainder_scaled;
    Ok(())
}

impl Market {
    pub fn has_active_hlp(&self) -> bool {
        self.base_hlp_vault.hlp_supply > 0
            || self.base_hlp_vault.residual_exposure != 0
            || self.quote_hlp_vault.hlp_supply > 0
            || self.quote_hlp_vault.residual_exposure != 0
    }

    pub fn hlp_yield_growth_indexes(&self, hlp_asset: MarketAsset, revenue_asset: MarketAsset) -> (u128, u128) {
        match hlp_asset {
            MarketAsset::Base => self.base_hlp_vault.yield_growth_indexes(revenue_asset),
            MarketAsset::Quote => self.quote_hlp_vault.yield_growth_indexes(revenue_asset),
        }
    }

    pub fn drain_hlp_unallocated_yield(
        &mut self,
        hlp_asset: MarketAsset,
        base_yield_account: &mut YieldAccount,
        quote_yield_account: &mut YieldAccount,
    ) -> Result<()> {
        let vault = match hlp_asset {
            MarketAsset::Base => &mut self.base_hlp_vault,
            MarketAsset::Quote => &mut self.quote_hlp_vault,
        };
        require_eq!(vault.hlp_supply, 0, ErrorCode::BrokenInvariant);
        base_yield_account.credit_unallocated(
            vault.unallocated_base_swap_fee_amount,
            vault.unallocated_base_interest_amount,
            (vault.base_swap_fee_remainder_q64 as u128)
                .checked_add(vault.base_swap_fee_growth_remainder_scaled as u128)
                .ok_or(ErrorCode::MarketMathOverflow)?,
            (vault.base_interest_remainder_q64 as u128)
                .checked_add(vault.base_interest_growth_remainder_scaled as u128)
                .ok_or(ErrorCode::MarketMathOverflow)?,
        )?;
        quote_yield_account.credit_unallocated(
            vault.unallocated_quote_swap_fee_amount,
            vault.unallocated_quote_interest_amount,
            (vault.quote_swap_fee_remainder_q64 as u128)
                .checked_add(vault.quote_swap_fee_growth_remainder_scaled as u128)
                .ok_or(ErrorCode::MarketMathOverflow)?,
            (vault.quote_interest_remainder_q64 as u128)
                .checked_add(vault.quote_interest_growth_remainder_scaled as u128)
                .ok_or(ErrorCode::MarketMathOverflow)?,
        )?;
        vault.unallocated_base_swap_fee_amount = 0;
        vault.unallocated_base_interest_amount = 0;
        vault.unallocated_quote_swap_fee_amount = 0;
        vault.unallocated_quote_interest_amount = 0;
        vault.base_swap_fee_remainder_q64 = 0;
        vault.base_interest_remainder_q64 = 0;
        vault.quote_swap_fee_remainder_q64 = 0;
        vault.quote_interest_remainder_q64 = 0;
        vault.base_swap_fee_growth_remainder_scaled = 0;
        vault.base_interest_growth_remainder_scaled = 0;
        vault.quote_swap_fee_growth_remainder_scaled = 0;
        vault.quote_interest_growth_remainder_scaled = 0;
        Ok(())
    }

    /// Validate that the current hLP hedge state admits new capital without
    /// exposing rebalance-engine intermediates outside the hLP domain.
    pub fn assert_hlp_entry_available(&self, target_asset: MarketAsset) -> Result<()> {
        let prices = current_hlp_curve_prices(self)?;
        let entry = current_hlp_entry_state_with_prices(self, target_asset, prices)?;
        require!(entry.disposition.admits_entry(), ErrorCode::HlpSettlementUnavailable);
        Ok(())
    }

    pub fn require_residual_hlp_swap_safety(
        &self,
        start_base_price_nad: u128,
        end_base_price_nad: u128,
        base_residual_on_entry: bool,
        quote_residual_on_entry: bool,
    ) -> Result<()> {
        let start_prices = rebalance::hlp_curve_prices_from_base_price_nad(start_base_price_nad)?;
        let end_prices = rebalance::hlp_curve_prices_from_base_price_nad(end_base_price_nad)?;
        rebalance::require_residual_hlp_swap_safe(
            self,
            MarketAsset::Base,
            start_prices,
            end_prices,
            base_residual_on_entry,
        )?;
        rebalance::require_residual_hlp_swap_safe(
            self,
            MarketAsset::Quote,
            start_prices,
            end_prices,
            quote_residual_on_entry,
        )
    }

    pub fn checkpoint_hlp_vaults(&mut self) -> Result<(i128, i128)> {
        let prices = current_hlp_curve_prices(self)?;
        checkpoint_hlp_yield_from_ylp(self, MarketAsset::Base)?;
        checkpoint_hlp_yield_from_ylp(self, MarketAsset::Quote)?;
        let base_active = self.base_hlp_vault.hlp_supply > 0 || self.base_hlp_vault.residual_exposure != 0;
        let quote_active = self.quote_hlp_vault.hlp_supply > 0 || self.quote_hlp_vault.residual_exposure != 0;
        let base_delta = if base_active {
            checkpoint_one_hlp_with_prices(self, MarketAsset::Base, prices)?
        } else {
            0
        };
        let quote_delta = if quote_active {
            checkpoint_one_hlp_with_prices(self, MarketAsset::Quote, prices)?
        } else {
            0
        };
        Ok((base_delta, quote_delta))
    }

    pub fn checkpoint_hlp_yield_from_ylp(&mut self, target_asset: MarketAsset) -> Result<()> {
        rebalance::checkpoint_hlp_yield_from_ylp(self, target_asset)
    }

    pub fn checkpoint_hlp_yield_from_ylp_shares(
        &mut self,
        target_asset: MarketAsset,
        eligible_ylp_shares: u64,
    ) -> Result<()> {
        rebalance::checkpoint_hlp_yield_from_ylp_shares(self, target_asset, eligible_ylp_shares)
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace)]
pub struct DailyBorrowBucket {
    /// Gross principal lent out through the public borrow path. Internal hLP
    /// funding and isolated leverage do not consume this capacity. This is a
    /// 24-hour leaky/token bucket, not an exact trailing-window sum: it permits
    /// a full burst after idle and then refills at the configured daily rate.
    pub borrowed_bucket: u64,
    pub last_decay_slot: u64,
    /// Numerator remainder from `limit * elapsed_ms / MS_PER_DAY`. For a fixed
    /// absolute limit, carrying it makes refill independent of how often the
    /// bucket is checkpointed. The bps-derived absolute limit can still move
    /// when conservative market depth changes.
    pub decay_remainder_ms: u64,
}

impl DailyBorrowBucket {
    pub fn decay_to_slot(&mut self, limit: u64, current_slot: u64) -> Result<()> {
        let elapsed_ms = slots_to_ms(self.last_decay_slot, current_slot).ok_or(ErrorCode::InvalidArgument)?;
        if self.borrowed_bucket == 0 {
            self.decay_remainder_ms = 0;
        } else if elapsed_ms > 0 {
            let released_numerator = (limit as u128)
                .checked_mul(elapsed_ms as u128)
                .and_then(|value| value.checked_add(self.decay_remainder_ms as u128))
                .ok_or(ErrorCode::MarketMathOverflow)?;
            let released = released_numerator / MS_PER_DAY as u128;
            if released >= self.borrowed_bucket as u128 {
                self.borrowed_bucket = 0;
                self.decay_remainder_ms = 0;
            } else {
                let released = u64::try_from(released).map_err(|_| ErrorCode::MarketMathOverflow)?;
                self.borrowed_bucket = self
                    .borrowed_bucket
                    .checked_sub(released)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                self.decay_remainder_ms = u64::try_from(released_numerator % MS_PER_DAY as u128)
                    .map_err(|_| ErrorCode::MarketMathOverflow)?;
            }
        }
        self.last_decay_slot = current_slot;
        Ok(())
    }

    pub fn record_borrow(&mut self, amount: u64, limit: u64, current_slot: u64) -> Result<()> {
        self.decay_to_slot(limit, current_slot)?;
        let next_bucket = self
            .borrowed_bucket
            .checked_add(amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require_gte!(limit, next_bucket, ErrorCode::DailyLimitExceeded);
        self.borrowed_bucket = next_bucket;
        Ok(())
    }

    pub fn remaining(&self, limit: u64, current_slot: u64) -> Result<u64> {
        let mut decayed = *self;
        decayed.decay_to_slot(limit, current_slot)?;
        Ok(limit.saturating_sub(decayed.borrowed_bucket))
    }
}

pub struct AddLiquidityReceipt {
    pub base_reserve_credit: u64,
    pub quote_reserve_credit: u64,
    pub ylp_amount: u64,
    pub ylp_supply: u64,
}

pub struct RemoveLiquidityReceipt {
    pub ylp_amount: u64,
    pub base_amount_out: u64,
    pub quote_amount_out: u64,
    pub ylp_supply: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DebtReceipt {
    pub debt_delta: i64,
    pub cash_repaid: u64,
    pub interest_paid: u64,
    pub fixed_base_debt: u128,
    pub fixed_quote_debt: u128,
    pub global_health_base_contribution_for_quote_debt: u64,
    pub global_health_quote_contribution_for_base_debt: u64,
    pub base_liquidation_cf_bps: u16,
    pub quote_liquidation_cf_bps: u16,
    pub base_debt_health_bps: u64,
    pub quote_debt_health_bps: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SwapReceipt {
    pub amount_in_after_fee: u64,
    pub reserve_input_credit: u64,
    pub amount_out: u64,
    pub fee_credit: u64,
    pub base_fee_credit: u64,
    pub distributed_surcharge_credit: u64,
    pub fee_breakdown: SwapFeeBreakdown,
    pub reserve_in_live_reserve: u64,
    pub reserve_out_live_reserve: u64,
    pub fees: FeesReceipt,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace, PartialEq, Eq)]
pub struct InsuranceDrawWindow {
    /// Slot at which the current 24-hour accounting window began. A zero
    /// value means no draw window has been opened yet.
    pub start_slot: u64,
    /// Available insurance at the start of the window, before any draws.
    pub opening_available: u64,
    /// Net token credits received after the window opened.
    pub credited: u64,
    /// Gross token amount debited by insurance draws in this window.
    pub drawn: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, InitSpace, PartialEq, Eq)]
pub struct Insurance {
    pub base_vault: Pubkey,
    pub quote_vault: Pubkey,
    pub base_available: u64,
    pub quote_available: u64,
    pub base_draw_window: InsuranceDrawWindow,
    pub quote_draw_window: InsuranceDrawWindow,
    pub per_event_draw_bps: u16,
    pub per_day_draw_bps: u16,
}

impl Default for Insurance {
    fn default() -> Self {
        Self {
            base_vault: Pubkey::default(),
            quote_vault: Pubkey::default(),
            base_available: 0,
            quote_available: 0,
            base_draw_window: InsuranceDrawWindow::default(),
            quote_draw_window: InsuranceDrawWindow::default(),
            per_event_draw_bps: crate::constants::MAX_INSURANCE_DRAW_PER_EVENT_BPS,
            per_day_draw_bps: crate::constants::MAX_INSURANCE_DRAW_PER_DAY_BPS,
        }
    }
}

impl Insurance {
    pub const fn available(&self, asset: MarketAsset) -> u64 {
        match asset {
            MarketAsset::Base => self.base_available,
            MarketAsset::Quote => self.quote_available,
        }
    }

    fn available_mut(&mut self, asset: MarketAsset) -> &mut u64 {
        match asset {
            MarketAsset::Base => &mut self.base_available,
            MarketAsset::Quote => &mut self.quote_available,
        }
    }

    fn draw_window(&self, asset: MarketAsset) -> &InsuranceDrawWindow {
        match asset {
            MarketAsset::Base => &self.base_draw_window,
            MarketAsset::Quote => &self.quote_draw_window,
        }
    }

    fn draw_window_mut(&mut self, asset: MarketAsset) -> &mut InsuranceDrawWindow {
        match asset {
            MarketAsset::Base => &mut self.base_draw_window,
            MarketAsset::Quote => &mut self.quote_draw_window,
        }
    }

    fn checkpoint_draw_window(&mut self, asset: MarketAsset, current_slot: u64) {
        let available = self.available(asset);
        let window = self.draw_window_mut(asset);
        if window.start_slot == 0
            || current_slot.saturating_sub(window.start_slot) >= crate::constants::INSURANCE_DRAW_WINDOW_SLOTS
        {
            *window = InsuranceDrawWindow {
                start_slot: current_slot.max(1),
                opening_available: available,
                credited: 0,
                drawn: 0,
            };
        }
    }

    pub fn draw_capacity(&mut self, asset: MarketAsset, current_slot: u64) -> Result<u64> {
        use crate::constants::BPS_DENOMINATOR;

        self.checkpoint_draw_window(asset, current_slot);
        let available = self.available(asset);
        let window = *self.draw_window(asset);
        let daily_basis = window
            .opening_available
            .checked_add(window.credited)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let daily_limit = u64::try_from(
            (daily_basis as u128)
                .checked_mul(self.per_day_draw_bps as u128)
                .ok_or(ErrorCode::MarketMathOverflow)?
                / BPS_DENOMINATOR as u128,
        )
        .map_err(|_| ErrorCode::MarketMathOverflow)?;
        let daily_remaining = daily_limit.saturating_sub(window.drawn);
        let event_limit = u64::try_from(
            (available as u128)
                .checked_mul(self.per_event_draw_bps as u128)
                .ok_or(ErrorCode::MarketMathOverflow)?
                / BPS_DENOMINATOR as u128,
        )
        .map_err(|_| ErrorCode::MarketMathOverflow)?;
        Ok(available.min(event_limit).min(daily_remaining))
    }

    pub fn credit(&mut self, asset: MarketAsset, actual_credit: u64, current_slot: u64) -> Result<()> {
        require!(actual_credit > 0, ErrorCode::AmountZero);
        self.checkpoint_draw_window(asset, current_slot);
        let available = self.available_mut(asset);
        *available = available
            .checked_add(actual_credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let window = self.draw_window_mut(asset);
        window.credited = window
            .credited
            .checked_add(actual_credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        Ok(())
    }

    pub fn reconcile_credit(&mut self, asset: MarketAsset, gross_credit: u64, actual_credit: u64) -> Result<()> {
        require_gte!(gross_credit, actual_credit, ErrorCode::MarketMathOverflow);
        let fee = gross_credit
            .checked_sub(actual_credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        if fee == 0 {
            return Ok(());
        }
        let available = self.available_mut(asset);
        *available = available.checked_sub(fee).ok_or(ErrorCode::MarketMathOverflow)?;
        let window = self.draw_window_mut(asset);
        window.credited = window.credited.checked_sub(fee).ok_or(ErrorCode::MarketMathOverflow)?;
        Ok(())
    }

    pub fn consume_draw(&mut self, asset: MarketAsset, amount: u64, current_slot: u64) -> Result<()> {
        let capacity = self.draw_capacity(asset, current_slot)?;
        require_gte!(capacity, amount, ErrorCode::InsuranceDrawExceeded);
        let available = self.available_mut(asset);
        *available = available.checked_sub(amount).ok_or(ErrorCode::InsufficientInsurance)?;
        let window = self.draw_window_mut(asset);
        window.drawn = window.drawn.checked_add(amount).ok_or(ErrorCode::MarketMathOverflow)?;
        Ok(())
    }
}

impl Market {
    pub fn credit_insurance_donation(
        &mut self,
        asset: MarketAsset,
        actual_credit: u64,
        current_slot: u64,
    ) -> Result<()> {
        self.insurance.credit(asset, actual_credit, current_slot)
    }
}

impl DebtReceipt {
    fn from_market(
        market: &Market,
        borrow_position: &BorrowPosition,
        debt_delta: i64,
        cash_repaid: u64,
        interest_paid: u64,
        health: &MarketHealth,
    ) -> Result<Self> {
        Ok(Self {
            debt_delta,
            cash_repaid,
            interest_paid,
            fixed_base_debt: market.debt.fixed_base_debt()?,
            fixed_quote_debt: market.debt.fixed_quote_debt()?,
            global_health_base_contribution_for_quote_debt: borrow_position
                .global_health_base_contribution_for_quote_debt,
            global_health_quote_contribution_for_base_debt: borrow_position
                .global_health_quote_contribution_for_base_debt,
            base_liquidation_cf_bps: borrow_position.base_liquidation_cf_bps,
            quote_liquidation_cf_bps: borrow_position.quote_liquidation_cf_bps,
            base_debt_health_bps: health.base_debt_health_bps,
            quote_debt_health_bps: health.quote_debt_health_bps,
        })
    }
}

#[account]
#[derive(InitSpace, Default)]
pub struct Market {
    pub version: u8,
    pub ylp_mint: Pubkey,
    pub base_side: MarketSide,
    pub quote_side: MarketSide,
    pub config: MarketConfig,
    pub amm: AmmState,
    pub debt: Debt,
    pub base_hlp_vault: HlpVault,
    pub quote_hlp_vault: HlpVault,
    pub risk: Risk,
    pub insurance: Insurance,
    pub params_hash: [u8; 32],
    /// One-shot signer allowed to provide the first fully-backed Base/Quote
    /// seed. It is cleared permanently once yLP supply becomes nonzero.
    pub initial_liquidity_authority: Pubkey,
    /// External yLP burned into active governance support. This is added back
    /// when computing direct-yLP eligibility; internal reserve-share supply is
    /// intentionally unchanged by governance locking.
    pub governance_locked_ylp: u64,
    /// Independent monotone revisions for fee, concentration, IRM, EMA,
    /// daily-borrow-limit, and center-controller parameter families.
    pub parameter_revisions: [u64; 7],
    /// Latest trader-visible marginal price committed by a curve mutation.
    pub last_marginal_observation_nad: u64,
    /// Monotone revision for executable-curve mutations.
    pub curve_revision: u64,
    /// Curve revision represented by the materialized lending-risk snapshot.
    pub risk_revision: u64,
    pub last_update_slot: u64,
    pub reduce_only: bool,
    pub bump: u8,
}

impl Market {
    pub(crate) fn validate_mint_domain(
        base_asset_mint: Pubkey,
        quote_asset_mint: Pubkey,
        ylp_mint: Pubkey,
        base_hlp_mint: Pubkey,
        quote_hlp_mint: Pubkey,
    ) -> Result<()> {
        require_keys_neq!(base_asset_mint, quote_asset_mint, ErrorCode::InvalidMint);
        require_keys_neq!(ylp_mint, base_asset_mint, ErrorCode::InvalidLpMintKey);
        require_keys_neq!(ylp_mint, quote_asset_mint, ErrorCode::InvalidLpMintKey);
        require_keys_neq!(base_hlp_mint, base_asset_mint, ErrorCode::InvalidLpMintKey);
        require_keys_neq!(base_hlp_mint, quote_asset_mint, ErrorCode::InvalidLpMintKey);
        require_keys_neq!(base_hlp_mint, ylp_mint, ErrorCode::InvalidLpMintKey);
        require_keys_neq!(quote_hlp_mint, base_asset_mint, ErrorCode::InvalidLpMintKey);
        require_keys_neq!(quote_hlp_mint, quote_asset_mint, ErrorCode::InvalidLpMintKey);
        require_keys_neq!(quote_hlp_mint, ylp_mint, ErrorCode::InvalidLpMintKey);
        require_keys_neq!(quote_hlp_mint, base_hlp_mint, ErrorCode::InvalidLpMintKey);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn initialize(
        &mut self,
        ylp_mint: Pubkey,
        base_side: MarketSide,
        quote_side: MarketSide,
        config: MarketConfig,
        base_hlp_ylp_vault: Pubkey,
        quote_hlp_ylp_vault: Pubkey,
        base_insurance_vault: Pubkey,
        quote_insurance_vault: Pubkey,
        params_hash: [u8; 32],
        initial_liquidity_authority: Pubkey,
        bootstrap_price_nad: u64,
        launch_fee_progress_offset: u16,
        current_slot: u64,
        bump: u8,
    ) -> Result<()> {
        config.validate()?;
        require!(
            launch_fee_progress_offset <= config.amm.launch_market_number_of_periods,
            ErrorCode::InvalidMarketConfig
        );
        Self::validate_mint_domain(
            base_side.asset_mint,
            quote_side.asset_mint,
            ylp_mint,
            base_side.hlp_mint,
            quote_side.hlp_mint,
        )?;
        self.version = MARKET_LAYOUT_VERSION;
        self.ylp_mint = ylp_mint;
        self.base_side = base_side;
        self.quote_side = quote_side;
        self.config = config;
        // Reserves are empty at market creation. The first balanced-liquidity
        // checkpoint initializes center, depth/share, and the applied parameters.
        self.amm = AmmState::default();
        self.amm.launch_reference_price_nad = bootstrap_price_nad;
        self.amm.launch_fee_progress_offset = launch_fee_progress_offset;
        self.debt = Debt {
            base_borrow_index_nad: NAD as u128,
            quote_borrow_index_nad: NAD as u128,
            base_rate_at_target_nad: INTEREST_INITIAL_RATE_AT_TARGET_NAD,
            quote_rate_at_target_nad: INTEREST_INITIAL_RATE_AT_TARGET_NAD,
            base_last_accrual_slot: current_slot,
            quote_last_accrual_slot: current_slot,
            ..Debt::default()
        };
        self.base_hlp_vault = {
            let mut vault = HlpVault::default();
            vault.initialize(base_hlp_ylp_vault);
            vault
        };
        self.quote_hlp_vault = {
            let mut vault = HlpVault::default();
            vault.initialize(quote_hlp_ylp_vault);
            vault
        };
        self.risk = Risk {
            last_snapshot_slot: current_slot,
            ..Risk::default()
        };
        self.insurance = Insurance {
            base_vault: base_insurance_vault,
            quote_vault: quote_insurance_vault,
            per_event_draw_bps: crate::constants::MAX_INSURANCE_DRAW_PER_EVENT_BPS,
            per_day_draw_bps: crate::constants::MAX_INSURANCE_DRAW_PER_DAY_BPS,
            ..Insurance::default()
        };
        self.params_hash = params_hash;
        self.initial_liquidity_authority = initial_liquidity_authority;
        self.governance_locked_ylp = 0;
        self.parameter_revisions = [0; 7];
        self.last_marginal_observation_nad = 0;
        self.curve_revision = 0;
        self.risk_revision = 0;
        self.last_update_slot = current_slot;
        self.reduce_only = false;
        self.bump = bump;
        Ok(())
    }

    pub(crate) fn require_initial_liquidity_authority(&self, signer: Pubkey) -> Result<()> {
        if self.base_side.shares.ylp_supply == 0 {
            require_keys_eq!(
                signer,
                self.initial_liquidity_authority,
                ErrorCode::InvalidInitialLiquidityAuthority
            );
        }
        Ok(())
    }

    pub fn assert_live_with_futarchy(&self, futarchy_authority: &FutarchyAuthority) -> Result<()> {
        self.assert_live_with_futarchy_at(futarchy_authority, Clock::get()?.unix_timestamp)
    }

    /// Allows ordinary yLP capital to be seeded before the configured trading
    /// start while preserving the same version and reduce-only guards used by
    /// live risk-increasing operations. Trading, borrowing, leverage, and hLP
    /// funding remain gated by `assert_live_with_futarchy_at`.
    pub(crate) fn assert_liquidity_seeding_available_with_futarchy(
        &self,
        futarchy_authority: &FutarchyAuthority,
    ) -> Result<()> {
        self.assert_current_version()?;
        require!(
            !futarchy_authority.is_reduce_only(self.reduce_only),
            ErrorCode::ReduceOnlyMode
        );
        Ok(())
    }

    pub(crate) fn assert_live_with_futarchy_at(
        &self,
        futarchy_authority: &FutarchyAuthority,
        unix_timestamp: i64,
    ) -> Result<()> {
        self.assert_started_at(unix_timestamp)?;
        require!(
            !futarchy_authority.is_reduce_only(self.reduce_only),
            ErrorCode::ReduceOnlyMode
        );
        Ok(())
    }

    pub fn assert_current_version(&self) -> Result<()> {
        require_eq!(self.version, MARKET_LAYOUT_VERSION, ErrorCode::InvalidVersion);
        Ok(())
    }

    pub fn assert_started(&self) -> Result<()> {
        self.assert_started_at(Clock::get()?.unix_timestamp)
    }

    pub(crate) fn assert_started_at(&self, unix_timestamp: i64) -> Result<()> {
        self.assert_current_version()?;
        require!(unix_timestamp >= self.config.start_time, ErrorCode::MarketNotStarted);
        Ok(())
    }

    /// Accrue borrow interest up to the current slot. Should be called before any
    /// debt-dependent computation in an instruction (borrow/repay, hedge,
    /// liquidation, yield claims, swaps, and liquidity changes).
    pub fn accrue_interest(&mut self) -> Result<()> {
        let current_slot = Clock::get()?.slot;
        self.accrue_interest_to_slot(current_slot)
    }

    pub fn update(&mut self) -> Result<()> {
        self.assert_current_version()?;
        let current_slot = Clock::get()?.slot;
        self.accrue_interest_to_slot(current_slot)?;
        if self.base_side.reserves.live_reserve > 0 && self.quote_side.reserves.live_reserve > 0 {
            // hLP exposure is checkpointed from actual state. New hLP entry
            // remains gated while a due concentrated controller target would
            // otherwise price the mint against a stale NAV basis.
            self.advance_amm_clock(current_slot)?;
            self.checkpoint_hlp_vaults()?;
            self.refresh_risk()?;
        }
        Ok(())
    }

    /// Advances debt, controller clocks, and hLP accounting for leverage
    /// margin changes without eagerly rebuilding risk that the transition will
    /// immediately invalidate. The transition records its final exact risk
    /// observation after the reserve/debt mutation.
    pub(crate) fn prepare_leverage_margin_operation(&mut self, current_slot: u64) -> Result<()> {
        self.assert_current_version()?;
        self.accrue_interest_to_slot(current_slot)?;
        if self.base_side.reserves.live_reserve > 0 && self.quote_side.reserves.live_reserve > 0 {
            self.advance_amm_clock(current_slot)?;
            self.checkpoint_hlp_vaults()?;
        }
        Ok(())
    }

    pub(crate) fn accrue_interest_to_slot(&mut self, current_slot: u64) -> Result<()> {
        accrue_side(self, MarketAsset::Base, current_slot)?;
        accrue_side(self, MarketAsset::Quote, current_slot)?;
        Ok(())
    }

    /// Validate one typed governance action without permitting unrelated
    /// configuration fields to move with it.
    pub fn validate_parameter_update(&self, update: &MarketParameterUpdate) -> Result<()> {
        match update {
            MarketParameterUpdate::Fee(profile) => {
                profile.validate()?;
                require!(
                    self.config.fee_profile() != *profile,
                    ErrorCode::ParameterUpdateNotMeaningful
                );
            }
            MarketParameterUpdate::Concentration {
                peak_amplification_nad,
                core_half_width_bps,
                fade_width_bps,
            } => {
                let target = ExplicitCurveParameters {
                    peak_amplification_nad: *peak_amplification_nad,
                    core_half_width_bps: *core_half_width_bps,
                    fade_width_bps: *fade_width_bps,
                };
                target.validate(MAX_AMM_AMPLIFICATION_NAD)?;
                require!(
                    self.config.amm.explicit_curve_parameters()? != target,
                    ErrorCode::ParameterUpdateNotMeaningful
                );
            }
            MarketParameterUpdate::Irm(irm) => {
                irm.validate()?;
                require!(self.config.irm != *irm, ErrorCode::ParameterUpdateNotMeaningful);
            }
            MarketParameterUpdate::EmaHalfLives {
                price_ms,
                directional_price_ms,
                curve_depth_ms,
                center_price_ms,
            } => {
                require!(
                    (MIN_HALF_LIFE_MS..=MAX_HALF_LIFE_MS).contains(price_ms)
                        && (MIN_HALF_LIFE_MS..=MAX_HALF_LIFE_MS).contains(directional_price_ms)
                        && (MIN_HALF_LIFE_MS..=MAX_HALF_LIFE_MS).contains(curve_depth_ms)
                        && (MIN_HALF_LIFE_MS..=MAX_HALF_LIFE_MS).contains(center_price_ms),
                    ErrorCode::InvalidHalfLife
                );
                require!(
                    self.config.ema_half_life_ms != *price_ms
                        || self.config.directional_ema_half_life_ms != *directional_price_ms
                        || self.config.curve_depth_ema_half_life_ms != *curve_depth_ms
                        || self.config.amm.center_ema_half_life_ms != *center_price_ms,
                    ErrorCode::ParameterUpdateNotMeaningful
                );
            }
            MarketParameterUpdate::DailyBorrowLimit { max_daily_borrow_bps } => {
                require!(
                    *max_daily_borrow_bps <= MAX_DAILY_BORROW_BPS,
                    ErrorCode::InvalidParameterUpdate
                );
                require!(
                    self.config.max_daily_borrow_bps != *max_daily_borrow_bps,
                    ErrorCode::ParameterUpdateNotMeaningful
                );
            }
            MarketParameterUpdate::CenterController {
                adjustment_threshold_nad,
                adjustment_step_nad,
                min_adjustment_interval_slots,
            } => {
                let mut next = self.config;
                next.amm.adjustment_threshold_nad = *adjustment_threshold_nad;
                next.amm.adjustment_step_nad = *adjustment_step_nad;
                next.amm.min_adjustment_interval_slots = *min_adjustment_interval_slots;
                next.validate()?;
                require!(next != self.config, ErrorCode::ParameterUpdateNotMeaningful);
            }
            MarketParameterUpdate::InsuranceDrawCaps {
                per_event_bps,
                per_day_bps,
            } => {
                require!(
                    *per_event_bps <= MAX_INSURANCE_DRAW_PER_EVENT_BPS
                        && *per_day_bps <= MAX_INSURANCE_DRAW_PER_DAY_BPS,
                    ErrorCode::InvalidParameterUpdate
                );
                require!(
                    self.insurance.per_event_draw_bps != *per_event_bps
                        || self.insurance.per_day_draw_bps != *per_day_bps,
                    ErrorCode::ParameterUpdateNotMeaningful
                );
            }
        }
        Ok(())
    }

    /// Checkpoint all elapsed state under the old parameters, apply exactly
    /// one typed family, enforce the point-in-time utilization guard, and then
    /// advance only that family's revision.
    pub fn execute_parameter_update(&mut self, update: &MarketParameterUpdate, current_slot: u64) -> Result<()> {
        self.assert_current_version()?;
        self.validate_parameter_update(update)?;
        let previous_config = self.config;
        let previous_base_side = self.base_side;
        let previous_quote_side = self.quote_side;
        let previous_amm = self.amm;
        let previous_debt = self.debt;
        let previous_risk = self.risk;
        let previous_insurance = self.insurance;
        let previous_revisions = self.parameter_revisions;
        let previous_last_marginal_observation_nad = self.last_marginal_observation_nad;
        let previous_curve_revision = self.curve_revision;
        let previous_risk_revision = self.risk_revision;
        let previous_last_update_slot = self.last_update_slot;

        let apply_result = (|| {
            // No elapsed interest, EMA decay, or risk integration may be
            // retroactively evaluated under the newly selected parameters.
            self.accrue_interest_to_slot(current_slot)?;
            if self.amm.initialized {
                self.amm
                    .observe_clock_from_validated_config(&previous_config.amm, current_slot)?;
            }
            self.refresh_risk_at_slot(current_slot)?;
            self.assert_parameter_execution_utilization()?;

            let family_index = update.family().code() as usize;
            match update {
                MarketParameterUpdate::Fee(profile) => {
                    self.config.apply_fee_profile(*profile)?;
                    if self.amm.initialized {
                        self.amm.invalidate_deferred_controller_target();
                    }
                }
                MarketParameterUpdate::Concentration {
                    peak_amplification_nad,
                    core_half_width_bps,
                    fade_width_bps,
                } => {
                    let mut next = self.config;
                    next.amm.set_explicit_curve_parameters(ExplicitCurveParameters {
                        peak_amplification_nad: *peak_amplification_nad,
                        core_half_width_bps: *core_half_width_bps,
                        fade_width_bps: *fade_width_bps,
                    })?;
                    next.validate()?;
                    self.config = next;
                    if self.amm.initialized {
                        self.apply_explicit_curve_parameter_update(current_slot)?;
                    }
                }
                MarketParameterUpdate::Irm(irm) => {
                    let mut next = self.config;
                    next.irm = *irm;
                    next.validate()?;
                    self.config = next;
                }
                MarketParameterUpdate::EmaHalfLives {
                    price_ms,
                    directional_price_ms,
                    curve_depth_ms,
                    center_price_ms,
                } => {
                    let mut next = self.config;
                    next.ema_half_life_ms = *price_ms;
                    next.directional_ema_half_life_ms = *directional_price_ms;
                    next.curve_depth_ema_half_life_ms = *curve_depth_ms;
                    next.amm.center_ema_half_life_ms = *center_price_ms;
                    next.validate()?;
                    self.config = next;
                }
                MarketParameterUpdate::DailyBorrowLimit { max_daily_borrow_bps } => {
                    // Close elapsed refill under the old governed rate. The
                    // newly selected rate applies only from this slot onward.
                    let old_limit_bps = self.config.max_daily_borrow_bps;
                    let base_limit = self.daily_limit_for_side(MarketAsset::Base, old_limit_bps)?;
                    let quote_limit = self.daily_limit_for_side(MarketAsset::Quote, old_limit_bps)?;
                    self.base_side
                        .daily_borrow_bucket
                        .decay_to_slot(base_limit, current_slot)?;
                    self.quote_side
                        .daily_borrow_bucket
                        .decay_to_slot(quote_limit, current_slot)?;
                    let mut next = self.config;
                    next.max_daily_borrow_bps = *max_daily_borrow_bps;
                    next.validate()?;
                    self.config = next;
                }
                MarketParameterUpdate::CenterController {
                    adjustment_threshold_nad,
                    adjustment_step_nad,
                    min_adjustment_interval_slots,
                } => {
                    let mut next = self.config;
                    next.amm.adjustment_threshold_nad = *adjustment_threshold_nad;
                    next.amm.adjustment_step_nad = *adjustment_step_nad;
                    next.amm.min_adjustment_interval_slots = *min_adjustment_interval_slots;
                    next.validate()?;
                    self.config = next;
                    if self.amm.initialized {
                        self.amm.invalidate_deferred_controller_target();
                    }
                }
                MarketParameterUpdate::InsuranceDrawCaps {
                    per_event_bps,
                    per_day_bps,
                } => {
                    // Open/checkpoint the current window under the old policy.
                    // Lowering a cap can therefore only reduce future capacity;
                    // it never retroactively restores already-spent allowance.
                    self.insurance.checkpoint_draw_window(MarketAsset::Base, current_slot);
                    self.insurance.checkpoint_draw_window(MarketAsset::Quote, current_slot);
                    self.insurance.per_event_draw_bps = *per_event_bps;
                    self.insurance.per_day_draw_bps = *per_day_bps;
                }
            }

            self.finalize_amm_transition(current_slot)?;
            self.refresh_risk_at_slot(current_slot)?;
            self.assert_market_health()?;
            self.parameter_revisions[family_index] = self.parameter_revisions[family_index]
                .checked_add(1)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            Ok(())
        })();

        if apply_result.is_err() {
            self.config = previous_config;
            self.base_side = previous_base_side;
            self.quote_side = previous_quote_side;
            self.amm = previous_amm;
            self.debt = previous_debt;
            self.risk = previous_risk;
            self.insurance = previous_insurance;
            self.parameter_revisions = previous_revisions;
            self.last_marginal_observation_nad = previous_last_marginal_observation_nad;
            self.curve_revision = previous_curve_revision;
            self.risk_revision = previous_risk_revision;
            self.last_update_slot = previous_last_update_slot;
        }
        apply_result
    }

    pub fn side(&self, market_asset: MarketAsset) -> &MarketSide {
        match market_asset {
            MarketAsset::Base => &self.base_side,
            MarketAsset::Quote => &self.quote_side,
        }
    }

    pub fn side_mut(&mut self, market_asset: MarketAsset) -> &mut MarketSide {
        match market_asset {
            MarketAsset::Base => &mut self.base_side,
            MarketAsset::Quote => &mut self.quote_side,
        }
    }

    pub fn asset_for_mint(&self, mint: Pubkey) -> Result<MarketAsset> {
        if mint == self.base_side.asset_mint {
            return Ok(MarketAsset::Base);
        }
        if mint == self.quote_side.asset_mint {
            return Ok(MarketAsset::Quote);
        }
        err!(ErrorCode::InvalidMint)
    }

    pub fn asset_for_hlp_mint(&self, mint: Pubkey) -> Result<MarketAsset> {
        if mint == self.base_side.hlp_mint {
            return Ok(MarketAsset::Base);
        }
        if mint == self.quote_side.hlp_mint {
            return Ok(MarketAsset::Quote);
        }
        err!(ErrorCode::InvalidLpMintKey)
    }

    pub fn swap_sides(&self, asset_in: MarketAsset) -> (&MarketSide, &MarketSide) {
        match asset_in {
            MarketAsset::Base => (&self.base_side, &self.quote_side),
            MarketAsset::Quote => (&self.quote_side, &self.base_side),
        }
    }

    pub fn swap_sides_mut(&mut self, asset_in: MarketAsset) -> (&mut MarketSide, &mut MarketSide) {
        match asset_in {
            MarketAsset::Base => (&mut self.base_side, &mut self.quote_side),
            MarketAsset::Quote => (&mut self.quote_side, &mut self.base_side),
        }
    }

    pub fn deposit_collateral(
        &mut self,
        borrow_position: &mut BorrowPosition,
        market_asset: MarketAsset,
        collateral_credit: u64,
    ) -> Result<CollateralReceipt> {
        require!(collateral_credit > 0, ErrorCode::AmountZero);
        let projected_collateral = borrow_position
            .collateral(market_asset)
            .checked_add(collateral_credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let debt_asset = market_asset.opposite();
        let projected_debt = match debt_asset {
            MarketAsset::Base => borrow_position.fixed_base_debt(&self.debt)?,
            MarketAsset::Quote => borrow_position.fixed_quote_debt(&self.debt)?,
        };
        let target_contribution =
            self.debt_capped_global_health_contribution(debt_asset, projected_debt, projected_collateral, &self.risk)?;

        match market_asset {
            MarketAsset::Base => borrow_position.base_collateral = projected_collateral,
            MarketAsset::Quote => borrow_position.quote_collateral = projected_collateral,
        }
        self.reconcile_global_health_contribution(borrow_position, debt_asset, target_contribution)?;
        self.reconcile_liquidation_auction(borrow_position)?;

        Ok(CollateralReceipt {
            collateral_credit,
            collateral_debit: 0,
            base_collateral: borrow_position.base_collateral,
            quote_collateral: borrow_position.quote_collateral,
            global_health_base_contribution_for_quote_debt: borrow_position
                .global_health_base_contribution_for_quote_debt,
            global_health_quote_contribution_for_base_debt: borrow_position
                .global_health_quote_contribution_for_base_debt,
            base_liquidation_cf_bps: borrow_position.base_liquidation_cf_bps,
            quote_liquidation_cf_bps: borrow_position.quote_liquidation_cf_bps,
        })
    }

    pub fn withdraw_collateral(
        &mut self,
        borrow_position: &mut BorrowPosition,
        market_asset: MarketAsset,
        collateral_debit: u64,
        min_liquidation_cf_bps: u16,
    ) -> Result<CollateralReceipt> {
        require!(collateral_debit > 0, ErrorCode::AmountZero);
        let projected_collateral = borrow_position
            .collateral(market_asset)
            .checked_sub(collateral_debit)
            .ok_or(ErrorCode::InsufficientBalance)?;
        let debt_asset = market_asset.opposite();
        let position_debt = match debt_asset {
            MarketAsset::Base => borrow_position.fixed_base_debt(&self.debt)?,
            MarketAsset::Quote => borrow_position.fixed_quote_debt(&self.debt)?,
        };
        let target_contribution =
            self.debt_capped_global_health_contribution(debt_asset, position_debt, projected_collateral, &self.risk)?;

        if position_debt > 0 {
            let total_debt_nad = self.total_fixed_debt_nad(debt_asset)?;
            let external_debt_nad = self.external_fixed_debt_nad(borrow_position, debt_asset)?;
            let projected_aggregate =
                self.projected_aggregate_global_health_contribution(borrow_position, debt_asset, target_contribution)?;
            let terms = self.dynamic_borrow_terms(
                debt_asset,
                projected_collateral,
                external_debt_nad,
                total_debt_nad,
                projected_aggregate,
                &self.risk,
            )?;
            // A third party cannot lower this position's already-issued terms.
            // The owner may withdraw whenever the post-withdraw position remains
            // inside its stored 5% buffered liquidation CF.
            let liquidation_cf_bps = borrow_position
                .liquidation_cf_bps(debt_asset)
                .max(terms.liquidation_cf_bps);
            let collateral_value_nad = self.collateral_value_nad(market_asset, projected_collateral, &self.risk)?;
            let max_debt_nad = collateral_value_nad
                .checked_mul(max_cf_bps_from_liquidation_cf(liquidation_cf_bps) as u128)
                .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
                .ok_or(ErrorCode::MarketMathOverflow)?;
            let max_debt = denormalize_from_nad_floor(max_debt_nad, self.side(market_asset.opposite()).asset_decimals)?;
            require_gte!(max_debt as u128, position_debt, ErrorCode::InsufficientMarketHealth);
            require_gte!(liquidation_cf_bps, min_liquidation_cf_bps, ErrorCode::SlippageExceeded);
            borrow_position.set_liquidation_cf_bps(debt_asset, liquidation_cf_bps);
        } else {
            borrow_position.set_liquidation_cf_bps(debt_asset, 0);
        }

        match market_asset {
            MarketAsset::Base => borrow_position.base_collateral = projected_collateral,
            MarketAsset::Quote => borrow_position.quote_collateral = projected_collateral,
        }
        self.reconcile_global_health_contribution(borrow_position, debt_asset, target_contribution)?;

        Ok(CollateralReceipt {
            collateral_credit: 0,
            collateral_debit,
            base_collateral: borrow_position.base_collateral,
            quote_collateral: borrow_position.quote_collateral,
            global_health_base_contribution_for_quote_debt: borrow_position
                .global_health_base_contribution_for_quote_debt,
            global_health_quote_contribution_for_base_debt: borrow_position
                .global_health_quote_contribution_for_base_debt,
            base_liquidation_cf_bps: borrow_position.base_liquidation_cf_bps,
            quote_liquidation_cf_bps: borrow_position.quote_liquidation_cf_bps,
        })
    }

    pub fn borrow(
        &mut self,
        borrow_position: &mut BorrowPosition,
        borrow_asset: MarketAsset,
        borrow_amount: u64,
        min_liquidation_cf_bps: u16,
        current_slot: u64,
    ) -> Result<DebtReceipt> {
        require!(borrow_amount > 0, ErrorCode::AmountZero);
        let debt_delta = i64::try_from(borrow_amount).map_err(|_| ErrorCode::Overflow)?;
        if self.risk.curve_depth_ema_nad == 0 {
            self.refresh_risk_at_slot(current_slot)?;
        }
        let risk = self.risk;
        let current_health = self.market_health_from_risk(&risk)?;
        self.assert_market_health_snapshot(&current_health)?;
        // The V1 curve prices debt already issued to other positions. Counting
        // this position's own debt here would make repeated draws worse than
        // opening equivalent split positions.
        let external_debt_nad = self.external_fixed_debt_nad(borrow_position, borrow_asset)?;
        let debt_shares = match borrow_asset {
            MarketAsset::Base => Debt::debt_to_shares(borrow_amount, self.debt.base_borrow_index_nad)?,
            MarketAsset::Quote => Debt::debt_to_shares(borrow_amount, self.debt.quote_borrow_index_nad)?,
        };
        let aggregate_debt_increase = self.debt.fixed_debt_increase_for_shares(borrow_asset, debt_shares)?;
        let (projected_position_debt, projected_total_debt) = match borrow_asset {
            MarketAsset::Base => (
                Debt::shares_to_debt(
                    borrow_position
                        .fixed_base_shares
                        .checked_add(debt_shares)
                        .ok_or(ErrorCode::MarketMathOverflow)?,
                    self.debt.base_borrow_index_nad,
                )?,
                Debt::shares_to_debt(
                    self.debt
                        .fixed_base_shares
                        .checked_add(debt_shares)
                        .ok_or(ErrorCode::MarketMathOverflow)?,
                    self.debt.base_borrow_index_nad,
                )?,
            ),
            MarketAsset::Quote => (
                Debt::shares_to_debt(
                    borrow_position
                        .fixed_quote_shares
                        .checked_add(debt_shares)
                        .ok_or(ErrorCode::MarketMathOverflow)?,
                    self.debt.quote_borrow_index_nad,
                )?,
                Debt::shares_to_debt(
                    self.debt
                        .fixed_quote_shares
                        .checked_add(debt_shares)
                        .ok_or(ErrorCode::MarketMathOverflow)?,
                    self.debt.quote_borrow_index_nad,
                )?,
            ),
        };
        let collateral_asset = borrow_asset.opposite();
        let collateral_amount = borrow_position.collateral(collateral_asset);
        let target_contribution = self.debt_capped_global_health_contribution(
            borrow_asset,
            projected_position_debt,
            collateral_amount,
            &risk,
        )?;
        let projected_aggregate =
            self.projected_aggregate_global_health_contribution(borrow_position, borrow_asset, target_contribution)?;
        let projected_total_debt_nad = normalize_to_nad(projected_total_debt, self.side(borrow_asset).asset_decimals)?;
        let terms = self.dynamic_borrow_terms(
            borrow_asset,
            collateral_amount,
            external_debt_nad,
            projected_total_debt_nad,
            projected_aggregate,
            &risk,
        )?;
        require_gte!(
            terms.max_debt as u128,
            projected_position_debt,
            ErrorCode::InsufficientMarketHealth
        );
        require_gte!(
            terms.liquidation_cf_bps,
            min_liquidation_cf_bps,
            ErrorCode::SlippageExceeded
        );
        require_gte!(
            terms.projected_market_health_bps,
            self.config.borrow_market_health_floor_bps as u64,
            ErrorCode::InsufficientMarketHealth
        );
        require_gte!(
            self.side(borrow_asset).reserves.cash_reserve,
            borrow_amount,
            ErrorCode::InsufficientBorrowHeadroom
        );
        let daily_borrow_limit = self.daily_limit_for_side(borrow_asset, self.config.max_daily_borrow_bps)?;
        self.side_mut(borrow_asset).daily_borrow_bucket.record_borrow(
            borrow_amount,
            daily_borrow_limit,
            current_slot,
        )?;
        let debt_side = self.side_mut(borrow_asset);
        debt_side.reserves.cash_reserve = debt_side
            .reserves
            .cash_reserve
            .checked_sub(borrow_amount)
            .ok_or(ErrorCode::CashReserveUnderflow)?;
        if aggregate_debt_increase > borrow_amount {
            debt_side.reserves.live_reserve = debt_side
                .reserves
                .live_reserve
                .checked_add(aggregate_debt_increase - borrow_amount)
                .ok_or(ErrorCode::ReserveOverflow)?;
        } else if aggregate_debt_increase < borrow_amount {
            debt_side.reserves.live_reserve = debt_side
                .reserves
                .live_reserve
                .checked_sub(borrow_amount - aggregate_debt_increase)
                .ok_or(ErrorCode::ReserveUnderflow)?;
        }

        match borrow_asset {
            MarketAsset::Base => {
                borrow_position.fixed_base_shares = borrow_position
                    .fixed_base_shares
                    .checked_add(debt_shares)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                self.debt.fixed_base_shares = self
                    .debt
                    .fixed_base_shares
                    .checked_add(debt_shares)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
            }
            MarketAsset::Quote => {
                borrow_position.fixed_quote_shares = borrow_position
                    .fixed_quote_shares
                    .checked_add(debt_shares)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                self.debt.fixed_quote_shares = self
                    .debt
                    .fixed_quote_shares
                    .checked_add(debt_shares)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
            }
        }
        self.debt.add_margin_principal(borrow_asset, borrow_amount)?;
        self.reconcile_global_health_contribution(borrow_position, borrow_asset, target_contribution)?;
        borrow_position.set_liquidation_cf_bps(borrow_asset, terms.liquidation_cf_bps);
        let market_health = self.market_health()?;
        DebtReceipt::from_market(self, borrow_position, debt_delta, 0, 0, &market_health)
    }

    pub(crate) fn projected_aggregate_global_health_contribution(
        &self,
        borrow_position: &BorrowPosition,
        debt_asset: MarketAsset,
        target_contribution: u64,
    ) -> Result<u64> {
        let (position_contribution, aggregate_contribution) = match debt_asset {
            MarketAsset::Base => (
                borrow_position.global_health_quote_contribution_for_base_debt,
                self.debt.global_health_quote_contribution_for_base_debt,
            ),
            MarketAsset::Quote => (
                borrow_position.global_health_base_contribution_for_quote_debt,
                self.debt.global_health_base_contribution_for_quote_debt,
            ),
        };
        aggregate_contribution
            .checked_sub(position_contribution)
            .and_then(|value| value.checked_add(target_contribution))
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    }

    pub(crate) fn reconcile_global_health_contribution(
        &mut self,
        borrow_position: &mut BorrowPosition,
        debt_asset: MarketAsset,
        target_contribution: u64,
    ) -> Result<()> {
        match debt_asset {
            MarketAsset::Base => reconcile_global_health_contribution(
                &mut borrow_position.global_health_quote_contribution_for_base_debt,
                &mut self.debt.global_health_quote_contribution_for_base_debt,
                target_contribution,
            ),
            MarketAsset::Quote => reconcile_global_health_contribution(
                &mut borrow_position.global_health_base_contribution_for_quote_debt,
                &mut self.debt.global_health_base_contribution_for_quote_debt,
                target_contribution,
            ),
        }
    }

    pub fn repay(
        &mut self,
        borrow_position: &mut BorrowPosition,
        repay_asset: MarketAsset,
        repay_credit: u64,
    ) -> Result<DebtReceipt> {
        let repayment = self.fixed_repayment_for_max(borrow_position, repay_asset, repay_credit)?;
        // Instruction handlers preview this amount before moving tokens. Keep
        // the state boundary exact so no transferred atom can become an
        // unaccounted donation if state changed unexpectedly.
        require_eq!(repayment.cash_repaid, repay_credit, ErrorCode::BrokenInvariant);
        let (interest_paid, debt_reduction) = match repay_asset {
            MarketAsset::Base => {
                let shares_to_burn = repayment.shares_to_burn;
                let debt_reduction = repayment.position_debt_reduced;
                let aggregate_debt_reduction =
                    self.debt.fixed_debt_reduction_for_shares(repay_asset, shares_to_burn)?;
                let interest_paid =
                    self.debt
                        .realize_margin_liquidation(repay_asset, repay_credit, aggregate_debt_reduction)?;
                let principal_credit = repay_credit
                    .checked_sub(interest_paid)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                let live_debit = aggregate_debt_reduction
                    .checked_sub(principal_credit)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                borrow_position.fixed_base_shares = borrow_position
                    .fixed_base_shares
                    .checked_sub(shares_to_burn)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                self.debt.fixed_base_shares = self
                    .debt
                    .fixed_base_shares
                    .checked_sub(shares_to_burn)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                self.base_side.reserves.live_reserve = self
                    .base_side
                    .reserves
                    .live_reserve
                    .checked_sub(live_debit)
                    .ok_or(ErrorCode::ReserveUnderflow)?;
                self.base_side.reserves.cash_reserve = self
                    .base_side
                    .reserves
                    .cash_reserve
                    .checked_add(principal_credit)
                    .ok_or(ErrorCode::ReserveOverflow)?;
                (interest_paid, debt_reduction)
            }
            MarketAsset::Quote => {
                let shares_to_burn = repayment.shares_to_burn;
                let debt_reduction = repayment.position_debt_reduced;
                let aggregate_debt_reduction =
                    self.debt.fixed_debt_reduction_for_shares(repay_asset, shares_to_burn)?;
                let interest_paid =
                    self.debt
                        .realize_margin_liquidation(repay_asset, repay_credit, aggregate_debt_reduction)?;
                let principal_credit = repay_credit
                    .checked_sub(interest_paid)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                let live_debit = aggregate_debt_reduction
                    .checked_sub(principal_credit)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                borrow_position.fixed_quote_shares = borrow_position
                    .fixed_quote_shares
                    .checked_sub(shares_to_burn)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                self.debt.fixed_quote_shares = self
                    .debt
                    .fixed_quote_shares
                    .checked_sub(shares_to_burn)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                self.quote_side.reserves.live_reserve = self
                    .quote_side
                    .reserves
                    .live_reserve
                    .checked_sub(live_debit)
                    .ok_or(ErrorCode::ReserveUnderflow)?;
                self.quote_side.reserves.cash_reserve = self
                    .quote_side
                    .reserves
                    .cash_reserve
                    .checked_add(principal_credit)
                    .ok_or(ErrorCode::ReserveOverflow)?;
                (interest_paid, debt_reduction)
            }
        };
        let debt_delta = -i64::try_from(debt_reduction).map_err(|_| ErrorCode::Overflow)?;
        self.refresh_risk()?;
        let debt_after = match repay_asset {
            MarketAsset::Base => borrow_position.fixed_base_debt(&self.debt)?,
            MarketAsset::Quote => borrow_position.fixed_quote_debt(&self.debt)?,
        };
        let target_contribution = self.debt_capped_global_health_contribution(
            repay_asset,
            debt_after,
            borrow_position.collateral(repay_asset.opposite()),
            &self.risk,
        )?;
        self.reconcile_global_health_contribution(borrow_position, repay_asset, target_contribution)?;
        if debt_after == 0 {
            borrow_position.set_liquidation_cf_bps(repay_asset, 0);
            borrow_position.clear_referral_binding(repay_asset);
        }
        self.reconcile_liquidation_auction(borrow_position)?;
        let market_health = self.market_health()?;
        DebtReceipt::from_market(
            self,
            borrow_position,
            debt_delta,
            repayment.cash_repaid,
            interest_paid,
            &market_health,
        )
    }

    pub fn fixed_repayment_for_max(
        &self,
        borrow_position: &BorrowPosition,
        repay_asset: MarketAsset,
        max_repay_amount: u64,
    ) -> Result<DebtRepaymentQuote> {
        let (position_shares, aggregate_shares, borrow_index_nad) = match repay_asset {
            MarketAsset::Base => (
                borrow_position.fixed_base_shares,
                self.debt.fixed_base_shares,
                self.debt.base_borrow_index_nad,
            ),
            MarketAsset::Quote => (
                borrow_position.fixed_quote_shares,
                self.debt.fixed_quote_shares,
                self.debt.quote_borrow_index_nad,
            ),
        };
        Debt::repayment_for_max(position_shares, aggregate_shares, borrow_index_nad, max_repay_amount)
    }

    pub fn add_liquidity(
        &mut self,
        max_base_reserve_credit: u64,
        max_quote_reserve_credit: u64,
    ) -> Result<AddLiquidityReceipt> {
        let receipt = self.preview_add_liquidity(max_base_reserve_credit, max_quote_reserve_credit)?;
        let supply_before = self.base_side.shares.ylp_supply;
        let internal_mint_amount = receipt
            .ylp_supply
            .checked_sub(supply_before)
            .ok_or(ErrorCode::SupplyUnderflow)?;
        let seeded_price_nad = if supply_before == 0 {
            let base_nad = normalize_to_nad(u128::from(receipt.base_reserve_credit), self.base_side.asset_decimals)?;
            let quote_nad = normalize_to_nad(u128::from(receipt.quote_reserve_credit), self.quote_side.asset_decimals)?;
            let price = quote_nad
                .checked_mul(u128::from(NAD))
                .and_then(|value| value.checked_div(base_nad))
                .ok_or(ErrorCode::MarketMathOverflow)?;
            let price = u64::try_from(price).map_err(|_| ErrorCode::MarketMathOverflow)?;
            require!(price > 0, ErrorCode::InvalidSettlementPrice);
            if self.amm.launch_reference_price_nad > 0 {
                require_eq!(
                    price,
                    self.amm.launch_reference_price_nad,
                    ErrorCode::InvalidSettlementPrice
                );
            }
            Some(price)
        } else {
            None
        };

        self.base_side.credit_reserve(receipt.base_reserve_credit, true)?;
        self.quote_side.credit_reserve(receipt.quote_reserve_credit, true)?;
        self.base_side.shares.mint(internal_mint_amount)?;
        self.quote_side.shares.mint(internal_mint_amount)?;
        self.base_side.assert_share_backing()?;
        self.quote_side.assert_share_backing()?;

        if let Some(seeded_price_nad) = seeded_price_nad {
            if self.amm.launch_reference_price_nad == 0 {
                self.amm.launch_reference_price_nad = seeded_price_nad;
            }
            self.initial_liquidity_authority = Pubkey::default();
        }

        Ok(receipt)
    }

    pub fn preview_add_liquidity(
        &self,
        max_base_reserve_credit: u64,
        max_quote_reserve_credit: u64,
    ) -> Result<AddLiquidityReceipt> {
        require!(
            max_base_reserve_credit > 0 && max_quote_reserve_credit > 0,
            ErrorCode::AmountZero
        );
        let base_reserve_before = self.base_side.reserves.live_reserve;
        let quote_reserve_before = self.quote_side.reserves.live_reserve;
        if base_reserve_before > 0 || quote_reserve_before > 0 {
            require!(
                base_reserve_before > 0 && quote_reserve_before > 0,
                ErrorCode::InsufficientLiquidity
            );
        }

        let ylp_amount = self.ylp_for_deposit(
            base_reserve_before,
            quote_reserve_before,
            max_base_reserve_credit,
            max_quote_reserve_credit,
        )?;
        require!(ylp_amount > 0, ErrorCode::SlippageExceeded);

        let (base_reserve_credit, quote_reserve_credit) = if self.base_side.shares.ylp_supply == 0 {
            (max_base_reserve_credit, max_quote_reserve_credit)
        } else {
            let supply_before = self.base_side.shares.ylp_supply;
            let base_reserve_credit = reserve_for_ylp_mint_ceil(base_reserve_before, supply_before, ylp_amount)?;
            let quote_reserve_credit = reserve_for_ylp_mint_ceil(quote_reserve_before, supply_before, ylp_amount)?;
            require_gte!(
                max_base_reserve_credit,
                base_reserve_credit,
                ErrorCode::SlippageExceeded
            );
            require_gte!(
                max_quote_reserve_credit,
                quote_reserve_credit,
                ErrorCode::SlippageExceeded
            );
            (base_reserve_credit, quote_reserve_credit)
        };
        require!(
            base_reserve_credit > 0 && quote_reserve_credit > 0,
            ErrorCode::AmountZero
        );

        let internal_mint_amount = if self.base_side.shares.ylp_supply == 0 {
            ylp_amount.checked_add(MIN_LIQUIDITY).ok_or(ErrorCode::SupplyOverflow)?
        } else {
            ylp_amount
        };
        let ylp_supply = self
            .base_side
            .shares
            .ylp_supply
            .checked_add(internal_mint_amount)
            .ok_or(ErrorCode::SupplyOverflow)?;

        Ok(AddLiquidityReceipt {
            base_reserve_credit,
            quote_reserve_credit,
            ylp_amount,
            ylp_supply,
        })
    }

    pub fn remove_liquidity(&mut self, ylp_amount: u64) -> Result<RemoveLiquidityReceipt> {
        require!(ylp_amount > 0, ErrorCode::AmountZero);
        require_eq!(
            self.base_side.shares.ylp_supply,
            self.quote_side.shares.ylp_supply,
            ErrorCode::BrokenInvariant
        );

        let base_amount_out = self
            .base_side
            .shares
            .reserve_for_burn(self.base_side.reserves.live_reserve, ylp_amount)?;
        let quote_amount_out = self
            .quote_side
            .shares
            .reserve_for_burn(self.quote_side.reserves.live_reserve, ylp_amount)?;
        require_gte!(
            self.base_side.reserves.cash_reserve,
            base_amount_out,
            ErrorCode::InsufficientLiquidity
        );
        require_gte!(
            self.quote_side.reserves.cash_reserve,
            quote_amount_out,
            ErrorCode::InsufficientLiquidity
        );

        self.base_side.debit_reserve(base_amount_out, true)?;
        self.quote_side.debit_reserve(quote_amount_out, true)?;
        self.base_side.shares.burn(ylp_amount)?;
        self.quote_side.shares.burn(ylp_amount)?;
        self.base_side.assert_share_backing()?;
        self.quote_side.assert_share_backing()?;

        // The instruction checkpoints the departing holder before this burn.
        // Once only the permanently burned minimum remains outside the two
        // hLP vaults, discard source-specific funding dust so a later yLP
        // cohort cannot inherit a fraction left by the prior cohort.
        let hlp_ylp_shares = self
            .base_hlp_vault
            .ylp_shares
            .checked_add(self.quote_hlp_vault.ylp_shares)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let non_hlp_ylp_supply = self
            .base_side
            .shares
            .ylp_supply
            .checked_sub(hlp_ylp_shares)
            .ok_or(ErrorCode::BrokenInvariant)?;
        if non_hlp_ylp_supply == MIN_LIQUIDITY {
            self.base_side.fees.hlp_funding_interest_growth_remainder_scaled = 0;
            self.quote_side.fees.hlp_funding_interest_growth_remainder_scaled = 0;
        }

        Ok(RemoveLiquidityReceipt {
            ylp_amount,
            base_amount_out,
            quote_amount_out,
            ylp_supply: self.base_side.shares.ylp_supply,
        })
    }

    pub(crate) fn ylp_for_deposit(
        &self,
        base_reserve_before: u64,
        quote_reserve_before: u64,
        base_amount: u64,
        quote_amount: u64,
    ) -> Result<u64> {
        require_eq!(
            self.base_side.shares.ylp_supply,
            self.quote_side.shares.ylp_supply,
            ErrorCode::BrokenInvariant
        );
        if self.base_side.shares.ylp_supply == 0 {
            // sqrt(amount0_in * amount1_in) - MINIMUM_LIQUIDITY
            // MINIMUM_LIQUIDITY = 1000
            // 9 decimals: 1000 / 10^9 = 1e-6 full LP tokens
            // 1000 units are burned permanently.
            // This burn (~1e-6 of supply) is larger than Uniswap V2's 1e-15 burn (with 18 decimals),
            // but still negligible for users and significantly raises the cost of share inflation attacks.
            return (base_amount as u128)
                .checked_mul(quote_amount as u128)
                .ok_or(ErrorCode::LiquidityMathOverflow)?
                .sqrt()
                .ok_or(ErrorCode::LiquiditySqrtOverflow)?
                .checked_sub(MIN_LIQUIDITY as u128)
                .ok_or(ErrorCode::LiquidityUnderflow)?
                .try_into()
                .map_err(|_| ErrorCode::LiquidityConversionOverflow.into());
        }
        let base_ylp = self
            .base_side
            .shares
            .shares_for_deposit(base_reserve_before, base_amount)?;
        let quote_ylp = self
            .quote_side
            .shares
            .shares_for_deposit(quote_reserve_before, quote_amount)?;
        Ok(base_ylp.min(quote_ylp))
    }

    pub fn swap_reserves(
        &mut self,
        asset_in: MarketAsset,
        amount_in_after_fee: u64,
        amount_out: u64,
        fee_credit: u64,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
    ) -> Result<SwapReceipt> {
        self.swap_reserves_with_fee_supply(
            asset_in,
            amount_in_after_fee,
            amount_out,
            fee_credit,
            protocol_fee_bps,
            protocol_auction_split,
            None,
        )
    }

    pub fn swap_reserves_with_fee_supply(
        &mut self,
        asset_in: MarketAsset,
        amount_in_after_fee: u64,
        amount_out: u64,
        fee_credit: u64,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        fee_eligible_ylp_supply: Option<u64>,
    ) -> Result<SwapReceipt> {
        let (market_side_in, market_side_out) = self.swap_sides_mut(asset_in);
        require_gte!(
            market_side_out.reserves.cash_reserve,
            amount_out,
            ErrorCode::InsufficientLiquidity
        );

        market_side_in.reserves.live_reserve = market_side_in
            .reserves
            .live_reserve
            .checked_add(amount_in_after_fee)
            .ok_or(ErrorCode::ReserveOverflow)?;
        market_side_in.reserves.cash_reserve = market_side_in
            .reserves
            .cash_reserve
            .checked_add(amount_in_after_fee)
            .ok_or(ErrorCode::ReserveOverflow)?;
        market_side_out.reserves.live_reserve = market_side_out
            .reserves
            .live_reserve
            .checked_sub(amount_out)
            .ok_or(ErrorCode::ReserveUnderflow)?;
        market_side_out.reserves.cash_reserve = market_side_out
            .reserves
            .cash_reserve
            .checked_sub(amount_out)
            .ok_or(ErrorCode::CashReserveUnderflow)?;

        let fees = match fee_eligible_ylp_supply {
            Some(supply) => market_side_in.record_swap_fee_credit_with_supply(
                fee_credit,
                protocol_fee_bps,
                protocol_auction_split,
                supply,
            )?,
            None => market_side_in.record_swap_fee_credit(fee_credit, protocol_fee_bps, protocol_auction_split)?,
        };
        market_side_in.assert_share_backing()?;
        market_side_out.assert_share_backing()?;
        market_side_in.fees.assert_backed()?;

        Ok(SwapReceipt {
            amount_in_after_fee,
            reserve_input_credit: amount_in_after_fee,
            amount_out,
            fee_credit,
            base_fee_credit: fee_credit,
            distributed_surcharge_credit: 0,
            fee_breakdown: SwapFeeBreakdown::default(),
            reserve_in_live_reserve: market_side_in.reserves.live_reserve,
            reserve_out_live_reserve: market_side_out.reserves.live_reserve,
            fees,
        })
    }

    pub fn assert_market_invariants(&self) -> Result<()> {
        self.base_side.assert_share_backing()?;
        self.quote_side.assert_share_backing()?;
        self.base_side.fees.assert_backed()?;
        self.quote_side.fees.assert_backed()?;
        if self.base_hlp_vault.hlp_supply == 0 {
            require_eq!(
                self.base_side.reserves.base_hlp_backing_inventory,
                0,
                ErrorCode::BrokenInvariant
            );
            require_eq!(
                self.quote_side.reserves.base_hlp_backing_inventory,
                0,
                ErrorCode::BrokenInvariant
            );
        }
        if self.quote_hlp_vault.hlp_supply == 0 {
            require_eq!(
                self.base_side.reserves.quote_hlp_backing_inventory,
                0,
                ErrorCode::BrokenInvariant
            );
            require_eq!(
                self.quote_side.reserves.quote_hlp_backing_inventory,
                0,
                ErrorCode::BrokenInvariant
            );
        }
        self.assert_virtual_reserve_invariant(MarketAsset::Base)?;
        self.assert_virtual_reserve_invariant(MarketAsset::Quote)?;
        Ok(())
    }

    pub fn assert_virtual_reserve_invariant(&self, asset: MarketAsset) -> Result<()> {
        let (side, cash_backed_debt) = match asset {
            MarketAsset::Base => (
                &self.base_side,
                total_cash_backed_borrowed(self, asset, self.debt.base_borrow_index_nad)?,
            ),
            MarketAsset::Quote => (
                &self.quote_side,
                total_cash_backed_borrowed(self, asset, self.debt.quote_borrow_index_nad)?,
            ),
        };
        let hlp_live = self.hlp_live_reserve(asset)?;
        // Invariants:
        // 1. x_virtual * y_virtual = k (Constant product invariant)
        // 2. r_virtual >= r_cash_backed_debt (Solvency invariant)
        // with a state transition:
        // ΔR_virtual = ΔR_cash + ΔR_cash_backed_debt + ΔR_hlp_live.
        // hLP funding debt is priced through utilization and hLP NAV, but it is
        // not same-side cash-backed reserve debt.
        let expected_live_reserve = (side.reserves.cash_reserve as u128)
            .checked_add(cash_backed_debt)
            .and_then(|value| value.checked_add(hlp_live))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require_eq!(
            side.reserves.live_reserve as u128,
            expected_live_reserve,
            ErrorCode::BrokenInvariant
        );
        Ok(())
    }

    pub fn hlp_live_reserve(&self, asset: MarketAsset) -> Result<u128> {
        (self.base_hlp_vault.hlp_live_reserve(asset) as u128)
            .checked_add(self.quote_hlp_vault.hlp_live_reserve(asset) as u128)
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    }

    /// Indexed aggregate hLP funding debt denominated in `asset`. Base debt is
    /// carried by the quote hLP vault and quote debt by the base hLP vault.
    pub fn hlp_funding_debt(&self, asset: MarketAsset) -> Result<u128> {
        match asset {
            MarketAsset::Base => {
                Debt::shares_to_debt(self.quote_hlp_vault.debt_shares, self.debt.base_borrow_index_nad)
            }
            MarketAsset::Quote => {
                Debt::shares_to_debt(self.base_hlp_vault.debt_shares, self.debt.quote_borrow_index_nad)
            }
        }
    }

    /// Admission-only capacity for new hLP funding. Existing hLP debt does not
    /// reserve cash from ordinary withdrawals, borrows, or liquidations.
    pub fn hlp_funding_headroom(&self, asset: MarketAsset) -> Result<u64> {
        let (debt_shares, borrow_index_nad) = match asset {
            MarketAsset::Base => (self.quote_hlp_vault.debt_shares, self.debt.base_borrow_index_nad),
            MarketAsset::Quote => (self.base_hlp_vault.debt_shares, self.debt.quote_borrow_index_nad),
        };
        let cash = self.side(asset).reserves.cash_reserve as u128;
        // Debt conversion floors total shares. Solve the largest total share
        // count whose converted debt remains <= cash; subtracting floored raw
        // debt would overstate capacity when the next borrow rounds shares up.
        let max_total_shares = mul_div_ceil_u128(
            cash.checked_add(1).ok_or(ErrorCode::MarketMathOverflow)?,
            NAD as u128,
            borrow_index_nad,
        )?
        .checked_sub(1)
        .ok_or(ErrorCode::MarketMathOverflow)?;
        let available_shares = max_total_shares.saturating_sub(debt_shares);
        let headroom = mul_div_u128(available_shares, borrow_index_nad, NAD as u128)?;
        u64::try_from(headroom).map_err(|_| ErrorCode::MarketMathOverflow.into())
    }

    /// Point-in-time lending utilization used by both the IRM and parameter
    /// execution guard. Funding debt belongs to the side whose token was
    /// borrowed, so it is stored on the opposite hLP aggregate vault.
    pub fn lending_utilization_bps(&self, asset: MarketAsset) -> Result<u64> {
        let fixed_debt = match asset {
            MarketAsset::Base => self.debt.fixed_base_debt()?,
            MarketAsset::Quote => self.debt.fixed_quote_debt()?,
        };
        let isolated_debt = self.debt.isolated_debt(asset)?;
        let hlp_funding_debt = self.hlp_funding_debt(asset)?;
        let total_debt = fixed_debt
            .checked_add(isolated_debt)
            .and_then(|value| value.checked_add(hlp_funding_debt))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        utilization_bps(total_debt, self.side(asset).reserves.cash_reserve as u128)
    }

    pub fn assert_parameter_execution_utilization(&self) -> Result<()> {
        require!(
            self.lending_utilization_bps(MarketAsset::Base)? < PARAMETER_EXECUTION_MAX_UTILIZATION_BPS
                && self.lending_utilization_bps(MarketAsset::Quote)? < PARAMETER_EXECUTION_MAX_UTILIZATION_BPS,
            ErrorCode::UtilizationGuardExceeded
        );
        Ok(())
    }

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

fn accrue_side(market: &mut Market, asset: MarketAsset, current_slot: u64) -> Result<()> {
    let (index, rate_at_target, last_accrual_slot, fixed_shares, isolated_shares) = match asset {
        MarketAsset::Base => (
            market.debt.base_borrow_index_nad,
            market.debt.base_rate_at_target_nad,
            market.debt.base_last_accrual_slot,
            market.debt.fixed_base_shares,
            market.debt.isolated_base_shares,
        ),
        MarketAsset::Quote => (
            market.debt.quote_borrow_index_nad,
            market.debt.quote_rate_at_target_nad,
            market.debt.quote_last_accrual_slot,
            market.debt.fixed_quote_shares,
            market.debt.isolated_quote_shares,
        ),
    };
    if current_slot <= last_accrual_slot {
        return Ok(());
    }
    let dt_ms = current_slot
        .checked_sub(last_accrual_slot)
        .ok_or(ErrorCode::MarketMathOverflow)?
        .saturating_mul(TARGET_MS_PER_SLOT);

    let hlp_shares = match asset {
        MarketAsset::Base => market.quote_hlp_vault.debt_shares,
        MarketAsset::Quote => market.base_hlp_vault.debt_shares,
    };
    if fixed_shares == 0 && isolated_shares == 0 && hlp_shares == 0 {
        let next_rate_at_target = adapt_rate_at_target_nad(
            rate_at_target,
            -(NAD as i128),
            dt_ms,
            market.config.irm.adjustment_speed_per_year as u128,
            INTEREST_MIN_RATE_AT_TARGET_NAD,
            INTEREST_MAX_RATE_AT_TARGET_NAD,
            INTEREST_MAX_ADAPTATION_STEP_NAD,
        )?;
        match asset {
            MarketAsset::Base => {
                market.debt.base_rate_at_target_nad = next_rate_at_target;
                market.debt.base_last_accrual_slot = current_slot;
            }
            MarketAsset::Quote => {
                market.debt.quote_rate_at_target_nad = next_rate_at_target;
                market.debt.quote_last_accrual_slot = current_slot;
            }
        }
        return Ok(());
    }
    let (cash, live) = match asset {
        MarketAsset::Base => (
            market.base_side.reserves.cash_reserve as u128,
            market.base_side.reserves.live_reserve as u128,
        ),
        MarketAsset::Quote => (
            market.quote_side.reserves.cash_reserve as u128,
            market.quote_side.reserves.live_reserve as u128,
        ),
    };
    let hlp_live = market.hlp_live_reserve(asset)?;
    let cash_backed_before = live
        .checked_sub(cash)
        .and_then(|value| value.checked_sub(hlp_live))
        .ok_or(ErrorCode::BrokenInvariant)?;
    if fixed_shares == 0 && isolated_shares == 0 {
        require_eq!(cash_backed_before, 0, ErrorCode::BrokenInvariant);
    }
    let hlp_debt_before = if hlp_shares == 0 {
        0
    } else {
        Debt::shares_to_debt(hlp_shares, index)?
    };

    // Calculate utilization rates. hLP funding debt counts toward funding cost,
    // but only cash-backed debt accrual grows virtual reserves.
    let debt_before = cash_backed_before
        .checked_add(hlp_debt_before)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let util = utilization_bps(debt_before, cash)?;
    let error = utilization_error_nad(util, market.config.irm.target_utilization_bps as u64)?;
    let rate = instantaneous_rate_apr_nad(rate_at_target, error, market.config.irm.curve_steepness_nad as u128)?;
    if hlp_shares > 0 {
        let vault = match asset {
            // Quote hLP borrows Base; Base hLP borrows Quote.
            MarketAsset::Base => &mut market.quote_hlp_vault,
            MarketAsset::Quote => &mut market.base_hlp_vault,
        };
        vault.funding_apr_ema_nad = crate::math::risk::ema_u128_including_zero(
            vault.funding_apr_ema_nad,
            rate,
            vault.funding_apr_ema_last_slot,
            current_slot,
            HLP_FUNDING_APR_EMA_HALF_LIFE_MS,
        );
        vault.funding_apr_ema_last_slot = current_slot;
    }
    let next_index = if index == 0 || dt_ms == 0 || rate == 0 {
        index
    } else {
        let elapsed_ms = dt_ms.min(MAX_INTEREST_ACCRUAL_MS) as u128;
        let growth_nad = rate
            .checked_mul(elapsed_ms)
            .and_then(|value| value.checked_div(MS_PER_YEAR as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        if growth_nad == 0 {
            index
        } else {
            let delta = index
                .checked_mul(growth_nad)
                .and_then(|value| value.checked_div(NAD as u128))
                .ok_or(ErrorCode::MarketMathOverflow)?;
            index.checked_add(delta).ok_or(ErrorCode::MarketMathOverflow)?
        }
    };
    let next_rate_at_target = adapt_rate_at_target_nad(
        rate_at_target,
        error,
        dt_ms,
        market.config.irm.adjustment_speed_per_year as u128,
        INTEREST_MIN_RATE_AT_TARGET_NAD,
        INTEREST_MAX_RATE_AT_TARGET_NAD,
        INTEREST_MAX_ADAPTATION_STEP_NAD,
    )?;
    // Fixed and isolated buckets remain separately floored. Combined
    // conversion would manufacture an atom at some index boundaries.
    let fixed_after = if fixed_shares == 0 {
        0
    } else {
        Debt::shares_to_debt(fixed_shares, next_index)?
    };
    let isolated_after = if isolated_shares == 0 {
        0
    } else {
        Debt::shares_to_debt(isolated_shares, next_index)?
    };
    let accrued_interest = fixed_after
        .checked_add(isolated_after)
        .and_then(|after| after.checked_sub(cash_backed_before))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    if accrued_interest > 0 {
        let accrued_interest = u64::try_from(accrued_interest).map_err(|_| ErrorCode::ReserveOverflow)?;
        let side = market.side_mut(asset);
        side.reserves.live_reserve = side
            .reserves
            .live_reserve
            .checked_add(accrued_interest)
            .ok_or(ErrorCode::ReserveOverflow)?;
    }

    match asset {
        MarketAsset::Base => {
            market.debt.base_borrow_index_nad = next_index;
            market.debt.base_rate_at_target_nad = next_rate_at_target;
            market.debt.base_last_accrual_slot = current_slot;
        }
        MarketAsset::Quote => {
            market.debt.quote_borrow_index_nad = next_index;
            market.debt.quote_rate_at_target_nad = next_rate_at_target;
            market.debt.quote_last_accrual_slot = current_slot;
        }
    }
    Ok(())
}

impl Market {
    /// Current opposite-asset funding APR for one target-asset hLP vault.
    pub fn current_hlp_funding_apr_nad(&self, target_asset: MarketAsset) -> Result<u128> {
        let borrowed_asset = target_asset.opposite();
        let side = self.side(borrowed_asset);
        let fixed_debt = match borrowed_asset {
            MarketAsset::Base => self.debt.fixed_base_debt()?,
            MarketAsset::Quote => self.debt.fixed_quote_debt()?,
        };
        let isolated_debt = self.debt.isolated_debt(borrowed_asset)?;
        let vault = match target_asset {
            MarketAsset::Base => &self.base_hlp_vault,
            MarketAsset::Quote => &self.quote_hlp_vault,
        };
        let hlp_debt = Debt::shares_to_debt(vault.debt_shares, self.debt.borrow_index(borrowed_asset))?;
        let total_debt = fixed_debt
            .checked_add(isolated_debt)
            .and_then(|value| value.checked_add(hlp_debt))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let utilization = utilization_bps(total_debt, side.reserves.cash_reserve as u128)?;
        let error = utilization_error_nad(utilization, self.config.irm.target_utilization_bps as u64)?;
        let rate_at_target = match borrowed_asset {
            MarketAsset::Base => self.debt.base_rate_at_target_nad,
            MarketAsset::Quote => self.debt.quote_rate_at_target_nad,
        };
        instantaneous_rate_apr_nad(rate_at_target, error, self.config.irm.curve_steepness_nad as u128)
    }

    /// Twelve-hour funding APR signal used by hLP Stop Rate orders.
    pub fn hlp_funding_apr_ema_nad(&self, target_asset: MarketAsset) -> Result<u128> {
        let vault = match target_asset {
            MarketAsset::Base => &self.base_hlp_vault,
            MarketAsset::Quote => &self.quote_hlp_vault,
        };
        if vault.funding_apr_ema_last_slot == 0 {
            self.current_hlp_funding_apr_nad(target_asset)
        } else {
            Ok(vault.funding_apr_ema_nad)
        }
    }
}

fn total_cash_backed_borrowed(market: &Market, asset: MarketAsset, index_nad: u128) -> Result<u128> {
    let (margin_fixed, isolated) = match asset {
        MarketAsset::Base => (market.debt.fixed_base_shares, market.debt.isolated_base_shares),
        MarketAsset::Quote => (market.debt.fixed_quote_shares, market.debt.isolated_quote_shares),
    };
    let margin_fixed_debt = Debt::shares_to_debt(margin_fixed, index_nad)?;
    let isolated_debt = Debt::shares_to_debt(isolated, index_nad)?;
    margin_fixed_debt
        .checked_add(isolated_debt)
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

fn reconcile_global_health_contribution(
    position_contribution: &mut u64,
    aggregate_contribution: &mut u64,
    target_contribution: u64,
) -> Result<()> {
    match target_contribution.cmp(position_contribution) {
        std::cmp::Ordering::Greater => {
            let delta = target_contribution
                .checked_sub(*position_contribution)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            *aggregate_contribution = aggregate_contribution
                .checked_add(delta)
                .ok_or(ErrorCode::MarketMathOverflow)?;
        }
        std::cmp::Ordering::Less => {
            let delta = position_contribution
                .checked_sub(target_contribution)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            *aggregate_contribution = aggregate_contribution
                .checked_sub(delta)
                .ok_or(ErrorCode::MarketMathOverflow)?;
        }
        std::cmp::Ordering::Equal => {}
    }

    *position_contribution = target_contribution;
    Ok(())
}

fn reserve_for_ylp_mint_ceil(reserve_before: u64, ylp_supply_before: u64, ylp_amount: u64) -> Result<u64> {
    require!(ylp_supply_before > 0, ErrorCode::InsufficientLiquidity);
    let reserve_amount = ceil_div(
        (ylp_amount as u128)
            .checked_mul(reserve_before as u128)
            .ok_or(ErrorCode::MarketMathOverflow)?,
        ylp_supply_before as u128,
    )
    .ok_or(ErrorCode::MarketMathOverflow)?;
    u64::try_from(reserve_amount).map_err(|_| ErrorCode::MarketMathOverflow.into())
}

#[macro_export]
macro_rules! generate_market_seeds {
    ($market:expr) => {
        [
            MARKET_V2_SEED_PREFIX,
            $market.base_side.asset_mint.as_ref(),
            $market.quote_side.asset_mint.as_ref(),
            $market.params_hash.as_ref(),
            &[$market.bump],
        ]
    };
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct Risk {
    pub base_price_ema_nad: u64,
    pub quote_price_ema_nad: u64,
    pub directional_base_price_ema_nad: u64,
    pub directional_quote_price_ema_nad: u64,
    pub cached_spot_base_price_nad: u64,
    pub cached_spot_quote_price_nad: u64,
    /// Last observed total active curve depth (full-range plus concentrated).
    pub observed_curve_depth_nad: u128,
    /// EMA of total active curve depth.
    pub curve_depth_ema_nad: u128,
    pub last_snapshot_slot: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace)]
pub struct MarketHealth {
    pub global_health_base_contribution_for_quote_debt: u64,
    pub global_health_quote_contribution_for_base_debt: u64,
    pub effective_base_debt_nad: u128,
    pub effective_quote_debt_nad: u128,
    pub base_debt_health_bps: u64,
    pub quote_debt_health_bps: u64,
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

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace)]
pub struct ReserveShares {
    pub ylp_supply: u64,
}

impl ReserveShares {
    pub fn shares_for_deposit(&self, reserve_before: u64, deposit_amount: u64) -> Result<u64> {
        if self.ylp_supply == 0 || reserve_before == 0 {
            return Ok(deposit_amount);
        }
        let shares = (deposit_amount as u128)
            .checked_mul(self.ylp_supply as u128)
            .and_then(|value| value.checked_div(reserve_before as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        u64::try_from(shares).map_err(|_| ErrorCode::MarketMathOverflow.into())
    }

    pub fn reserve_for_burn(&self, reserve_before: u64, share_amount: u64) -> Result<u64> {
        require!(share_amount > 0, ErrorCode::AmountZero);
        require_gte!(self.ylp_supply, share_amount, ErrorCode::InsufficientBalance);
        let reserve_amount = (share_amount as u128)
            .checked_mul(reserve_before as u128)
            .and_then(|value| value.checked_div(self.ylp_supply as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        u64::try_from(reserve_amount).map_err(|_| ErrorCode::MarketMathOverflow.into())
    }

    pub fn mint(&mut self, share_amount: u64) -> Result<()> {
        require!(share_amount > 0, ErrorCode::AmountZero);
        self.ylp_supply = self
            .ylp_supply
            .checked_add(share_amount)
            .ok_or(ErrorCode::SupplyOverflow)?;
        Ok(())
    }

    pub fn burn(&mut self, share_amount: u64) -> Result<()> {
        require!(share_amount > 0, ErrorCode::AmountZero);
        self.ylp_supply = self
            .ylp_supply
            .checked_sub(share_amount)
            .ok_or(ErrorCode::SupplyUnderflow)?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FeesReceipt {
    pub swap_fee_growth_index_q64: u128,
    pub interest_growth_index_q64: u128,
    pub swap_fee_liability: u64,
    pub interest_liability: u64,
    pub unallocated_swap_fee_liability: u64,
    pub unallocated_interest_liability: u64,
    pub referral_interest_liability: u64,
    pub protocol_fee_liability: u64,
    pub buyback_fee_liability: u64,
    pub swap_fee_custody_balance: u64,
    pub interest_vault_balance: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct YieldClaimReceipt {
    pub claim_amount: u64,
    pub swap_fee_amount: u64,
    pub interest_amount: u64,
    pub remaining_swap_fee_liability: u64,
    pub remaining_interest_liability: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace)]
pub struct Reserves {
    // Virtual Reserves (r_virtual = r_cash + r_cash_backed_debt + r_hlp_live)
    pub live_reserve: u64,
    // Cash Reserves (r_cash)
    pub cash_reserve: u64,
    /// Physical reserve-vault atoms removed from executable AMM inventory by
    /// base-hLP deleveraging. They are conservation-only bookkeeping, excluded
    /// from hLP NAV and exit output, and return to executable cash pro rata as
    /// base hLP exits.
    pub base_hlp_backing_inventory: u64,
    /// Quote-hLP counterpart of `base_hlp_backing_inventory`; never a second
    /// hLP NAV or withdrawal claim.
    pub quote_hlp_backing_inventory: u64,
    /// Physical reserve-vault atoms retained from toxicity surcharge for a
    /// future protected recenter. They are custody-backed but excluded from
    /// executable cash/live reserves, yLP NAV, and every withdrawal claim.
    pub protected_recenter_reserve: u64,
}

impl Reserves {
    pub fn hlp_backing_inventory(&self, target_asset: MarketAsset) -> u64 {
        match target_asset {
            MarketAsset::Base => self.base_hlp_backing_inventory,
            MarketAsset::Quote => self.quote_hlp_backing_inventory,
        }
    }

    pub fn total_hlp_backing_inventory(&self) -> Result<u64> {
        self.base_hlp_backing_inventory
            .checked_add(self.quote_hlp_backing_inventory)
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    }

    pub fn credit_hlp_backing_inventory(&mut self, target_asset: MarketAsset, amount: u64) -> Result<()> {
        let inventory = match target_asset {
            MarketAsset::Base => &mut self.base_hlp_backing_inventory,
            MarketAsset::Quote => &mut self.quote_hlp_backing_inventory,
        };
        *inventory = inventory.checked_add(amount).ok_or(ErrorCode::MarketMathOverflow)?;
        Ok(())
    }

    pub fn debit_hlp_backing_inventory(&mut self, target_asset: MarketAsset, amount: u64) -> Result<()> {
        let inventory = match target_asset {
            MarketAsset::Base => &mut self.base_hlp_backing_inventory,
            MarketAsset::Quote => &mut self.quote_hlp_backing_inventory,
        };
        *inventory = inventory.checked_sub(amount).ok_or(ErrorCode::MarketMathOverflow)?;
        Ok(())
    }
}

impl FeesReceipt {
    fn from_side(market_side: &MarketSide) -> Result<Self> {
        let fees = &market_side.fees;
        Ok(Self {
            swap_fee_growth_index_q64: fees.swap_fee_growth_index_q64,
            interest_growth_index_q64: fees.interest_growth_index_q64,
            swap_fee_liability: fees.swap_fee_liability,
            interest_liability: fees.interest_liability,
            unallocated_swap_fee_liability: fees.unallocated_swap_fee_liability,
            unallocated_interest_liability: fees.unallocated_interest_liability,
            referral_interest_liability: fees.referral_interest_liability,
            protocol_fee_liability: fees.protocol_fee_liability()?,
            buyback_fee_liability: fees.buyback_fee_liability()?,
            swap_fee_custody_balance: fees.swap_fee_custody_balance,
            interest_vault_balance: fees.interest_vault_balance,
        })
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarketAsset {
    Base,
    Quote,
}

impl MarketAsset {
    pub fn code(self) -> u8 {
        match self {
            Self::Base => 0,
            Self::Quote => 1,
        }
    }

    pub fn try_from_code(code: u8) -> Result<Self> {
        match code {
            0 => Ok(Self::Base),
            1 => Ok(Self::Quote),
            _ => err!(ErrorCode::InvalidArgument),
        }
    }

    pub fn opposite(self) -> Self {
        match self {
            Self::Base => Self::Quote,
            Self::Quote => Self::Base,
        }
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace)]
pub struct MarketSide {
    pub asset_mint: Pubkey,
    pub asset_decimals: u8,
    pub hlp_mint: Pubkey,
    pub reserve_vault: Pubkey,
    pub collateral_vault: Pubkey,
    pub interest_vault: Pubkey,
    pub reserves: Reserves,
    pub shares: ReserveShares,
    pub fees: Fees,
    pub daily_borrow_bucket: DailyBorrowBucket,
}

impl MarketSide {
    pub fn assert_share_backing(&self) -> Result<()> {
        if self.shares.ylp_supply == 0 {
            require_eq!(self.reserves.live_reserve, 0, ErrorCode::BrokenInvariant);
        }
        Ok(())
    }

    pub fn ylp_exchange_rate_nad(&self) -> Result<u128> {
        if self.shares.ylp_supply == 0 {
            return Ok(0);
        }
        (self.reserves.live_reserve as u128)
            .checked_mul(crate::constants::NAD as u128)
            .and_then(|value| value.checked_div(self.shares.ylp_supply as u128))
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    }

    pub fn credit_reserve(&mut self, amount: u64, credit_cash: bool) -> Result<()> {
        self.reserves.live_reserve = self
            .reserves
            .live_reserve
            .checked_add(amount)
            .ok_or(ErrorCode::ReserveOverflow)?;
        if credit_cash {
            self.reserves.cash_reserve = self
                .reserves
                .cash_reserve
                .checked_add(amount)
                .ok_or(ErrorCode::ReserveOverflow)?;
        }
        Ok(())
    }

    pub fn debit_reserve(&mut self, amount: u64, debit_cash: bool) -> Result<()> {
        self.reserves.live_reserve = self
            .reserves
            .live_reserve
            .checked_sub(amount)
            .ok_or(ErrorCode::ReserveUnderflow)?;
        if debit_cash {
            self.reserves.cash_reserve = self
                .reserves
                .cash_reserve
                .checked_sub(amount)
                .ok_or(ErrorCode::CashReserveUnderflow)?;
        }
        Ok(())
    }

    pub fn record_swap_fee_credit(
        &mut self,
        fee_credit: u64,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
    ) -> Result<FeesReceipt> {
        self.record_claimable_swap_fees(
            fee_credit,
            0,
            protocol_fee_bps,
            protocol_auction_split,
            self.shares.ylp_supply,
        )
    }

    pub fn record_swap_fee_credit_with_supply(
        &mut self,
        fee_credit: u64,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        eligible_ylp_supply: u64,
    ) -> Result<FeesReceipt> {
        self.record_claimable_swap_fees(
            fee_credit,
            0,
            protocol_fee_bps,
            protocol_auction_split,
            eligible_ylp_supply,
        )
    }

    /// Records swap fees physically held in the reserve vault but excluded
    /// from executable reserves as explicit liabilities.
    ///
    /// The protocol split applies only to `base_fee_credit`.
    /// A distributed dynamic surcharge belongs entirely to yLPs; retained
    /// surcharge must stay in the reserve and must not be passed here.
    pub fn record_claimable_swap_fees(
        &mut self,
        base_fee_credit: u64,
        distributed_dynamic_surcharge_credit: u64,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        eligible_ylp_supply: u64,
    ) -> Result<FeesReceipt> {
        self.record_swap_fee_allocation(
            base_fee_credit,
            distributed_dynamic_surcharge_credit,
            0,
            0,
            protocol_fee_bps,
            protocol_auction_split,
            eligible_ylp_supply,
        )
    }

    /// Records the claimable remainder of a swap fee after a separately
    /// materialized principal-compounding credit. Protocol revenue is always
    /// split from the full base fee before any LP-owned atoms compound.
    #[allow(clippy::too_many_arguments)]
    pub fn record_swap_fee_allocation(
        &mut self,
        base_fee_credit: u64,
        distributed_dynamic_surcharge_credit: u64,
        compounded_base_fee_credit: u64,
        compounded_dynamic_surcharge_credit: u64,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        eligible_ylp_supply: u64,
    ) -> Result<FeesReceipt> {
        if base_fee_credit == 0 && distributed_dynamic_surcharge_credit == 0 {
            require_eq!(compounded_base_fee_credit, 0, ErrorCode::BrokenInvariant);
            require_eq!(compounded_dynamic_surcharge_credit, 0, ErrorCode::BrokenInvariant);
            return FeesReceipt::from_side(self);
        }
        let (protocol_fee, base_lp_fee) = split_revenue(base_fee_credit, protocol_fee_bps)?;
        require_gte!(base_lp_fee, compounded_base_fee_credit, ErrorCode::BrokenInvariant);
        require_gte!(
            distributed_dynamic_surcharge_credit,
            compounded_dynamic_surcharge_credit,
            ErrorCode::BrokenInvariant
        );
        let claimable_base_lp_fee = base_lp_fee
            .checked_sub(compounded_base_fee_credit)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        let claimable_dynamic_surcharge = distributed_dynamic_surcharge_credit
            .checked_sub(compounded_dynamic_surcharge_credit)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        let lp_fee = claimable_base_lp_fee
            .checked_add(claimable_dynamic_surcharge)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let claimable_fee_credit = protocol_fee.checked_add(lp_fee).ok_or(ErrorCode::MarketMathOverflow)?;
        let (fee_auction_amount, buyback_auction_amount) =
            split_protocol_auction_fee(protocol_fee, &protocol_auction_split)?;
        self.fees.swap_fee_custody_balance = self
            .fees
            .swap_fee_custody_balance
            .checked_add(claimable_fee_credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.swap_protocol_fee_liability = self
            .fees
            .swap_protocol_fee_liability
            .checked_add(fee_auction_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.swap_buyback_fee_liability = self
            .fees
            .swap_buyback_fee_liability
            .checked_add(buyback_auction_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.unallocated_swap_fee_liability = self
            .fees
            .unallocated_swap_fee_liability
            .checked_add(lp_fee)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.carry_forward_swap_fees_with_supply(eligible_ylp_supply)?;
        self.fees.assert_backed()?;
        FeesReceipt::from_side(self)
    }

    pub fn record_interest_credit(
        &mut self,
        interest_credit: u64,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        referral_interest_amount: u64,
    ) -> Result<FeesReceipt> {
        self.record_interest_credit_with_supply(
            interest_credit,
            protocol_fee_bps,
            protocol_auction_split,
            referral_interest_amount,
            self.shares.ylp_supply,
        )
    }

    pub fn record_interest_credit_with_supply(
        &mut self,
        interest_credit: u64,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        referral_interest_amount: u64,
        eligible_ylp_supply: u64,
    ) -> Result<FeesReceipt> {
        if interest_credit == 0 {
            return FeesReceipt::from_side(self);
        }
        let (protocol_fee, lp_interest) = split_revenue(interest_credit, protocol_fee_bps)?;
        require_gte!(protocol_fee, referral_interest_amount, ErrorCode::FeeMathOverflow);
        let remaining_protocol_fee = protocol_fee
            .checked_sub(referral_interest_amount)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        let (fee_auction_amount, buyback_auction_amount) =
            split_protocol_auction_fee(remaining_protocol_fee, &protocol_auction_split)?;
        self.fees.interest_vault_balance = self
            .fees
            .interest_vault_balance
            .checked_add(interest_credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.referral_interest_liability = self
            .fees
            .referral_interest_liability
            .checked_add(referral_interest_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.interest_protocol_fee_liability = self
            .fees
            .interest_protocol_fee_liability
            .checked_add(fee_auction_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.interest_buyback_fee_liability = self
            .fees
            .interest_buyback_fee_liability
            .checked_add(buyback_auction_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.unallocated_interest_liability = self
            .fees
            .unallocated_interest_liability
            .checked_add(lp_interest)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.carry_forward_interest_with_supply(eligible_ylp_supply)?;
        self.fees.assert_backed()?;
        FeesReceipt::from_side(self)
    }

    /// Record interest paid by an hLP funding position. Its LP share is
    /// indexed over ordinary yLP plus the permanently burned minimum shares,
    /// using a source-specific carry so public-interest rounding cannot cross
    /// into or out of this distribution lane.
    pub fn record_hlp_funding_interest_credit_with_supply(
        &mut self,
        interest_credit: u64,
        protocol_fee_bps: u16,
        protocol_auction_split: ProtocolAuctionSplit,
        eligible_ylp_supply: u64,
    ) -> Result<FeesReceipt> {
        if interest_credit == 0 {
            return FeesReceipt::from_side(self);
        }
        require!(eligible_ylp_supply > 0, ErrorCode::BrokenInvariant);
        let (protocol_fee, lp_interest) = split_revenue(interest_credit, protocol_fee_bps)?;
        let (fee_auction_amount, buyback_auction_amount) =
            split_protocol_auction_fee(protocol_fee, &protocol_auction_split)?;
        let (growth_delta, remainder_scaled) = distribute_growth_q64(
            lp_interest,
            eligible_ylp_supply,
            self.fees.hlp_funding_interest_growth_remainder_scaled,
        )?;

        self.fees.interest_vault_balance = self
            .fees
            .interest_vault_balance
            .checked_add(interest_credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.interest_protocol_fee_liability = self
            .fees
            .interest_protocol_fee_liability
            .checked_add(fee_auction_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.interest_buyback_fee_liability = self
            .fees
            .interest_buyback_fee_liability
            .checked_add(buyback_auction_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.interest_growth_index_q64 = self
            .fees
            .interest_growth_index_q64
            .checked_add(growth_delta)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.interest_liability = self
            .fees
            .interest_liability
            .checked_add(lp_interest)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        // With no ordinary holder, burned MIN_LIQUIDITY is the complete sink.
        // Discard its sub-Q64 carry as well so a later depositor cannot inherit
        // any fraction of already-published funding interest.
        self.fees.hlp_funding_interest_growth_remainder_scaled = if eligible_ylp_supply == MIN_LIQUIDITY {
            0
        } else {
            remainder_scaled
        };
        self.fees.assert_backed()?;
        FeesReceipt::from_side(self)
    }

    pub fn settle_referral_interest_claim(&mut self, amount: u64, interest_vault_balance: u64) -> Result<()> {
        require!(amount > 0, ErrorCode::AmountZero);
        self.fees.referral_interest_liability = self
            .fees
            .referral_interest_liability
            .checked_sub(amount)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        self.fees.interest_vault_balance = interest_vault_balance;
        self.fees.assert_backed()
    }

    pub fn carry_forward_swap_fees(&mut self) -> Result<()> {
        self.carry_forward_swap_fees_with_supply(self.shares.ylp_supply)
    }

    pub fn carry_forward_swap_fees_with_supply(&mut self, supply: u64) -> Result<()> {
        if supply == 0
            || (self.fees.unallocated_swap_fee_liability == 0 && self.fees.swap_fee_growth_remainder_scaled == 0)
        {
            return Ok(());
        }
        let allocated = self.fees.unallocated_swap_fee_liability;
        let (growth_delta, remainder_scaled) =
            distribute_growth_q64(allocated, supply, self.fees.swap_fee_growth_remainder_scaled)?;
        self.fees.swap_fee_growth_index_q64 = self
            .fees
            .swap_fee_growth_index_q64
            .checked_add(growth_delta)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.swap_fee_liability = self
            .fees
            .swap_fee_liability
            .checked_add(allocated)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.unallocated_swap_fee_liability = self
            .fees
            .unallocated_swap_fee_liability
            .checked_sub(allocated)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.swap_fee_growth_remainder_scaled = remainder_scaled;
        Ok(())
    }

    pub fn carry_forward_interest(&mut self) -> Result<()> {
        self.carry_forward_interest_with_supply(self.shares.ylp_supply)
    }

    pub fn carry_forward_interest_with_supply(&mut self, supply: u64) -> Result<()> {
        if supply == 0
            || (self.fees.unallocated_interest_liability == 0 && self.fees.interest_growth_remainder_scaled == 0)
        {
            return Ok(());
        }
        let allocated = self.fees.unallocated_interest_liability;
        let (growth_delta, remainder_scaled) =
            distribute_growth_q64(allocated, supply, self.fees.interest_growth_remainder_scaled)?;
        self.fees.interest_growth_index_q64 = self
            .fees
            .interest_growth_index_q64
            .checked_add(growth_delta)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.interest_liability = self
            .fees
            .interest_liability
            .checked_add(allocated)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.unallocated_interest_liability = self
            .fees
            .unallocated_interest_liability
            .checked_sub(allocated)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.interest_growth_remainder_scaled = remainder_scaled;
        Ok(())
    }

    pub fn prepare_yield_claim(
        &mut self,
        yield_account: &mut YieldAccount,
        swap_fee_custody_balance: u64,
        interest_vault_balance: u64,
        holder_balance: u64,
    ) -> Result<YieldClaimReceipt> {
        self.carry_forward_swap_fees()?;
        self.carry_forward_interest()?;
        yield_account.accrue(
            holder_balance,
            self.fees.swap_fee_growth_index_q64,
            self.fees.interest_growth_index_q64,
        )?;
        let claim_amount = yield_account.claimable_amount()?;
        require!(claim_amount > 0, ErrorCode::AmountZero);
        require_gte!(
            swap_fee_custody_balance,
            yield_account.accrued_swap_fee_amount,
            ErrorCode::UnbackedFeeLiability
        );
        require_gte!(
            interest_vault_balance,
            yield_account.accrued_interest_amount,
            ErrorCode::UnbackedFeeLiability
        );
        Ok(YieldClaimReceipt {
            claim_amount,
            swap_fee_amount: yield_account.accrued_swap_fee_amount,
            interest_amount: yield_account.accrued_interest_amount,
            remaining_swap_fee_liability: self.fees.swap_fee_liability,
            remaining_interest_liability: self.fees.interest_liability,
        })
    }

    pub fn settle_yield_claim(
        &mut self,
        yield_account: &mut YieldAccount,
        claim_amount: u64,
        swap_fee_amount: u64,
        interest_amount: u64,
    ) -> Result<YieldClaimReceipt> {
        self.fees.swap_fee_liability = self
            .fees
            .swap_fee_liability
            .checked_sub(swap_fee_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.interest_liability = self
            .fees
            .interest_liability
            .checked_sub(interest_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.swap_fee_custody_balance = self
            .fees
            .swap_fee_custody_balance
            .checked_sub(swap_fee_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.fees.interest_vault_balance = self
            .fees
            .interest_vault_balance
            .checked_sub(interest_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        yield_account.clear_claimed();
        self.fees.assert_backed()?;
        Ok(YieldClaimReceipt {
            claim_amount,
            swap_fee_amount,
            interest_amount,
            remaining_swap_fee_liability: self.fees.swap_fee_liability,
            remaining_interest_liability: self.fees.interest_liability,
        })
    }
}

fn split_revenue(amount: u64, protocol_bps: u16) -> Result<(u64, u64)> {
    require_gte!(BPS_DENOMINATOR, protocol_bps, ErrorCode::InvalidMarketConfig);
    let protocol_fee = proportional_bps(amount, protocol_bps)?;
    let lp_amount = amount.checked_sub(protocol_fee).ok_or(ErrorCode::MarketMathOverflow)?;
    Ok((protocol_fee, lp_amount))
}

fn split_protocol_auction_fee(protocol_fee: u64, split: &ProtocolAuctionSplit) -> Result<(u64, u64)> {
    require!(split.is_valid(), ErrorCode::InvalidDistribution);
    let buyback_amount = proportional_bps(protocol_fee, split.buyback_auction_bps)?;
    let fee_amount = protocol_fee
        .checked_sub(buyback_amount)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    Ok((fee_amount, buyback_amount))
}

fn proportional_bps(amount: u64, bps: u16) -> Result<u64> {
    let value = (amount as u128)
        .checked_mul(bps as u128)
        .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    u64::try_from(value).map_err(|_| ErrorCode::MarketMathOverflow.into())
}

#[cfg(test)]
mod config_tests {
    include!("../tests/state/config.rs");
}

#[cfg(test)]
mod debt_tests {
    include!("../tests/state/debt.rs");
}

#[cfg(test)]
mod fees_tests {
    include!("../tests/state/fees.rs");
}

#[cfg(test)]
mod hlp_tests {
    include!("../tests/state/hlp.rs");
}

#[cfg(test)]
mod limits_tests {
    include!("../tests/state/limits.rs");
}

#[cfg(test)]
mod market_tests {
    include!("../tests/state/market.rs");
}

#[cfg(test)]
mod market_reserve_tests {
    include!("../tests/state/market_reserves.rs");
}

#[cfg(test)]
mod market_interest_tests {
    include!("../tests/state/market_interest.rs");
}

#[cfg(test)]
mod side_accounting_tests {
    include!("../tests/state/side_accounting.rs");
}
