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

use crate::{
    constants::*,
    errors::ErrorCode,
    generate_market_seeds,
    instructions::{
        accounts::{require_reserve_custody, require_supported_asset_mint, token_program_for_mint},
        leverage_collateral_fee, leverage_collateral_liquidation_fee,
    },
    state::*,
    token::transfer_checked_with_remaining_accounts,
    transitions::{flash_liquidation::LiquidationPositionRef, flash_settlement::LiquidationPositionMut},
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct BeginFlashLiquidationArgs {
    pub position: Pubkey,
    pub debt_asset: u8,
    pub max_repayment: u64,
    pub max_payment: u64,
    pub min_collateral_credit: u64,
    pub full: bool,
    pub settle_index: u16,
}

#[derive(Accounts)]
pub struct FlashLiquidationAccounts<'info> {
    #[account(mut, seeds = [MARKET_V2_SEED_PREFIX, market.base_side.asset_mint.as_ref(), market.quote_side.asset_mint.as_ref(), market.params_hash.as_ref()], bump = market.bump)]
    pub market: Box<Account<'info, Market>>,
    #[account(mut)]
    pub borrow_position: Option<Box<Account<'info, BorrowPosition>>>,
    #[account(mut)]
    pub leverage_position: Option<Box<Account<'info, LeveragePosition>>>,
    #[account(mut)]
    pub buyer: Signer<'info>,
    pub collateral_mint: Box<InterfaceAccount<'info, Mint>>,
    pub debt_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut)]
    pub collateral_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub interest_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub insurance_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub buyer_collateral_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub buyer_refund_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub owner_debt_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(seeds = [FUTARCHY_AUTHORITY_SEED_PREFIX], bump = futarchy_authority.bump)]
    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,
    pub referral_partner: Option<Box<Account<'info, ReferralPartner>>>,
    #[account(mut)]
    pub referral_accrual: Option<Box<Account<'info, ReferralAccrual>>>,
    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
    pub debt_token_program: Interface<'info, TokenInterface>,
}

