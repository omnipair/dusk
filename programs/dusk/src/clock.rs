use anchor_lang::prelude::*;

use crate::errors::ErrorCode;

/// One execution snapshot: slots order observations; timestamps measure time.
/// `Clock.unix_timestamp` has whole-second resolution, including on Alpenglow.
/// Repeated timestamps are valid and never imply elapsed economic time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MarketClock {
    pub slot: u64,
    pub timestamp_ms: u64,
}

impl MarketClock {
    #[inline(always)]
    pub fn from_clock(clock: &Clock) -> Result<Self> {
        Self::from_timestamp(clock.slot, clock.unix_timestamp)
    }

    #[inline(always)]
    pub fn from_timestamp(slot: u64, unix_timestamp: i64) -> Result<Self> {
        let timestamp_ms = u64::try_from(unix_timestamp)
            .ok()
            .and_then(|seconds| seconds.checked_mul(1_000))
            .ok_or(ErrorCode::InvalidArgument)?;
        Ok(Self { slot, timestamp_ms })
    }

    pub fn get() -> Result<Self> {
        Self::from_clock(&Clock::get()?)
    }

    #[inline(always)]
    pub(crate) fn elapsed_ms(self, previous_slot: u64, previous_timestamp_ms: u64) -> Result<u64> {
        require!(
            self.slot >= previous_slot && self.timestamp_ms >= previous_timestamp_ms,
            ErrorCode::InvalidArgument
        );
        if self.slot == previous_slot {
            return Ok(0);
        }
        Ok(self.timestamp_ms - previous_timestamp_ms)
    }
}

#[cfg(test)]
pub(crate) fn test_clock(slot: u64) -> MarketClock {
    // Synthetic fixture cadence only. Production always reads unix_timestamp.
    MarketClock {
        slot,
        timestamp_ms: slot.saturating_mul(400) / 1_000 * 1_000,
    }
}

#[cfg(test)]
mod tests {
    include!("tests/state/clock.rs");
}
