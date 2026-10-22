use crate::{
    constants::*,
    errors::ErrorCode,
    generate_market_seeds,
    instructions::{
        accounts::{
            require_reserve_custody, require_supported_asset_mint, token_account_credit, token_program_for_mint,
            validate_owner_asset_account, validate_side_vault_accounts, HlpSwapAccountLayout,
        },
        leverage::settlement::settle_inline_hlp_accounts,
        leverage_collateral_fee, validate_hlp_authority_pdas,
    },
    state::*,
    token::transfer_checked_with_remaining_accounts,
    transitions::{liquidity::HlpFlashQuote, HlpYieldEligibility},
};
use anchor_lang::{
    prelude::*,
    solana_program::{
        instruction::{get_stack_height, TRANSACTION_LEVEL_STACK_HEIGHT},
        sysvar::instructions::{load_current_index_checked, load_instruction_at_checked},
    },
    InstructionData,
};
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount, TokenInterface},
};

pub const HLP_FLASH_SESSION_SEED: &[u8] = b"hlp-flash-session";
pub const HLP_FLASH_PAYMENT_SEED: &[u8] = b"hlp-flash-payment";

#[account]
#[derive(InitSpace)]
pub struct HlpFlashSession {
    pub market: Pubkey,
    pub buyer: Pubkey,
    pub quote: HlpFlashQuote,
    pub max_insurance_draw: u64,
    pub max_socialized_loss: u64,
    pub payment_balance_before: u64,
    pub hlp_supply: u64,
    pub begin_index: u16,
    pub settle_index: u16,
    pub slot: u64,
    pub bump: u8,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct BeginHlpFlashArgs {
    pub target_asset: u8,
    pub max_payment: u64,
    pub allow_full: bool,
    pub max_insurance_draw: u64,
    pub max_socialized_loss: u64,
    pub max_collateral_debit: u64,
    pub min_collateral_credit: u64,
    pub settle_index: u16,
}

#[event]
pub struct HlpFlashStarted {
    pub market: Pubkey,
    pub buyer: Pubkey,
    pub quote: HlpFlashQuote,
}
#[event]
pub struct HlpFlashSettled {
    pub market: Pubkey,
    pub buyer: Pubkey,
    pub quote: HlpFlashQuote,
    pub funding_interest_paid: [u64; 2],
    pub loss: LiquidationLossAllocation,
    pub insurance_debit: u64,
    pub surplus_credit: u64,
}

#[derive(Accounts)]
pub struct HlpFlashAccounts<'info> {
    #[account(mut)]
    pub market: Box<Account<'info, Market>>,
    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,
    #[account(mut)]
    pub buyer: Signer<'info>,
    pub target_mint: Box<InterfaceAccount<'info, Mint>>,
    pub borrowed_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut)]
    pub target_reserve: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub borrowed_reserve: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub buyer_collateral_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub buyer_refund_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub insurance_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
    pub borrowed_token_program: Interface<'info, TokenInterface>,
}
impl HlpFlashAccounts<'_> {
    fn validate(&self, target: MarketAsset) -> Result<()> {
        validate_hlp_authority_pdas(
            &self.market,
            self.market.key(),
            &self.futarchy_authority,
            self.futarchy_authority.key(),
        )?;
        self.market.assert_current_version()?;
        self.market.assert_started()?;
        require_supported_asset_mint(&self.target_mint)?;
        require_supported_asset_mint(&self.borrowed_mint)?;
        validate_side_vault_accounts(&self.market, target, &self.target_mint, &self.target_reserve)?;
        validate_side_vault_accounts(
            &self.market,
            target.opposite(),
            &self.borrowed_mint,
            &self.borrowed_reserve,
        )?;
        require_keys_eq!(self.target_reserve.owner, self.market.key(), ErrorCode::InvalidVault);
        require_keys_eq!(self.borrowed_reserve.owner, self.market.key(), ErrorCode::InvalidVault);
        let insurance = if target.opposite() == MarketAsset::Base {
            self.market.insurance.base_vault
        } else {
            self.market.insurance.quote_vault
        };
        require_keys_eq!(self.insurance_vault.key(), insurance, ErrorCode::InvalidVault);
        require_keys_eq!(self.insurance_vault.owner, self.market.key(), ErrorCode::InvalidVault);
        require_keys_eq!(
            self.insurance_vault.mint,
            self.borrowed_mint.key(),
            ErrorCode::InvalidMint
        );
        require_keys_eq!(
            self.borrowed_token_program.key(),
            *self.borrowed_mint.to_account_info().owner,
            ErrorCode::InvalidTokenProgram
        );
        validate_owner_asset_account(self.buyer.key(), &self.target_mint, &self.buyer_collateral_account)?;
        validate_owner_asset_account(self.buyer.key(), &self.borrowed_mint, &self.buyer_refund_account)
    }
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(args: BeginHlpFlashArgs)]
pub struct BeginHlpFlash<'info> {
    pub accounts: HlpFlashAccounts<'info>,
    #[account(init, payer = accounts.buyer, space = 8 + HlpFlashSession::INIT_SPACE,
        seeds = [HLP_FLASH_SESSION_SEED, accounts.market.key().as_ref(), &[args.target_asset]], bump)]
    pub session: Box<Account<'info, HlpFlashSession>>,
    #[account(init_if_needed, payer = accounts.buyer,
        seeds = [HLP_FLASH_PAYMENT_SEED, session.key().as_ref()], bump,
        token::mint = accounts.borrowed_mint, token::authority = session,
        token::token_program = accounts.borrowed_token_program)]
    pub repayment_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    /// CHECK: Canonical instructions sysvar.
    #[account(address = anchor_lang::solana_program::sysvar::instructions::ID)]
    pub instructions: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[event_cpi]
