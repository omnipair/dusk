use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::SwapExecuted,
    generate_market_seeds,
    state::{FutarchyAuthority, Market, MarketAsset},
    token::{get_transfer_fee_for_epoch, token_burn, token_mint_to, transfer_checked_with_remaining_accounts},
    transitions::{HlpRebalanceReceipt, HlpRecoveryBreakdown},
};

use crate::instructions::accounts::{
    require_reserve_custody, require_supported_asset_mint, token_account_info_amount, token_account_info_credit,
    token_program_for_mint, validate_owner_asset_account, HlpSwapAccountLayout, BASE_HLP_YLP_VAULT_INDEX,
    BASE_INTEREST_VAULT_INDEX, HLP_SWAP_ACCOUNT_PREFIX_LEN, HLP_YLP_MINT_INDEX, QUOTE_HLP_YLP_VAULT_INDEX,
    QUOTE_INTEREST_VAULT_INDEX,
};
use crate::instructions::liquidity::record_inline_hlp_interest_credit;
use crate::instructions::{enforce_launch_same_transaction_guard, rebalance_executes_token_changes, SwapRequest};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct SwapArgs {
    pub exact_asset_in: u64,
    pub min_asset_out: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SwapExecutionMode {
    Ordinary,
    HlpRecovery,
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(args: SwapArgs)]
pub struct Swap<'info> {
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
    pub trader: Signer<'info>,

    pub asset_in_mint: Box<InterfaceAccount<'info, Mint>>,

    pub asset_out_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(mut)]
    pub reserve_in_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut)]
    pub reserve_out_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut)]
    pub trader_asset_in_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut)]
    pub trader_asset_out_account: Box<InterfaceAccount<'info, TokenAccount>>,

    /// CHECK: Address-constrained canonical Instructions sysvar used only
    /// during the configured launch rate-limit window.
    #[account(address = anchor_lang::solana_program::sysvar::instructions::ID)]
    pub instructions_sysvar: UncheckedAccount<'info>,

    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> Swap<'info> {
    pub(crate) fn validate_and_read_clock(&self, args: &SwapArgs, mode: SwapExecutionMode) -> Result<(u64, u64, i64)> {
        // Read the sysvar once for the complete swap. In particular, do not
        // call `Market::assert_started`, which would fetch `Clock` a second
        // time before the slot-driven AMM/debt pipeline begins.
        let clock = Clock::get()?;
        self.market.assert_current_version()?;
        require!(
            clock.unix_timestamp >= self.market.config.start_time,
            ErrorCode::MarketNotStarted
        );
        if mode == SwapExecutionMode::Ordinary {
            require!(
                !self.futarchy_authority.is_reduce_only(self.market.reduce_only),
                ErrorCode::ReduceOnlyMode
            );
        }
        require!(args.exact_asset_in > 0, ErrorCode::AmountZero);
        require_gte!(
            self.trader_asset_in_account.amount,
            args.exact_asset_in,
            ErrorCode::InsufficientBalance
        );
        let asset_in = self.market.asset_for_mint(self.asset_in_mint.key())?;
        let asset_out = self.market.asset_for_mint(self.asset_out_mint.key())?;
        require!(asset_out == asset_in.opposite(), ErrorCode::InvalidMint);
        enforce_launch_same_transaction_guard(
            &self.market,
            self.market.key(),
            asset_in,
            clock.unix_timestamp,
            &self.instructions_sysvar.to_account_info(),
        )?;
        let (market_side_in, market_side_out) = self.market.swap_sides(asset_in);
        require_keys_eq!(
            market_side_in.reserve_vault,
            self.reserve_in_vault.key(),
            ErrorCode::InvalidVault
        );
        require_keys_eq!(
            market_side_out.reserve_vault,
            self.reserve_out_vault.key(),
            ErrorCode::InvalidVault
        );
        require_keys_eq!(
            self.reserve_in_vault.mint,
            self.asset_in_mint.key(),
            ErrorCode::InvalidVault
        );
        require_keys_eq!(
            self.reserve_out_vault.mint,
            self.asset_out_mint.key(),
            ErrorCode::InvalidVault
        );
        require_keys_eq!(self.reserve_in_vault.owner, self.market.key(), ErrorCode::InvalidVault);
        require_keys_eq!(self.reserve_out_vault.owner, self.market.key(), ErrorCode::InvalidVault);
        validate_owner_asset_account(self.trader.key(), &self.asset_in_mint, &self.trader_asset_in_account)?;
        validate_owner_asset_account(self.trader.key(), &self.asset_out_mint, &self.trader_asset_out_account)?;
        require_supported_asset_mint(&self.asset_in_mint)?;
        require_supported_asset_mint(&self.asset_out_mint)?;
        Ok((clock.slot, clock.epoch, clock.unix_timestamp))
    }

    pub(crate) fn handle_swap(
        mut ctx: Context<'_, '_, '_, 'info, Self>,
        args: SwapArgs,
        current_slot: u64,
        current_epoch: u64,
        current_unix_timestamp: i64,
        mode: SwapExecutionMode,
    ) -> Result<()> {
        // The fixed hLP prefix is checked before transfer-fee, invariant,
        // controller, or hedge math. Only the trailing account slice is ever
        // offered to Token-2022 transfer-hook resolution.
        let h_lp_accounts = {
            let market: &Market = &ctx.accounts.market;
            HlpSwapAccountLayout::try_from((market, ctx.remaining_accounts))?
        };
        let market_key = ctx.accounts.market.key();
        let trader_key = ctx.accounts.trader.key();
        let asset_in = ctx.accounts.market.asset_for_mint(ctx.accounts.asset_in_mint.key())?;
        let protocol_fee_bps = ctx.accounts.futarchy_authority.revenue_share.swap_bps;
        let protocol_auction_split = ctx.accounts.futarchy_authority.protocol_auction_split;

        let reserve_credit = args
            .exact_asset_in
            .checked_sub(get_transfer_fee_for_epoch(
                &ctx.accounts.asset_in_mint.to_account_info(),
                args.exact_asset_in,
                current_epoch,
            )?)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let prepared = SwapRequest {
            current_slot,
            current_unix_timestamp,
            asset_in,
            reserve_credit,
            protocol_fee_bps,
        }
        .prepare(&mut ctx.accounts.market)?;
        if mode == SwapExecutionMode::HlpRecovery {
            require_hlp_recovery_swap(prepared.quote.recovery, asset_in)?;
        }
        let finalized = prepared.finalize_state(
            &mut ctx.accounts.market,
            current_slot,
            protocol_fee_bps,
            protocol_auction_split,
        )?;
        let quote = prepared.quote;
        let interest_eligibility = prepared.interest_eligibility;
        let base_rebalance = finalized.base_rebalance;
        let quote_rebalance = finalized.quote_rebalance;

        // Final commit validation uses cached vault balances plus the known
        // net input, gross output, and gross hLP-interest deltas. This catches
        // unbacked executable cash or fee liabilities before any token CPI.
        let (base_vault_before, quote_vault_before) = match asset_in {
            MarketAsset::Base => (
                ctx.accounts.reserve_in_vault.amount,
                ctx.accounts.reserve_out_vault.amount,
            ),
            MarketAsset::Quote => (
                ctx.accounts.reserve_out_vault.amount,
                ctx.accounts.reserve_in_vault.amount,
            ),
        };
        let base_projected = projected_reserve_vault_balance(
            base_vault_before,
            if asset_in == MarketAsset::Base {
                reserve_credit
            } else {
                0
            },
            if asset_in == MarketAsset::Quote {
                quote.amount_out
            } else {
                0
            },
            quote_rebalance.interest_paid,
        )?;
        let quote_projected = projected_reserve_vault_balance(
            quote_vault_before,
            if asset_in == MarketAsset::Quote {
                reserve_credit
            } else {
                0
            },
            if asset_in == MarketAsset::Base {
                quote.amount_out
            } else {
                0
            },
            base_rebalance.interest_paid,
        )?;
        require_reserve_custody(base_projected, &ctx.accounts.market.base_side)?;
        require_reserve_custody(quote_projected, &ctx.accounts.market.quote_side)?;

        let asset_in_token_program = token_program_for_mint(
            &ctx.accounts.asset_in_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        transfer_checked_with_remaining_accounts(
            ctx.accounts.trader.to_account_info(),
            ctx.accounts.trader_asset_in_account.to_account_info(),
            ctx.accounts.reserve_in_vault.to_account_info(),
            ctx.accounts.asset_in_mint.to_account_info(),
            asset_in_token_program,
            args.exact_asset_in,
            ctx.accounts.asset_in_mint.decimals,
            &[],
            h_lp_accounts.hook_accounts(ctx.remaining_accounts),
        )?;

        // One call per side and one net token settlement per receipt. A side
        // can mint or burn yLP, never both, and can transfer interest once.
        apply_single_hlp_rebalance_token_changes(
            &mut ctx,
            &base_rebalance,
            BASE_HLP_YLP_VAULT_INDEX,
            QUOTE_INTEREST_VAULT_INDEX,
            h_lp_accounts,
            interest_eligibility,
        )?;
        apply_single_hlp_rebalance_token_changes(
            &mut ctx,
            &quote_rebalance,
            QUOTE_HLP_YLP_VAULT_INDEX,
            BASE_INTEREST_VAULT_INDEX,
            h_lp_accounts,
            interest_eligibility,
        )?;

        let asset_out_token_program = token_program_for_mint(
            &ctx.accounts.asset_out_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        transfer_checked_with_remaining_accounts(
            ctx.accounts.market.to_account_info(),
            ctx.accounts.reserve_out_vault.to_account_info(),
            ctx.accounts.trader_asset_out_account.to_account_info(),
            ctx.accounts.asset_out_mint.to_account_info(),
            asset_out_token_program,
            quote.amount_out,
            ctx.accounts.asset_out_mint.decimals,
            &[&generate_market_seeds!(ctx.accounts.market)[..]],
            h_lp_accounts.hook_accounts(ctx.remaining_accounts),
        )?;
        let asset_out_credit = quote
            .amount_out
            .checked_sub(get_transfer_fee_for_epoch(
                &ctx.accounts.asset_out_mint.to_account_info(),
                quote.amount_out,
                current_epoch,
            )?)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require_gte!(asset_out_credit, args.min_asset_out, ErrorCode::SlippageExceeded);

        emit_cpi!(SwapExecuted {
            market: market_key,
            trader: trader_key,
            asset_in_side: asset_in.code(),
            amount_in: args.exact_asset_in,
            amount_out: asset_out_credit,
            gross_amount_out: quote.gross_amount_out,
            fee_asset_side: quote.fee.fee_asset,
            amount_in_after_fee: quote.fee.amount_in_for_quote,
            base_fee: quote.fee.base_fee_debit,
            divergence_fee: quote.fee.divergence_surcharge_debit,
            volatility_fee: quote.fee.volatility_surcharge_debit,
            retained_fee: quote.fee.retained_surcharge,
            compounded_fee: quote.fee.compounded_fee_debit,
            hlp_recovery_target_asset: quote.recovery.target_asset,
            hlp_recovery_funding_gap: quote.recovery.funding_gap,
            hlp_recovery_matched_input: quote.recovery.matched_input,
            hlp_recovery_bonus_output: quote.recovery.bonus_output,
            hlp_recovery_discount_bps: quote.recovery.discount_bps,
            hlp_recovery_critical: quote.recovery.critical,
            base_live_reserve: ctx.accounts.market.base_side.reserves.live_reserve,
            quote_live_reserve: ctx.accounts.market.quote_side.reserves.live_reserve,
        });

        Ok(())
    }
}

fn require_hlp_recovery_swap(recovery: HlpRecoveryBreakdown, asset_in: MarketAsset) -> Result<()> {
    require!(
        recovery.critical
            && recovery.matched_input > 0
            && recovery.bonus_output > 0
            && recovery.target_asset == asset_in.opposite().code(),
        ErrorCode::HlpNotLiquidatable
    );
    Ok(())
}

fn apply_single_hlp_rebalance_token_changes<'info>(
    ctx: &mut anchor_lang::context::Context<'_, '_, '_, 'info, Swap<'info>>,
    receipt: &HlpRebalanceReceipt,
    ylp_vault_index: usize,
    interest_vault_index: usize,
    accounts: HlpSwapAccountLayout,
    interest_eligibility: crate::transitions::HlpYieldEligibility,
) -> Result<()> {
    if !rebalance_executes_token_changes(receipt) {
        return Ok(());
    }
    require_eq!(
        accounts.prefix_len,
        HLP_SWAP_ACCOUNT_PREFIX_LEN,
        ErrorCode::NotEnoughAccounts
    );
    require!(
        receipt.ylp_mint_amount == 0 || receipt.ylp_burn_amount == 0,
        ErrorCode::BrokenInvariant
    );
    let ylp_mint = &ctx.remaining_accounts[HLP_YLP_MINT_INDEX];
    let ylp_vault = &ctx.remaining_accounts[ylp_vault_index];
    let market_seeds = generate_market_seeds!(ctx.accounts.market);
    let signer_seeds = [&market_seeds[..]];

    if receipt.ylp_mint_amount > 0 {
        token_mint_to(
            ctx.accounts.market.to_account_info(),
            ctx.accounts.token_2022_program.to_account_info(),
            ylp_mint.clone(),
            ylp_vault.clone(),
            receipt.ylp_mint_amount,
            &signer_seeds,
        )?;
    }
    if receipt.ylp_burn_amount > 0 {
        token_burn(
            ctx.accounts.market.to_account_info(),
            ctx.accounts.token_2022_program.to_account_info(),
            ylp_mint.clone(),
            ylp_vault.clone(),
            receipt.ylp_burn_amount,
            &signer_seeds,
        )?;
    }
    if receipt.interest_paid > 0 {
        let borrowed_asset = receipt.target_asset.opposite();
        let interest_vault = &ctx.remaining_accounts[interest_vault_index];
        let interest_vault_balance_before = token_account_info_amount(interest_vault)?;
        let asset_in = ctx.accounts.market.asset_for_mint(ctx.accounts.asset_in_mint.key())?;
        let (reserve_vault, mint, token_program, decimals) = if asset_in == borrowed_asset {
            (
                ctx.accounts.reserve_in_vault.to_account_info(),
                ctx.accounts.asset_in_mint.to_account_info(),
                token_program_for_mint(
                    &ctx.accounts.asset_in_mint,
                    &ctx.accounts.token_program,
                    &ctx.accounts.token_2022_program,
                )?,
                ctx.accounts.asset_in_mint.decimals,
            )
        } else {
            (
                ctx.accounts.reserve_out_vault.to_account_info(),
                ctx.accounts.asset_out_mint.to_account_info(),
                token_program_for_mint(
                    &ctx.accounts.asset_out_mint,
                    &ctx.accounts.token_program,
                    &ctx.accounts.token_2022_program,
                )?,
                ctx.accounts.asset_out_mint.decimals,
            )
        };
        transfer_checked_with_remaining_accounts(
            ctx.accounts.market.to_account_info(),
            reserve_vault,
            interest_vault.clone(),
            mint,
            token_program,
            receipt.interest_paid,
            decimals,
            &signer_seeds,
            accounts.hook_accounts(ctx.remaining_accounts),
        )?;
        let interest_vault_credit = token_account_info_credit(interest_vault_balance_before, interest_vault)?;
        record_inline_hlp_interest_credit(
            &mut ctx.accounts.market,
            borrowed_asset,
            interest_vault_credit,
            ctx.accounts.futarchy_authority.revenue_share.interest_bps,
            ctx.accounts.futarchy_authority.protocol_auction_split,
            interest_eligibility,
        )?;
    }
    Ok(())
}

fn projected_reserve_vault_balance(
    balance_before: u64,
    net_input_credit: u64,
    gross_swap_output: u64,
    gross_interest_debit: u64,
) -> Result<u64> {
    balance_before
        .checked_add(net_input_credit)
        .and_then(|value| value.checked_sub(gross_swap_output))
        .and_then(|value| value.checked_sub(gross_interest_debit))
        .ok_or_else(|| ErrorCode::UnbackedFeeLiability.into())
}

#[cfg(test)]
mod tests {
    include!("../../tests/instructions/spot/swap.rs");
}