impl FlashLiquidationAccounts<'_> {
    pub(super) fn position(&self) -> Result<LiquidationPositionRef<'_>> {
        match (&self.borrow_position, &self.leverage_position) {
            (Some(p), None) => Ok(LiquidationPositionRef::Borrow(p)),
            (None, Some(p)) => Ok(LiquidationPositionRef::Leverage(p)),
            _ => err!(ErrorCode::InvalidLiquidationSession),
        }
    }

    pub(super) fn position_key(&self) -> Result<Pubkey> {
        match (&self.borrow_position, &self.leverage_position) {
            (Some(p), None) => Ok(p.key()),
            (None, Some(p)) => Ok(p.key()),
            _ => err!(ErrorCode::InvalidLiquidationSession),
        }
    }

    pub(super) fn owner(&self) -> Result<Pubkey> {
        match self.position()? {
            LiquidationPositionRef::Borrow(p) => Ok(p.owner),
            LiquidationPositionRef::Leverage(p) => Ok(p.owner),
        }
    }

    pub(super) fn referral(&self, asset: MarketAsset) -> Result<(Pubkey, u16)> {
        match self.position()? {
            LiquidationPositionRef::Borrow(p) => Ok((p.referral_partner(asset), p.referral_interest_share_bps(asset))),
            LiquidationPositionRef::Leverage(p) => Ok((p.referral_partner, p.referral_interest_share_bps)),
        }
    }

    pub(super) fn validate(&self, asset: MarketAsset) -> Result<()> {
        self.market.assert_started()?;
        require_supported_asset_mint(&self.debt_mint)?;
        require_supported_asset_mint(&self.collateral_mint)?;
        require_keys_eq!(
            self.debt_token_program.key(),
            *self.debt_mint.to_account_info().owner,
            ErrorCode::InvalidTokenProgram
        );
        let side = self.market.side(asset);
        require_keys_eq!(self.debt_mint.key(), side.asset_mint, ErrorCode::InvalidMint);
        require_keys_eq!(
            self.collateral_mint.key(),
            self.market.side(asset.opposite()).asset_mint,
            ErrorCode::InvalidMint
        );
        require_keys_eq!(self.reserve_vault.key(), side.reserve_vault, ErrorCode::InvalidVault);
        require_keys_eq!(self.interest_vault.key(), side.interest_vault, ErrorCode::InvalidVault);
        let insurance = match asset {
            MarketAsset::Base => self.market.insurance.base_vault,
            MarketAsset::Quote => self.market.insurance.quote_vault,
        };
        require_keys_eq!(self.insurance_vault.key(), insurance, ErrorCode::InvalidVault);
        for account in [&self.reserve_vault, &self.interest_vault, &self.insurance_vault] {
            require_keys_eq!(account.owner, self.market.key(), ErrorCode::InvalidVault);
            require_keys_eq!(account.mint, self.debt_mint.key(), ErrorCode::InvalidMint);
        }
        let expected_collateral = match self.position()? {
            LiquidationPositionRef::Borrow(p) => {
                require_keys_eq!(p.market, self.market.key(), ErrorCode::InvalidBorrowPosition);
                let key = Pubkey::create_program_address(
                    &[
                        BORROW_POSITION_SEED_PREFIX,
                        p.market.as_ref(),
                        p.owner.as_ref(),
                        p.position_id.as_ref(),
                        &[p.bump],
                    ],
                    &crate::ID,
                )
                .map_err(|_| ErrorCode::InvalidBorrowPosition)?;
                require_keys_eq!(key, self.position_key()?, ErrorCode::InvalidBorrowPosition);
                self.market.side(asset.opposite()).collateral_vault
            }
            LiquidationPositionRef::Leverage(p) => {
                require_keys_eq!(p.market, self.market.key(), ErrorCode::InvalidLeveragePosition);
                require!(p.debt_asset()? == asset, ErrorCode::InvalidLeveragePosition);
                let key = Pubkey::create_program_address(
                    &[
                        LEVERAGE_POSITION_SEED_PREFIX,
                        p.market.as_ref(),
                        p.owner.as_ref(),
                        p.namespace_authority.as_ref(),
                        p.position_id.as_ref(),
                        &[p.bump],
                    ],
                    &crate::ID,
                )
                .map_err(|_| ErrorCode::InvalidLeveragePosition)?;
                require_keys_eq!(key, self.position_key()?, ErrorCode::InvalidLeveragePosition);
                Pubkey::find_program_address(
                    &[
                        LEVERAGE_COLLATERAL_VAULT_SEED_PREFIX,
                        self.market.key().as_ref(),
                        self.collateral_mint.key().as_ref(),
                    ],
                    &crate::ID,
                )
                .0
            }
        };
        require_keys_eq!(
            self.collateral_vault.key(),
            expected_collateral,
            ErrorCode::InvalidVault
        );
        require_keys_eq!(self.collateral_vault.owner, self.market.key(), ErrorCode::InvalidVault);
        require_keys_eq!(
            self.collateral_vault.mint,
            self.collateral_mint.key(),
            ErrorCode::InvalidMint
        );
        for (account, owner, mint) in [
            (
                &self.buyer_collateral_account,
                self.buyer.key(),
                self.collateral_mint.key(),
            ),
            (&self.buyer_refund_account, self.buyer.key(), self.debt_mint.key()),
            (&self.owner_debt_account, self.owner()?, self.debt_mint.key()),
        ] {
            require_keys_eq!(account.owner, owner, ErrorCode::InvalidTokenAccount);
            require_keys_eq!(account.mint, mint, ErrorCode::InvalidMint);
        }
        let (partner, share) = self.referral(asset)?;
        crate::instructions::referral::accounting::validate_referral_binding(
            None,
            partner,
            share,
            true,
            &self.futarchy_authority,
            self.referral_partner.as_deref(),
            self.referral_accrual.as_deref(),
            self.market.key(),
            &self.debt_mint,
        )?;
        Ok(())
    }
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(args: BeginFlashLiquidationArgs)]
pub struct BeginFlashLiquidation<'info> {
    pub accounts: FlashLiquidationAccounts<'info>,
    #[account(init, payer = accounts.buyer, space = 8 + LiquidationSession::INIT_SPACE,
        seeds = [LIQUIDATION_SESSION_SEED, args.position.as_ref()], bump)]
    pub session: Box<Account<'info, LiquidationSession>>,
    #[account(init_if_needed, payer = accounts.buyer,
        seeds = [LIQUIDATION_PAYMENT_SEED, args.position.as_ref(), accounts.debt_mint.key().as_ref()], bump,
        token::mint = accounts.debt_mint, token::authority = session,
        token::token_program = accounts.debt_token_program)]
    pub repayment_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    /// CHECK: Canonical instructions sysvar, used with checked deserialization.
    #[account(address = anchor_lang::solana_program::sysvar::instructions::ID)]
    pub instructions: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[event_cpi]
