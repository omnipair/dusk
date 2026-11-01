use crate::{
    account::get_size_with_discriminator,
    constants::{MARKET_V2_SEED_PREFIX, YIELD_ACCOUNT_SEED_PREFIX},
    errors::ErrorCode,
    generate_market_seeds,
    instructions::accounts::validate_owner_lp_account,
    state::{JuniorExitAccount, Market, MarketAsset, YieldAccount, YieldTokenKind},
    token::{token_burn, token_mint_to},
};
use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, Token2022, TokenAccount};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub enum TrancheAction {
    Enter { senior_amount: u64, minimum_junior: u64 },
    RequestExit { additional_junior: u64 },
    ConvertExit { minimum_senior: u64 },
    CancelExit,
}

#[event]
pub struct TrancheConverted {
    pub market: Pubkey,
    pub owner: Pubkey,
    pub action: u8,
    pub senior_amount: u64,
    pub junior_amount: u64,
    pub junior_fee: u64,
    pub ready_at: u64,
    pub expires_at: u64,
}

#[event_cpi]
#[derive(Accounts)]
pub struct ConvertTranche<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,
    #[account(mut)]
    pub market: Box<Account<'info, Market>>,
    #[account(mut, address = market.ylp_mint)]
    pub senior_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut, address = market.junior_mint)]
    pub junior_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut)]
    pub owner_senior: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub owner_junior: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(init_if_needed, payer = owner, space = get_size_with_discriminator::<JuniorExitAccount>(),
        seeds = [b"junior_exit", market.key().as_ref(), owner.key().as_ref(), junior_mint.key().as_ref()], bump)]
    pub exit: Box<Account<'info, JuniorExitAccount>>,
    #[account(init_if_needed, payer = owner, space = get_size_with_discriminator::<YieldAccount>(),
        seeds = [YIELD_ACCOUNT_SEED_PREFIX, market.key().as_ref(), owner.key().as_ref(), senior_mint.key().as_ref(), market.base_side.asset_mint.as_ref(), &[YieldTokenKind::Ylp.code()]], bump)]
    pub senior_base_yield: Box<Account<'info, YieldAccount>>,
    #[account(init_if_needed, payer = owner, space = get_size_with_discriminator::<YieldAccount>(),
        seeds = [YIELD_ACCOUNT_SEED_PREFIX, market.key().as_ref(), owner.key().as_ref(), senior_mint.key().as_ref(), market.quote_side.asset_mint.as_ref(), &[YieldTokenKind::Ylp.code()]], bump)]
    pub senior_quote_yield: Box<Account<'info, YieldAccount>>,
    #[account(init_if_needed, payer = owner, space = get_size_with_discriminator::<YieldAccount>(),
        seeds = [YIELD_ACCOUNT_SEED_PREFIX, market.key().as_ref(), owner.key().as_ref(), junior_mint.key().as_ref(), market.base_side.asset_mint.as_ref(), &[YieldTokenKind::Junior.code()]], bump)]
    pub junior_base_yield: Box<Account<'info, YieldAccount>>,
    #[account(init_if_needed, payer = owner, space = get_size_with_discriminator::<YieldAccount>(),
        seeds = [YIELD_ACCOUNT_SEED_PREFIX, market.key().as_ref(), owner.key().as_ref(), junior_mint.key().as_ref(), market.quote_side.asset_mint.as_ref(), &[YieldTokenKind::Junior.code()]], bump)]
    pub junior_quote_yield: Box<Account<'info, YieldAccount>>,
    pub token_2022_program: Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
}

