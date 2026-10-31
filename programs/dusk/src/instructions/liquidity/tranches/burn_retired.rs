use crate::{
    constants::{MARKET_V2_SEED_PREFIX, YIELD_ACCOUNT_SEED_PREFIX},
    errors::ErrorCode,
    instructions::accounts::{validate_lp_mint, validate_owner_lp_account},
    state::{JuniorCohort, JuniorExitAccount, Market, MarketAsset, YieldAccount, YieldTokenKind},
    token::token_burn,
};
use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, Token2022, TokenAccount};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct BurnRetiredJuniorArgs {
    pub amount: u64,
}

#[event_cpi]
#[derive(Accounts)]
pub struct BurnRetiredJunior<'info> {
    #[account(seeds = [MARKET_V2_SEED_PREFIX, market.base_side.asset_mint.as_ref(), market.quote_side.asset_mint.as_ref(), market.params_hash.as_ref()], bump = market.bump)]
    pub market: Box<Account<'info, Market>>,
    pub owner: Signer<'info>,
    #[account(mut)]
    pub retired_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(seeds = [b"junior_cohort", market.key().as_ref(), retired_mint.key().as_ref()], bump = cohort.bump)]
    pub cohort: Box<Account<'info, JuniorCohort>>,
    #[account(mut)]
    pub owner_junior: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, seeds = [YIELD_ACCOUNT_SEED_PREFIX, market.key().as_ref(), owner.key().as_ref(), retired_mint.key().as_ref(), market.base_side.asset_mint.as_ref(), &[YieldTokenKind::Junior.code()]], bump = base_yield.bump)]
    pub base_yield: Box<Account<'info, YieldAccount>>,
    #[account(mut, seeds = [YIELD_ACCOUNT_SEED_PREFIX, market.key().as_ref(), owner.key().as_ref(), retired_mint.key().as_ref(), market.quote_side.asset_mint.as_ref(), &[YieldTokenKind::Junior.code()]], bump = quote_yield.bump)]
    pub quote_yield: Box<Account<'info, YieldAccount>>,
    #[account(mut)]
    pub exit: Option<Box<Account<'info, JuniorExitAccount>>>,
    pub token_2022_program: Program<'info, Token2022>,
}

#[event]
pub struct RetiredJuniorBurned {
    pub market: Pubkey,
    pub owner: Pubkey,
    pub mint: Pubkey,
    pub wallet_amount: u64,
    pub escrow_amount: u64,
}

impl BurnRetiredJunior<'_> {
    pub fn handle_burn(ctx: Context<Self>, args: BurnRetiredJuniorArgs) -> Result<()> {
        let a = &mut *ctx.accounts;
        a.market.assert_current_version()?;
        let market = a.market.key();
        let owner = a.owner.key();
        let mint = a.retired_mint.key();
        require!(a.cohort.retired, ErrorCode::InvalidArgument);
        require_keys_eq!(a.cohort.market, market, ErrorCode::InvalidMarket);
        require_keys_eq!(a.cohort.mint, mint, ErrorCode::InvalidLpMintKey);
        require_keys_neq!(a.market.junior_mint, mint, ErrorCode::InvalidLpMintKey);
        validate_lp_mint(&a.retired_mint, market, a.market.base_side.asset_decimals)?;
        validate_owner_lp_account(owner, &a.retired_mint, &a.owner_junior)?;
        require_gte!(a.owner_junior.amount, args.amount, ErrorCode::InsufficientBalance);
        let escrow = a.base_yield.escrow_balance;
        require_eq!(escrow, a.quote_yield.escrow_balance, ErrorCode::BrokenInvariant);
        require!(args.amount > 0 || escrow > 0, ErrorCode::AmountZero);
        if let Some(exit) = a.exit.as_mut() {
            let expected = Pubkey::find_program_address(
                &[b"junior_exit", market.as_ref(), owner.as_ref(), mint.as_ref()],
                &crate::ID,
            )
            .0;
            require_keys_eq!(exit.key(), expected, ErrorCode::InvalidArgument);
            require_keys_eq!(exit.market, market, ErrorCode::InvalidMarket);
            require_keys_eq!(exit.owner, owner, ErrorCode::InvalidSigner);
            require_keys_eq!(exit.mint, mint, ErrorCode::InvalidLpMintKey);
            require_eq!(exit.ticket.receipts, escrow, ErrorCode::BrokenInvariant);
            exit.ticket = Default::default();
        } else {
            require_eq!(escrow, 0, ErrorCode::InvalidArgument);
        }
        for asset in [MarketAsset::Base, MarketAsset::Quote] {
            let claim = if asset == MarketAsset::Base {
                &mut a.base_yield
            } else {
                &mut a.quote_yield
            };
            claim.assert_account(
                owner,
                market,
                mint,
                a.market.side(asset).asset_mint,
                YieldTokenKind::Junior,
            )?;
            a.market
                .checkpoint_holder_income(claim, a.owner_junior.amount, Some(a.cohort.income_end))?;
            let (swap, interest) = a.cohort.yield_indexes(asset);
            claim.accrue(a.owner_junior.amount, swap, interest)?;
            claim.escrow_balance = 0;
        }
        if args.amount > 0 {
            token_burn(
                a.owner.to_account_info(),
                a.token_2022_program.to_account_info(),
                a.retired_mint.to_account_info(),
                a.owner_junior.to_account_info(),
                args.amount,
                &[],
            )?;
        }
        emit_cpi!(RetiredJuniorBurned {
            market,
            owner,
            mint,
            wallet_amount: args.amount,
            escrow_amount: escrow
        });
        Ok(())
    }
}