#[derive(Accounts)]
pub struct SettleFlashLiquidation<'info> {
    pub accounts: FlashLiquidationAccounts<'info>,
    #[account(mut,
        seeds = [LIQUIDATION_SESSION_SEED, session.position.as_ref()], bump = session.bump)]
    pub session: Box<Account<'info, LiquidationSession>>,
    #[account(mut, address = session.repayment_vault,
        token::mint = accounts.debt_mint, token::authority = session,
        token::token_program = accounts.debt_token_program)]
    pub repayment_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    /// Supplying the canonical opposite reserve selects direct Dusk execution
    /// with principal repayment netted inside the AMM settlement.
    #[account(mut)]
    pub collateral_reserve_vault: Option<Box<InterfaceAccount<'info, TokenAccount>>>,
    /// CHECK: Canonical instructions sysvar, used with checked deserialization.
    #[account(address = anchor_lang::solana_program::sysvar::instructions::ID)]
    pub instructions: UncheckedAccount<'info>,
}

#[event]
pub struct FlashLiquidationBegun {
    pub market: Pubkey,
    pub position: Pubkey,
    pub session: Pubkey,
    pub buyer: Pubkey,
    pub debt_asset: u8,
    pub quote: FlashLiquidationQuote,
}

fn check_top_level() -> Result<()> {
    require_eq!(
        get_stack_height(),
        TRANSACTION_LEVEL_STACK_HEIGHT,
        ErrorCode::InvalidLiquidationSession
    );
    Ok(())
}

pub(super) fn emit_liquidation_interest<'info>(
    accounts: &FlashLiquidationAccounts<'info>,
    asset: MarketAsset,
    receipt: &crate::instructions::referral::accounting::ReferralInterestAccrualReceipt,
    event_authority: AccountInfo<'info>,
    slot: u64,
) -> Result<()> {
    let position = accounts.position_key()?;
    crate::instructions::accounting::emit_interest_paid(
        &accounts.market,
        asset,
        if accounts.leverage_position.is_some() {
            crate::events::DebtSource::Margin
        } else {
            crate::events::DebtSource::Credit
        },
        Some(position),
        receipt.quote,
        0,
        event_authority.clone(),
    )?;
    if let Some(event) = crate::instructions::referral::accounting::referral_interest_accrued_event_at_slot(
        receipt,
        accounts.market.key(),
        position,
        accounts.owner()?,
        accounts.buyer.key(),
        accounts.debt_mint.key(),
        slot,
    )? {
        crate::instructions::accounting::emit_accounting_event(&event, event_authority)?;
    }
    Ok(())
}

