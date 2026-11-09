use crate::transitions::amm::SwapRequest;
use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::{HlpRebalanced, SwapExecuted, SwapOrigin},
    generate_market_seeds,
    state::{FutarchyAuthority, Market, MarketAsset},
    token::{
        get_transfer_fee_for_epoch, invoke_signed_reusing_instruction,
        transfer_checked_with_remaining_accounts_and_instruction,
    },
    transitions::HlpRebalanceReceipt,
};

use crate::instructions::accounts::{
    require_reserve_custody, require_supported_asset_mint, token_account_info_amount, token_account_info_credit,
    token_program_for_mint, validate_owner_asset_account, HlpSwapAccountLayout, BASE_HLP_SRLP_VAULT_INDEX,
    BASE_INTEREST_VAULT_INDEX, HLP_SRLP_MINT_INDEX, HLP_SWAP_ACCOUNT_PREFIX_LEN, QUOTE_HLP_SRLP_VAULT_INDEX,
    QUOTE_INTEREST_VAULT_INDEX,
};
use crate::instructions::enforce_launch_same_transaction_guard;
use crate::instructions::liquidity::record_inline_hlp_interest_credit;

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct SwapArgs {
    pub exact_asset_in: u64,
    pub min_asset_out: u64,
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
    pub fn handle_swap(mut ctx: Context<'_, '_, '_, 'info, Self>, args: SwapArgs) -> Result<()> {
        let accounts = &ctx.accounts;
        // Read the sysvar once for the complete swap. Unix time only gates the
        // existing launch schedule; AMM/debt economic time below uses its slot.
        // Do not call `Market::assert_started`, which would fetch it again.
        let clock = Clock::get()?;
        accounts.market.assert_current_version()?;
        require!(
            clock.unix_timestamp >= accounts.market.config.start_time,
            ErrorCode::MarketNotStarted
        );
        require!(args.exact_asset_in > 0, ErrorCode::AmountZero);
        require_gte!(
            accounts.trader_asset_in_account.amount,
            args.exact_asset_in,
            ErrorCode::InsufficientBalance
        );
        let asset_in = accounts.market.asset_for_mint(accounts.asset_in_mint.key())?;
        require!(
            accounts.asset_out_mint.key() == accounts.market.side(asset_in.opposite()).asset_mint,
            ErrorCode::InvalidMint
        );
        enforce_launch_same_transaction_guard(
            &accounts.market,
            accounts.market.key(),
            asset_in,
            clock.unix_timestamp,
            &accounts.instructions_sysvar.to_account_info(),
        )?;
        let (market_side_in, market_side_out) = accounts.market.swap_sides(asset_in);
        require_keys_eq!(
            market_side_in.reserve_vault,
            accounts.reserve_in_vault.key(),
            ErrorCode::InvalidVault
        );
        require_keys_eq!(
            market_side_out.reserve_vault,
            accounts.reserve_out_vault.key(),
            ErrorCode::InvalidVault
        );
        require_keys_eq!(
            accounts.reserve_in_vault.mint,
            accounts.asset_in_mint.key(),
            ErrorCode::InvalidVault
        );
        require_keys_eq!(
            accounts.reserve_out_vault.mint,
            accounts.asset_out_mint.key(),
            ErrorCode::InvalidVault
        );
        require_keys_eq!(
            accounts.reserve_in_vault.owner,
            accounts.market.key(),
            ErrorCode::InvalidVault
        );
        require_keys_eq!(
            accounts.reserve_out_vault.owner,
            accounts.market.key(),
            ErrorCode::InvalidVault
        );
        validate_owner_asset_account(
            accounts.trader.key(),
            &accounts.asset_in_mint,
            &accounts.trader_asset_in_account,
        )?;
        validate_owner_asset_account(
            accounts.trader.key(),
            &accounts.asset_out_mint,
            &accounts.trader_asset_out_account,
        )?;
        require_supported_asset_mint(&accounts.asset_in_mint)?;
        require_supported_asset_mint(&accounts.asset_out_mint)?;
        let current_slot = clock.slot;
        let current_epoch = clock.epoch;
        let current_unix_timestamp = clock.unix_timestamp;
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
        crate::instructions::accounting::accrue_market_interest(
            &mut ctx.accounts.market,
            current_slot,
            ctx.accounts.event_authority.to_account_info(),
            ctx.accounts.futarchy_authority.revenue_share.interest_bps,
        )?;
        let mut prepared = SwapRequest {
            current_slot,
            current_unix_timestamp,
            asset_in,
            reserve_credit,
            protocol_fee_bps,
        }
        .prepare(&mut ctx.accounts.market)?;
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

        let mut event_instruction = anchor_lang::solana_program::instruction::Instruction {
            program_id: crate::ID,
            accounts: Vec::with_capacity(4),
            data: Vec::with_capacity(std::mem::size_of::<SwapExecuted>() + 16),
        };

        let asset_in_token_program = token_program_for_mint(
            &ctx.accounts.asset_in_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        transfer_checked_with_remaining_accounts_and_instruction(
            &mut event_instruction,
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
        // can mint or burn srLP, never both, and can transfer interest once.
        apply_single_hlp_rebalance_token_changes(
            &mut ctx,
            &base_rebalance,
            BASE_HLP_SRLP_VAULT_INDEX,
            QUOTE_INTEREST_VAULT_INDEX,
            h_lp_accounts,
            interest_eligibility,
            &mut event_instruction,
        )?;
        apply_single_hlp_rebalance_token_changes(
            &mut ctx,
            &quote_rebalance,
            QUOTE_HLP_SRLP_VAULT_INDEX,
            BASE_INTEREST_VAULT_INDEX,
            h_lp_accounts,
            interest_eligibility,
            &mut event_instruction,
        )?;

        let asset_out_token_program = token_program_for_mint(
            &ctx.accounts.asset_out_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        transfer_checked_with_remaining_accounts_and_instruction(
            &mut event_instruction,
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

        // Reuse one event buffer: Solana's bump allocator does not reclaim
        // the temporary vectors produced by consecutive emit_cpi! calls.
        if let Some(event) = HlpRebalanced::from_receipts(
            market_key,
            &ctx.accounts.market,
            base_rebalance,
            quote_rebalance,
            current_slot,
        )? {
            crate::instructions::accounting::emit_cpi_event_with_instruction(
                &event,
                ctx.accounts.event_authority.to_account_info(),
                &mut event_instruction,
            )?;
        }
        crate::instructions::accounting::emit_cpi_event_with_instruction(
            &SwapExecuted::from_amm(
                market_key,
                trader_key,
                trader_key,
                None,
                SwapOrigin::Spot,
                current_slot,
                quote,
                &ctx.accounts.market,
            )?,
            ctx.accounts.event_authority.to_account_info(),
            &mut event_instruction,
        )?;

        Ok(())
    }
}

fn apply_single_hlp_rebalance_token_changes<'info>(
    ctx: &mut anchor_lang::context::Context<'_, '_, '_, 'info, Swap<'info>>,
    receipt: &HlpRebalanceReceipt,
    srlp_vault_index: usize,
    interest_vault_index: usize,
    accounts: HlpSwapAccountLayout,
    interest_eligibility: crate::transitions::HlpYieldEligibility,
    event_instruction: &mut anchor_lang::solana_program::instruction::Instruction,
) -> Result<()> {
    if receipt.srlp_mint_amount == 0 && receipt.srlp_burn_amount == 0 && receipt.interest_paid == 0 {
        return Ok(());
    }
    require_gte!(
        accounts.prefix_len,
        HLP_SWAP_ACCOUNT_PREFIX_LEN,
        ErrorCode::NotEnoughAccounts
    );
    require!(
        receipt.srlp_mint_amount == 0 || receipt.srlp_burn_amount == 0,
        ErrorCode::BrokenInvariant
    );
    let srlp_mint = &ctx.remaining_accounts[HLP_SRLP_MINT_INDEX];
    let srlp_vault = &ctx.remaining_accounts[srlp_vault_index];
    let market_seeds = generate_market_seeds!(ctx.accounts.market);
    let signer_seeds = [&market_seeds[..]];

    // MintTo and Burn have the same three account roles and nine-byte wire
    // format. Reuse the swap's CPI vectors instead of abandoning two fresh
    // instruction buffers on Solana's bump heap for each vault.
    let (tag, amount, first, second) = if receipt.srlp_mint_amount > 0 {
        (7, receipt.srlp_mint_amount, srlp_mint, srlp_vault)
    } else {
        (8, receipt.srlp_burn_amount, srlp_vault, srlp_mint)
    };
    if amount > 0 {
        event_instruction.program_id = ctx.accounts.token_2022_program.key();
        event_instruction.accounts.clear();
        event_instruction.accounts.extend([
            AccountMeta::new(first.key(), false),
            AccountMeta::new(second.key(), false),
            AccountMeta::new_readonly(ctx.accounts.market.key(), true),
        ]);
        event_instruction.data.clear();
        event_instruction.data.push(tag);
        event_instruction.data.extend_from_slice(&amount.to_le_bytes());
        invoke_signed_reusing_instruction(
            event_instruction,
            &[
                first.clone(),
                second.clone(),
                ctx.accounts.market.to_account_info(),
                ctx.accounts.token_2022_program.to_account_info(),
            ],
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
        transfer_checked_with_remaining_accounts_and_instruction(
            event_instruction,
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
        crate::instructions::accounting::emit_interest_paid_with_instruction(
            &ctx.accounts.market,
            borrowed_asset,
            crate::events::DebtSource::Hlp,
            None,
            crate::state::ReferralInterestQuote::with_protocol_revenue(
                receipt.interest_paid,
                interest_vault_credit,
                ctx.accounts
                    .market
                    .quote_interest_revenue_credit(borrowed_asset, true, interest_vault_credit)?
                    .protocol,
                None,
            )?,
            0,
            ctx.accounts.event_authority.to_account_info(),
            event_instruction,
        )?;
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

#[inline(always)]
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
