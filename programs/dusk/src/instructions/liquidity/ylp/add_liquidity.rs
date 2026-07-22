use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::{LiquidityAdded, MarketEventMetadata},
    generate_market_seeds,
    shared::{
        account::get_size_with_discriminator,
        token::{get_transfer_fee, get_transfer_inverse_fee, token_mint_to, transfer_from_user_to_vault},
    },
    state::{FutarchyAuthority, Market, YieldAccount, YieldTokenKind},
};

use crate::instructions::common::{
    require_supported_asset_mint, token_program_for_mint, validate_lp_mint, validate_owner_asset_account,
    validate_owner_lp_account, validate_side_vault_accounts,
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct AddLiquidityArgs {
    pub base_deposit_amount: u64,
    pub quote_deposit_amount: u64,
    pub min_ylp_amount: u64,
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(args: AddLiquidityArgs)]
pub struct AddLiquidity<'info> {
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
        seeds = [FUTARCHY_AUTHORITY_SEED_PREFIX],
        bump = futarchy_authority.bump
    )]
    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,

    #[account(mut)]
    pub owner: Signer<'info>,

    pub base_mint: Box<InterfaceAccount<'info, Mint>>,
    pub quote_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(mut)]
    pub ylp_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(mut)]
    pub base_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub quote_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut)]
    pub owner_base_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub owner_quote_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub owner_ylp_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        init_if_needed,
        payer = owner,
        space = get_size_with_discriminator::<YieldAccount>(),
        seeds = [
            YIELD_ACCOUNT_SEED_PREFIX,
            market.key().as_ref(),
            owner.key().as_ref(),
            base_mint.key().as_ref(),
            &[YieldTokenKind::Ylp.code()],
        ],
        bump
    )]
    pub base_yield_account: Box<Account<'info, YieldAccount>>,

    #[account(
        init_if_needed,
        payer = owner,
        space = get_size_with_discriminator::<YieldAccount>(),
        seeds = [
            YIELD_ACCOUNT_SEED_PREFIX,
            market.key().as_ref(),
            owner.key().as_ref(),
            quote_mint.key().as_ref(),
            &[YieldTokenKind::Ylp.code()],
        ],
        bump
    )]
    pub quote_yield_account: Box<Account<'info, YieldAccount>>,

    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
}

struct AddLiquidityTransferPlan {
    base_transfer_amount: u64,
    quote_transfer_amount: u64,
}

impl<'info> AddLiquidity<'info> {
    pub fn validate(&self, args: &AddLiquidityArgs) -> Result<()> {
        self.market.assert_live_with_futarchy(&self.futarchy_authority)?;
        require!(
            args.base_deposit_amount > 0 && args.quote_deposit_amount > 0,
            ErrorCode::AmountZero
        );
        validate_side_vault_accounts(
            &self.market,
            crate::state::MarketAsset::Base,
            &self.base_mint,
            &self.base_reserve_vault,
        )?;
        validate_side_vault_accounts(
            &self.market,
            crate::state::MarketAsset::Quote,
            &self.quote_mint,
            &self.quote_reserve_vault,
        )?;
        require_keys_eq!(self.market.ylp_mint, self.ylp_mint.key(), ErrorCode::InvalidLpMintKey);
        validate_owner_asset_account(self.owner.key(), &self.base_mint, &self.owner_base_account)?;
        validate_owner_asset_account(self.owner.key(), &self.quote_mint, &self.owner_quote_account)?;
        validate_owner_lp_account(self.owner.key(), &self.ylp_mint, &self.owner_ylp_account)?;
        require_supported_asset_mint(&self.base_mint)?;
        require_supported_asset_mint(&self.quote_mint)?;
        validate_lp_mint(&self.ylp_mint, self.market.key(), self.base_mint.decimals)?;
        let transfer_plan = self.transfer_plan(args)?;
        require_gte!(
            self.owner_base_account.amount,
            transfer_plan.base_transfer_amount,
            ErrorCode::InsufficientBalance
        );
        require_gte!(
            self.owner_quote_account.amount,
            transfer_plan.quote_transfer_amount,
            ErrorCode::InsufficientBalance
        );
        Ok(())
    }

    crate::instructions::common::market_update_and_validate!(AddLiquidityArgs);

    fn transfer_plan(&self, args: &AddLiquidityArgs) -> Result<AddLiquidityTransferPlan> {
        let base_transfer_fee = get_transfer_fee(&self.base_mint.to_account_info(), args.base_deposit_amount)?;
        let quote_transfer_fee = get_transfer_fee(&self.quote_mint.to_account_info(), args.quote_deposit_amount)?;
        let max_base_reserve_credit = args
            .base_deposit_amount
            .checked_sub(base_transfer_fee)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let max_quote_reserve_credit = args
            .quote_deposit_amount
            .checked_sub(quote_transfer_fee)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let receipt = self
            .market
            .preview_add_liquidity(max_base_reserve_credit, max_quote_reserve_credit)?;
        require_gte!(receipt.ylp_amount, args.min_ylp_amount, ErrorCode::SlippageExceeded);

        let base_transfer_amount = receipt
            .base_reserve_credit
            .checked_add(get_transfer_inverse_fee(
                &self.base_mint.to_account_info(),
                receipt.base_reserve_credit,
            )?)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let quote_transfer_amount = receipt
            .quote_reserve_credit
            .checked_add(get_transfer_inverse_fee(
                &self.quote_mint.to_account_info(),
                receipt.quote_reserve_credit,
            )?)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require_gte!(
            args.base_deposit_amount,
            base_transfer_amount,
            ErrorCode::SlippageExceeded
        );
        require_gte!(
            args.quote_deposit_amount,
            quote_transfer_amount,
            ErrorCode::SlippageExceeded
        );

        Ok(AddLiquidityTransferPlan {
            base_transfer_amount,
            quote_transfer_amount,
        })
    }