impl<'info> BeginFlashLiquidation<'info> {
    pub fn handle(ctx: Context<'_, '_, '_, 'info, Self>, args: BeginFlashLiquidationArgs) -> Result<()> {
        check_top_level()?;
        let clock = Clock::get()?;
        let asset = MarketAsset::try_from_code(args.debt_asset)?;
        let a = &mut ctx.accounts.accounts;
        crate::instructions::accounting::accrue_market_interest(
            &mut a.market,
            clock.slot,
            ctx.accounts.event_authority.to_account_info(),
        )?;
        a.market.update()?;
        a.validate(asset)?;
        require_keys_eq!(a.position_key()?, args.position, ErrorCode::InvalidLiquidationSession);
        a.position()?.require_idle()?;
        let begin_index = load_current_index_checked(&ctx.accounts.instructions)?;
        require_gt!(args.settle_index, begin_index, ErrorCode::InvalidLiquidationSession);
        let settle = load_instruction_at_checked(args.settle_index as usize, &ctx.accounts.instructions)?;
        require_keys_eq!(settle.program_id, crate::ID, ErrorCode::InvalidLiquidationSession);
        require!(
            settle.data == crate::instruction::SettleFlashLiquidation {}.data(),
            ErrorCode::InvalidLiquidationSession
        );
        // Both instructions begin with the same flattened economic accounts.
        // Compare even optional placeholder keys, then bind the session/custody.
        let begin = load_instruction_at_checked(begin_index as usize, &ctx.accounts.instructions)?;
        let count = a.to_account_metas(None).len();
        require!(
            begin.accounts.len() >= count + 2 && settle.accounts.len() >= count + 2,
            ErrorCode::InvalidLiquidationSession
        );
        for index in 0..count + 2 {
            require_keys_eq!(
                begin.accounts[index].pubkey,
                settle.accounts[index].pubkey,
                ErrorCode::InvalidLiquidationSession
            );
        }
        let eligibility_fee = leverage_collateral_liquidation_fee(&a.collateral_mint, clock.epoch)?;
        let health = a
            .market
            .flash_liquidation_health(&a.position()?, asset, eligibility_fee)?;
        require!(health.eligible()?, ErrorCode::PositionNotLiquidatable);
        match (&mut a.borrow_position, &mut a.leverage_position) {
            (Some(p), None) => p.distress_mut(asset).observe(true, clock.unix_timestamp, clock.slot)?,
            (None, Some(p)) => p.distress.observe(true, clock.unix_timestamp, clock.slot)?,
            _ => return err!(ErrorCode::InvalidLiquidationSession),
        }
        let quote = a.market.quote_flash_liquidation(
            &a.position()?,
            asset,
            args.max_repayment,
            args.full,
            leverage_collateral_fee(&a.collateral_mint, clock.epoch)?,
            eligibility_fee,
            leverage_collateral_fee(&a.debt_mint, clock.epoch)?,
            clock.unix_timestamp,
            clock.slot,
        )?;
        require_gte!(args.max_payment, quote.payment, ErrorCode::SlippageExceeded);
        require_gte!(
            quote.collateral_credit,
            args.min_collateral_credit,
            ErrorCode::SlippageExceeded
        );
        let session_key = ctx.accounts.session.key();
        let principal = match a.position()? {
            LiquidationPositionRef::Borrow(_) => 0,
            LiquidationPositionRef::Leverage(p) => p.debt_principal,
        };
        **ctx.accounts.session = LiquidationSession {
            market: a.market.key(),
            position: args.position,
            owner: a.owner()?,
            buyer: a.buyer.key(),
            collateral_mint: a.collateral_mint.key(),
            debt_mint: a.debt_mint.key(),
            collateral_vault: a.collateral_vault.key(),
            buyer_collateral_account: a.buyer_collateral_account.key(),
            repayment_vault: ctx.accounts.repayment_vault.key(),
            buyer_refund_account: a.buyer_refund_account.key(),
            owner_debt_account: a.owner_debt_account.key(),
            original_collateral: a.position()?.collateral(asset),
            original_debt_shares: a.position()?.shares(asset),
            original_principal: principal,
            borrow_index: a.market.debt.borrow_index(asset),
            payment_balance_before: ctx.accounts.repayment_vault.amount,
            insurance_balance_at_quote: a.market.insurance.available(asset),
            insurance_target_at_quote: a.market.liquidation_insurance_target(asset)?,
            quote,
            debt_asset: asset.code(),
            leverage: a.leverage_position.is_some(),
            begin_index,
            settle_index: args.settle_index,
            slot: clock.slot,
            bump: ctx.bumps.session,
        };
        match (&mut a.borrow_position, &mut a.leverage_position) {
            (Some(p), None) => {
                p.active_liquidation_session = session_key;
                p.exit(&crate::ID)?;
            }
            (None, Some(p)) => {
                p.active_liquidation_session = session_key;
                p.exit(&crate::ID)?;
            }
            _ => return err!(ErrorCode::InvalidLiquidationSession),
        }
        ctx.accounts.session.exit(&crate::ID)?;
        let balance = a.buyer_collateral_account.amount;
        transfer_checked_with_remaining_accounts(
            a.market.to_account_info(),
            a.collateral_vault.to_account_info(),
            a.buyer_collateral_account.to_account_info(),
            a.collateral_mint.to_account_info(),
            token_program_for_mint(&a.collateral_mint, &a.token_program, &a.token_2022_program)?,
            quote.collateral_debit,
            a.collateral_mint.decimals,
            &[&generate_market_seeds!(a.market)[..]],
            ctx.remaining_accounts,
        )?;
        a.buyer_collateral_account.reload()?;
        require_eq!(
            a.buyer_collateral_account
                .amount
                .checked_sub(balance)
                .ok_or(ErrorCode::BrokenInvariant)?,
            quote.collateral_credit,
            ErrorCode::BrokenInvariant
        );
        emit_cpi!(FlashLiquidationBegun {
            market: a.market.key(),
            position: args.position,
            session: session_key,
            buyer: a.buyer.key(),
            debt_asset: asset.code(),
            quote
        });
        Ok(())
    }
}

