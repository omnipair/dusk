use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::{MarketEventMetadata, ProtocolAuctionSettled},
    generate_market_seeds,
    math::{denormalize_from_nad_ceil, normalize_to_nad},
    shared::token::{is_fee_free_mint, transfer_checked_with_remaining_accounts},
    state::{FutarchyAuthority, Market, ProtocolAuctionLane, ProtocolRevenueSource},
};

use crate::instructions::common::{require_supported_asset_mint, token_program_for_mint, validate_owner_asset_account};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct SettleProtocolAuctionArgs {
    pub lane: ProtocolAuctionLane,
    pub source: ProtocolRevenueSource,
    pub sold_amount: u64,
    pub max_payment_amount: u64,
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(args: SettleProtocolAuctionArgs)]
pub struct SettleProtocolAuction<'info> {
    #[account(mut)]
    pub bidder: Signer<'info>,

    #[account(
        mut,
        seeds = [
            MARKET_V2_SEED_PREFIX,
            market.base_side.asset_mint.as_ref(),
            market.quote_side.asset_mint.as_ref(),
            market.params_hash.as_ref(),
        ],
        bump = market.bump
    )]
    pub market: Box<Account<'info, Market>>,

    #[account(
        mut,
        seeds = [FUTARCHY_AUTHORITY_SEED_PREFIX],
        bump = futarchy_authority.bump
    )]
    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,

    pub sold_mint: Box<InterfaceAccount<'info, Mint>>,
    pub accepted_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(mut)]
    pub sold_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub bidder_payment_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub bidder_receive_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub treasury_payment_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub staking_vault_payment_account: Box<InterfaceAccount<'info, TokenAccount>>,

    pub reference_market: Box<Account<'info, Market>>,

    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> SettleProtocolAuction<'info> {
    pub fn validate(&self, args: &SettleProtocolAuctionArgs) -> Result<()> {
        self.market.assert_started()?;
        self.futarchy_authority.validate()?;
        require!(args.sold_amount > 0, ErrorCode::AmountZero);
        require!(args.max_payment_amount > 0, ErrorCode::InsufficientAuctionPayment);

        let auction = self.futarchy_authority.auction_config(args.lane);
        require_keys_eq!(self.accepted_mint.key(), auction.accepted_mint, ErrorCode::InvalidMint);
        require!(is_fee_free_mint(&self.accepted_mint)?, ErrorCode::InvalidMint);

        let sold_side = self.market.asset_for_mint(self.sold_mint.key())?;
        let market_side = self.market.side(sold_side);
        market_side.fees.assert_backed()?;
        require_keys_eq!(self.sold_mint.key(), market_side.asset_mint, ErrorCode::InvalidMint);
        let (expected_vault, tracked_custody) = match args.source {
            ProtocolRevenueSource::Swap => (
                market_side.reserve_vault,
                market_side
                    .reserves
                    .cash_reserve
                    .checked_add(market_side.fees.swap_fee_custody_balance)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
            ),
            ProtocolRevenueSource::Interest => (market_side.interest_vault, market_side.fees.interest_vault_balance),
        };
        require_keys_eq!(self.sold_vault.key(), expected_vault, ErrorCode::InvalidVault);
        require_keys_eq!(self.sold_vault.mint, self.sold_mint.key(), ErrorCode::InvalidVault);
        require_keys_eq!(self.sold_vault.owner, self.market.key(), ErrorCode::InvalidVault);
        require_gte!(
            market_side.fees.protocol_auction_liability(args.lane, args.source),
            args.sold_amount,
            ErrorCode::UnbackedFeeLiability
        );
        require_gte!(self.sold_vault.amount, tracked_custody, ErrorCode::UnbackedFeeLiability);

        let sold_mint = self.sold_mint.key();
        let accepted_mint = self.accepted_mint.key();
        let expected_reference_market =
            expected_reference_market(&self.market, self.market.key(), args.lane, sold_mint, accepted_mint)?;
        require_keys_eq!(
            self.reference_market.key(),
            expected_reference_market,
            ErrorCode::InvalidMarket
        );

        validate_owner_asset_account(self.bidder.key(), &self.accepted_mint, &self.bidder_payment_account)?;
        validate_owner_asset_account(self.bidder.key(), &self.sold_mint, &self.bidder_receive_account)?;
        validate_recipient_payment_account(
            &self.treasury_payment_account,
            auction.recipients.treasury,
            self.accepted_mint.key(),
        )?;
        validate_recipient_payment_account(
            &self.staking_vault_payment_account,
            auction.recipients.staking_vault,
            self.accepted_mint.key(),
        )?;
        require_supported_asset_mint(&self.sold_mint)?;
        require_supported_asset_mint(&self.accepted_mint)?;
        Ok(())
    }

    pub fn handle_settle(ctx: Context<'_, '_, '_, 'info, Self>, args: SettleProtocolAuctionArgs) -> Result<()> {
        let current_slot = Clock::get()?.slot;
        let sold_mint = ctx.accounts.sold_mint.key();
        let accepted_mint = ctx.accounts.accepted_mint.key();
        let sold_side = ctx.accounts.market.asset_for_mint(sold_mint)?;
        let auction_epoch =
            ctx.accounts
                .market
                .side(sold_side)
                .fees
                .protocol_auction_epoch(args.lane, args.source, current_slot);
        let (max_reference_age_slots, start_multiplier_bps, floor_multiplier_bps, duration_slots, staking_vault_bps) = {
            let auction = ctx.accounts.futarchy_authority.auction_config(args.lane);
            (
                auction.params.max_reference_age_slots,
                auction.params.start_multiplier_bps,
                auction.params.floor_multiplier_bps,
                auction.params.duration_slots,
                auction.recipients.staking_vault_bps,
            )
        };
        let approved_reference_market = ctx
            .accounts
            .market
            .side(sold_side)
            .fees
            .protocol_auction_reference_market(args.lane);
        let expected_reference_market = expected_reference_market(
            &ctx.accounts.market,
            ctx.accounts.market.key(),
            args.lane,
            sold_mint,
            accepted_mint,
        )?;
        require_keys_eq!(
            ctx.accounts.reference_market.key(),
            expected_reference_market,
            ErrorCode::InvalidMarket
        );
        let (reference_market, reference_price_nad) = if sold_mint == accepted_mint {
            (ctx.accounts.market.key(), NAD)
        } else if approved_reference_market == Pubkey::default() {
            let price_nad =
                price_from_market(&ctx.accounts.market, sold_mint, accepted_mint).ok_or(ErrorCode::InvalidMarket)?;
            assert_fresh_reference(
                ctx.accounts.market.risk.last_snapshot_slot,
                current_slot,
                max_reference_age_slots,
            )?;
            (ctx.accounts.market.key(), price_nad)
        } else {
            let price_nad = price_from_market(&ctx.accounts.reference_market, sold_mint, accepted_mint)
                .ok_or(ErrorCode::InvalidMarket)?;
            assert_fresh_reference(
                ctx.accounts.reference_market.risk.last_snapshot_slot,
                current_slot,
                max_reference_age_slots,
            )?;
            (ctx.accounts.reference_market.key(), price_nad)
        };
        require!(reference_price_nad > 0, ErrorCode::InvalidSettlementPrice);
        let start_price = (reference_price_nad as u128)
            .checked_mul(start_multiplier_bps as u128)
            .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let floor_price = (reference_price_nad as u128)
            .checked_mul(floor_multiplier_bps as u128)
            .and_then(|value| value.checked_div(BPS_DENOMINATOR as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let elapsed_slots = current_slot
            .saturating_sub(auction_epoch.start_slot)
            .min(duration_slots);
        let decay = start_price
            .checked_sub(floor_price)
            .ok_or(ErrorCode::MarketMathOverflow)?
            .checked_mul(elapsed_slots as u128)
            .and_then(|value| value.checked_div(duration_slots as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let auction_price_nad = u64::try_from(start_price.checked_sub(decay).ok_or(ErrorCode::MarketMathOverflow)?)
            .map_err(|_| ErrorCode::MarketMathOverflow)?;
        let sold_nad = normalize_to_nad(args.sold_amount as u128, ctx.accounts.sold_mint.decimals)?;
        let payment_nad = sold_nad
            .checked_mul(auction_price_nad as u128)
            .and_then(|value| value.checked_div(NAD as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let payment_amount = denormalize_from_nad_ceil(payment_nad, ctx.accounts.accepted_mint.decimals)?;
        require!(
            payment_amount <= args.max_payment_amount,
            ErrorCode::InsufficientAuctionPayment
        );
        require_gte!(
            ctx.accounts.bidder_payment_account.amount,
            payment_amount,
            ErrorCode::InsufficientBalance
        );
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

        let accepted_token_program = token_program_for_mint(
            &ctx.accounts.accepted_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        transfer_checked_with_remaining_accounts(
            ctx.accounts.bidder.to_account_info(),
            ctx.accounts.bidder_payment_account.to_account_info(),
            ctx.accounts.treasury_payment_account.to_account_info(),
            ctx.accounts.accepted_mint.to_account_info(),
            accepted_token_program.clone(),
            treasury_amount,
            ctx.accounts.accepted_mint.decimals,
            &[],
            ctx.remaining_accounts,
        )?;
        transfer_checked_with_remaining_accounts(
            ctx.accounts.bidder.to_account_info(),
            ctx.accounts.bidder_payment_account.to_account_info(),
            ctx.accounts.staking_vault_payment_account.to_account_info(),
            ctx.accounts.accepted_mint.to_account_info(),
            accepted_token_program,
            staking_vault_amount,
            ctx.accounts.accepted_mint.decimals,
            &[],
            ctx.remaining_accounts,
        )?;
        let sold_token_program = token_program_for_mint(
            &ctx.accounts.sold_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        transfer_checked_with_remaining_accounts(
            ctx.accounts.market.to_account_info(),
            ctx.accounts.sold_vault.to_account_info(),
            ctx.accounts.bidder_receive_account.to_account_info(),
            ctx.accounts.sold_mint.to_account_info(),
            sold_token_program,
            args.sold_amount,
            ctx.accounts.sold_mint.decimals,
            &[&generate_market_seeds!(ctx.accounts.market)[..]],
            ctx.remaining_accounts,
        )?;

        ctx.accounts.sold_vault.reload()?;
        let market_side = ctx.accounts.market.side_mut(sold_side);
        market_side.fees.settle_protocol_auction_liability(
            args.lane,
            args.source,
            args.sold_amount,
            auction_epoch.start_slot,
        )?;
        market_side.fees.assert_backed()?;
        let required_custody = match args.source {
            ProtocolRevenueSource::Swap => market_side
                .reserves
                .cash_reserve
                .checked_add(market_side.fees.swap_fee_custody_balance)
                .ok_or(ErrorCode::MarketMathOverflow)?,
            ProtocolRevenueSource::Interest => market_side.fees.interest_vault_balance,
        };
        require_gte!(
            ctx.accounts.sold_vault.amount,
            required_custody,
            ErrorCode::UnbackedFeeLiability
        );
        let remaining_fee_liability = market_side
            .fees
            .protocol_auction_liability(ProtocolAuctionLane::Fee, args.source);
        let remaining_buyback_liability = market_side
            .fees
            .protocol_auction_liability(ProtocolAuctionLane::Buyback, args.source);
        let market_key = ctx.accounts.market.key();
        let bidder_key = ctx.accounts.bidder.key();
        emit_cpi!(ProtocolAuctionSettled {
            market: market_key,
            reference_market,
            lane: args.lane.code(),
            source: args.source.code(),
            side: sold_side.code(),
            bidder: bidder_key,
            sold_mint,
            accepted_mint,
            sold_amount: args.sold_amount,
            payment_amount,
            treasury_amount,
            staking_vault_amount,
            reference_price_nad,
            auction_price_nad,
            remaining_fee_liability,
            remaining_buyback_liability,
            metadata: MarketEventMetadata::new(bidder_key, market_key)?,
        });
        Ok(())
    }
}

fn validate_recipient_payment_account(
    token_account: &InterfaceAccount<TokenAccount>,
    expected_owner: Pubkey,
    expected_mint: Pubkey,
) -> Result<()> {
    require_keys_eq!(token_account.owner, expected_owner, ErrorCode::InvalidRecipient);
    require_keys_eq!(token_account.mint, expected_mint, ErrorCode::InvalidMint);
    Ok(())
}

fn price_from_market(market: &Market, sold_mint: Pubkey, accepted_mint: Pubkey) -> Option<u64> {
    if sold_mint == market.base_side.asset_mint && accepted_mint == market.quote_side.asset_mint {
        Some(market.risk.base_price_ema_nad)
    } else if sold_mint == market.quote_side.asset_mint && accepted_mint == market.base_side.asset_mint {
        Some(market.risk.quote_price_ema_nad)
    } else {
        None
    }
}

fn expected_reference_market(
    market: &Market,
    market_key: Pubkey,
    lane: ProtocolAuctionLane,
    sold_mint: Pubkey,
    accepted_mint: Pubkey,
) -> Result<Pubkey> {
    if sold_mint == accepted_mint {
        return Ok(market_key);
    }
    let sold_side = market.asset_for_mint(sold_mint)?;
    let approved = market.side(sold_side).fees.protocol_auction_reference_market(lane);
    if approved != Pubkey::default() {
        return Ok(approved);
    }
    require!(
        price_from_market(market, sold_mint, accepted_mint).is_some(),
        ErrorCode::InvalidMarket
    );
    Ok(market_key)
}

fn assert_fresh_reference(last_snapshot_slot: u64, current_slot: u64, max_reference_age_slots: u64) -> Result<()> {
    require!(last_snapshot_slot > 0, ErrorCode::StaleAuctionReference);
    let age = current_slot.saturating_sub(last_snapshot_slot);
    require!(age <= max_reference_age_slots, ErrorCode::StaleAuctionReference);
    Ok(())
}

#[cfg(test)]
mod tests {
    include!("../../tests/instructions/futarchy/settle_protocol_auction.rs");
}
