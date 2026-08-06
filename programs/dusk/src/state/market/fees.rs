use anchor_lang::prelude::*;

use crate::{
    constants::{YIELD_GROWTH_FRACTION_MASK_Q64, YIELD_GROWTH_SCALE_Q64},
    errors::ErrorCode,
    state::futarchy_authority::{ProtocolAuctionLane, ProtocolRevenueSource},
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace)]
pub struct Fees {
    pub swap_fee_growth_index_q64: u128,
    pub interest_growth_index_q64: u128,
    /// Scaled fee entitlement not yet representable by the integer growth
    /// index. The corresponding whole-token backing already sits in
    /// `swap_fee_liability`; it must never be redistributed as unallocated
    /// revenue.
    pub swap_fee_growth_remainder_scaled: u64,
    /// Interest counterpart of `swap_fee_growth_remainder_scaled`.
    pub interest_growth_remainder_scaled: u64,
    /// Claimable swap fees physically held in the reserve vault but excluded
    /// from executable cash and live reserves.
    pub swap_fee_custody_balance: u64,
    pub interest_vault_balance: u64,
    pub swap_fee_liability: u64,
    pub interest_liability: u64,
    pub unallocated_swap_fee_liability: u64,
    pub unallocated_interest_liability: u64,
    pub swap_protocol_fee_liability: u64,
    pub swap_buyback_fee_liability: u64,
    pub interest_protocol_fee_liability: u64,
    pub interest_buyback_fee_liability: u64,
    pub referral_interest_liability: u64,
    /// Governance-approved reference market for fee-lane auctions. A default
    /// key permits only the sold market itself when it directly pairs the sold
    /// and accepted mints.
    pub fee_auction_reference_market: Pubkey,
    /// Governance-approved reference market for buyback-lane auctions. A
    /// default key has the same direct-market-only meaning as above.
    pub buyback_auction_reference_market: Pubkey,
    pub fee_swap_auction_epoch: ProtocolAuctionEpoch,
    pub fee_interest_auction_epoch: ProtocolAuctionEpoch,
    pub buyback_swap_auction_epoch: ProtocolAuctionEpoch,
    pub buyback_interest_auction_epoch: ProtocolAuctionEpoch,
}

