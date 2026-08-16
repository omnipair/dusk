use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::Instruction;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::HlpOpened,
    generate_market_seeds,
    state::{FutarchyAuthority, Market, MarketAsset, YieldAccount, YieldTokenKind},
    token::{create_token_account, token_mint_to_with_instruction, transfer_checked_with_remaining_accounts},
};

use crate::instructions::accounts::{
    derive_hlp_ylp_vault_address, require_reserve_custody, require_supported_asset_mint, token_program_for_mint,
    validate_lp_mint, validate_owner_asset_account, validate_owner_lp_account, validate_side_vault_accounts,
};

use super::{reconcile_live_hlp_supply, validate_hlp_authority_pdas, validate_hlp_yield_account_pda};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct DepositSingleSidedArgs {
    pub deposit_amount: u64,
    pub min_hlp_amount: u64,
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(args: DepositSingleSidedArgs)]
pub struct DepositSingleSided<'info> {
    #[account(mut)]
    pub market: Box<Account<'info, Market>>,

    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,

    #[account(mut)]
    pub owner: Signer<'info>,

    pub base_mint: Box<InterfaceAccount<'info, Mint>>,
    pub quote_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut)]
    pub ylp_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut)]
    pub target_hlp_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(mut)]
    pub base_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub quote_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut)]
    pub owner_target_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub owner_hlp_account: Box<InterfaceAccount<'info, TokenAccount>>,

    /// CHECK: Canonical PDA, initialization state, token program, mint, and
    /// authority are all validated before any market mutation. A System-owned
    /// empty PDA is initialized inline by the deposit handler.
    #[account(mut)]
    pub hlp_ylp_account: UncheckedAccount<'info>,

    #[account(mut)]
    pub base_yield_account: Box<Account<'info, YieldAccount>>,

    #[account(mut)]
    pub quote_yield_account: Box<Account<'info, YieldAccount>>,

    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
}

impl<'info> DepositSingleSided<'info> {
    pub fn validate(&self, args: &DepositSingleSidedArgs) -> Result<()> {
        validate_hlp_authority_pdas(
            &self.market,
            self.market.key(),
            &self.futarchy_authority,
            self.futarchy_authority.key(),
        )?;
        self.market.assert_live_with_futarchy(&self.futarchy_authority)?;
        require!(args.deposit_amount > 0, ErrorCode::AmountZero);
        validate_side_vault_accounts(
            &self.market,
            MarketAsset::Base,
            &self.base_mint,
            &self.base_reserve_vault,
        )?;
        validate_side_vault_accounts(
            &self.market,
            MarketAsset::Quote,
            &self.quote_mint,
            &self.quote_reserve_vault,
        )?;
        require_keys_eq!(self.market.ylp_mint, self.ylp_mint.key(), ErrorCode::InvalidLpMintKey);
        let target_asset = self.market.asset_for_hlp_mint(self.target_hlp_mint.key())?;
        let target_mint = match target_asset {
            MarketAsset::Base => &self.base_mint,
            MarketAsset::Quote => &self.quote_mint,
        };
        let target_hlp_mint = self.market.side(target_asset).hlp_mint;
        require_keys_eq!(target_hlp_mint, self.target_hlp_mint.key(), ErrorCode::InvalidMint);
        validate_owner_asset_account(self.owner.key(), target_mint, &self.owner_target_account)?;
        validate_owner_lp_account(self.owner.key(), &self.target_hlp_mint, &self.owner_hlp_account)?;
        validate_lp_mint(&self.target_hlp_mint, self.market.key(), target_mint.decimals)?;
        validate_lp_mint(&self.ylp_mint, self.market.key(), self.base_mint.decimals)?;
        validate_hlp_yield_account_pda(
            self.base_yield_account.key(),
            self.base_yield_account.bump,
            self.market.key(),
            self.owner.key(),
            self.target_hlp_mint.key(),
            self.base_mint.key(),
        )?;
        self.base_yield_account.assert_account(
            self.owner.key(),
            self.market.key(),
            self.target_hlp_mint.key(),
            self.base_mint.key(),
            YieldTokenKind::Hlp,
        )?;
        validate_hlp_yield_account_pda(
            self.quote_yield_account.key(),
            self.quote_yield_account.bump,
            self.market.key(),
            self.owner.key(),
            self.target_hlp_mint.key(),
            self.quote_mint.key(),
        )?;
        self.quote_yield_account.assert_account(
            self.owner.key(),
            self.market.key(),
            self.target_hlp_mint.key(),
            self.quote_mint.key(),
            YieldTokenKind::Hlp,
        )?;
        let (expected_hlp_ylp_account, _) =
            derive_hlp_ylp_vault_address(self.market.key(), self.target_hlp_mint.key(), self.ylp_mint.key());
        require_keys_eq!(
            self.hlp_ylp_account.key(),
            expected_hlp_ylp_account,
            ErrorCode::InvalidVault
        );
        let hlp_ylp_info = self.hlp_ylp_account.to_account_info();
        if *hlp_ylp_info.owner == System::id() {
            require!(hlp_ylp_info.data_is_empty(), ErrorCode::InvalidVault);
        } else {
            require_keys_eq!(
                *hlp_ylp_info.owner,
                self.token_2022_program.key(),
                ErrorCode::InvalidTokenProgram
            );
            let data = hlp_ylp_info.try_borrow_data()?;
            let mut data_slice: &[u8] = &data;
            let account = TokenAccount::try_deserialize_unchecked(&mut data_slice)?;
            require_keys_eq!(account.mint, self.ylp_mint.key(), ErrorCode::InvalidTokenAccount);
            require_keys_eq!(account.owner, self.market.key(), ErrorCode::InvalidVault);
        }
        require_supported_asset_mint(&self.base_mint)?;
        require_supported_asset_mint(&self.quote_mint)?;
        Ok(())
    }