    pub fn handle_add_liquidity(ctx: Context<Self>, args: AddLiquidityArgs) -> Result<()> {
        let market_key = ctx.accounts.market.key();
        let owner_key = ctx.accounts.owner.key();

        initialize_or_validate_yield_account(
            &mut ctx.accounts.base_yield_account,
            owner_key,
            market_key,
            ctx.accounts.base_mint.key(),
            ctx.bumps.base_yield_account,
        )?;
        initialize_or_validate_yield_account(
            &mut ctx.accounts.quote_yield_account,
            owner_key,
            market_key,
            ctx.accounts.quote_mint.key(),
            ctx.bumps.quote_yield_account,
        )?;

        {
            let market = &mut ctx.accounts.market;
            market.base_side.carry_forward_swap_fees()?;
            market.base_side.carry_forward_interest()?;
            market.quote_side.carry_forward_swap_fees()?;
            market.quote_side.carry_forward_interest()?;
            ctx.accounts.base_yield_account.accrue(
                ctx.accounts.owner_ylp_account.amount,
                market.base_side.fees.swap_fee_growth_index_nad,
                market.base_side.fees.interest_growth_index_nad,
            )?;
            ctx.accounts.quote_yield_account.accrue(
                ctx.accounts.owner_ylp_account.amount,
                market.quote_side.fees.swap_fee_growth_index_nad,
                market.quote_side.fees.interest_growth_index_nad,
            )?;
        }

        let transfer_plan = ctx.accounts.transfer_plan(&args)?;
        let base_reserve_before = ctx.accounts.base_reserve_vault.amount;
        let quote_reserve_before = ctx.accounts.quote_reserve_vault.amount;
        let base_token_program = token_program_for_mint(
            &ctx.accounts.base_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        let quote_token_program = token_program_for_mint(
            &ctx.accounts.quote_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        transfer_from_user_to_vault(
            ctx.accounts.owner.to_account_info(),
            ctx.accounts.owner_base_account.to_account_info(),
            ctx.accounts.base_reserve_vault.to_account_info(),
            ctx.accounts.base_mint.to_account_info(),
            base_token_program,
            transfer_plan.base_transfer_amount,
            ctx.accounts.base_mint.decimals,
        )?;
        transfer_from_user_to_vault(
            ctx.accounts.owner.to_account_info(),
            ctx.accounts.owner_quote_account.to_account_info(),
            ctx.accounts.quote_reserve_vault.to_account_info(),
            ctx.accounts.quote_mint.to_account_info(),
            quote_token_program,
            transfer_plan.quote_transfer_amount,
            ctx.accounts.quote_mint.decimals,
        )?;
        ctx.accounts.base_reserve_vault.reload()?;
        ctx.accounts.quote_reserve_vault.reload()?;
        let base_reserve_credit = ctx
            .accounts
            .base_reserve_vault
            .amount
            .checked_sub(base_reserve_before)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let quote_reserve_credit = ctx
            .accounts
            .quote_reserve_vault
            .amount
            .checked_sub(quote_reserve_before)
            .ok_or(ErrorCode::MarketMathOverflow)?;

        let receipt = ctx
            .accounts
            .market
            .add_liquidity(base_reserve_credit, quote_reserve_credit)?;
        require_gte!(receipt.ylp_amount, args.min_ylp_amount, ErrorCode::SlippageExceeded);

        let ylp_program = token_program_for_mint(
            &ctx.accounts.ylp_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        token_mint_to(
            ctx.accounts.market.to_account_info(),
            ylp_program,
            ctx.accounts.ylp_mint.to_account_info(),
            ctx.accounts.owner_ylp_account.to_account_info(),
            receipt.ylp_amount,
            &[&generate_market_seeds!(ctx.accounts.market)[..]],
        )?;

        emit_cpi!(LiquidityAdded {
            market: market_key,
            owner: owner_key,
            base_reserve_credit: receipt.base_reserve_credit,
            quote_reserve_credit: receipt.quote_reserve_credit,
            ylp_amount: receipt.ylp_amount,
            ylp_supply: receipt.ylp_supply,
            metadata: MarketEventMetadata::new(owner_key, market_key)?,
        });

        Ok(())
    }
}

fn initialize_or_validate_yield_account(
    yield_account: &mut Account<YieldAccount>,
    owner: Pubkey,
    market: Pubkey,
    asset_mint: Pubkey,
    bump: u8,
) -> Result<()> {
    if yield_account.owner == Pubkey::default() {
        yield_account.initialize(owner, market, asset_mint, YieldTokenKind::Ylp, owner, bump);
    }
    yield_account.assert_account(owner, market, asset_mint, YieldTokenKind::Ylp)
}