/// Convert newly backed whole atoms plus an earlier sub-index remainder into
/// an integer per-share growth increment. The returned remainder stays in
/// scaled token-atom units and is carried into the next distribution. It is
/// always smaller than the active `u64` supply, so its persisted form is also
/// `u64` even though the numerator is evaluated in `u128`.
///
/// Moving the complete `amount` into the allocated liability is essential:
/// `growth_delta * supply` already belongs fractionally to holders even when
/// individual accounts cannot claim a whole atom yet. Leaving the rounded
/// difference in an unallocated bucket would promise it a second time.
/// Every successful distribution preserves the exact identity
/// `amount * 2^64 + prior_remainder = growth_delta * supply + remainder`.
/// The remainder is backed but neither directly claimable nor eligible for a
/// second allocation; only a later call can fold it into another index delta.
pub(crate) fn distribute_growth_q64(amount: u64, supply: u64, prior_remainder_scaled: u64) -> Result<(u128, u64)> {
    require!(supply > 0, ErrorCode::SupplyUnderflow);
    // Maximum numerator is `(2^64 - 1) * 2^64 + (2^64 - 1)`, exactly
    // `u128::MAX`. No wider production integer is required.
    let scaled = (amount as u128)
        .checked_mul(YIELD_GROWTH_SCALE_Q64)
        .and_then(|value| value.checked_add(prior_remainder_scaled as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let remainder = u64::try_from(scaled % supply as u128).map_err(|_| ErrorCode::MarketMathOverflow)?;
    Ok((scaled / supply as u128, remainder))
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq, InitSpace)]
pub struct ProtocolAuctionEpoch {
    pub start_slot: u64,
    /// Liability remaining immediately after the preceding fill. A larger
    /// current liability proves that new inventory arrived and starts a new
    /// epoch instead of inheriting an old floor price.
    pub tracked_inventory: u64,
}

pub fn accrue_fee_liability(shares: u64, fee_growth_index_q64: u128, fee_growth_checkpoint_q64: u128) -> Result<u64> {
    accrue_fee_liability_with_remainder(shares, fee_growth_index_q64, fee_growth_checkpoint_q64, 0)
        .map(|(amount, _)| amount)
}

pub fn accrue_fee_liability_with_remainder(
    shares: u64,
    fee_growth_index_q64: u128,
    fee_growth_checkpoint_q64: u128,
    prior_remainder_q64: u64,
) -> Result<(u64, u64)> {
    if shares == 0 || fee_growth_index_q64 <= fee_growth_checkpoint_q64 {
        return Ok((0, prior_remainder_q64));
    }
    let delta = fee_growth_index_q64
        .checked_sub(fee_growth_checkpoint_q64)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    // Never multiply a u64 balance by the accumulated u128 index directly.
    // Split the delta into whole and fractional Q64 limbs. Each product is at
    // most `(2^64 - 1)^2`; the fractional product plus its prior remainder is
    // at most `u128::MAX`.
    let whole_per_share = u64::try_from(delta >> 64).map_err(|_| ErrorCode::MarketMathOverflow)?;
    let whole_accrual = (shares as u128)
        .checked_mul(whole_per_share as u128)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let fractional_scaled = (shares as u128)
        .checked_mul(delta & YIELD_GROWTH_FRACTION_MASK_Q64)
        .and_then(|value| value.checked_add(prior_remainder_q64 as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let accrued = whole_accrual
        .checked_add(fractional_scaled >> 64)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let remainder =
        u64::try_from(fractional_scaled & YIELD_GROWTH_FRACTION_MASK_Q64).map_err(|_| ErrorCode::MarketMathOverflow)?;
    Ok((
        u64::try_from(accrued).map_err(|_| ErrorCode::MarketMathOverflow)?,
        remainder,
    ))
}

impl Fees {
    pub fn protocol_auction_reference_market(&self, lane: ProtocolAuctionLane) -> Pubkey {
        match lane {
            ProtocolAuctionLane::Fee => self.fee_auction_reference_market,
            ProtocolAuctionLane::Buyback => self.buyback_auction_reference_market,
        }
    }

    pub fn set_protocol_auction_reference_market(&mut self, lane: ProtocolAuctionLane, reference_market: Pubkey) {
        match lane {
            ProtocolAuctionLane::Fee => self.fee_auction_reference_market = reference_market,
            ProtocolAuctionLane::Buyback => self.buyback_auction_reference_market = reference_market,
        }
        self.reset_protocol_auction_epochs(lane);
    }

    pub fn protocol_auction_epoch(
        &self,
        lane: ProtocolAuctionLane,
        source: ProtocolRevenueSource,
        current_slot: u64,
    ) -> ProtocolAuctionEpoch {
        let liability = self.protocol_auction_liability(lane, source);
        let stored = match (lane, source) {
            (ProtocolAuctionLane::Fee, ProtocolRevenueSource::Swap) => self.fee_swap_auction_epoch,
            (ProtocolAuctionLane::Fee, ProtocolRevenueSource::Interest) => self.fee_interest_auction_epoch,
            (ProtocolAuctionLane::Buyback, ProtocolRevenueSource::Swap) => self.buyback_swap_auction_epoch,
            (ProtocolAuctionLane::Buyback, ProtocolRevenueSource::Interest) => self.buyback_interest_auction_epoch,
        };
        if stored.start_slot == 0 || liability > stored.tracked_inventory {
            ProtocolAuctionEpoch {
                start_slot: current_slot,
                tracked_inventory: liability,
            }
        } else {
            stored
        }
    }

    pub fn reset_protocol_auction_epochs(&mut self, lane: ProtocolAuctionLane) {
        match lane {
            ProtocolAuctionLane::Fee => {
                self.fee_swap_auction_epoch = ProtocolAuctionEpoch::default();
                self.fee_interest_auction_epoch = ProtocolAuctionEpoch::default();
            }
            ProtocolAuctionLane::Buyback => {
                self.buyback_swap_auction_epoch = ProtocolAuctionEpoch::default();
                self.buyback_interest_auction_epoch = ProtocolAuctionEpoch::default();
            }
        }
    }

    pub fn total_liability(&self) -> Result<u64> {
        self.swap_fee_liability
            .checked_add(self.interest_liability)
            .and_then(|value| value.checked_add(self.unallocated_swap_fee_liability))
            .and_then(|value| value.checked_add(self.unallocated_interest_liability))
            .and_then(|value| value.checked_add(self.swap_protocol_fee_liability))
            .and_then(|value| value.checked_add(self.swap_buyback_fee_liability))
            .and_then(|value| value.checked_add(self.interest_protocol_fee_liability))
            .and_then(|value| value.checked_add(self.interest_buyback_fee_liability))
            .and_then(|value| value.checked_add(self.referral_interest_liability))
            .ok_or(ErrorCode::MarketMathOverflow.into())
    }

    pub fn assert_backed(&self) -> Result<()> {
        let swap_liability = self
            .swap_fee_liability
            .checked_add(self.unallocated_swap_fee_liability)
            .and_then(|value| value.checked_add(self.swap_protocol_fee_liability))
            .and_then(|value| value.checked_add(self.swap_buyback_fee_liability))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require_gte!(
            self.swap_fee_custody_balance,
            swap_liability,
            ErrorCode::UnbackedFeeLiability
        );
        let interest_liability = self
            .interest_liability
            .checked_add(self.unallocated_interest_liability)
            .and_then(|value| value.checked_add(self.referral_interest_liability))
            .and_then(|value| value.checked_add(self.interest_protocol_fee_liability))
            .and_then(|value| value.checked_add(self.interest_buyback_fee_liability))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require_gte!(
            self.interest_vault_balance,
            interest_liability,
            ErrorCode::UnbackedFeeLiability
        );
        Ok(())
    }

    pub fn protocol_auction_liability(&self, lane: ProtocolAuctionLane, source: ProtocolRevenueSource) -> u64 {
        match (lane, source) {
            (ProtocolAuctionLane::Fee, ProtocolRevenueSource::Swap) => self.swap_protocol_fee_liability,
            (ProtocolAuctionLane::Buyback, ProtocolRevenueSource::Swap) => self.swap_buyback_fee_liability,
            (ProtocolAuctionLane::Fee, ProtocolRevenueSource::Interest) => self.interest_protocol_fee_liability,
            (ProtocolAuctionLane::Buyback, ProtocolRevenueSource::Interest) => self.interest_buyback_fee_liability,
        }
    }

    pub fn settle_protocol_auction_liability(
        &mut self,
        lane: ProtocolAuctionLane,
        source: ProtocolRevenueSource,
        amount: u64,
        epoch_start_slot: u64,
    ) -> Result<()> {
        require!(amount > 0, ErrorCode::AmountZero);
        let liability = match (lane, source) {
            (ProtocolAuctionLane::Fee, ProtocolRevenueSource::Swap) => &mut self.swap_protocol_fee_liability,
            (ProtocolAuctionLane::Buyback, ProtocolRevenueSource::Swap) => &mut self.swap_buyback_fee_liability,
            (ProtocolAuctionLane::Fee, ProtocolRevenueSource::Interest) => &mut self.interest_protocol_fee_liability,
            (ProtocolAuctionLane::Buyback, ProtocolRevenueSource::Interest) => &mut self.interest_buyback_fee_liability,
        };
        *liability = liability.checked_sub(amount).ok_or(ErrorCode::MarketMathOverflow)?;
        match source {
            ProtocolRevenueSource::Swap => {
                self.swap_fee_custody_balance = self
                    .swap_fee_custody_balance
                    .checked_sub(amount)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
            }
            ProtocolRevenueSource::Interest => {
                self.interest_vault_balance = self
                    .interest_vault_balance
                    .checked_sub(amount)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
            }
        }
        let remaining = self.protocol_auction_liability(lane, source);
        let next_epoch = if remaining == 0 {
            ProtocolAuctionEpoch::default()
        } else {
            ProtocolAuctionEpoch {
                start_slot: epoch_start_slot,
                tracked_inventory: remaining,
            }
        };
        match (lane, source) {
            (ProtocolAuctionLane::Fee, ProtocolRevenueSource::Swap) => self.fee_swap_auction_epoch = next_epoch,
            (ProtocolAuctionLane::Fee, ProtocolRevenueSource::Interest) => self.fee_interest_auction_epoch = next_epoch,
            (ProtocolAuctionLane::Buyback, ProtocolRevenueSource::Swap) => self.buyback_swap_auction_epoch = next_epoch,
            (ProtocolAuctionLane::Buyback, ProtocolRevenueSource::Interest) => {
                self.buyback_interest_auction_epoch = next_epoch
            }
        }
        Ok(())
    }

    pub fn protocol_fee_liability(&self) -> Result<u64> {
        self.swap_protocol_fee_liability
            .checked_add(self.interest_protocol_fee_liability)
            .ok_or(ErrorCode::MarketMathOverflow.into())
    }

    pub fn buyback_fee_liability(&self) -> Result<u64> {
        self.swap_buyback_fee_liability
            .checked_add(self.interest_buyback_fee_liability)
            .ok_or(ErrorCode::MarketMathOverflow.into())
    }
}

#[cfg(test)]
mod tests {
    include!("../../tests/state/fees.rs");
}
