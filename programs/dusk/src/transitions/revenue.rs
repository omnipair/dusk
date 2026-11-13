use anchor_lang::prelude::*;

use crate::{constants::*, errors::ErrorCode, math::mul_div_ceil_u128, state::*};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProtocolAuctionSettlementQuote {
    pub next_state: ProtocolAuctionState,
    pub payment_amount: u64,
    pub treasury_amount: u64,
    pub staking_vault_amount: u64,
}

pub(crate) fn quote_protocol_auction_settlement(
    state: ProtocolAuctionState,
    sold_amount: u64,
    current_slot: u64,
    staking_vault_bps: u16,
) -> Result<ProtocolAuctionSettlementQuote> {
    require!(sold_amount > 0, ErrorCode::AmountZero);
    require_keys_neq!(state.accepted_mint, Pubkey::default(), ErrorCode::InvalidAuctionConfig);
    require!(
        state.virtual_sold_reserve > 0 && state.virtual_payment_reserve > 0,
        ErrorCode::InvalidAuctionConfig
    );
    let elapsed_slots = current_slot
        .checked_sub(state.last_update_slot)
        .ok_or(ErrorCode::InvalidAuctionClock)?;
    let slots_per_day = MS_PER_DAY / TARGET_MS_PER_SLOT;
    require!(state.stream_remainder < slots_per_day, ErrorCode::InvalidAuctionConfig);
    let streamed = (state.sold_rate_per_day as u128)
        .checked_mul(elapsed_slots as u128)
        .and_then(|value| value.checked_add(state.stream_remainder as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let virtual_sold = state
        .virtual_sold_reserve
        .checked_add(streamed / slots_per_day as u128)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    // Inverse's streaming virtual inventory preserves x*y. Round the payment
    // reserve up so finite token precision never permits a free purchase.
    let virtual_payment = mul_div_ceil_u128(state.virtual_payment_reserve, state.virtual_sold_reserve, virtual_sold)?;
    let remaining_sold = virtual_sold
        .checked_sub(sold_amount as u128)
        .ok_or(ErrorCode::InsufficientAmount)?;
    require!(remaining_sold > 0, ErrorCode::InsufficientAmount);
    let payment_amount = u64::try_from(mul_div_ceil_u128(virtual_payment, sold_amount as u128, remaining_sold)?)
        .map_err(|_| ErrorCode::MarketMathOverflow)?;
    let next_state = ProtocolAuctionState {
        virtual_sold_reserve: remaining_sold,
        virtual_payment_reserve: virtual_payment
            .checked_add(payment_amount as u128)
            .ok_or(ErrorCode::MarketMathOverflow)?,
        last_update_slot: current_slot,
        stream_remainder: (streamed % slots_per_day as u128) as u64,
        ..state
    };

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
        next_state,
        payment_amount,
        treasury_amount,
        staking_vault_amount,
    })
}

impl Fees {
    pub(crate) fn credit_protocol_interest_revenue(
        &mut self,
        protocol: u64,
        split: ProtocolAuctionSplit,
        referral: u64,
    ) -> Result<()> {
        require_gte!(protocol, referral, ErrorCode::FeeMathOverflow);
        let (fee, buyback) = split_protocol_auction_fee(protocol - referral, &split)?;
        self.referral_interest_liability = self
            .referral_interest_liability
            .checked_add(referral)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.interest_protocol_fee_liability = self
            .interest_protocol_fee_liability
            .checked_add(fee)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.interest_buyback_fee_liability = self
            .interest_buyback_fee_liability
            .checked_add(buyback)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        Ok(())
    }

    pub fn protocol_auction(&self, lane: ProtocolAuctionLane, source: ProtocolRevenueSource) -> ProtocolAuctionState {
        match (lane, source) {
            (ProtocolAuctionLane::Fee, ProtocolRevenueSource::Swap) => self.fee_swap_auction,
            (ProtocolAuctionLane::Fee, ProtocolRevenueSource::Interest) => self.fee_interest_auction,
            (ProtocolAuctionLane::Buyback, ProtocolRevenueSource::Swap) => self.buyback_swap_auction,
            (ProtocolAuctionLane::Buyback, ProtocolRevenueSource::Interest) => self.buyback_interest_auction,
        }
    }

    pub fn protocol_auction_mut(
        &mut self,
        lane: ProtocolAuctionLane,
        source: ProtocolRevenueSource,
    ) -> &mut ProtocolAuctionState {
        match (lane, source) {
            (ProtocolAuctionLane::Fee, ProtocolRevenueSource::Swap) => &mut self.fee_swap_auction,
            (ProtocolAuctionLane::Fee, ProtocolRevenueSource::Interest) => &mut self.fee_interest_auction,
            (ProtocolAuctionLane::Buyback, ProtocolRevenueSource::Swap) => &mut self.buyback_swap_auction,
            (ProtocolAuctionLane::Buyback, ProtocolRevenueSource::Interest) => &mut self.buyback_interest_auction,
        }
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
            .and_then(|value| value.checked_add(self.revenue_interest_liability))
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

#[cfg(test)]
include!("../tests/support/revenue.rs");
