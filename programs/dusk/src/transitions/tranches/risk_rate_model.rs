use anchor_lang::prelude::*;

use crate::{
    constants::{MS_PER_DAY, NAD},
    errors::ErrorCode,
};

/// Per-market inputs for the coverage band controller. A zero maximum leaves
/// APR economically uncapped; arithmetic still rejects unrepresentable debt.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, InitSpace, PartialEq, Eq)]
pub struct RiskRateConfig {
    pub lower_signal_nad: u64,
    pub upper_signal_nad: u64,
    pub half_life_ms: u64,
    pub initial_apr_nad: u64,
    pub minimum_apr_nad: u64,
    pub maximum_apr_nad: u64,
    pub borrow_utilization_limit_bps: u16,
}

impl Default for RiskRateConfig {
    fn default() -> Self {
        Self {
            lower_signal_nad: NAD / 20,
            upper_signal_nad: NAD / 16,
            half_life_ms: 14 * MS_PER_DAY,
            initial_apr_nad: 4 * NAD / 100,
            minimum_apr_nad: 2 * NAD / 100,
            maximum_apr_nad: 0,
            borrow_utilization_limit_bps: 8_000,
        }
    }
}

impl RiskRateConfig {
    pub fn validate(&self) -> Result<()> {
        require!(
            self.lower_signal_nad > 0
                && self.lower_signal_nad < self.upper_signal_nad
                && (3_600_000..=30 * MS_PER_DAY).contains(&self.half_life_ms)
                && self.initial_apr_nad > 0
                && self.minimum_apr_nad <= self.initial_apr_nad
                && (self.maximum_apr_nad == 0 || self.maximum_apr_nad >= self.initial_apr_nad)
                && (1..=10_000).contains(&self.borrow_utilization_limit_bps),
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
    pub half_life_ms: Option<u64>,
    pub initial_apr_nad: Option<u64>,
    pub minimum_apr_nad: Option<u64>,
    pub maximum_apr_nad: Option<u64>,
    pub borrow_utilization_limit_bps: Option<u16>,
}

impl RiskRateOverrides {
    pub fn resolve(self) -> Result<RiskRateConfig> {
        let defaults = RiskRateConfig::default();
        let config = RiskRateConfig {
            lower_signal_nad: self.lower_signal_nad.unwrap_or(defaults.lower_signal_nad),
            upper_signal_nad: self.upper_signal_nad.unwrap_or(defaults.upper_signal_nad),
            half_life_ms: self.half_life_ms.unwrap_or(defaults.half_life_ms),
            initial_apr_nad: self.initial_apr_nad.unwrap_or(defaults.initial_apr_nad),
            minimum_apr_nad: self.minimum_apr_nad.unwrap_or(defaults.minimum_apr_nad),
            maximum_apr_nad: self.maximum_apr_nad.unwrap_or(defaults.maximum_apr_nad),
            borrow_utilization_limit_bps: self
                .borrow_utilization_limit_bps
                .unwrap_or(defaults.borrow_utilization_limit_bps),
        };
        config.validate()?;
        Ok(config)
    }
}
