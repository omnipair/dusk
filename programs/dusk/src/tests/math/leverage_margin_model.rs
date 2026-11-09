use anchor_lang::prelude::*;

use crate::{constants::BPS_DENOMINATOR, errors::ErrorCode, math::ceil_div};

// Parameterized calibration model. Runtime admission continues to use the
// existing rules until the liquidity definition and settings are selected.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LeverageMarginSchedule {
    pub base_initial_bps: u16,
    pub free_exposure_bps: u16,
    pub exposure_slope_numerator: u16,
    pub exposure_slope_denominator: u16,
    pub maintenance_boundaries: [u128; 2],
    pub maintenance_rates_bps: [u16; 3],
    pub entry_buffer_bps: u16,
    pub recovery_buffer_bps: u16,
}

impl LeverageMarginSchedule {
    pub fn validate(&self) -> Result<()> {
        let rates = self.maintenance_rates_bps;
        require!(
            self.base_initial_bps > 0
                && self.base_initial_bps <= BPS_DENOMINATOR
                && self.exposure_slope_denominator > 0
                && self.maintenance_boundaries[0] > 0
                && self.maintenance_boundaries[1] > self.maintenance_boundaries[0]
                && rates[0] > 0
                && rates[0] <= rates[1]
                && rates[1] <= rates[2]
                && u32::from(rates[2]) + u32::from(self.entry_buffer_bps) <= u32::from(BPS_DENOMINATOR)
                && u32::from(rates[2]) + u32::from(self.recovery_buffer_bps) <= u32::from(BPS_DENOMINATOR),
            ErrorCode::InvalidMarketConfig
        );
        Ok(())
    }

    pub fn market_initial_bps(&self, exposure: u128, reference_liquidity: u128) -> Result<u16> {
        self.validate()?;
        require_gt!(reference_liquidity, 0, ErrorCode::InsufficientLiquidity);
        let exposure_bps = ceil_div(
            exposure
                .checked_mul(u128::from(BPS_DENOMINATOR))
                .ok_or(ErrorCode::MarketMathOverflow)?,
            reference_liquidity,
        )
        .ok_or(ErrorCode::MarketMathOverflow)?;
        let excess = exposure_bps.saturating_sub(u128::from(self.free_exposure_bps));
        let surcharge = ceil_div(
            excess
                .checked_mul(u128::from(self.exposure_slope_numerator))
                .ok_or(ErrorCode::MarketMathOverflow)?,
            u128::from(self.exposure_slope_denominator),
        )
        .ok_or(ErrorCode::MarketMathOverflow)?;
        let required = u128::from(self.base_initial_bps)
            .checked_add(surcharge)
            .ok_or(ErrorCode::MarketMathOverflow)?
            .min(u128::from(BPS_DENOMINATOR));
        u16::try_from(required).map_err(|_| ErrorCode::MarketMathOverflow.into())
    }

    pub fn effective_maintenance_bps(&self, notional: u128) -> Result<u16> {
        self.validate()?;
        if notional == 0 {
            return Ok(0);
        }
        // Compute the rate from unrounded weighted portions. Rounding the
        // maintenance amount to a whole token atom first would make a one-atom
        // position appear to require 100% maintenance.
        let [first, second] = self.maintenance_boundaries;
        let portions = [
            notional.min(first),
            notional.saturating_sub(first).min(second - first),
            notional.saturating_sub(second),
        ];
        let weighted = portions
            .into_iter()
            .zip(self.maintenance_rates_bps)
            .try_fold(0u128, |total, (portion, rate)| {
                portion
                    .checked_mul(u128::from(rate))
                    .and_then(|part| total.checked_add(part))
            })
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let rate = ceil_div(weighted, notional).ok_or(ErrorCode::MarketMathOverflow)?;
        u16::try_from(rate).map_err(|_| ErrorCode::MarketMathOverflow.into())
    }

    pub fn initial_bps(&self, notional: u128, exposure: u128, reference_liquidity: u128) -> Result<u16> {
        let market = self.market_initial_bps(exposure, reference_liquidity)?;
        let position = self
            .effective_maintenance_bps(notional)?
            .checked_add(self.entry_buffer_bps)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        Ok(market.max(position))
    }

    pub fn maintenance_amount(&self, notional: u128) -> Result<u128> {
        self.validate()?;
        let [first, second] = self.maintenance_boundaries;
        let portions = [
            notional.min(first),
            notional.saturating_sub(first).min(second - first),
            notional.saturating_sub(second),
        ];
        let weighted = portions
            .into_iter()
            .zip(self.maintenance_rates_bps)
            .try_fold(0u128, |total, (portion, rate)| {
                portion
                    .checked_mul(u128::from(rate))
                    .and_then(|part| total.checked_add(part))
            })
            .ok_or(ErrorCode::MarketMathOverflow)?;
        ceil_div(weighted, u128::from(BPS_DENOMINATOR)).ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    }

    pub fn recovery_bps(&self, remaining_notional: u128) -> Result<u16> {
        self.effective_maintenance_bps(remaining_notional)?
            .checked_add(self.recovery_buffer_bps)
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    }
}

#[cfg(test)]
mod tests {
    include!("leverage_margin.rs");
}