    pub fn update_and_validate(&mut self, args: &DepositSingleSidedArgs) -> Result<()> {
        self.validate(args)?;
        let target_asset = self.market.asset_for_hlp_mint(self.target_hlp_mint.key())?;
        let current_slot = Clock::get()?.slot;
        self.market.accrue_interest_to_slot(current_slot)?;
        reconcile_live_hlp_supply(&mut self.market, target_asset, self.target_hlp_mint.supply)?;
        self.market.assert_current_version()?;
        if self.market.base_side.reserves.live_reserve > 0 && self.market.quote_side.reserves.live_reserve > 0 {
            self.market.advance_amm_clock(current_slot)?;
            self.market.checkpoint_hlp_vaults()?;
            self.market.assert_hlp_entry_available(target_asset)?;
            self.market.observe_current_risk(current_slot)?;
        }
        Ok(())
    }

    pub fn handle_deposit(ctx: Context<'_, '_, '_, 'info, Self>, args: DepositSingleSidedArgs) -> Result<()> {
        let market_key = ctx.accounts.market.key();
        let owner_key = ctx.accounts.owner.key();
        let target_hlp_mint_key = ctx.accounts.target_hlp_mint.key();
        let ylp_mint_key = ctx.accounts.ylp_mint.key();

        // Create the canonical yLP custody account on its first deposit.
        let (_, hlp_ylp_bump) = derive_hlp_ylp_vault_address(market_key, target_hlp_mint_key, ylp_mint_key);
        create_token_account(
            &ctx.accounts.market.to_account_info(),
            &ctx.accounts.owner.to_account_info(),
            &ctx.accounts.hlp_ylp_account.to_account_info(),
            &ctx.accounts.ylp_mint.to_account_info(),
            &ctx.accounts.system_program.to_account_info(),
            &ctx.accounts.token_2022_program.to_account_info(),
            &[
                HLP_YLP_VAULT_SEED_PREFIX,
                market_key.as_ref(),
                target_hlp_mint_key.as_ref(),
                ylp_mint_key.as_ref(),
                &[hlp_ylp_bump],
            ],
        )?;
        let target_asset = ctx
            .accounts
            .market
            .asset_for_hlp_mint(ctx.accounts.target_hlp_mint.key())?;

        // Transfer the target asset and measure the reserve's net credit.
        let (target_reserve_vault, target_mint) = match target_asset {
            MarketAsset::Base => (
                ctx.accounts.base_reserve_vault.to_account_info(),
                ctx.accounts.base_mint.to_account_info(),
            ),
            MarketAsset::Quote => (
                ctx.accounts.quote_reserve_vault.to_account_info(),
                ctx.accounts.quote_mint.to_account_info(),
            ),
        };
        let reserve_before = match target_asset {
            MarketAsset::Base => ctx.accounts.base_reserve_vault.amount,
            MarketAsset::Quote => ctx.accounts.quote_reserve_vault.amount,
        };
        let target_token_program = token_program_for_mint(
            match target_asset {
                MarketAsset::Base => &ctx.accounts.base_mint,
                MarketAsset::Quote => &ctx.accounts.quote_mint,
            },
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        transfer_checked_with_remaining_accounts(
            ctx.accounts.owner.to_account_info(),
            ctx.accounts.owner_target_account.to_account_info(),
            target_reserve_vault,
            target_mint,
            target_token_program,
            args.deposit_amount,
            match target_asset {
                MarketAsset::Base => ctx.accounts.base_mint.decimals,
                MarketAsset::Quote => ctx.accounts.quote_mint.decimals,
            },
            &[],
            ctx.remaining_accounts,
        )?;
        match target_asset {
            MarketAsset::Base => ctx.accounts.base_reserve_vault.reload()?,
            MarketAsset::Quote => ctx.accounts.quote_reserve_vault.reload()?,
        }
        let deposit_credit = match target_asset {
            MarketAsset::Base => ctx.accounts.base_reserve_vault.amount.checked_sub(reserve_before),
            MarketAsset::Quote => ctx.accounts.quote_reserve_vault.amount.checked_sub(reserve_before),
        }
        .ok_or(ErrorCode::MarketMathOverflow)?;

        // Apply hLP accounting and checkpoint the immutable post-deposit curve.
        let current_slot = Clock::get()?.slot;
        let receipt = ctx
            .accounts
            .market
            .deposit_single_sided(target_asset, deposit_credit, args.min_hlp_amount)?;
        // Validation verified that no due concentrated controller
        // state would price this entry against a stale NAV. One final curve
        // evaluation now supplies D/Q accounting and the exact risk observation
        // for the immutable post-deposit state.
        ctx.accounts
            .market
            .finalize_amm_transition_and_observe_risk(current_slot)?;
        require_reserve_custody(ctx.accounts.base_reserve_vault.amount, &ctx.accounts.market.base_side)?;
        require_reserve_custody(ctx.accounts.quote_reserve_vault.amount, &ctx.accounts.market.quote_side)?;
        let (base_swap_growth, base_interest_growth) = ctx
            .accounts
            .market
            .hlp_yield_growth_indexes(target_asset, MarketAsset::Base);
        let (quote_swap_growth, quote_interest_growth) = ctx
            .accounts
            .market
            .hlp_yield_growth_indexes(target_asset, MarketAsset::Quote);
        ctx.accounts.base_yield_account.accrue(
            ctx.accounts.owner_hlp_account.amount,
            base_swap_growth,
            base_interest_growth,
        )?;
        ctx.accounts.quote_yield_account.accrue(
            ctx.accounts.owner_hlp_account.amount,
            quote_swap_growth,
            quote_interest_growth,
        )?;

        // Mint backing yLP into custody and hLP to the depositor.
        let ylp_program = token_program_for_mint(
            &ctx.accounts.ylp_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        let hlp_program = token_program_for_mint(
            &ctx.accounts.target_hlp_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        let market_seeds = generate_market_seeds!(ctx.accounts.market);
        let signer_seeds = [&market_seeds[..]];
        let mut mint_instruction = Instruction {
            program_id: *ylp_program.key,
            accounts: Vec::with_capacity(3),
            data: Vec::with_capacity(9),
        };
        token_mint_to_with_instruction(
            &mut mint_instruction,
            ctx.accounts.market.to_account_info(),
            ylp_program.clone(),
            ctx.accounts.ylp_mint.to_account_info(),
            ctx.accounts.hlp_ylp_account.to_account_info(),
            receipt.ylp_amount,
            &signer_seeds,
        )?;
        token_mint_to_with_instruction(
            &mut mint_instruction,
            ctx.accounts.market.to_account_info(),
            hlp_program,
            ctx.accounts.target_hlp_mint.to_account_info(),
            ctx.accounts.owner_hlp_account.to_account_info(),
            receipt.hlp_amount,
            &signer_seeds,
        )?;

        emit_cpi!(HlpOpened {
            market: market_key,
            owner: owner_key,
            asset_side: target_asset.code(),
            deposit_amount: receipt.deposit_amount,
            borrowed_amount: receipt.borrowed_amount,
            ylp_amount: receipt.ylp_amount,
            hlp_amount: receipt.hlp_amount,
            ylp_supply: ctx.accounts.market.base_side.shares.ylp_supply,
            hlp_supply: receipt.hlp_supply,
            base_live_reserve: ctx.accounts.market.base_side.reserves.live_reserve,
            quote_live_reserve: ctx.accounts.market.quote_side.reserves.live_reserve,
        });

        Ok(())
    }
}
