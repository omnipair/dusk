use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::{MarketCollateralDeposited, MarketEventMetadata},
    shared::{account::get_size_with_discriminator, token::transfer_from_user_to_vault},
    state::{BorrowPosition, Market},
};

use crate::instructions::common::{require_supported_asset_mint, token_program_for_mint};

use super::common::validate_collateral_accounts;

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct DepositCollateralArgs {
    pub position_id: Pubkey,
    pub deposit_amount: u64,
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(args: DepositCollateralArgs)]
pub struct DepositCollateral<'info> {
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

    #[account(mut)]
    pub collateral_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut)]
    pub owner_asset_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        init_if_needed,
        payer = owner,
        space = get_size_with_discriminator::<BorrowPosition>(),
        seeds = [
            BORROW_POSITION_SEED_PREFIX,
            market.key().as_ref(),
            args.position_id.as_ref(),
        ],
        bump
    )]
    pub borrow_position: Box<Account<'info, BorrowPosition>>,

    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
}

impl<'info> DepositCollateral<'info> {
    pub fn validate(&self, args: &DepositCollateralArgs) -> Result<()> {
        self.market.assert_started()?;
        require!(args.deposit_amount > 0, ErrorCode::AmountZero);
        require_gte!(
            self.owner_asset_account.amount,
            args.deposit_amount,
            ErrorCode::InsufficientBalance
        );
        validate_collateral_accounts(
            &self.market,
            self.owner.key(),
            &self.asset_mint,
            &self.collateral_vault,
            &self.owner_asset_account,
        )?;
        require_supported_asset_mint(&self.asset_mint)?;
        if self.borrow_position.is_initialized() {
            self.borrow_position
                .assert_position(self.owner.key(), self.market.key())?;
        }
        Ok(())
    }

    crate::instructions::common::market_update_and_validate!(DepositCollateralArgs);

    pub fn handle_deposit(mut ctx: Context<Self>, args: DepositCollateralArgs) -> Result<()> {
        let borrow_position_bump = ctx.bumps.borrow_position;
        let (market_key, owner_key, asset_mint_key, collateral_receipt) = {
            let accounts = &mut ctx.accounts;
            let market_key = accounts.market.key();
            let owner_key = accounts.owner.key();
            let asset_mint_key = accounts.asset_mint.key();
            let market_asset = accounts.market.asset_for_mint(asset_mint_key)?;

            if !accounts.borrow_position.is_initialized() {
                accounts
                    .borrow_position
                    .initialize(owner_key, market_key, args.position_id, borrow_position_bump);
            }
            accounts.borrow_position.assert_position(owner_key, market_key)?;

            let collateral_balance_before = accounts.collateral_vault.amount;
            let asset_token_program = token_program_for_mint(
                &accounts.asset_mint,
                &accounts.token_program,
                &accounts.token_2022_program,
            )?;
            transfer_from_user_to_vault(
                accounts.owner.to_account_info(),
                accounts.owner_asset_account.to_account_info(),
                accounts.collateral_vault.to_account_info(),
                accounts.asset_mint.to_account_info(),
                asset_token_program,
                args.deposit_amount,
                accounts.asset_mint.decimals,
            )?;
            accounts.collateral_vault.reload()?;
            let collateral_credit = accounts
                .collateral_vault
                .amount
                .checked_sub(collateral_balance_before)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            require!(collateral_credit > 0, ErrorCode::AmountZero);

            let collateral_receipt = accounts
                .borrow_position
                .deposit_collateral(market_asset, collateral_credit)?;
            (market_key, owner_key, asset_mint_key, collateral_receipt)
        };

        emit_cpi!(MarketCollateralDeposited {
            market: market_key,
            owner: owner_key,
            asset_mint: asset_mint_key,
            collateral_credit: collateral_receipt.collateral_credit,
            base_collateral: collateral_receipt.base_collateral,
            quote_collateral: collateral_receipt.quote_collateral,
            metadata: MarketEventMetadata::new(owner_key, market_key)?,
        });

        Ok(())
    }
}
