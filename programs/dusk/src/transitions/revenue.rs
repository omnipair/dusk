use anchor_lang::prelude::*;

use crate::{
    constants::*,
    errors::ErrorCode,
    math::{denormalize_from_nad_ceil, normalize_to_nad},
    state::*,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProtocolAuctionSettlementQuote {
    pub auction_price_nad: u64,
    pub payment_amount: u64,
    pub treasury_amount: u64,
    pub staking_vault_amount: u64,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn quote_protocol_auction_settlement(
    sold_amount: u64,
    sold_decimals: u8,
    accepted_decimals: u8,
    reference_price_nad: u64,
    start_multiplier_bps: u16,
    floor_multiplier_bps: u16,
    elapsed_slots: u64,
    duration_slots: u64,
    staking_vault_bps: u16,
) -> Result<ProtocolAuctionSettlementQuote> {
    require!(reference_price_nad > 0, ErrorCode::InvalidSettlementPrice);
    let start_price = (reference_price_nad as u128)
        .checked_mul(start_multiplier_bps as u128)
        .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let floor_price = (reference_price_nad as u128)
        .checked_mul(floor_multiplier_bps as u128)
        .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let decay = start_price
        .checked_sub(floor_price)
        .ok_or(ErrorCode::MarketMathOverflow)?
        .checked_mul(elapsed_slots.min(duration_slots) as u128)
        .and_then(|value| value.checked_div(duration_slots as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let auction_price_nad = u64::try_from(start_price.checked_sub(decay).ok_or(ErrorCode::MarketMathOverflow)?)
        .map_err(|_| ErrorCode::MarketMathOverflow)?;
    let sold_nad = normalize_to_nad(sold_amount as u128, sold_decimals)?;
    let payment_nad = sold_nad
        .checked_mul(auction_price_nad as u128)
        .and_then(|value| value.checked_div(NAD as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let payment_amount = denormalize_from_nad_ceil(payment_nad, accepted_decimals)?;

    require_gte!(BPS_DENOMINATOR, staking_vault_bps, ErrorCode::InvalidDistribution);
    let staking_vault_amount = u64::try_from(
        (payment_amount as u128)
            .checked_mul(staking_vault_bps as u128)
            .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?,
    )
    .map_err(|_| ErrorCode::MarketMathOverflow)?;
    let treasury_amount = payment_amount
        .checked_sub(staking_vault_amount)
        .ok_or(ErrorCode::MarketMathOverflow)?;

    Ok(ProtocolAuctionSettlementQuote {
        auction_price_nad,
        payment_amount,
        treasury_amount,
        staking_vault_amount,
    })
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
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
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
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    }

    pub fn buyback_fee_liability(&self) -> Result<u64> {
        self.swap_buyback_fee_liability
            .checked_add(self.interest_buyback_fee_liability)
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    }
}

pub(crate) fn split_revenue(amount: u64, protocol_bps: u16) -> Result<(u64, u64)> {
    require_gte!(BPS_DENOMINATOR, protocol_bps, ErrorCode::InvalidMarketConfig);
    let protocol_fee = proportional_bps(amount, protocol_bps)?;
    let lp_amount = amount.checked_sub(protocol_fee).ok_or(ErrorCode::MarketMathOverflow)?;
    Ok((protocol_fee, lp_amount))
}

pub(crate) fn split_protocol_auction_fee(protocol_fee: u64, split: &ProtocolAuctionSplit) -> Result<(u64, u64)> {
    require!(split.is_valid(), ErrorCode::InvalidDistribution);
    let buyback_amount = proportional_bps(protocol_fee, split.buyback_auction_bps)?;
    let fee_amount = protocol_fee
        .checked_sub(buyback_amount)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    Ok((fee_amount, buyback_amount))
}

fn proportional_bps(amount: u64, bps: u16) -> Result<u64> {
    let value = (amount as u128)
        .checked_mul(bps as u128)
        .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    u64::try_from(value).map_err(|_| ErrorCode::MarketMathOverflow.into())
}
