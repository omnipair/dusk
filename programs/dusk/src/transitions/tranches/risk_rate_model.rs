use anchor_lang::prelude::*;

use crate::{
    constants::{DEFAULT_BORROW_APR_NAD, MS_PER_DAY, NAD},
    errors::ErrorCode,
    state::{RiskRateConfig, RiskRateOverrides, RiskRateUpdate},
};

/// Minimum economic borrowing APR for every Dusk market. There is no
/// configurable economic ceiling; checked arithmetic bounds representation.
#[constant]
pub const MINIMUM_BORROW_APR_NAD: u64 = NAD / 100;
#[constant]
pub const MIN_BORROW_HALF_LIFE_MS: u64 = MS_PER_DAY / 2;
#[constant]
pub const MAX_BORROW_HALF_LIFE_MS: u64 = 30 * MS_PER_DAY;

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
