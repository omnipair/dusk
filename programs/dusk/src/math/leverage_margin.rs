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

    pub fn recovery_bps(&self, remaining_notional: u128) -> Result<u16> {
        self.effective_maintenance_bps(remaining_notional)?
            .checked_add(self.recovery_buffer_bps)
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn candidate() -> LeverageMarginSchedule {
        LeverageMarginSchedule {
            base_initial_bps: 1_000,
            free_exposure_bps: 2_000,
            exposure_slope_numerator: 1,
            exposure_slope_denominator: 10,
            maintenance_boundaries: [10_000_000, 30_000_000],
            maintenance_rates_bps: [700, 800, 1_000],
            entry_buffer_bps: 300,
            recovery_buffer_bps: 200,
        }
    }

    #[test]
    fn candidate_examples_are_progressive_and_reversible() {
        let schedule = candidate();
        for (exposure, expected) in [(20_000, 1_000), (21_000, 1_010), (40_000, 1_200), (80_000, 1_600)] {
            assert_eq!(schedule.market_initial_bps(exposure, 100_000).unwrap(), expected);
        }
        assert_eq!(schedule.market_initial_bps(40_000, 100_000).unwrap(), 1_200);
        for (notional, expected) in [(10_000, 700), (25_000, 1_900), (30_000, 2_300), (50_000, 4_300)] {
            assert_eq!(schedule.maintenance_amount(notional * 1_000).unwrap(), expected * 1_000);
        }
        assert_eq!(
            schedule.initial_bps(25_000_000, 50_000_000, 100_000_000).unwrap(),
            1_300
        );
        assert_eq!(schedule.recovery_bps(25_000_000).unwrap(), 960);
    }

    #[test]
    fn invalid_or_missing_capacity_cannot_create_margin_capacity() {
        let mut schedule = candidate();
        assert!(schedule.market_initial_bps(0, 0).is_err());
        assert!(schedule.market_initial_bps(1, 0).is_err());
        assert!(schedule.market_initial_bps(u128::MAX, 1).is_err());
        assert!(schedule.maintenance_amount(u128::MAX).is_err());
        assert_eq!(schedule.market_initial_bps(920_000, 100_000).unwrap(), 10_000);
        schedule.exposure_slope_denominator = 0;
        assert!(schedule.validate().is_err());
        schedule = candidate();
        schedule.maintenance_rates_bps[1] = 600;
        assert!(schedule.validate().is_err());
        schedule = candidate();
        schedule.maintenance_rates_bps[2] = 9_800;
        assert!(schedule.validate().is_err());
    }

    #[test]
    fn rounding_does_not_turn_a_small_position_into_a_full_equity_tier() {
        let schedule = candidate();
        assert_eq!(schedule.effective_maintenance_bps(0).unwrap(), 0);
        assert_eq!(schedule.maintenance_amount(1).unwrap(), 1);
        assert_eq!(schedule.effective_maintenance_bps(1).unwrap(), 700);
        for boundary in schedule.maintenance_boundaries {
            let before = schedule.maintenance_amount(boundary - 1).unwrap();
            let at = schedule.maintenance_amount(boundary).unwrap();
            let after = schedule.maintenance_amount(boundary + 1).unwrap();
            assert!(before <= at && at <= after && after - before <= 1);
        }
    }

    proptest! {
        #[test]
        fn progressive_amounts_and_rates_are_monotone(a in 1u64..1_000_000_000, b in 1u64..1_000_000_000) {
            let schedule = candidate();
            let lo = u128::from(a.min(b));
            let hi = u128::from(a.max(b));
            prop_assert!(schedule.maintenance_amount(lo).unwrap() <= schedule.maintenance_amount(hi).unwrap());
            let lo_rate = schedule.effective_maintenance_bps(lo).unwrap();
            let hi_rate = schedule.effective_maintenance_bps(hi).unwrap();
            prop_assert!(700 <= lo_rate && lo_rate <= hi_rate && hi_rate <= 1_000);
            prop_assert!(schedule.market_initial_bps(lo, 100_000_000).unwrap()
                <= schedule.market_initial_bps(hi, 100_000_000).unwrap());
        }

        #[test]
        fn entry_and_recovery_use_separate_buffers(notional in 1u64..1_000_000_000, exposure in 0u64..1_000_000_000) {
            let schedule = candidate();
            let notional = u128::from(notional);
            let maintenance = schedule.effective_maintenance_bps(notional).unwrap();
            prop_assert!(schedule.initial_bps(notional, u128::from(exposure), 100_000_000).unwrap() >= maintenance + 300);
            prop_assert_eq!(schedule.recovery_bps(notional).unwrap(), maintenance + 200);
        }
    }
}
