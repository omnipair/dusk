use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    account::{get_size_with_discriminator, initialize_pda_account_if_needed},
    constants::*,
    errors::ErrorCode,
    events::{LiquidityAdded, MarketEventMetadata},
    generate_market_seeds,
    state::{FutarchyAuthority, Market, YieldAccount, YieldTokenKind},
    token::{get_transfer_fee, get_transfer_inverse_fee, token_mint_to, transfer_checked_with_remaining_accounts},
};

use super::{validate_ylp_market_pda, ylp_yield_account_pda};

use super::super::initialize_or_validate_yield_account;
use crate::instructions::accounts::{
    require_reserve_custody, require_supported_asset_mint, token_program_for_mint, validate_lp_mint,
    validate_owner_asset_account, validate_owner_lp_account, validate_side_vault_accounts,
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct AddLiquidityArgs {
    pub base_deposit_amount: u64,
    pub quote_deposit_amount: u64,
    pub min_ylp_amount: u64,
}

#[event_cpi]
#[derive(Accounts)]
pub struct AddLiquidity<'info> {
    #[account(mut)]
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

    /// CHECK: Canonical PDA and typed state are initialized or validated before
    /// any liquidity mutation.
    #[account(mut)]
    pub base_yield_account: UncheckedAccount<'info>,

    /// CHECK: Canonical PDA and typed state are initialized or validated before
    /// any liquidity mutation.
    #[account(mut)]
    pub quote_yield_account: UncheckedAccount<'info>,

    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
}

struct AddLiquidityTransfers {
    base_transfer_amount: u64,
    quote_transfer_amount: u64,
}

impl<'info> AddLiquidity<'info> {
    pub fn validate(&self, args: &AddLiquidityArgs) -> Result<()> {
        validate_ylp_market_pda(&self.market, self.market.key())?;
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
        let (expected_base_yield_account, _) = ylp_yield_account_pda(
            self.market.key(),
            self.owner.key(),
            self.ylp_mint.key(),
            self.base_mint.key(),
        )?;
        require_keys_eq!(
            self.base_yield_account.key(),
            expected_base_yield_account,
            ErrorCode::InvalidYieldAccount
        );
        let (expected_quote_yield_account, _) = ylp_yield_account_pda(
            self.market.key(),
            self.owner.key(),
            self.ylp_mint.key(),
            self.quote_mint.key(),
        )?;
        require_keys_eq!(
            self.quote_yield_account.key(),
            expected_quote_yield_account,
            ErrorCode::InvalidYieldAccount
        );
        let transfers = self.transfers(args)?;
        require_gte!(
            self.owner_base_account.amount,
            transfers.base_transfer_amount,
            ErrorCode::InsufficientBalance
        );
        require_gte!(
            self.owner_quote_account.amount,
            transfers.quote_transfer_amount,
            ErrorCode::InsufficientBalance
        );
        Ok(())
    }

    crate::instructions::accounts::market_update_and_validate!(AddLiquidityArgs);

    fn transfers(&self, args: &AddLiquidityArgs) -> Result<AddLiquidityTransfers> {
        // Preview against the maximum credits available after transfer fees.
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

        // Gross up the exact reserve credits for transfer-fee mints.
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

        Ok(AddLiquidityTransfers {
            base_transfer_amount,
            quote_transfer_amount,
        })
    }