#[derive(Accounts)]
pub struct SettleHlpFlash<'info> {
    pub accounts: HlpFlashAccounts<'info>,
    #[account(mut,
        seeds = [HLP_FLASH_SESSION_SEED, accounts.market.key().as_ref(), &[session.quote.target_asset]], bump = session.bump)]
    pub session: Box<Account<'info, HlpFlashSession>>,
    #[account(mut, seeds = [HLP_FLASH_PAYMENT_SEED, session.key().as_ref()], bump,
        token::mint = accounts.borrowed_mint, token::authority = session,
        token::token_program = accounts.borrowed_token_program)]
    pub repayment_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    /// CHECK: Canonical instructions sysvar.
    #[account(address = anchor_lang::solana_program::sysvar::instructions::ID)]
    pub instructions: UncheckedAccount<'info>,
}

impl<'info> BeginHlpFlash<'info> {
    pub fn handle(ctx: Context<'_, '_, '_, 'info, Self>, args: BeginHlpFlashArgs) -> Result<()> {
        require_eq!(
            get_stack_height(),
            TRANSACTION_LEVEL_STACK_HEIGHT,
            ErrorCode::InvalidLiquidationSession
        );
        let a = &mut ctx.accounts.accounts;
        let target = MarketAsset::try_from_code(args.target_asset)?;
        a.validate(target)?;
        let clock = Clock::get()?;
        crate::instructions::accounting::accrue_market_interest(
            &mut a.market,
            crate::clock::MarketClock::from_clock(&clock)?,
            ctx.accounts.event_authority.to_account_info(),
        )?;
        a.market
            .advance_amm_clock(crate::clock::MarketClock::from_clock(&clock)?)?;
        let quote = a.market.quote_hlp_flash_recovery(
            target,
            leverage_collateral_fee(&a.target_mint, clock.epoch)?,
            leverage_collateral_fee(&a.borrowed_mint, clock.epoch)?,
        )?;
        require!(!quote.full || args.allow_full, ErrorCode::SlippageExceeded);
        require_gte!(args.max_payment, quote.payment, ErrorCode::SlippageExceeded);
        require_gte!(
            args.max_collateral_debit,
            quote.collateral_debit,
            ErrorCode::SlippageExceeded
        );
        require_gte!(
            quote.collateral_credit,
            args.min_collateral_credit,
            ErrorCode::SlippageExceeded
        );
        let begin_index = load_current_index_checked(&ctx.accounts.instructions)?;
        require_gt!(args.settle_index, begin_index, ErrorCode::InvalidLiquidationSession);
        let settle = load_instruction_at_checked(usize::from(args.settle_index), &ctx.accounts.instructions)?;
        require_keys_eq!(settle.program_id, crate::ID, ErrorCode::InvalidLiquidationSession);
        require!(
            settle.data == crate::instruction::SettleHlpFlash {}.data(),
            ErrorCode::InvalidLiquidationSession
        );
        let begin = load_instruction_at_checked(usize::from(begin_index), &ctx.accounts.instructions)?;
        let count = a.to_account_metas(None).len() + 2;
        require!(
            settle.accounts.len() >= count && begin.accounts.len() >= count,
            ErrorCode::InvalidLiquidationSession
        );
        for i in 0..count {
            require_keys_eq!(
                begin.accounts[i].pubkey,
                settle.accounts[i].pubkey,
                ErrorCode::InvalidLiquidationSession
            );
        }
        let hlp_supply = if target == MarketAsset::Base {
            a.market.base_hlp_vault.hlp_supply
        } else {
            a.market.quote_hlp_vault.hlp_supply
        };
        a.market.begin_hlp_flash_recovery(quote)?;
        ctx.accounts.session.set_inner(HlpFlashSession {
            market: a.market.key(),
            buyer: a.buyer.key(),
            quote,
            max_insurance_draw: args.max_insurance_draw,
            max_socialized_loss: args.max_socialized_loss,
            payment_balance_before: ctx.accounts.repayment_vault.amount,
            hlp_supply,
            begin_index,
            settle_index: args.settle_index,
            slot: clock.slot,
            bump: ctx.bumps.session,
        });
        let before = a.buyer_collateral_account.amount;
        transfer_checked_with_remaining_accounts(
            a.market.to_account_info(),
            a.target_reserve.to_account_info(),
            a.buyer_collateral_account.to_account_info(),
            a.target_mint.to_account_info(),
            token_program_for_mint(&a.target_mint, &a.token_program, &a.token_2022_program)?,
            quote.collateral_debit,
            a.target_mint.decimals,
            &[&generate_market_seeds!(a.market)[..]],
            ctx.remaining_accounts,
        )?;
        a.buyer_collateral_account.reload()?;
        a.target_reserve.reload()?;
        require_eq!(
            token_account_credit(before, &a.buyer_collateral_account)?,
            quote.collateral_credit,
            ErrorCode::BrokenInvariant
        );
        require_reserve_custody(a.target_reserve.amount, a.market.side(target))?;
        crate::instructions::accounting::emit_accounting_event(
            &HlpFlashStarted {
                market: a.market.key(),
                buyer: a.buyer.key(),
                quote,
            },
            ctx.accounts.event_authority.to_account_info(),
        )
    }
}

