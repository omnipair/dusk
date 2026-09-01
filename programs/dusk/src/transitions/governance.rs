use anchor_lang::prelude::*;

use crate::{constants::*, errors::ErrorCode, state::*, transitions::amm::*};

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

#[cfg(test)]
mod config_tests {
    include!("../tests/transitions/governance_config.rs");
}

#[cfg(test)]
mod market_tests {
    include!("../tests/transitions/governance_market.rs");
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

impl Market {
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
                let target = ConcentratedCurveParameters {
                    peak_amplification_nad: *peak_amplification_nad,
                    core_half_width_bps: *core_half_width_bps,
                    fade_width_bps: *fade_width_bps,
                };
                target.validate(MAX_AMM_AMPLIFICATION_NAD)?;
                require!(
                    self.config.amm.concentrated_curve_parameters()? != target,
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
                    next.amm
                        .set_concentrated_curve_parameters(ConcentratedCurveParameters {
                            peak_amplification_nad: *peak_amplification_nad,
                            core_half_width_bps: *core_half_width_bps,
                            fade_width_bps: *fade_width_bps,
                        })?;
                    next.validate()?;
                    self.config = next;
                    if self.amm.initialized {
                        self.apply_concentrated_curve_parameter_update(current_slot)?;
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

    pub fn assert_parameter_execution_utilization(&self) -> Result<()> {
        require!(
            self.lending_utilization_bps(MarketAsset::Base)? < PARAMETER_EXECUTION_MAX_UTILIZATION_BPS
                && self.lending_utilization_bps(MarketAsset::Quote)? < PARAMETER_EXECUTION_MAX_UTILIZATION_BPS,
            ErrorCode::UtilizationGuardExceeded
        );
        Ok(())
    }
}