#[event]
pub struct FlashLiquidationSettled {
    pub market: Pubkey,
    pub position: Pubkey,
    pub buyer: Pubkey,
    pub leverage: bool,
    pub debt_asset: u8,
    pub collateral_debit: u64,
    pub debt_repaid: u64,
    pub owner_surplus: u64,
    pub fee: LiquidationFeeAllocation,
    pub loss: LiquidationLossAllocation,
    pub remaining_collateral: u64,
    pub remaining_debt: u64,
}

impl<'info> SettleFlashLiquidation<'info> {
    pub fn handle(mut ctx: Context<'_, '_, '_, 'info, Self>) -> Result<()> {
        check_top_level()?;
        let clock = Clock::get()?;
        let session = &ctx.accounts.session;
        let asset = MarketAsset::try_from_code(session.debt_asset)?;
        let index = load_current_index_checked(&ctx.accounts.instructions)?;
        require_eq!(index, session.settle_index, ErrorCode::InvalidLiquidationSession);
        require_eq!(clock.slot, session.slot, ErrorCode::InvalidLiquidationSession);
        let a = &mut ctx.accounts.accounts;
        a.validate(asset)?;
        require_keys_eq!(a.market.key(), session.market, ErrorCode::InvalidLiquidationSession);
        require_keys_eq!(
            a.position_key()?,
            session.position,
            ErrorCode::InvalidLiquidationSession
        );
        require_keys_eq!(a.owner()?, session.owner, ErrorCode::InvalidLiquidationSession);
        require_keys_eq!(a.buyer.key(), session.buyer, ErrorCode::InvalidLiquidationSession);
        require_keys_eq!(
            a.owner_debt_account.key(),
            session.owner_debt_account,
            ErrorCode::InvalidLiquidationSession
        );
        require_keys_eq!(
            a.buyer_refund_account.key(),
            session.buyer_refund_account,
            ErrorCode::InvalidLiquidationSession
        );
        require_keys_eq!(
            a.buyer_collateral_account.key(),
            session.buyer_collateral_account,
            ErrorCode::InvalidLiquidationSession
        );
        require_keys_eq!(
            a.collateral_vault.key(),
            session.collateral_vault,
            ErrorCode::InvalidLiquidationSession
        );
        require!(
            a.leverage_position.is_some() == session.leverage,
            ErrorCode::InvalidLiquidationSession
        );
        require_eq!(
            a.position()?.collateral(asset),
            session.original_collateral,
            ErrorCode::InvalidLiquidationSession
        );
        require_eq!(
            a.position()?.shares(asset),
            session.original_debt_shares,
            ErrorCode::InvalidLiquidationSession
        );
        require_eq!(
            a.market.debt.borrow_index(asset),
            session.borrow_index,
            ErrorCode::InvalidLiquidationSession
        );
        let locked = match a.position()? {
            LiquidationPositionRef::Borrow(p) => p.active_liquidation_session,
            LiquidationPositionRef::Leverage(p) => {
                require_eq!(
                    p.debt_principal,
                    session.original_principal,
                    ErrorCode::InvalidLiquidationSession
                );
                p.active_liquidation_session
            }
        };
        require_keys_eq!(locked, session.key(), ErrorCode::InvalidLiquidationSession);
        let received = ctx
            .accounts
            .repayment_vault
            .amount
            .checked_sub(session.payment_balance_before)
            .ok_or(ErrorCode::InsufficientAmount)?;
        let allocation =
            a.market
                .liquidation_debt_allocation(&a.position()?, asset, session.quote.debt_shares_to_burn)?;
        let quote = session.quote;
        let budget = quote
            .repayment
            .checked_add(quote.fee.total)
            .and_then(|v| v.checked_add(quote.owner_surplus))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let repaid = if quote.full {
            budget.min(allocation.debt)
        } else {
            require_gte!(quote.repayment, allocation.debt, ErrorCode::InvalidLiquidationSession);
            allocation.debt
        };
        let fee = LiquidationFeeAllocation::quote(
            repaid,
            (budget - repaid).min(quote.fee.total),
            session.insurance_balance_at_quote,
            session.insurance_target_at_quote,
        );
        let owner_surplus = budget
            .checked_sub(repaid)
            .and_then(|v| v.checked_sub(fee.total))
            .ok_or(ErrorCode::BrokenInvariant)?;
        let debt_fee = leverage_collateral_fee(&a.debt_mint, clock.epoch)?;
        let gross_capacity = if quote.full {
            a.market.insurance.draw_capacity(asset, clock.slot)?
        } else {
            0
        };
        let net_capacity = debt_fee.unwind_credit(gross_capacity)?;
        let expected_loss = LiquidationLossAllocation::quote_with_coverage(
            allocation.principal.min(allocation.debt),
            allocation.debt,
            repaid,
            net_capacity,
            a.market.insurance.principal_coverage_bps,
        )?;
        let insurance_credit = expected_loss.insurance_credit;
        let insurance_debit = if insurance_credit == 0 {
            0
        } else {
            debt_fee.effective_gross_for_credit(insurance_credit)?
        };
        if ctx.accounts.collateral_reserve_vault.is_some() {
            return super::flash_amm_settlement::settle_with_amm(
                &mut ctx,
                asset,
                allocation,
                repaid,
                fee,
                owner_surplus,
                insurance_debit,
                expected_loss,
                clock,
            );
        }
        require_gte!(received, session.quote.payment, ErrorCode::InsufficientAmount);
        let (partner, share) = a.referral(asset)?;
        let reserve_before = a.reserve_vault.amount;
        let insurance_before = a.insurance_vault.amount;
        let interest_before = a.interest_vault.amount;
        let owner_before = a.owner_debt_account.amount;
        let payment_seeds: &[&[u8]] = &[LIQUIDATION_SESSION_SEED, session.position.as_ref(), &[session.bump]];
        // Repayment custody is unique to this position/debt mint. Only its
        // spendable increase is accepted; unrelated reserve transfers never
        // satisfy the buyer's obligation.
        for (destination, net) in [
            (a.reserve_vault.to_account_info(), repaid),
            (a.insurance_vault.to_account_info(), fee.insurance),
            (a.interest_vault.to_account_info(), fee.lp + fee.protocol),
            (a.owner_debt_account.to_account_info(), owner_surplus),
        ] {
            if net == 0 {
                continue;
            }
            transfer_checked_with_remaining_accounts(
                session.to_account_info(),
                ctx.accounts.repayment_vault.to_account_info(),
                destination,
                a.debt_mint.to_account_info(),
                a.debt_token_program.to_account_info(),
                debt_fee.effective_gross_for_credit(net)?,
                a.debt_mint.decimals,
                &[payment_seeds],
                ctx.remaining_accounts,
            )?;
        }
        a.reserve_vault.reload()?;
        a.insurance_vault.reload()?;
        a.interest_vault.reload()?;
        a.owner_debt_account.reload()?;
        require_eq!(
            a.reserve_vault
                .amount
                .checked_sub(reserve_before)
                .ok_or(ErrorCode::BrokenInvariant)?,
            repaid,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            a.insurance_vault
                .amount
                .checked_sub(insurance_before)
                .ok_or(ErrorCode::BrokenInvariant)?,
            fee.insurance,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            a.interest_vault
                .amount
                .checked_sub(interest_before)
                .ok_or(ErrorCode::BrokenInvariant)?,
            fee.lp + fee.protocol,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            a.owner_debt_account
                .amount
                .checked_sub(owner_before)
                .ok_or(ErrorCode::BrokenInvariant)?,
            owner_surplus,
            ErrorCode::BrokenInvariant
        );
        let reserve_after_payment = a.reserve_vault.amount;
        if insurance_debit > 0 {
            transfer_checked_with_remaining_accounts(
                a.market.to_account_info(),
                a.insurance_vault.to_account_info(),
                a.reserve_vault.to_account_info(),
                a.debt_mint.to_account_info(),
                a.debt_token_program.to_account_info(),
                insurance_debit,
                a.debt_mint.decimals,
                &[&generate_market_seeds!(a.market)[..]],
                ctx.remaining_accounts,
            )?;
            a.reserve_vault.reload()?;
            require_eq!(
                a.reserve_vault
                    .amount
                    .checked_sub(reserve_after_payment)
                    .ok_or(ErrorCode::BrokenInvariant)?,
                insurance_credit,
                ErrorCode::BrokenInvariant
            );
        }
        let eligibility_fee = leverage_collateral_liquidation_fee(&a.collateral_mint, clock.epoch)?;
        let loss = {
            let mut position = match (&mut a.borrow_position, &mut a.leverage_position) {
                (Some(p), None) => LiquidationPositionMut::Borrow(p),
                (None, Some(p)) => LiquidationPositionMut::Leverage(p),
                _ => return err!(ErrorCode::InvalidLiquidationSession),
            };
            a.market.settle_flash_recovery(
                &mut position,
                asset,
                allocation,
                quote.collateral_debit,
                repaid,
                insurance_debit,
                insurance_credit,
                quote.full,
                eligibility_fee,
                clock.unix_timestamp,
                clock.slot,
            )?
        };
        // Credit new insurance only after consuming the preexisting claim
        // budget, so a liquidation cannot fund its own loss coverage.
        if fee.insurance > 0 {
            a.market.insurance.credit(asset, fee.insurance, clock.slot)?;
        }
        a.interest_vault.reload()?;
        let eligibility = crate::transitions::HlpYieldEligibility {
            ylp_supply: a.market.base_side.shares.ylp_supply,
            base_hlp_ylp_shares: a.market.base_hlp_vault.ylp_shares,
            quote_hlp_ylp_shares: a.market.quote_hlp_vault.ylp_shares,
        };
        a.market.side_mut(asset).record_liquidation_fee_credit(
            fee.lp,
            fee.protocol,
            a.futarchy_authority.protocol_auction_split,
            eligibility.ylp_supply,
        )?;
        let referral_receipt = crate::instructions::leverage::settlement::record_leverage_interest(
            &mut a.market,
            asset,
            &a.debt_mint,
            &mut a.reserve_vault,
            &mut a.interest_vault,
            &a.token_program,
            &a.token_2022_program,
            &a.futarchy_authority,
            partner,
            share,
            a.referral_partner.as_deref(),
            a.referral_accrual.as_deref_mut(),
            loss.interest_paid,
            eligibility,
            ctx.remaining_accounts,
        )?;
        emit_liquidation_interest(
            a,
            asset,
            &referral_receipt,
            ctx.accounts.event_authority.to_account_info(),
            clock.slot,
        )?;
        a.market
            .checkpoint_hlp_yield_from_ylp_shares(MarketAsset::Base, eligibility.base_hlp_ylp_shares)?;
        a.market
            .checkpoint_hlp_yield_from_ylp_shares(MarketAsset::Quote, eligibility.quote_hlp_ylp_shares)?;
        ctx.accounts.repayment_vault.reload()?;
        let refund = ctx
            .accounts
            .repayment_vault
            .amount
            .checked_sub(session.payment_balance_before)
            .ok_or(ErrorCode::BrokenInvariant)?;
        if refund > 0 {
            transfer_checked_with_remaining_accounts(
                session.to_account_info(),
                ctx.accounts.repayment_vault.to_account_info(),
                a.buyer_refund_account.to_account_info(),
                a.debt_mint.to_account_info(),
                a.debt_token_program.to_account_info(),
                refund,
                a.debt_mint.decimals,
                &[payment_seeds],
                ctx.remaining_accounts,
            )?;
        }
        a.reserve_vault.reload()?;
        a.insurance_vault.reload()?;
        a.interest_vault.reload()?;
        require_reserve_custody(a.reserve_vault.amount, a.market.side(asset))?;
        require_gte!(
            a.insurance_vault.amount,
            a.market.insurance.available(asset),
            ErrorCode::UnbackedFeeLiability
        );
        require_gte!(
            a.interest_vault.amount,
            a.market.side(asset).fees.interest_vault_balance,
            ErrorCode::UnbackedFeeLiability
        );
        match (&mut a.borrow_position, &mut a.leverage_position) {
            (Some(p), None) => p.active_liquidation_session = Pubkey::default(),
            (None, Some(p)) => p.active_liquidation_session = Pubkey::default(),
            _ => return err!(ErrorCode::InvalidLiquidationSession),
        }
        emit_cpi!(FlashLiquidationSettled {
            market: a.market.key(),
            position: session.position,
            buyer: a.buyer.key(),
            leverage: session.leverage,
            debt_asset: asset.code(),
            collateral_debit: quote.collateral_debit,
            debt_repaid: repaid,
            owner_surplus,
            fee,
            loss,
            remaining_collateral: a.position()?.collateral(asset),
            remaining_debt: allocation.remaining_debt,
        });
        ctx.accounts.session.close(a.buyer.to_account_info())?;
        Ok(())
    }
}
