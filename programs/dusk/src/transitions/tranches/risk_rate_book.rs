use crate::{
    constants::{MS_PER_YEAR, NAD},
    errors::ErrorCode,
    math::{
        borrow_controller::{accrue_risk_rate, RateAccrual, RATE_SCALE},
        mul_div_rem_u128, slots_to_ms,
    },
    state::RiskRateConfig,
};
use anchor_lang::prelude::*;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, InitSpace, PartialEq, Eq)]
pub struct RiskRateSide {
    pub apr_e18: u128,
    pub borrow_index_nad: u128,
    pub borrow_index_remainder: u128,
}

impl RiskRateSide {
    fn apply(&mut self, accrual: RateAccrual) -> Result<()> {
        let denominator = NAD as u128 * MS_PER_YEAR as u128;
        require!(self.borrow_index_remainder < denominator, ErrorCode::BrokenInvariant);
        let (whole, remainder) = mul_div_rem_u128(self.borrow_index_nad, accrual.integrated_apr_nad_ms, denominator)?;
        let remainder = remainder
            .checked_add(self.borrow_index_remainder)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let next_index = self
            .borrow_index_nad
            .checked_add(whole)
            .and_then(|value| value.checked_add(remainder / denominator))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.apr_e18 = accrual.end_apr_e18;
        self.borrow_index_nad = next_index;
        self.borrow_index_remainder = remainder % denominator;
        Ok(())
    }
}

/// Both sides accrue from the same pre-operation coverage snapshot. Computing
/// quote coverage after base accrual would otherwise make asset ordering change
/// the common debt denominator and therefore the quote-side interest charge.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, InitSpace, PartialEq, Eq)]
pub struct RiskRateBook {
    pub config: RiskRateConfig,
    pub sides: [RiskRateSide; 2],
    pub last_accrual_slot: u64,
}

impl RiskRateBook {
    pub fn new(config: RiskRateConfig, current_slot: u64) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            config,
            sides: [RiskRateSide {
                apr_e18: config.initial_apr_nad as u128 * (RATE_SCALE / NAD as u128),
                borrow_index_nad: NAD as u128,
                borrow_index_remainder: 0,
            }; 2],
            last_accrual_slot: current_slot,
        })
    }

    pub fn accrue(&mut self, signals_nad: [Option<u128>; 2], current_slot: u64) -> Result<[RateAccrual; 2]> {
        require!(current_slot >= self.last_accrual_slot, ErrorCode::InvalidArgument);
        let elapsed_ms = slots_to_ms(self.last_accrual_slot, current_slot).ok_or(ErrorCode::MarketMathOverflow)?;
        let mut next = *self;
        let mut results = [RateAccrual {
            end_apr_e18: 0,
            integrated_apr_nad_ms: 0,
        }; 2];
        for i in 0..2 {
            results[i] = accrue_risk_rate(&self.config, self.sides[i].apr_e18, signals_nad[i], elapsed_ms)?;
            if signals_nad[i].is_some() {
                next.sides[i].apply(results[i])?;
            } else {
                // No borrower owes interest in this interval. Preserve the
                // index and discard the previous empty cohort's carry.
                next.sides[i].apr_e18 = results[i].end_apr_e18;
                next.sides[i].borrow_index_remainder = 0;
            }
        }
        next.last_accrual_slot = current_slot;
        *self = next;
        Ok(results)
    }

    /// The governance handler authenticates approval before calling this.
    /// Accrue under the old model, then install the new parameters prospectively.
    /// Changing the creation default never resets an existing market's APR.
    pub fn replace_config(
        &mut self,
        config: RiskRateConfig,
        signals_nad: [Option<u128>; 2],
        current_slot: u64,
    ) -> Result<[RateAccrual; 2]> {
        config.validate()?;
        let results = self.accrue(signals_nad, current_slot)?;
        self.config = config;
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    include!("../../tests/transitions/tranches/risk_rate_book.rs");
}
