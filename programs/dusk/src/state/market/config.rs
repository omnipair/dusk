use anchor_lang::prelude::*;

use crate::{
    constants::{BPS_DENOMINATOR, MAX_HALF_LIFE_MS, MIN_HALF_LIFE_MS, NAD},
    errors::ErrorCode,
    math::{asymptotic_scaled_rate_nad, fee_share_cap_to_marginal_rate_nad, validate_fee_share_caps},
};

use super::{AmmConfig, MAX_AMM_FEE_COEFFICIENT_NAD, MAX_AMM_VOLATILITY_NAD};

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
    pub q_ema_half_life_ms: u64,
    pub max_daily_borrow_bps: u16,
    pub global_health_contribution_cap_bps: u16,
    pub borrow_market_health_floor_bps: u16,
    pub amm: AmmConfig,
    pub irm: IrmConfig,
    pub start_time: i64,
}

impl MarketConfig {
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
        next.validate()?;
        *self = next;
        Ok(())
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
                && half_life_in_bounds(self.q_ema_half_life_ms),
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
mod tests {
    include!("../../tests/state/config.rs");
}