impl<'info> SettleHlpFlash<'info> {
    pub fn handle(ctx: Context<'_, '_, '_, 'info, Self>) -> Result<()> {
        require_eq!(
            get_stack_height(),
            TRANSACTION_LEVEL_STACK_HEIGHT,
            ErrorCode::InvalidLiquidationSession
        );
        let a = &mut ctx.accounts.accounts;
        let session = &ctx.accounts.session;
        let q = session.quote;
        let target = MarketAsset::try_from_code(q.target_asset)?;
        a.validate(target)?;
        require_keys_eq!(session.market, a.market.key(), ErrorCode::InvalidLiquidationSession);
        require_keys_eq!(session.buyer, a.buyer.key(), ErrorCode::InvalidLiquidationSession);
        require_eq!(
            load_current_index_checked(&ctx.accounts.instructions)?,
            session.settle_index,
            ErrorCode::InvalidLiquidationSession
        );
        let clock = Clock::get()?;
        require_eq!(clock.slot, session.slot, ErrorCode::InvalidLiquidationSession);
        let vault = if target == MarketAsset::Base {
            a.market.base_hlp_vault
        } else {
            a.market.quote_hlp_vault
        };
        require_eq!(
            vault.hlp_supply,
            session.hlp_supply,
            ErrorCode::InvalidLiquidationSession
        );
        let received = ctx
            .accounts
            .repayment_vault
            .amount
            .checked_sub(session.payment_balance_before)
            .ok_or(ErrorCode::InsufficientAmount)?;
        require_gte!(received, q.payment, ErrorCode::InsufficientAmount);
        let seeds: &[&[u8]] = &[
            HLP_FLASH_SESSION_SEED,
            session.market.as_ref(),
            &[q.target_asset],
            &[session.bump],
        ];
        let before = a.borrowed_reserve.amount;
        transfer_checked_with_remaining_accounts(
            session.to_account_info(),
            ctx.accounts.repayment_vault.to_account_info(),
            a.borrowed_reserve.to_account_info(),
            a.borrowed_mint.to_account_info(),
            a.borrowed_token_program.to_account_info(),
            q.payment,
            a.borrowed_mint.decimals,
            &[seeds],
            ctx.remaining_accounts,
        )?;
        a.borrowed_reserve.reload()?;
        require_eq!(
            token_account_credit(before, &a.borrowed_reserve)?,
            q.reserve_credit,
            ErrorCode::BrokenInvariant
        );
        let eligibility = HlpYieldEligibility {
            ylp_supply: a.market.base_side.shares.ylp_supply,
            base_hlp_ylp_shares: a.market.base_hlp_vault.ylp_shares,
            quote_hlp_ylp_shares: a.market.quote_hlp_vault.ylp_shares,
        };
        let layout = HlpSwapAccountLayout::try_from((&**a.market, ctx.remaining_accounts))?;
        let terminal = if q.full {
            a.market.settle_terminal_hlp_flash(
                q,
                leverage_collateral_fee(&a.borrowed_mint, clock.epoch)?,
                session.max_insurance_draw,
                session.max_socialized_loss,
                crate::clock::MarketClock::from_clock(&clock)?,
            )?
        } else {
            let (base, quote) = a
                .market
                .settle_hlp_flash_recovery(q, crate::clock::MarketClock::from_clock(&clock)?)?;
            crate::transitions::liquidity::HlpFlashTerminalSettlement {
                base,
                quote,
                ..Default::default()
            }
        };
        let (base, quote) = (terminal.base, terminal.quote);
        if terminal.insurance_debit > 0 {
            let before = a.borrowed_reserve.amount;
            transfer_checked_with_remaining_accounts(
                a.market.to_account_info(),
                a.insurance_vault.to_account_info(),
                a.borrowed_reserve.to_account_info(),
                a.borrowed_mint.to_account_info(),
                a.borrowed_token_program.to_account_info(),
                terminal.insurance_debit,
                a.borrowed_mint.decimals,
                &[&generate_market_seeds!(a.market)[..]],
                ctx.remaining_accounts,
            )?;
            a.borrowed_reserve.reload()?;
            require_eq!(
                token_account_credit(before, &a.borrowed_reserve)?,
                terminal.loss.insurance_credit,
                ErrorCode::BrokenInvariant
            );
        }
        if terminal.surplus_debit > 0 {
            let index = if target == MarketAsset::Base { 4 } else { 3 };
            let interest = &ctx.remaining_accounts[index];
            let before = crate::instructions::accounts::token_account_info_amount(interest)?;
            transfer_checked_with_remaining_accounts(
                a.market.to_account_info(),
                a.borrowed_reserve.to_account_info(),
                interest.clone(),
                a.borrowed_mint.to_account_info(),
                a.borrowed_token_program.to_account_info(),
                terminal.surplus_debit,
                a.borrowed_mint.decimals,
                &[&generate_market_seeds!(a.market)[..]],
                layout.hook_accounts(ctx.remaining_accounts),
            )?;
            require_eq!(
                crate::instructions::accounts::token_account_info_credit(before, interest)?,
                terminal.surplus_credit,
                ErrorCode::BrokenInvariant
            );
        }
        let target_accounts = (a.target_mint.to_account_info(), a.target_reserve.to_account_info());
        let borrowed_accounts = (a.borrowed_mint.to_account_info(), a.borrowed_reserve.to_account_info());
        let assets = if target == MarketAsset::Base {
            [target_accounts, borrowed_accounts]
        } else {
            [borrowed_accounts, target_accounts]
        };
        settle_inline_hlp_accounts(
            &mut a.market,
            &a.futarchy_authority,
            assets,
            &a.token_program,
            &a.token_2022_program,
            ctx.remaining_accounts,
            layout,
            base,
            quote,
            eligibility,
            ctx.accounts.event_authority.to_account_info(),
        )?;
        let refund = received - q.payment;
        if refund > 0 {
            transfer_checked_with_remaining_accounts(
                session.to_account_info(),
                ctx.accounts.repayment_vault.to_account_info(),
                a.buyer_refund_account.to_account_info(),
                a.borrowed_mint.to_account_info(),
                a.borrowed_token_program.to_account_info(),
                refund,
                a.borrowed_mint.decimals,
                &[seeds],
                ctx.remaining_accounts,
            )?;
        }
        a.target_reserve.reload()?;
        a.borrowed_reserve.reload()?;
        require_reserve_custody(a.target_reserve.amount, a.market.side(target))?;
        require_reserve_custody(a.borrowed_reserve.amount, a.market.side(target.opposite()))?;
        crate::instructions::accounting::emit_accounting_event(
            &HlpFlashSettled {
                market: a.market.key(),
                buyer: a.buyer.key(),
                quote: q,
                funding_interest_paid: [base.interest_paid, quote.interest_paid],
                loss: terminal.loss,
                insurance_debit: terminal.insurance_debit,
                surplus_credit: terminal.surplus_credit,
            },
            ctx.accounts.event_authority.to_account_info(),
        )?;
        ctx.accounts.session.close(a.buyer.to_account_info())
    }
}

