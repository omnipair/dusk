use crate::{
    account::get_size_with_discriminator,
    errors::ErrorCode,
    state::{IncomeEpoch, Market},
};
use anchor_lang::prelude::*;

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct RollIncomeEpochArgs {
    pub book_index: u8,
    pub epoch: u64,
}

#[derive(Accounts)]
#[instruction(args: RollIncomeEpochArgs)]
pub struct RollIncomeEpoch<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(mut)]
    pub market: Box<Account<'info, Market>>,
    #[account(init, payer = payer, space = get_size_with_discriminator::<IncomeEpoch>(),
        seeds = [b"income_epoch", market.key().as_ref(), &[args.book_index], &args.epoch.to_le_bytes()], bump)]
    pub income_epoch: Box<Account<'info, IncomeEpoch>>,
    pub system_program: Program<'info, System>,
}

impl RollIncomeEpoch<'_> {
    pub fn handle_roll(ctx: Context<Self>, args: RollIncomeEpochArgs) -> Result<()> {
        ctx.accounts.market.assert_current_version()?;
        let i = args.book_index as usize;
        require!(i < 4, ErrorCode::InvalidArgument);
        // Never archive an index between debt clearance and its custody credit.
        require_eq!(
            ctx.accounts.market.income.get()?.pending[i],
            0,
            ErrorCode::BrokenInvariant
        );
        require!(
            ctx.accounts.market.base_side.reserves.flash_receivable == 0
                && ctx.accounts.market.quote_side.reserves.flash_receivable == 0
                && ctx.accounts.market.base_hlp_vault.flash_collateral_debit == 0
                && ctx.accounts.market.quote_hlp_vault.flash_collateral_debit == 0,
            ErrorCode::InvalidArgument
        );
        let book = ctx
            .accounts
            .market
            .income
            .get_mut()?
            .books
            .get_mut(args.book_index as usize)
            .ok_or(ErrorCode::InvalidArgument)?;
        require_eq!(book.epoch, args.epoch, ErrorCode::InvalidArgument);
        // A roll requires actual settlement activity. Requiring a fixed
        // percentage decay could prevent recovery from unit overflow after
        // a smaller repayment followed by a large new accrual.
        require!(book.units > book.unpaid, ErrorCode::InvalidArgument);
        ctx.accounts.income_epoch.book = book.roll()?;
        let balance = if ctx.accounts.market.tranches.junior_reserve_shares == 0 {
            1
        } else {
            ctx.accounts.market.tranches.permanent_receipts
        };
        let income = ctx.accounts.market.income.get_mut()?;
        income.seed[i].advance_epoch(balance, &ctx.accounts.income_epoch.book, 1)?;
        if i < 2 {
            for target in 0..2 {
                income.hlp[target][i].advance_epoch(1, &ctx.accounts.income_epoch.book, target + 2)?;
            }
        }
        ctx.accounts.income_epoch.market = ctx.accounts.market.key();
        ctx.accounts.income_epoch.book_index = args.book_index;
        ctx.accounts.income_epoch.bump = ctx.bumps.income_epoch;
        Ok(())
    }
}
