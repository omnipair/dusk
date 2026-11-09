use anchor_lang::prelude::*;

use crate::{
    constants::{DEFAULT_BORROW_APR_NAD, MS_PER_DAY, NAD},
    errors::ErrorCode,
};

/// Minimum economic borrowing APR for every Dusk market. There is no
/// configurable economic ceiling; checked arithmetic bounds representation.
#[constant]
pub const MINIMUM_BORROW_APR_NAD: u64 = NAD / 100;
#[constant]
pub const MIN_BORROW_HALF_LIFE_MS: u64 = MS_PER_DAY / 2;
#[constant]
pub const MAX_BORROW_HALF_LIFE_MS: u64 = 30 * MS_PER_DAY;

/// Coverage-and-utilization controller configuration. Initial APR is creation-only.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, InitSpace, PartialEq, Eq)]
pub struct RiskRateConfig {
    pub lower_signal_nad: u64,
    pub upper_signal_nad: u64,
    pub lower_utilization_bps: u16,
    pub upper_utilization_bps: u16,
    pub half_life_ms: u64,
    pub initial_apr_nad: u64,
}

impl Default for RiskRateConfig {
    fn default() -> Self {
        Self {
            lower_signal_nad: NAD / 20,
            upper_signal_nad: NAD / 16,
            lower_utilization_bps: 7_500,
            upper_utilization_bps: 8_000,
            half_life_ms: 14 * MS_PER_DAY,
            initial_apr_nad: DEFAULT_BORROW_APR_NAD as u64,
        }
    }
}

impl RiskRateConfig {
    pub fn validate(&self) -> Result<()> {
        require!(
            self.lower_signal_nad > 0
                && self.lower_signal_nad < self.upper_signal_nad
                && self.lower_utilization_bps < self.upper_utilization_bps
                && self.upper_utilization_bps < 10_000
                && (MIN_BORROW_HALF_LIFE_MS..=MAX_BORROW_HALF_LIFE_MS).contains(&self.half_life_ms)
                && self.initial_apr_nad >= MINIMUM_BORROW_APR_NAD,
            ErrorCode::InvalidMarketConfig
        );
        Ok(())
    }
}

/// Creation-only overrides. Missing fields use defaults; explicit zero values
/// are preserved and validated rather than treated as missing.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace)]
pub struct RiskRateOverrides {
    pub lower_signal_nad: Option<u64>,
    pub upper_signal_nad: Option<u64>,
    pub lower_utilization_bps: Option<u16>,
    pub upper_utilization_bps: Option<u16>,
    pub half_life_ms: Option<u64>,
    pub initial_apr_nad: Option<u64>,
}

impl RiskRateOverrides {
    pub fn resolve(self) -> Result<RiskRateConfig> {
        let defaults = RiskRateConfig::default();
        let config = RiskRateConfig {
            lower_signal_nad: self.lower_signal_nad.unwrap_or(defaults.lower_signal_nad),
            upper_signal_nad: self.upper_signal_nad.unwrap_or(defaults.upper_signal_nad),
            lower_utilization_bps: self.lower_utilization_bps.unwrap_or(defaults.lower_utilization_bps),
            upper_utilization_bps: self.upper_utilization_bps.unwrap_or(defaults.upper_utilization_bps),
            half_life_ms: self.half_life_ms.unwrap_or(defaults.half_life_ms),
            initial_apr_nad: self.initial_apr_nad.unwrap_or(defaults.initial_apr_nad),
        };
        config.validate()?;
        Ok(config)
    }
}

/// Prospective controller settings. Initial APR and the protocol-wide floor
/// cannot be changed after pool creation.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, InitSpace, PartialEq, Eq)]
pub struct RiskRateUpdate {
    pub lower_signal_nad: u64,
    pub upper_signal_nad: u64,
    pub lower_utilization_bps: u16,
    pub upper_utilization_bps: u16,
    pub half_life_ms: u64,
}
impl From<RiskRateConfig> for RiskRateUpdate {
    fn from(config: RiskRateConfig) -> Self {
        Self {
            lower_signal_nad: config.lower_signal_nad,
            upper_signal_nad: config.upper_signal_nad,
            lower_utilization_bps: config.lower_utilization_bps,
            upper_utilization_bps: config.upper_utilization_bps,
            half_life_ms: config.half_life_ms,
        }
    }
}
impl RiskRateUpdate {
    pub fn apply(self, current: RiskRateConfig) -> Result<RiskRateConfig> {
        let next = RiskRateConfig {
            lower_signal_nad: self.lower_signal_nad,
            upper_signal_nad: self.upper_signal_nad,
            lower_utilization_bps: self.lower_utilization_bps,
            upper_utilization_bps: self.upper_utilization_bps,
            half_life_ms: self.half_life_ms,
            ..current
        };
        next.validate()?;
        Ok(next)
    }
}