impl<'info> ConvertTranche<'info> {
    pub fn handle_convert(ctx: Context<'_, '_, '_, 'info, Self>, action: TrancheAction) -> Result<()> {
        let a = &mut *ctx.accounts;
        let owner = a.owner.key();
        let market_key = a.market.key();
        a.market.assert_current_version()?;
        validate_owner_lp_account(owner, &a.senior_mint, &a.owner_senior)?;
        validate_owner_lp_account(owner, &a.junior_mint, &a.owner_junior)?;
        let slot = Clock::get()?.slot;
        crate::instructions::accounting::accrue_market_interest(
            &mut a.market,
            slot,
            a.event_authority.to_account_info(),
        )?;
        a.market.reconcile_junior_receipts(a.junior_mint.supply)?;
        a.market.checkpoint_junior_seed()?;
        if a.exit.owner == Pubkey::default() {
            a.exit.owner = owner;
            a.exit.market = market_key;
            a.exit.mint = a.junior_mint.key();
            a.exit.bump = ctx.bumps.exit;
        }
        require_keys_eq!(a.exit.owner, owner, ErrorCode::InvalidSigner);
        require_keys_eq!(a.exit.market, market_key, ErrorCode::InvalidMarket);
        require_keys_eq!(a.exit.mint, a.junior_mint.key(), ErrorCode::InvalidLpMintKey);
        super::super::initialize_or_validate_yield_account(
            &mut a.senior_base_yield,
            owner,
            market_key,
            a.senior_mint.key(),
            a.market.base_side.asset_mint,
            YieldTokenKind::Ylp,
            ctx.bumps.senior_base_yield,
        )?;
        super::super::initialize_or_validate_yield_account(
            &mut a.senior_quote_yield,
            owner,
            market_key,
            a.senior_mint.key(),
            a.market.quote_side.asset_mint,
            YieldTokenKind::Ylp,
            ctx.bumps.senior_quote_yield,
        )?;
        super::super::initialize_or_validate_yield_account(
            &mut a.junior_base_yield,
            owner,
            market_key,
            a.junior_mint.key(),
            a.market.base_side.asset_mint,
            YieldTokenKind::Junior,
            ctx.bumps.junior_base_yield,
        )?;
        super::super::initialize_or_validate_yield_account(
            &mut a.junior_quote_yield,
            owner,
            market_key,
            a.junior_mint.key(),
            a.market.quote_side.asset_mint,
            YieldTokenKind::Junior,
            ctx.bumps.junior_quote_yield,
        )?;
        for asset in [MarketAsset::Base, MarketAsset::Quote] {
            let (senior, junior) = if asset == MarketAsset::Base {
                (&mut a.senior_base_yield, &mut a.junior_base_yield)
            } else {
                (&mut a.senior_quote_yield, &mut a.junior_quote_yield)
            };
            require_eq!(
                junior.escrow_balance,
                a.exit.ticket.receipts,
                ErrorCode::BrokenInvariant
            );
            a.market.checkpoint_holder_income(senior, a.owner_senior.amount, None)?;
            a.market.checkpoint_holder_income(junior, a.owner_junior.amount, None)?;
            let fees = &a.market.side(asset).fees;
            senior.accrue(
                a.owner_senior.amount,
                fees.swap_fee_growth_index_q64,
                fees.interest_growth_index_q64,
            )?;
            junior.accrue(
                a.owner_junior.amount,
                fees.junior.swap_growth_q64,
                fees.junior.interest_growth_q64,
            )?;
        }
        let mut receipt = TrancheConverted {
            market: market_key,
            owner,
            action: 0,
            senior_amount: 0,
            junior_amount: 0,
            junior_fee: 0,
            ready_at: 0,
            expires_at: 0,
        };
        match action {
            TrancheAction::Enter {
                senior_amount,
                minimum_junior,
            } => {
                require_gte!(a.owner_senior.amount, senior_amount, ErrorCode::InsufficientBalance);
                let supply = a.market.base_side.shares.ylp_supply;
                let junior_amount = a.market.tranches.enter(senior_amount, supply)?;
                require_gte!(junior_amount, minimum_junior, ErrorCode::InsufficientOutputAmount);
                token_burn(
                    a.owner.to_account_info(),
                    a.token_2022_program.to_account_info(),
                    a.senior_mint.to_account_info(),
                    a.owner_senior.to_account_info(),
                    senior_amount,
                    &[],
                )?;
                token_mint_to(
                    a.market.to_account_info(),
                    a.token_2022_program.to_account_info(),
                    a.junior_mint.to_account_info(),
                    a.owner_junior.to_account_info(),
                    junior_amount,
                    &[&generate_market_seeds!(a.market)[..]],
                )?;
                receipt.senior_amount = senior_amount;
                receipt.junior_amount = junior_amount;
            }
            TrancheAction::RequestExit { additional_junior } => {
                require_gte!(a.owner_junior.amount, additional_junior, ErrorCode::InsufficientBalance);
                let config = a.market.junior_exit_config;
                receipt.junior_fee = a
                    .exit
                    .ticket
                    .request(&mut a.market.tranches, &config, additional_junior, slot)?;
                token_burn(
                    a.owner.to_account_info(),
                    a.token_2022_program.to_account_info(),
                    a.junior_mint.to_account_info(),
                    a.owner_junior.to_account_info(),
                    additional_junior,
                    &[],
                )?;
                receipt.action = 1;
                receipt.junior_amount = additional_junior;
            }
            TrancheAction::ConvertExit { minimum_senior } => {
                // Flash recovery begins and settles within one transaction.
                // These authenticated receivables cannot span clock slots, so
                // an active session pauses no measurable exercise-window time.
                let blocked = a.market.base_side.reserves.flash_receivable > 0
                    || a.market.quote_side.reserves.flash_receivable > 0
                    || a.market.base_hlp_vault.flash_collateral_debit > 0
                    || a.market.quote_hlp_vault.flash_collateral_debit > 0;
                receipt.junior_amount = a.exit.ticket.receipts;
                let senior_amount = a.exit.ticket.convert(&mut a.market.tranches, slot, 0, blocked)?;
                require_gte!(senior_amount, minimum_senior, ErrorCode::InsufficientOutputAmount);
                token_mint_to(
                    a.market.to_account_info(),
                    a.token_2022_program.to_account_info(),
                    a.senior_mint.to_account_info(),
                    a.owner_senior.to_account_info(),
                    senior_amount,
                    &[&generate_market_seeds!(a.market)[..]],
                )?;
                receipt.action = 2;
                receipt.senior_amount = senior_amount;
            }
            TrancheAction::CancelExit => {
                let junior_amount = a.exit.ticket.cancel(&mut a.market.tranches, slot)?;
                token_mint_to(
                    a.market.to_account_info(),
                    a.token_2022_program.to_account_info(),
                    a.junior_mint.to_account_info(),
                    a.owner_junior.to_account_info(),
                    junior_amount,
                    &[&generate_market_seeds!(a.market)[..]],
                )?;
                receipt.action = 3;
                receipt.junior_amount = junior_amount;
            }
        }
        a.junior_base_yield.escrow_balance = a.exit.ticket.receipts;
        a.junior_quote_yield.escrow_balance = a.exit.ticket.receipts;
        a.market.synchronize_tranche_income();
        a.market.assert_market_invariants()?;
        receipt.ready_at = a.exit.ticket.ready_at;
        receipt.expires_at = a.exit.ticket.expires_at;
        emit_cpi!(receipt);
        Ok(())
    }
}
