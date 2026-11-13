use crate::{
    constants::{DEVNET_JUNIOR_NOTICE_SLOTS, JUNIOR_MAX_NOTICE_SLOTS, MS_PER_DAY, TARGET_MS_PER_SLOT},
    errors::ErrorCode,
    math::{mul_div_ceil_u128, mul_div_u128},
    state::{JuniorExitConfig, JuniorExitTicket, TrancheCapital},
};
use anchor_lang::prelude::*;

impl Default for JuniorExitConfig {
    fn default() -> Self {
        let (minimum_notice_slots, maximum_notice_slots) = if cfg!(feature = "devnet-short-notice") {
            (DEVNET_JUNIOR_NOTICE_SLOTS, DEVNET_JUNIOR_NOTICE_SLOTS)
        } else {
            (7 * MS_PER_DAY / TARGET_MS_PER_SLOT, JUNIOR_MAX_NOTICE_SLOTS)
        };
        Self {
            minimum_notice_slots,
            maximum_notice_slots,
            exercise_window_slots: 2 * MS_PER_DAY / TARGET_MS_PER_SLOT,
        }
    }
}

impl JuniorExitConfig {
    pub fn validate(&self) -> Result<()> {
        require!(
            self.minimum_notice_slots > 0
                && self.maximum_notice_slots >= self.minimum_notice_slots
                && self.exercise_window_slots > 0,
            ErrorCode::InvalidMarketConfig
        );
        Ok(())
    }
}

impl JuniorExitTicket {
    /// Start a fresh notice. Cancel an existing ticket before requesting again.
    /// The returned fee is retained virtually by the seed. Requested receipts
    /// remain junior, loss-bearing and income-earning until conversion.
    pub fn request(
        &mut self,
        capital: &mut TrancheCapital,
        config: &JuniorExitConfig,
        receipts: u64,
        clock: u64,
    ) -> Result<u64> {
        config.validate()?;
        require!(self.receipts == 0, ErrorCode::InvalidArgument);
        require!(capital.junior_reserve_shares > 0, ErrorCode::InvalidArgument);
        require!(receipts > 0, ErrorCode::AmountZero);
        let fee = u64::try_from(mul_div_u128(
            receipts as u128,
            crate::constants::JUNIOR_EXIT_FEE_BPS as u128,
            10_000,
        )?)
        .map_err(|_| ErrorCode::MarketMathOverflow)?;
        let net = receipts.checked_sub(fee).ok_or(ErrorCode::InsufficientBalance)?;
        require!(net > 0, ErrorCode::AmountZero);
        let mut next = *capital;
        next.pay_seed_fee(fee)?;
        next.queued_receipts = next
            .queued_receipts
            .checked_add(net)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require!(
            next.queued_receipts <= next.junior_receipt_supply - next.permanent_receipts,
            ErrorCode::InsufficientBalance
        );
        let extra = mul_div_ceil_u128(
            (config.maximum_notice_slots - config.minimum_notice_slots) as u128,
            next.queued_receipts as u128,
            next.junior_receipt_supply as u128,
        )?;
        let notice = config
            .minimum_notice_slots
            .checked_add(u64::try_from(extra).map_err(|_| ErrorCode::MarketMathOverflow)?)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let ready_at = clock.checked_add(notice).ok_or(ErrorCode::MarketMathOverflow)?;
        let expires_at = ready_at
            .checked_add(config.exercise_window_slots)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        *capital = next;
        *self = Self {
            generation: capital.generation,
            receipts: net,
            ready_at,
            expires_at,
        };
        Ok(fee)
    }

    /// Blocked duration and the active flag must come from authenticated market
    /// restriction records, never from unchecked instruction arguments.
    pub fn convert(
        &mut self,
        capital: &mut TrancheCapital,
        clock: u64,
        blocked_slots_since_ready: u64,
        conversion_blocked: bool,
    ) -> Result<u64> {
        require!(
            self.receipts > 0 && self.generation == capital.generation,
            ErrorCode::InvalidArgument
        );
        require!(
            !conversion_blocked && clock >= self.ready_at,
            ErrorCode::InvalidArgument
        );
        let elapsed = clock - self.ready_at;
        require!(blocked_slots_since_ready <= elapsed, ErrorCode::InvalidArgument);
        let window = self
            .expires_at
            .checked_sub(self.ready_at)
            .ok_or(ErrorCode::BrokenInvariant)?;
        require!(elapsed - blocked_slots_since_ready < window, ErrorCode::InvalidArgument);
        let senior_receipts = capital.convert_queued(self.receipts)?;
        *self = Self::default();
        Ok(senior_receipts)
    }

    pub fn cancel(&mut self, capital: &mut TrancheCapital) -> Result<u64> {
        require!(
            self.receipts > 0 && self.generation == capital.generation,
            ErrorCode::InvalidArgument
        );
        capital.queued_receipts = capital
            .queued_receipts
            .checked_sub(self.receipts)
            .ok_or(ErrorCode::BrokenInvariant)?;
        let receipts = self.receipts;
        *self = Self::default();
        Ok(receipts)
    }
}

#[cfg(test)]
mod tests {
    include!("../../tests/transitions/tranches/junior_exit.rs");
}