    pub fn handle_add_liquidity(ctx: Context<'_, '_, '_, 'info, Self>, args: AddLiquidityArgs) -> Result<()> {
        let market_key = ctx.accounts.market.key();
        let owner_key = ctx.accounts.owner.key();
        let ylp_mint_key = ctx.accounts.ylp_mint.key();
        let base_mint_key = ctx.accounts.base_mint.key();
        let quote_mint_key = ctx.accounts.quote_mint.key();

        // Initialize per-asset yield checkpoints before LP ownership changes.
        let (expected_base_yield_account, base_yield_bump) =
            ylp_yield_account_pda(market_key, owner_key, ylp_mint_key, base_mint_key)?;
        require_keys_eq!(
            ctx.accounts.base_yield_account.key(),
            expected_base_yield_account,
            ErrorCode::InvalidYieldAccount
        );
        let base_yield_bump_seed = [base_yield_bump];
        let base_yield_kind_seed = [YieldTokenKind::Ylp.code()];
        let base_yield_seeds = [
            YIELD_ACCOUNT_SEED_PREFIX,
            market_key.as_ref(),
            owner_key.as_ref(),
            ylp_mint_key.as_ref(),
            base_mint_key.as_ref(),
            &base_yield_kind_seed,
            &base_yield_bump_seed,
        ];
        let base_created = initialize_pda_account_if_needed(
            ctx.accounts.owner.to_account_info(),
            ctx.accounts.base_yield_account.to_account_info(),
            ctx.accounts.system_program.to_account_info(),
            get_size_with_discriminator::<YieldAccount>(),
            &base_yield_seeds,
        )?;
        let mut base_yield_account = if base_created {
            let data = ctx.accounts.base_yield_account.try_borrow_data()?;
            let mut data_slice: &[u8] = &data;
            YieldAccount::try_deserialize_unchecked(&mut data_slice)?
        } else {
            let data = ctx.accounts.base_yield_account.try_borrow_data()?;
            let mut data_slice: &[u8] = &data;
            YieldAccount::try_deserialize(&mut data_slice)?
        };
        initialize_or_validate_yield_account(
            &mut base_yield_account,
            owner_key,
            market_key,
            ylp_mint_key,
            base_mint_key,
            YieldTokenKind::Ylp,
            base_yield_bump,
        )?;

        let (expected_quote_yield_account, quote_yield_bump) =
            ylp_yield_account_pda(market_key, owner_key, ylp_mint_key, quote_mint_key)?;
        require_keys_eq!(
            ctx.accounts.quote_yield_account.key(),
            expected_quote_yield_account,
            ErrorCode::InvalidYieldAccount
        );
        let quote_yield_bump_seed = [quote_yield_bump];
        let quote_yield_kind_seed = [YieldTokenKind::Ylp.code()];
        let quote_yield_seeds = [
            YIELD_ACCOUNT_SEED_PREFIX,
            market_key.as_ref(),
            owner_key.as_ref(),
            ylp_mint_key.as_ref(),
            quote_mint_key.as_ref(),
            &quote_yield_kind_seed,
            &quote_yield_bump_seed,
        ];
        let quote_created = initialize_pda_account_if_needed(
            ctx.accounts.owner.to_account_info(),
            ctx.accounts.quote_yield_account.to_account_info(),
            ctx.accounts.system_program.to_account_info(),
            get_size_with_discriminator::<YieldAccount>(),
            &quote_yield_seeds,
        )?;
        let mut quote_yield_account = if quote_created {
            let data = ctx.accounts.quote_yield_account.try_borrow_data()?;
            let mut data_slice: &[u8] = &data;
            YieldAccount::try_deserialize_unchecked(&mut data_slice)?
        } else {
            let data = ctx.accounts.quote_yield_account.try_borrow_data()?;
            let mut data_slice: &[u8] = &data;
            YieldAccount::try_deserialize(&mut data_slice)?
        };
        initialize_or_validate_yield_account(
            &mut quote_yield_account,
            owner_key,
            market_key,
            ylp_mint_key,
            quote_mint_key,
            YieldTokenKind::Ylp,
            quote_yield_bump,
        )?;

        // Checkpoint existing yLP yield before minting new supply.
        {
            let market = &mut ctx.accounts.market;
            market.base_side.carry_forward_swap_fees()?;
            market.base_side.carry_forward_interest()?;
            market.quote_side.carry_forward_swap_fees()?;
            market.quote_side.carry_forward_interest()?;
            base_yield_account.accrue(
                ctx.accounts.owner_ylp_account.amount,
                market.base_side.fees.swap_fee_growth_index_q64,
                market.base_side.fees.interest_growth_index_q64,
            )?;
            quote_yield_account.accrue(
                ctx.accounts.owner_ylp_account.amount,
                market.quote_side.fees.swap_fee_growth_index_q64,
                market.quote_side.fees.interest_growth_index_q64,
            )?;
        }
        {
            let mut data = ctx.accounts.base_yield_account.try_borrow_mut_data()?;
            let mut data_slice: &mut [u8] = &mut data;
            base_yield_account.try_serialize(&mut data_slice)?;
        }
        {
            let mut data = ctx.accounts.quote_yield_account.try_borrow_mut_data()?;
            let mut data_slice: &mut [u8] = &mut data;
            quote_yield_account.try_serialize(&mut data_slice)?;
        }

        // Transfer both assets and measure their actual reserve credits.
        let transfers = ctx.accounts.transfers(&args)?;
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
        transfer_checked_with_remaining_accounts(
            ctx.accounts.owner.to_account_info(),
            ctx.accounts.owner_base_account.to_account_info(),
            ctx.accounts.base_reserve_vault.to_account_info(),
            ctx.accounts.base_mint.to_account_info(),
            base_token_program,
            transfers.base_transfer_amount,
            ctx.accounts.base_mint.decimals,
            &[],
            ctx.remaining_accounts,
        )?;
        transfer_checked_with_remaining_accounts(
            ctx.accounts.owner.to_account_info(),
            ctx.accounts.owner_quote_account.to_account_info(),
            ctx.accounts.quote_reserve_vault.to_account_info(),
            ctx.accounts.quote_mint.to_account_info(),
            quote_token_program,
            transfers.quote_transfer_amount,
            ctx.accounts.quote_mint.decimals,
            &[],
            ctx.remaining_accounts,
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

        // Commit measured liquidity before finalizing the curve observation.
        let receipt = ctx
            .accounts
            .market
            .add_liquidity(base_reserve_credit, quote_reserve_credit)?;
        require_gte!(receipt.ylp_amount, args.min_ylp_amount, ErrorCode::SlippageExceeded);
        let current_slot = Clock::get()?.slot;
        // Adding depth cannot consume an underwriting shape in this
        // instruction. One canonical curve evaluation finalizes D/Q and the
        // exact observation; pessimistic lending shapes remain lazy.
        ctx.accounts
            .market
            .finalize_amm_transition_and_observe_risk(current_slot)?;
        require_reserve_custody(ctx.accounts.base_reserve_vault.amount, &ctx.accounts.market.base_side)?;
        require_reserve_custody(ctx.accounts.quote_reserve_vault.amount, &ctx.accounts.market.quote_side)?;

        // Mint yLP only after reserve and curve accounting succeeds.
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
            base_live_reserve: ctx.accounts.market.base_side.reserves.live_reserve,
            quote_live_reserve: ctx.accounts.market.quote_side.reserves.live_reserve,
            metadata: MarketEventMetadata::new(owner_key, market_key)?,
        });

        Ok(())
    }
}
