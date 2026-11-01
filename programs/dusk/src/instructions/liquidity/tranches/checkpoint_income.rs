use crate::{
    constants::{MARKET_V2_SEED_PREFIX, YIELD_ACCOUNT_SEED_PREFIX},
    errors::ErrorCode,
    instructions::accounts::validate_owner_lp_account,
    state::{IncomeEpoch, IncomeExposureEnd, JuniorCohort, Market, YieldAccount, YieldTokenKind},
};
use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, TokenAccount};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct CheckpointIncomeArgs {
    pub source: u8,
}

/// Permissionless bounded catch-up; caller cannot redirect or redeem income.
#[derive(Accounts)]
pub struct CheckpointIncome<'info> {
    #[account(seeds = [MARKET_V2_SEED_PREFIX, market.base_side.asset_mint.as_ref(), market.quote_side.asset_mint.as_ref(), market.params_hash.as_ref()], bump = market.bump)]
    pub market: Box<Account<'info, Market>>,
    pub lp_mint: Box<InterfaceAccount<'info, Mint>>,
    pub owner_lp: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, seeds = [YIELD_ACCOUNT_SEED_PREFIX, market.key().as_ref(), yield_account.owner.as_ref(), lp_mint.key().as_ref(), yield_account.asset_mint.as_ref(), &[yield_account.token_kind]], bump = yield_account.bump)]
    pub yield_account: Box<Account<'info, YieldAccount>>,
}

impl CheckpointIncome<'_> {
    pub fn handle_checkpoint(ctx: Context<Self>, args: CheckpointIncomeArgs) -> Result<()> {
        let a = ctx.accounts;
        a.market.assert_current_version()?;
        require!(args.source < 2, ErrorCode::InvalidArgument);
        require_keys_eq!(a.yield_account.market, a.market.key(), ErrorCode::InvalidMarket);
        require_keys_eq!(a.yield_account.lp_mint, a.lp_mint.key(), ErrorCode::InvalidLpMintKey);
        validate_owner_lp_account(a.yield_account.owner, &a.lp_mint, &a.owner_lp)?;
        let junior = a.yield_account.token_kind == YieldTokenKind::Junior.code();
        require!(
            junior || a.yield_account.token_kind == YieldTokenKind::Ylp.code(),
            ErrorCode::InvalidArgument
        );
        if !junior {
            require_keys_eq!(a.lp_mint.key(), a.market.ylp_mint, ErrorCode::InvalidLpMintKey);
        }
        let asset = a.market.asset_for_mint(a.yield_account.asset_mint)?;
        let index = asset.code() as usize + args.source as usize * 2;
        let (end, archives) = if junior && a.lp_mint.key() != a.market.junior_mint {
            let cohort = JuniorCohort::from_remaining(
                ctx.remaining_accounts.first().ok_or(ErrorCode::InvalidLpMintKey)?,
                a.market.key(),
                a.lp_mint.key(),
            )?;
            (cohort.income_end[index], &ctx.remaining_accounts[1..])
        } else {
            (
                if junior {
                    a.market.income.get()?.junior_end[index]
                } else {
                    IncomeExposureEnd::default()
                },
                ctx.remaining_accounts,
            )
        };
        require!(archives.len() <= 8, ErrorCode::InvalidArgument);
        // A newly created ledger has no historical units. Every supported mint,
        // transfer and conversion initializes it before receiving a balance.
        require!(a.yield_account.income_initialized, ErrorCode::InvalidYieldAccount);
        let balance = a
            .owner_lp
            .amount
            .checked_add(a.yield_account.escrow_balance)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let claim = &mut a.yield_account.income[args.source as usize];
        let lane = usize::from(junior);
        for info in archives {
            require_keys_eq!(*info.owner, crate::ID, ErrorCode::InvalidArgument);
            let data = info.try_borrow_data()?;
            let archive = IncomeEpoch::try_deserialize(&mut data.as_ref())?;
            let expected = Pubkey::find_program_address(
                &[
                    b"income_epoch",
                    a.market.key().as_ref(),
                    &[index as u8],
                    &claim.epoch.to_le_bytes(),
                ],
                &crate::ID,
            )
            .0;
            require_keys_eq!(*info.key, expected, ErrorCode::InvalidArgument);
            require_keys_eq!(archive.market, a.market.key(), ErrorCode::InvalidMarket);
            require_eq!(archive.book_index, index as u8, ErrorCode::InvalidArgument);
            claim.advance_exposure_epoch(balance, &archive.book, lane, end)?;
        }
        let live = &a.market.income.get()?.books[index];
        if claim.epoch == live.epoch {
            claim.checkpoint_exposure(balance, live, lane, end)?;
        }
        require!(claim.epoch <= live.epoch, ErrorCode::BrokenInvariant);
        let cash = claim.claimable;
        claim.claimable = 0;
        a.yield_account.accrued_interest_amount = a
            .yield_account
            .accrued_interest_amount
            .checked_add(cash)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        Ok(())
    }
}
