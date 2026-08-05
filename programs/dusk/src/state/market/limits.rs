use anchor_lang::prelude::*;

use crate::{constants::MS_PER_DAY, errors::ErrorCode, shared::math::slots_to_ms};

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace)]
pub struct DailyLimits {
    pub borrowed_bucket: u64,
    pub last_decay_slot: u64,
}

impl DailyLimits {
    pub fn decay_to_slot(&mut self, current_slot: u64) -> Result<()> {
        self.borrowed_bucket = if self.borrowed_bucket == 0 {
            0
        } else if let Some(elapsed_ms) = slots_to_ms(self.last_decay_slot, current_slot) {
            if elapsed_ms >= MS_PER_DAY {
                0
            } else {
                let remaining_ms = (MS_PER_DAY - elapsed_ms) as u128;
                let decayed = (self.borrowed_bucket as u128)
                    .checked_mul(remaining_ms)
                    .and_then(|value| value.checked_div(MS_PER_DAY as u128))
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                u64::try_from(decayed).map_err(|_| ErrorCode::MarketMathOverflow)?
            }
        } else {
            self.borrowed_bucket
        };
        self.last_decay_slot = current_slot;
        Ok(())
    }

    pub fn record_borrow(&mut self, amount: u64, limit: u64, current_slot: u64) -> Result<()> {
        self.decay_to_slot(current_slot)?;
        let next_bucket = self
            .borrowed_bucket
            .checked_add(amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require_gte!(limit, next_bucket, ErrorCode::DailyLimitExceeded);
        self.borrowed_bucket = next_bucket;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    include!("../../tests/state/limits.rs");
}
