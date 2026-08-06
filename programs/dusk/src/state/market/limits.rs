use anchor_lang::prelude::*;

use crate::{constants::MS_PER_DAY, errors::ErrorCode, shared::math::slots_to_ms};

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace)]
pub struct DailyLimits {
    /// Gross new principal currently consuming the per-side borrow-flow
    /// capacity. This is a 24-hour leaky/token bucket, not an exact trailing
    /// window sum: it permits a full burst after idle and then refills at the
    /// configured daily rate.
    pub borrowed_bucket: u64,
    pub last_decay_slot: u64,
    /// Numerator remainder from `limit * elapsed_ms / MS_PER_DAY`. For a fixed
    /// absolute limit, carrying it makes refill independent of how often the
    /// bucket is checkpointed. The bps-derived absolute limit can still move
    /// when conservative market depth changes.
    pub decay_remainder_ms: u64,
}

impl DailyLimits {
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

#[cfg(test)]
mod tests {
    include!("../../tests/state/limits.rs");
}
