use crate::transitions::amm::SwapRequest;
use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::{LeveragePositionUpdated, LeverageSwapReceipt, MarketEventMetadata, SwapExecuted, SwapOrigin},
    generate_market_seeds,
    state::{FutarchyAuthority, LeveragePosition, Market, MarketAsset},
    token::transfer_checked_with_remaining_accounts,
    transitions::liquidity::SwapCashPolicy,
};

use super::settlement::{
    leverage_collateral_admission_fee, leverage_collateral_credit, leverage_swap_fee_credit, prepare_leverage_swap,
    settle_inline_leverage_hlp, validate_leverage_mints, validate_leverage_reserve_accounts,
};
use crate::instructions::accounts::{
    require_reserve_custody, token_account_credit, token_program_for_mint, HlpSwapAccountLayout,
};
use crate::instructions::enforce_launch_same_transaction_guard;

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct IncreaseLeverageArgs {
    pub debt_asset: u8,
    pub debt_amount: u64,
    /// Gross debt-token funding from the owner. Zero increases size using borrowing only.
    pub margin_amount: u64,
    pub min_collateral_out: u64,
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(args: IncreaseLeverageArgs)]
pub struct IncreaseLeverage<'info> {
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

    #[account(seeds = [FUTARCHY_AUTHORITY_SEED_PREFIX], bump = futarchy_authority.bump)]
    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,

    #[account(address = leverage_position.owner)]
    /// CHECK: Owner address bound by leverage_position.
    pub position_owner: AccountInfo<'info>,

    #[account(
        mut,
        seeds = [
            LEVERAGE_POSITION_SEED_PREFIX,
            market.key().as_ref(),
            leverage_position.owner.as_ref(),
            leverage_position.namespace_authority.as_ref(),
            leverage_position.position_id.as_ref(),
        ],
        bump = leverage_position.bump,
        constraint = leverage_position.market == market.key() @ ErrorCode::InvalidLeveragePosition,
        constraint = leverage_position.debt_asset == args.debt_asset @ ErrorCode::InvalidLeveragePosition,
    )]
    pub leverage_position: Box<Account<'info, LeveragePosition>>,

    pub debt_mint: Box<InterfaceAccount<'info, Mint>>,
    pub collateral_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(mut)]
    pub debt_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub collateral_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        seeds = [
            LEVERAGE_COLLATERAL_VAULT_SEED_PREFIX,
            market.key().as_ref(),
            collateral_mint.key().as_ref(),
        ],
        bump,
        constraint = leverage_collateral_vault.mint == collateral_mint.key() @ ErrorCode::InvalidVault,
        constraint = leverage_collateral_vault.owner == market.key() @ ErrorCode::InvalidVault
    )]
    pub leverage_collateral_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut)]
    pub owner: Signer<'info>,

    /// Required only when margin_amount is positive; always owned by the position owner.
    #[account(mut)]
    pub owner_debt_account: Option<Box<InterfaceAccount<'info, TokenAccount>>>,

    /// CHECK: Canonical Instructions sysvar for the launch split guard.
    #[account(address = anchor_lang::solana_program::sysvar::instructions::ID)]
    pub instructions_sysvar: UncheckedAccount<'info>,

    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> IncreaseLeverage<'info> {
    pub fn validate_at(&self, args: &IncreaseLeverageArgs, unix_timestamp: i64) -> Result<()> {
        self.market
            .assert_live_with_futarchy_at(&self.futarchy_authority, unix_timestamp)?;
        require_keys_eq!(self.owner.key(), self.position_owner.key(), ErrorCode::InvalidSigner);
        require!(args.debt_amount > 0 || args.margin_amount > 0, ErrorCode::AmountZero);
        if args.margin_amount > 0 {
            let funding = self.owner_debt_account.as_ref().ok_or(ErrorCode::InvalidTokenAccount)?;
            super::settlement::validate_owner_debt_account(self.owner.key(), &self.debt_mint, funding)?;
            require_gte!(funding.amount, args.margin_amount, ErrorCode::InsufficientBalance);
        }
        let debt_asset = MarketAsset::try_from_code(args.debt_asset)?;
        enforce_launch_same_transaction_guard(
            &self.market,
            self.market.key(),
            debt_asset,
            unix_timestamp,
            &self.instructions_sysvar.to_account_info(),
        )?;
        validate_leverage_mints(&self.market, debt_asset, &self.debt_mint, &self.collateral_mint)?;
        validate_leverage_reserve_accounts(
            &self.market,
            debt_asset,
            &self.debt_mint,
            &self.collateral_mint,
            &self.debt_reserve_vault,
            &self.collateral_reserve_vault,
        )?;
        self.leverage_position.require_collateral()?;
        Ok(())
    }

    pub fn handle_increase(
        ctx: Context<'_, '_, '_, 'info, Self>,
        args: IncreaseLeverageArgs,
        current_slot: u64,
        current_epoch: u64,
        current_unix_timestamp: i64,
    ) -> Result<()> {
        let h_lp_accounts = {
            let market: &Market = &ctx.accounts.market;
            HlpSwapAccountLayout::try_from((market, ctx.remaining_accounts))?
        };
        let market_key = ctx.accounts.market.key();
        let owner_key = ctx.accounts.owner.key();
        let debt_asset = MarketAsset::try_from_code(args.debt_asset)?;
        let debt_mint_key = ctx.accounts.debt_mint.key();
        let collateral_mint_key = ctx.accounts.collateral_mint.key();
        let position_key = ctx.accounts.leverage_position.key();

        // User funding and borrowed debt are swapped together, just as on entry.
        crate::instructions::accounting::accrue_market_interest(
            &mut ctx.accounts.market,
            current_slot,
            ctx.accounts.event_authority.to_account_info(),
        )?;
        let margin_credit = if args.margin_amount > 0 {
            let before = ctx.accounts.debt_reserve_vault.amount;
            let funding = ctx
                .accounts
                .owner_debt_account
                .as_ref()
                .ok_or(ErrorCode::InvalidTokenAccount)?;
            transfer_checked_with_remaining_accounts(
                ctx.accounts.owner.to_account_info(),
                funding.to_account_info(),
                ctx.accounts.debt_reserve_vault.to_account_info(),
                ctx.accounts.debt_mint.to_account_info(),
                token_program_for_mint(
                    &ctx.accounts.debt_mint,
                    &ctx.accounts.token_program,
                    &ctx.accounts.token_2022_program,
                )?,
                args.margin_amount,
                ctx.accounts.debt_mint.decimals,
                &[],
                h_lp_accounts.hook_accounts(ctx.remaining_accounts),
            )?;
            ctx.accounts.debt_reserve_vault.reload()?;
            let credit = token_account_credit(before, &ctx.accounts.debt_reserve_vault)?;
            require_gt!(credit, 0, ErrorCode::InsufficientAmount);
            credit
        } else {
            0
        };
        let input = args
            .debt_amount
            .checked_add(margin_credit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let cash_policy = if args.debt_amount == 0 {
            SwapCashPolicy::Spot
        } else {
            SwapCashPolicy::Borrow {
                asset: debt_asset,
                amount: args.debt_amount,
            }
        };
        let prepared_swap = prepare_leverage_swap(
            &mut ctx.accounts.market,
            SwapRequest {
                current_slot,
                current_unix_timestamp,
                asset_in: debt_asset,
                reserve_credit: input,
                protocol_fee_bps: ctx.accounts.futarchy_authority.revenue_share.swap_bps,
            },
            cash_policy,
        )?;
        let swap = prepared_swap.leverage_quote();
        let interest_eligibility = prepared_swap.interest_eligibility;
        let collateral_credit =
            leverage_collateral_credit(&ctx.accounts.collateral_mint, swap.amount_out, current_epoch)?;
        require_gte!(collateral_credit, args.min_collateral_out, ErrorCode::SlippageExceeded);

        // Move purchased collateral from the reserve into position custody.
        let swap_fee_credit = leverage_swap_fee_credit(&swap)?;
        let collateral_token_program = token_program_for_mint(
            &ctx.accounts.collateral_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        transfer_checked_with_remaining_accounts(
            ctx.accounts.market.to_account_info(),
            ctx.accounts.collateral_reserve_vault.to_account_info(),
            ctx.accounts.leverage_collateral_vault.to_account_info(),
            ctx.accounts.collateral_mint.to_account_info(),
            collateral_token_program,
            swap.amount_out,
            ctx.accounts.collateral_mint.decimals,
            &[&generate_market_seeds!(ctx.accounts.market)[..]],
            h_lp_accounts.hook_accounts(ctx.remaining_accounts),
        )?;

        // Commit position accounting and settle the resulting hLP exposure.
        let receipt = ctx.accounts.market.increase_leverage(
            &mut ctx.accounts.leverage_position,
            args.debt_amount,
            margin_credit,
            collateral_credit,
            prepared_swap,
            swap_fee_credit,
            ctx.accounts.futarchy_authority.revenue_share.swap_bps,
            ctx.accounts.futarchy_authority.protocol_auction_split,
            current_slot,
            current_unix_timestamp,
            leverage_collateral_admission_fee(&ctx.accounts.collateral_mint, current_epoch)?,
        )?;

        settle_inline_leverage_hlp(
            &mut ctx.accounts.market,
            &ctx.accounts.futarchy_authority,
            debt_asset,
            &ctx.accounts.debt_mint,
            &ctx.accounts.collateral_mint,
            &ctx.accounts.debt_reserve_vault,
            &ctx.accounts.collateral_reserve_vault,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
            ctx.remaining_accounts,
            h_lp_accounts,
            receipt.base_hlp_rebalance,
            receipt.quote_hlp_rebalance,
            interest_eligibility,
            ctx.accounts.event_authority.to_account_info(),
        )?;

        // Reconcile physical reserve custody after inline settlement.
        ctx.accounts.debt_reserve_vault.reload()?;
        ctx.accounts.collateral_reserve_vault.reload()?;
        require_reserve_custody(
            ctx.accounts.debt_reserve_vault.amount,
            ctx.accounts.market.side(debt_asset),
        )?;
        require_reserve_custody(
            ctx.accounts.collateral_reserve_vault.amount,
            ctx.accounts.market.side(debt_asset.opposite()),
        )?;

        // Emit the final position state.
        let swap_event = LeverageSwapReceipt::new(
            swap,
            swap_fee_credit,
            ctx.accounts.market.base_side.reserves.live_reserve,
            ctx.accounts.market.quote_side.reserves.live_reserve,
        )?;
        emit_cpi!(SwapExecuted::from_leverage(
            market_key,
            owner_key,
            owner_key,
            position_key,
            SwapOrigin::LeverageIncrease,
            current_slot,
            swap_event,
            swap.start_price_nad,
            &ctx.accounts.market,
        )?);
        emit_cpi!(LeveragePositionUpdated {
            market: market_key,
            position: position_key,
            owner: owner_key,
            debt_asset_mint: debt_mint_key,
            collateral_asset_mint: collateral_mint_key,
            owner_debit: args.margin_amount,
            borrowed_amount: receipt.borrowed_amount,
            debt_delta: receipt.debt_delta,
            collateral_delta: receipt.collateral_delta,
            debt_amount: receipt.debt_amount,
            debt_shares: receipt.debt_shares,
            collateral_amount: receipt.collateral_amount,
            margin_terms: ctx.accounts.leverage_position.margin_terms,
            closeout_value: receipt.closeout_value,
            owner_credit: 0,
            interest_paid: receipt.interest_paid,
            swap: Some(swap_event),
            metadata: MarketEventMetadata::at_slot(owner_key, market_key, current_slot),
        });
        Ok(())
    }
}