#[derive(Accounts)]
pub struct PreviewHlpFlash<'info> {
    pub market: Box<Account<'info, Market>>,
    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,
    pub target_mint: Box<InterfaceAccount<'info, Mint>>,
    pub borrowed_mint: Box<InterfaceAccount<'info, Mint>>,
}
impl PreviewHlpFlash<'_> {
    pub fn handle(ctx: Context<Self>, target_asset: u8) -> Result<HlpFlashQuote> {
        let a = ctx.accounts;
        let target = MarketAsset::try_from_code(target_asset)?;
        validate_hlp_authority_pdas(
            &a.market,
            a.market.key(),
            &a.futarchy_authority,
            a.futarchy_authority.key(),
        )?;
        a.market.assert_current_version()?;
        a.market.assert_started()?;
        require_keys_eq!(
            a.target_mint.key(),
            a.market.side(target).asset_mint,
            ErrorCode::InvalidMint
        );
        require_keys_eq!(
            a.borrowed_mint.key(),
            a.market.side(target.opposite()).asset_mint,
            ErrorCode::InvalidMint
        );
        require_supported_asset_mint(&a.target_mint)?;
        require_supported_asset_mint(&a.borrowed_mint)?;
        let clock = Clock::get()?;
        a.market
            .accrue_interest_to_clock(crate::clock::MarketClock::from_clock(&clock)?)?;
        a.market
            .advance_amm_clock(crate::clock::MarketClock::from_clock(&clock)?)?;
        a.market.quote_hlp_flash_recovery(
            target,
            leverage_collateral_fee(&a.target_mint, clock.epoch)?,
            leverage_collateral_fee(&a.borrowed_mint, clock.epoch)?,
        )
    }
}
