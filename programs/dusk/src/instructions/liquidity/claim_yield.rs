use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::{MarketEventMetadata, YieldClaimed},
    generate_market_seeds,
    state::{Market, YieldAccount, YieldClaimReceipt, YieldTokenKind},
    token::transfer_checked_with_remaining_accounts,
};

use crate::instructions::accounts::{
    require_reserve_custody, token_account_credit, token_program_for_mint, validate_interest_accounts,
    validate_owner_lp_account, validate_swap_fee_custody_accounts,
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct ClaimYieldArgs {
    pub token_kind: YieldTokenKind,
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(args: ClaimYieldArgs)]
pub struct ClaimYield<'info> {
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

    #[account(mut)]
    pub owner: Signer<'info>,

    pub asset_mint: Box<InterfaceAccount<'info, Mint>>,
    pub lp_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(mut)]
    pub owner_lp_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub interest_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub recipient_asset_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        mut,
        seeds = [
            YIELD_ACCOUNT_SEED_PREFIX,
            market.key().as_ref(),
            owner.key().as_ref(),
            lp_mint.key().as_ref(),
            asset_mint.key().as_ref(),
            &[args.token_kind.code()],
        ],
        bump = yield_account.bump
    )]
    pub yield_account: Box<Account<'info, YieldAccount>>,

    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> ClaimYield<'info> {
    pub fn validate(&self, args: &ClaimYieldArgs) -> Result<()> {
        // Bind the requested LP kind and asset to this market.
        let market_asset = self.market.asset_for_mint(self.asset_mint.key())?;
        let market_side = self.market.side(market_asset);
        require_keys_eq!(market_side.asset_mint, self.asset_mint.key(), ErrorCode::InvalidMint);
        match args.token_kind {
            YieldTokenKind::Ylp => {
                require_keys_eq!(self.market.ylp_mint, self.lp_mint.key(), ErrorCode::InvalidMint)
            }
            YieldTokenKind::Hlp => {
                self.market.asset_for_hlp_mint(self.lp_mint.key())?;
            }
        }
        validate_owner_lp_account(self.owner.key(), &self.lp_mint, &self.owner_lp_account)?;

        // Bind the destination and both physical yield sources.
        require_keys_eq!(
            self.recipient_asset_account.owner,
            self.yield_account.recipient,
            ErrorCode::InvalidRecipient
        );
        require_keys_eq!(
            self.recipient_asset_account.mint,
            self.asset_mint.key(),
            ErrorCode::InvalidTokenAccount
        );
        let fee_asset = validate_swap_fee_custody_accounts(&self.market, &self.asset_mint, &self.reserve_vault)?;
        let interest_asset = validate_interest_accounts(&self.market, &self.asset_mint, &self.interest_vault)?;
        require!(fee_asset == market_asset, ErrorCode::InvalidVault);
        require!(interest_asset == market_asset, ErrorCode::InvalidVault);
        require_gte!(
            self.reserve_vault.amount,
            market_side
                .reserves
                .cash_reserve
                .checked_add(market_side.fees.swap_fee_custody_balance)
                .ok_or(ErrorCode::MarketMathOverflow)?,
            ErrorCode::UnbackedFeeLiability
        );
        self.yield_account.assert_account(
            self.owner.key(),
            self.market.key(),
            self.lp_mint.key(),
            self.asset_mint.key(),
            args.token_kind,
        )
    }

    crate::instructions::accounts::market_update_and_validate!(ClaimYieldArgs);

    pub fn handle_claim(ctx: Context<'_, '_, '_, 'info, Self>, args: ClaimYieldArgs) -> Result<()> {
        let market_key = ctx.accounts.market.key();
        let owner_key = ctx.accounts.owner.key();
        let asset_mint_key = ctx.accounts.asset_mint.key();
        let market_asset = ctx.accounts.market.asset_for_mint(asset_mint_key)?;
        let token_program = token_program_for_mint(
            &ctx.accounts.asset_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        let swap_fee_custody_balance = ctx.accounts.market.side(market_asset).fees.swap_fee_custody_balance;

        // Materialize claimable yield from the appropriate LP growth indexes.
        let receipt = match args.token_kind {
            YieldTokenKind::Ylp => {
                let market_side = ctx.accounts.market.side_mut(market_asset);
                market_side.prepare_yield_claim(
                    &mut ctx.accounts.yield_account,
                    swap_fee_custody_balance,
                    ctx.accounts.interest_vault.amount,
                    ctx.accounts.owner_lp_account.amount,
                )?
            }
            YieldTokenKind::Hlp => {
                let hlp_asset = ctx.accounts.market.asset_for_hlp_mint(ctx.accounts.lp_mint.key())?;
                ctx.accounts.market.checkpoint_hlp_yield_from_ylp(hlp_asset)?;
                let (swap_fee_growth_index_q64, interest_growth_index_q64) =
                    ctx.accounts.market.hlp_yield_growth_indexes(hlp_asset, market_asset);
                ctx.accounts.yield_account.accrue(
                    ctx.accounts.owner_lp_account.amount,
                    swap_fee_growth_index_q64,
                    interest_growth_index_q64,
                )?;
                let claim_amount = ctx.accounts.yield_account.claimable_amount()?;
                require!(claim_amount > 0, ErrorCode::AmountZero);
                require_gte!(
                    swap_fee_custody_balance,
                    ctx.accounts.yield_account.accrued_swap_fee_amount,
                    ErrorCode::UnbackedFeeLiability
                );
                require_gte!(
                    ctx.accounts.interest_vault.amount,
                    ctx.accounts.yield_account.accrued_interest_amount,
                    ErrorCode::UnbackedFeeLiability
                );
                YieldClaimReceipt {
                    claim_amount,
                    swap_fee_amount: ctx.accounts.yield_account.accrued_swap_fee_amount,
                    interest_amount: ctx.accounts.yield_account.accrued_interest_amount,
                    remaining_swap_fee_liability: ctx.accounts.market.side(market_asset).fees.swap_fee_liability,
                    remaining_interest_liability: ctx.accounts.market.side(market_asset).fees.interest_liability,
                }
            }
        };

        // Pay swap-fee and interest yield from their separate custody vaults.
        let recipient_balance_before = ctx.accounts.recipient_asset_account.amount;
        if receipt.swap_fee_amount > 0 {
            transfer_checked_with_remaining_accounts(
                ctx.accounts.market.to_account_info(),
                ctx.accounts.reserve_vault.to_account_info(),
                ctx.accounts.recipient_asset_account.to_account_info(),
                ctx.accounts.asset_mint.to_account_info(),
                token_program.clone(),
                receipt.swap_fee_amount,
                ctx.accounts.asset_mint.decimals,
                &[&generate_market_seeds!(ctx.accounts.market)[..]],
                ctx.remaining_accounts,
            )?;
            ctx.accounts.reserve_vault.reload()?;
        }
        if receipt.interest_amount > 0 {
            transfer_checked_with_remaining_accounts(
                ctx.accounts.market.to_account_info(),
                ctx.accounts.interest_vault.to_account_info(),
                ctx.accounts.recipient_asset_account.to_account_info(),
                ctx.accounts.asset_mint.to_account_info(),
                token_program,
                receipt.interest_amount,
                ctx.accounts.asset_mint.decimals,
                &[&generate_market_seeds!(ctx.accounts.market)[..]],
                ctx.remaining_accounts,
            )?;
        }
        ctx.accounts.recipient_asset_account.reload()?;
        let recipient_credit = token_account_credit(recipient_balance_before, &ctx.accounts.recipient_asset_account)?;

        // Retire liabilities only after both payment transfers succeed.
        {
            let market_side = ctx.accounts.market.side_mut(market_asset);
            market_side.settle_yield_claim(
                &mut ctx.accounts.yield_account,
                receipt.claim_amount,
                receipt.swap_fee_amount,
                receipt.interest_amount,
            )?;
        }

        // Preserve executable reserve backing after removing fee custody.
        require_reserve_custody(
            ctx.accounts.reserve_vault.amount,
            ctx.accounts.market.side(market_asset),
        )?;
        emit_cpi!(YieldClaimed {
            market: market_key,
            owner: owner_key,
            lp_mint: ctx.accounts.lp_mint.key(),
            asset_mint: asset_mint_key,
            token_kind: args.token_kind.code(),
            recipient: ctx.accounts.yield_account.recipient,
            swap_fee_amount: receipt.swap_fee_amount,
            interest_amount: receipt.interest_amount,
            recipient_credit,
            metadata: MarketEventMetadata::new(owner_key, market_key)?,
        });
        Ok(())
    }
}
