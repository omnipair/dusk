use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    instruction::{AccountMeta, Instruction},
    program::invoke_signed,
};
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::log::{
        emit_hlp_rebalanced_low_heap, emit_market_health_updated_low_heap, emit_swap_executed_low_heap,
        emit_swap_settled_low_heap,
    },
    generate_market_seeds,
    shared::token::{get_transfer_fee, transfer_from_user_to_vault},
    state::{
        AmmSwapQuote, FutarchyAuthority, HlpRebalanceReceipt, Market, MarketAsset, ProtocolAuctionSplit,
        SwapFeeBreakdown, SwapReceipt,
    },
};

use crate::instructions::common::{
    require_supported_asset_mint, token_account_info_amount, token_account_info_credit, token_program_for_mint,
    validate_swap_accounts,
};
use crate::instructions::liquidity::record_hlp_interest_credit;

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
    pub fee_in_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut)]
    pub trader_asset_in_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut)]
    pub trader_asset_out_account: Box<InterfaceAccount<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> Swap<'info> {
    pub fn validate(&self, args: &SwapArgs) -> Result<()> {
        self.market.assert_live_with_futarchy(&self.futarchy_authority)?;
        require!(args.exact_asset_in > 0, ErrorCode::AmountZero);
        require_gte!(
            self.trader_asset_in_account.amount,
            args.exact_asset_in,
            ErrorCode::InsufficientBalance
        );
        validate_swap_accounts(
            &self.market,
            self.trader.key(),
            &self.asset_in_mint,
            &self.asset_out_mint,
            &self.reserve_in_vault,
            &self.reserve_out_vault,
            &self.fee_in_vault,
            &self.trader_asset_in_account,
            &self.trader_asset_out_account,
        )?;
        require_supported_asset_mint(&self.asset_in_mint)?;
        require_supported_asset_mint(&self.asset_out_mint)?;
        Ok(())
    }

    pub fn update(&mut self) -> Result<()> {
        self.market.accrue_interest()?;
        let current_slot = Clock::get()?.slot;
        if self.market.base_side.reserves.live_reserve > 0 && self.market.quote_side.reserves.live_reserve > 0 {
            // Advance clock-driven fee/EMA signals and the one-quote retention
            // probe before freezing this swap. Heavy ramp and center
            // maintenance remains exclusive to the permissionless crank.
            self.market.prepare_amm_for_swap(current_slot)?;
        }
        // Spot validation and hLP execution do not consume lending-risk
        // snapshots. Refresh once from the final post-swap curve in the
        // handler instead of reconstructing all pessimistic shapes twice.
        Ok(())
    }

    pub fn update_and_validate(&mut self, args: &SwapArgs) -> Result<()> {
        self.update()?;
        self.validate(args)
    }

    pub fn handle_swap(mut ctx: Context<'_, '_, '_, 'info, Self>, args: SwapArgs) -> Result<()> {
        let keys = SwapKeys::new(ctx.accounts);
        let asset_in = ctx.accounts.market.asset_for_mint(keys.asset_in_mint)?;
        let fee_config = SwapFeeConfig::new(ctx.accounts);
        let mut token_scratch = TokenInstructionScratch::new(ctx.accounts.token_2022_program.key());

        let reserve_credit = input_credit(&ctx, args.exact_asset_in)?;
        let current_slot = Clock::get()?.slot;
        let preliminary_inputs = ctx
            .accounts
            .market
            .preliminary_swap_inputs(reserve_credit, current_slot)?;
        let pre_quote_rebalance = maybe_rebalance_hlp_before_quote(
            &mut ctx.accounts.market,
            asset_in,
            preliminary_inputs.amount_in_for_quote,
            preliminary_inputs.reserve_input_credit,
        )?;
        if pre_quote_rebalance.receipts.mutates_curve_inventory() {
            ctx.accounts.market.checkpoint_amm_neutral_inventory(current_slot)?;
        } else {
            ctx.accounts.market.ensure_amm_initialized(current_slot)?;
        }
        let quote = ctx
            .accounts
            .market
            .quote_amm_swap(asset_in, reserve_credit, current_slot)?;
        let fee_credit = claimable_swap_fee_credit(&ctx, &quote.fee)?;

        let swap_receipt = record_swap(
            &mut ctx.accounts.market,
            &quote,
            fee_credit,
            fee_config,
            pre_quote_rebalance.fee_eligible_ylp_supply,
            current_slot,
        )?;
        let defer_concentrated_hlp = ctx.accounts.market.has_active_hlp()
            && !ctx.accounts.market.current_curve_parameters(current_slot).is_cpmm();
        // Finalize trade/EMA/retention observations. Heavy ramp and recenter
        // admission stays in the permissionless maintenance instruction.
        let finalized_curve_certificate = ctx.accounts.market.finalize_amm_trade_after_inventory_checkpoint(
            quote.start_price_nad,
            quote.end_price_nad,
            current_slot,
        )?;
        let final_curve_certificate = finalized_curve_certificate.unwrap_or(quote.reserve_endpoint_certificate()?);
        let rebalance = if defer_concentrated_hlp {
            // Active Dusk Concentrated AMM hLP correction is intentionally asynchronous: the
            // exact trade endpoint is observed first, then this transaction
            // records bounded pending exposure for a permissionless crank.
            // `record_swap` has just identity-validated and applied the quote
            // endpoint; a maintenance endpoint, when present, is produced by
            // the same private certificate path. No curve inventory mutation
            // occurs between that application and this observation.
            let final_evaluation = final_curve_certificate.certified_evaluation();
            let final_price_nad =
                u64::try_from(final_evaluation.marginal_price_nad).map_err(|_| ErrorCode::MarketMathOverflow)?;
            // Reuse that exact evaluation for risk instead of walking the
            // same curve identity a second time.
            ctx.accounts
                .market
                .observe_risk_from_curve_evaluation(final_evaluation, current_slot)?;
            let (base, quote_hlp) = ctx
                .accounts
                .market
                .defer_hlp_vaults_after_concentrated_swap(quote.start_price_nad, final_price_nad)?;
            HlpRebalancePair::new(base, quote_hlp)
        } else {
            maybe_rebalance_hlp_after_swap(&mut ctx.accounts.market, pre_quote_rebalance.receipts)?
        };
        let h_lp_tokens_will_change = rebalance.executes_token_changes();
        let h_lp_mutates_curve_inventory = rebalance.mutates_curve_inventory();
        require!(
            !h_lp_tokens_will_change || h_lp_mutates_curve_inventory,
            ErrorCode::BrokenInvariant
        );
        if defer_concentrated_hlp {
            require!(!h_lp_mutates_curve_inventory, ErrorCode::BrokenInvariant);
        } else if h_lp_mutates_curve_inventory {
            // The hLP correction is internal, price-neutral inventory. Preserve
            // the protected budget and refresh its next-step target without
            // counting the correction as flow or attempting a second recenter.
            ctx.accounts
                .market
                .checkpoint_amm_neutral_inventory_and_observe_risk(current_slot)?;
        } else {
            // Reuse the raw-executable quote endpoint. Exact
            // reserve/center/parameter matching still fails closed.
            let reused = ctx
                .accounts
                .market
                .try_observe_risk_from_curve_certificate(final_curve_certificate, current_slot)?;
            if !reused {
                ctx.accounts.market.observe_current_risk(current_slot)?;
            }
        }
        validate_hlp_rebalance_accounts(&ctx.accounts.market, &rebalance, ctx.remaining_accounts)?;
        let received_credit = receive_input(&mut ctx, args.exact_asset_in)?;
        require_eq!(received_credit, reserve_credit, ErrorCode::BrokenInvariant);
        let h_lp_tokens_changed = apply_token_changes(&mut ctx, &rebalance, &mut token_scratch)?;
        require_eq!(h_lp_tokens_changed, h_lp_tokens_will_change, ErrorCode::BrokenInvariant);
        move_swap_fee(&mut ctx, quote.fee.claimable_fee_debit, &mut token_scratch)?;
        settle_swap(&mut ctx, quote.amount_out, args.min_asset_out, &mut token_scratch)?;
        emit_swap_events(
            &ctx,
            keys,
            asset_in,
            &quote,
            &swap_receipt,
            &rebalance,
            h_lp_tokens_changed,
            current_slot,
        )?;

        Ok(())
    }
}

#[derive(Clone, Copy)]
struct SwapKeys {
    market: Pubkey,
    trader: Pubkey,
    asset_in_mint: Pubkey,
    asset_out_mint: Pubkey,
}

impl SwapKeys {
    fn new(accounts: &Swap<'_>) -> Self {
        Self {
            market: accounts.market.key(),
            trader: accounts.trader.key(),
            asset_in_mint: accounts.asset_in_mint.key(),
            asset_out_mint: accounts.asset_out_mint.key(),
        }
    }
}

#[derive(Clone, Copy)]
struct SwapFeeConfig {
    manager_fee_bps: u16,
    protocol_fee_bps: u16,
    protocol_auction_split: ProtocolAuctionSplit,
}

impl SwapFeeConfig {
    fn new(accounts: &Swap<'_>) -> Self {
        Self {
            manager_fee_bps: accounts.market.config.manager_fee_bps,
            protocol_fee_bps: accounts.futarchy_authority.revenue_share.swap_bps,
            protocol_auction_split: accounts.futarchy_authority.protocol_auction_split,
        }
    }
}

#[derive(Clone, Copy)]
struct ClaimableSwapFeeCredit {
    base: u64,
    distributed_surcharge: u64,
}

struct PreQuoteHlpRebalance {
    receipts: HlpRebalancePair,
    fee_eligible_ylp_supply: u64,
}

struct HlpRebalancePair {
    base: HlpRebalanceReceipt,
    quote: HlpRebalanceReceipt,
}

impl HlpRebalancePair {
    fn new(base: HlpRebalanceReceipt, quote: HlpRebalanceReceipt) -> Self {
        Self { base, quote }
    }

    fn executes_token_changes(&self) -> bool {
        rebalance_executes_token_changes(&self.base) || rebalance_executes_token_changes(&self.quote)
    }

    fn mutates_curve_inventory(&self) -> bool {
        [self.base, self.quote].iter().any(|receipt| {
            receipt.executed_delta != 0
                || receipt.ylp_mint_amount != 0
                || receipt.ylp_burn_amount != 0
                || receipt.debt_delta != 0
                || receipt.interest_paid != 0
        })
    }
}

fn receive_input<'info>(ctx: &mut Context<'_, '_, '_, 'info, Swap<'info>>, exact_asset_in: u64) -> Result<u64> {
    receive_swap_inventory(ctx, exact_asset_in)
}

fn input_credit<'info>(ctx: &Context<'_, '_, '_, 'info, Swap<'info>>, exact_asset_in: u64) -> Result<u64> {
    let transfer_fee = get_transfer_fee(&ctx.accounts.asset_in_mint.to_account_info(), exact_asset_in)?;
    exact_asset_in
        .checked_sub(transfer_fee)
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

fn claimable_swap_fee_credit<'info>(
    ctx: &Context<'_, '_, '_, 'info, Swap<'info>>,
    fee: &SwapFeeBreakdown,
) -> Result<ClaimableSwapFeeCredit> {
    require_eq!(
        fee.base_fee_debit
            .checked_add(fee.distributed_surcharge_debit)
            .ok_or(ErrorCode::FeeMathOverflow)?,
        fee.claimable_fee_debit,
        ErrorCode::BrokenInvariant
    );
    if fee.claimable_fee_debit == 0 {
        return Ok(ClaimableSwapFeeCredit {
            base: 0,
            distributed_surcharge: 0,
        });
    }

    let transfer_fee = get_transfer_fee(&ctx.accounts.asset_in_mint.to_account_info(), fee.claimable_fee_debit)?;
    let total_credit = fee
        .claimable_fee_debit
        .checked_sub(transfer_fee)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    split_claimable_swap_fee_credit(fee, total_credit)
}

fn split_claimable_swap_fee_credit(fee: &SwapFeeBreakdown, total_credit: u64) -> Result<ClaimableSwapFeeCredit> {
    require_gte!(fee.claimable_fee_debit, total_credit, ErrorCode::BrokenInvariant);
    if fee.claimable_fee_debit == 0 {
        return Ok(ClaimableSwapFeeCredit {
            base: 0,
            distributed_surcharge: 0,
        });
    }
    let base = u64::try_from(
        (total_credit as u128)
            .checked_mul(fee.base_fee_debit as u128)
            .and_then(|value| value.checked_div(fee.claimable_fee_debit as u128))
            .ok_or(ErrorCode::FeeMathOverflow)?,
    )
    .map_err(|_| ErrorCode::FeeMathOverflow)?;
    let distributed_surcharge = total_credit.checked_sub(base).ok_or(ErrorCode::FeeMathOverflow)?;

    Ok(ClaimableSwapFeeCredit {
        base,
        distributed_surcharge,
    })
}

fn maybe_rebalance_hlp_before_quote(
    market: &mut Market,
    asset_in: MarketAsset,
    amount_in_for_quote: u64,
    reserve_input_credit: u64,
) -> Result<PreQuoteHlpRebalance> {
    let (base, quote) =
        market.pre_solve_hlp_vaults_for_swap_with_reserve_input(asset_in, amount_in_for_quote, reserve_input_credit)?;
    let pre_solve_ylp_mint_amount = base
        .ylp_mint_amount
        .checked_add(quote.ylp_mint_amount)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let fee_eligible_ylp_supply = market
        .side(asset_in)
        .shares
        .ylp_supply
        .checked_sub(pre_solve_ylp_mint_amount)
        .ok_or(ErrorCode::MarketMathOverflow)?;

    Ok(PreQuoteHlpRebalance {
        receipts: HlpRebalancePair::new(base, quote),
        fee_eligible_ylp_supply,
    })
}

fn settle_swap<'info>(
    ctx: &mut Context<'_, '_, '_, 'info, Swap<'info>>,
    amount_out: u64,
    min_asset_out: u64,
    scratch: &mut TokenInstructionScratch,
) -> Result<()> {
    let asset_out_token_program = token_program_for_mint(
        &ctx.accounts.asset_out_mint,
        &ctx.accounts.token_program,
        &ctx.accounts.token_2022_program,
    )?;
    token_transfer_checked_with_scratch(
        scratch,
        ctx.accounts.market.to_account_info(),
        ctx.accounts.reserve_out_vault.to_account_info(),
        ctx.accounts.trader_asset_out_account.to_account_info(),
        ctx.accounts.asset_out_mint.to_account_info(),
        asset_out_token_program,
        amount_out,
        ctx.accounts.asset_out_mint.decimals,
        &[&generate_market_seeds!(ctx.accounts.market)[..]],
    )?;
    let transfer_fee = get_transfer_fee(&ctx.accounts.asset_out_mint.to_account_info(), amount_out)?;
    let asset_out_credit = amount_out
        .checked_sub(transfer_fee)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    require_gte!(asset_out_credit, min_asset_out, ErrorCode::SlippageExceeded);
    Ok(())
}

fn record_swap(
    market: &mut Market,
    quote: &AmmSwapQuote,
    fee_credit: ClaimableSwapFeeCredit,
    fee_config: SwapFeeConfig,
    fee_eligible_ylp_supply: u64,
    current_slot: u64,
) -> Result<SwapReceipt> {
    market.swap_reserves_with_dynamic_fee_supply(
        quote.asset_in,
        quote.fee.amount_in_for_quote,
        quote.fee.reserve_input_credit,
        quote.amount_out,
        fee_credit.base,
        fee_credit.distributed_surcharge,
        quote.fee,
        fee_config.manager_fee_bps,
        fee_config.protocol_fee_bps,
        fee_config.protocol_auction_split,
        fee_eligible_ylp_supply,
        current_slot,
        quote.trade_endpoint_certificate()?,
        quote.reserve_endpoint_certificate()?,
    )
}

fn maybe_rebalance_hlp_after_swap(market: &mut Market, pre_rebalance: HlpRebalancePair) -> Result<HlpRebalancePair> {
    let (base, quote) = market.finalize_hlp_vaults_for_swap(pre_rebalance.base, pre_rebalance.quote)?;
    Ok(HlpRebalancePair::new(base, quote))
}

fn apply_token_changes<'info>(
    ctx: &mut Context<'_, '_, '_, 'info, Swap<'info>>,
    rebalance: &HlpRebalancePair,
    scratch: &mut TokenInstructionScratch,
) -> Result<bool> {
    let h_lp_tokens_changed = rebalance.executes_token_changes();
    if h_lp_tokens_changed {
        apply_hlp_rebalance_token_changes(ctx, &rebalance.base, &rebalance.quote, scratch)?;
    }
    Ok(h_lp_tokens_changed)
}

fn emit_swap_events<'info>(
    ctx: &Context<'_, '_, '_, 'info, Swap<'info>>,
    keys: SwapKeys,
    asset_in: MarketAsset,
    quote: &AmmSwapQuote,
    swap_receipt: &SwapReceipt,
    rebalance: &HlpRebalancePair,
    h_lp_tokens_changed: bool,
    current_slot: u64,
) -> Result<()> {
    if h_lp_tokens_changed {
        emit_swap_settled_low_heap(
            keys.market,
            keys.trader,
            asset_in.code(),
            quote,
            swap_receipt,
            ctx.accounts.market.base_hlp_vault.pending_rebalance,
            ctx.accounts.market.quote_hlp_vault.pending_rebalance,
        );
        return Ok(());
    }

    emit_swap_executed_low_heap(
        keys.market,
        keys.trader,
        keys.asset_in_mint,
        keys.asset_out_mint,
        quote,
        swap_receipt,
        ctx.accounts.market.base_hlp_vault.pending_rebalance,
        ctx.accounts.market.quote_hlp_vault.pending_rebalance,
        current_slot,
    );
    emit_hlp_rebalance_events(ctx, keys, rebalance, current_slot);
    emit_market_health_event(ctx, keys, current_slot)
}

fn emit_hlp_rebalance_events<'info>(
    ctx: &Context<'_, '_, '_, 'info, Swap<'info>>,
    keys: SwapKeys,
    rebalance: &HlpRebalancePair,
    current_slot: u64,
) {
    if should_emit_hlp_rebalance(
        rebalance.base.ideal_delta,
        ctx.accounts.market.base_hlp_vault.pending_rebalance,
        ctx.accounts.market.base_hlp_vault.hlp_supply,
    ) {
        emit_hlp_rebalanced_low_heap(
            keys.market,
            keys.trader,
            MarketAsset::Base.code(),
            rebalance.base.ideal_delta,
            rebalance.base.executed_delta,
            ctx.accounts.market.base_hlp_vault.pending_rebalance,
            ctx.accounts.market.base_hlp_vault.last_nav_nad,
            current_slot,
        );
    }
    if should_emit_hlp_rebalance(
        rebalance.quote.ideal_delta,
        ctx.accounts.market.quote_hlp_vault.pending_rebalance,
        ctx.accounts.market.quote_hlp_vault.hlp_supply,
    ) {
        emit_hlp_rebalanced_low_heap(
            keys.market,
            keys.trader,
            MarketAsset::Quote.code(),
            rebalance.quote.ideal_delta,
            rebalance.quote.executed_delta,
            ctx.accounts.market.quote_hlp_vault.pending_rebalance,
            ctx.accounts.market.quote_hlp_vault.last_nav_nad,
            current_slot,
        );
    }
}

fn emit_market_health_event<'info>(
    ctx: &Context<'_, '_, '_, 'info, Swap<'info>>,
    keys: SwapKeys,
    current_slot: u64,
) -> Result<()> {
    let health = ctx.accounts.market.market_health()?;
    emit_market_health_updated_low_heap(
        keys.market,
        keys.trader,
        health.global_health_base_contribution_for_quote_debt,
        health.global_health_quote_contribution_for_base_debt,
        health.effective_base_debt_nad,
        health.effective_quote_debt_nad,
        health.base_debt_health_bps,
        health.quote_debt_health_bps,
        current_slot,
    );
    Ok(())
}

fn should_emit_hlp_rebalance(ideal_delta: i128, pending_rebalance: i128, hlp_supply: u64) -> bool {
    hlp_supply > 0 || ideal_delta != 0 || pending_rebalance != 0
}

fn rebalance_executes_token_changes(receipt: &HlpRebalanceReceipt) -> bool {
    receipt.ylp_mint_amount > 0 || receipt.ylp_burn_amount > 0 || receipt.interest_paid > 0
}

fn validate_hlp_rebalance_accounts(
    market: &Market,
    rebalance: &HlpRebalancePair,
    remaining_accounts: &[AccountInfo],
) -> Result<()> {
    let mut cursor = 0usize;
    if rebalance_executes_token_changes(&rebalance.base) {
        require_gte!(remaining_accounts.len(), cursor + 3, ErrorCode::NotEnoughAccounts);
        require_hlp_rebalance_accounts(market, rebalance.base.target_asset, remaining_accounts, cursor)?;
        cursor += 3;
    }
    if rebalance_executes_token_changes(&rebalance.quote) {
        require_gte!(remaining_accounts.len(), cursor + 3, ErrorCode::NotEnoughAccounts);
        require_hlp_rebalance_accounts(market, rebalance.quote.target_asset, remaining_accounts, cursor)?;
    }
    Ok(())
}

fn require_hlp_rebalance_accounts(
    market: &Market,
    target_asset: MarketAsset,
    remaining_accounts: &[AccountInfo],
    cursor: usize,
) -> Result<()> {
    let expected_ylp_vault = match target_asset {
        MarketAsset::Base => market.base_hlp_vault.ylp_vault,
        MarketAsset::Quote => market.quote_hlp_vault.ylp_vault,
    };
    let expected_interest_vault = market.side(target_asset.opposite()).interest_vault;
    require_hlp_mint_account(&remaining_accounts[cursor], market.ylp_mint)?;
    require_hlp_vault_account(&remaining_accounts[cursor + 1], expected_ylp_vault)?;
    require_hlp_interest_vault_account(&remaining_accounts[cursor + 2], expected_interest_vault)?;
    Ok(())
}

fn require_hlp_mint_account(account: &AccountInfo, expected_key: Pubkey) -> Result<()> {
    require_keys_eq!(account.key(), expected_key, ErrorCode::InvalidMint);
    require!(account.is_writable, ErrorCode::InvalidMint);
    Ok(())
}

fn require_hlp_vault_account(account: &AccountInfo, expected_key: Pubkey) -> Result<()> {
    require_keys_eq!(account.key(), expected_key, ErrorCode::InvalidVault);
    require!(account.is_writable, ErrorCode::InvalidVault);
    Ok(())
}

fn require_hlp_interest_vault_account(account: &AccountInfo, expected_key: Pubkey) -> Result<()> {
    require_keys_eq!(account.key(), expected_key, ErrorCode::InvalidVault);
    require!(account.is_writable, ErrorCode::InvalidVault);
    Ok(())
}

fn apply_hlp_rebalance_token_changes<'info>(
    ctx: &mut anchor_lang::context::Context<'_, '_, '_, 'info, Swap<'info>>,
    base_receipt: &HlpRebalanceReceipt,
    quote_receipt: &HlpRebalanceReceipt,
    scratch: &mut TokenInstructionScratch,
) -> Result<()> {
    let mut cursor = 0usize;
    if rebalance_executes_token_changes(base_receipt) {
        apply_single_hlp_rebalance_token_changes(ctx, base_receipt, cursor, scratch)?;
        cursor += 3;
    }
    if rebalance_executes_token_changes(quote_receipt) {
        apply_single_hlp_rebalance_token_changes(ctx, quote_receipt, cursor, scratch)?;
    }
    Ok(())
}

struct TokenInstructionScratch {
    instruction: Instruction,
}

impl TokenInstructionScratch {
    fn new(program_id: Pubkey) -> Self {
        Self {
            instruction: Instruction {
                program_id,
                accounts: Vec::with_capacity(4),
                data: Vec::with_capacity(10),
            },
        }
    }

    fn mint_to(&mut self, mint: Pubkey, destination: Pubkey, authority: Pubkey, amount: u64) {
        self.instruction.accounts.clear();
        self.instruction.accounts.push(AccountMeta::new(mint, false));
        self.instruction.accounts.push(AccountMeta::new(destination, false));
        self.instruction
            .accounts
            .push(AccountMeta::new_readonly(authority, true));

        self.instruction.data.clear();
        self.instruction.data.push(7);
        self.instruction.data.extend_from_slice(&amount.to_le_bytes());
    }

    fn burn(&mut self, source: Pubkey, mint: Pubkey, authority: Pubkey, amount: u64) {
        self.instruction.accounts.clear();
        self.instruction.accounts.push(AccountMeta::new(source, false));
        self.instruction.accounts.push(AccountMeta::new(mint, false));
        self.instruction
            .accounts
            .push(AccountMeta::new_readonly(authority, true));

        self.instruction.data.clear();
        self.instruction.data.push(8);
        self.instruction.data.extend_from_slice(&amount.to_le_bytes());
    }

    fn transfer_checked(
        &mut self,
        source: Pubkey,
        mint: Pubkey,
        destination: Pubkey,
        authority: Pubkey,
        token_program: Pubkey,
        amount: u64,
        decimals: u8,
    ) {
        self.instruction.program_id = token_program;
        self.instruction.accounts.clear();
        self.instruction.accounts.push(AccountMeta::new(source, false));
        self.instruction.accounts.push(AccountMeta::new_readonly(mint, false));
        self.instruction.accounts.push(AccountMeta::new(destination, false));
        self.instruction
            .accounts
            .push(AccountMeta::new_readonly(authority, true));

        self.instruction.data.clear();
        self.instruction.data.push(12);
        self.instruction.data.extend_from_slice(&amount.to_le_bytes());
        self.instruction.data.push(decimals);
    }
}

fn apply_single_hlp_rebalance_token_changes<'info>(
    ctx: &mut anchor_lang::context::Context<'_, '_, '_, 'info, Swap<'info>>,
    receipt: &HlpRebalanceReceipt,
    cursor: usize,
    scratch: &mut TokenInstructionScratch,
) -> Result<()> {
    let ylp_mint = &ctx.remaining_accounts[cursor];
    let ylp_vault = &ctx.remaining_accounts[cursor + 1];
    let market_seeds = generate_market_seeds!(ctx.accounts.market);
    let signer_seeds = [&market_seeds[..]];
    let market = ctx.accounts.market.to_account_info();
    let token_2022_program = ctx.accounts.token_2022_program.to_account_info();

    if receipt.ylp_mint_amount > 0 {
        token_2022_mint_to_with_scratch(
            scratch,
            market.clone(),
            token_2022_program.clone(),
            ylp_mint.clone(),
            ylp_vault.clone(),
            receipt.ylp_mint_amount,
            &signer_seeds,
        )?;
    }
    if receipt.ylp_burn_amount > 0 {
        token_2022_burn_with_scratch(
            scratch,
            market,
            token_2022_program,
            ylp_mint.clone(),
            ylp_vault.clone(),
            receipt.ylp_burn_amount,
            &signer_seeds,
        )?;
    }
    if receipt.interest_paid > 0 {
        move_hlp_rebalance_interest(ctx, receipt, cursor, scratch)?;
    }
    Ok(())
}

fn move_hlp_rebalance_interest<'info>(
    ctx: &mut anchor_lang::context::Context<'_, '_, '_, 'info, Swap<'info>>,
    receipt: &HlpRebalanceReceipt,
    cursor: usize,
    scratch: &mut TokenInstructionScratch,
) -> Result<()> {
    let borrowed_asset = receipt.target_asset.opposite();
    let interest_vault = &ctx.remaining_accounts[cursor + 2];
    let interest_vault_balance_before = token_account_info_amount(interest_vault)?;
    let (borrowed_reserve_vault, borrowed_mint, borrowed_token_program, borrowed_decimals) =
        rebalance_interest_transfer_accounts(ctx, borrowed_asset)?;
    token_transfer_checked_with_scratch(
        scratch,
        ctx.accounts.market.to_account_info(),
        borrowed_reserve_vault,
        interest_vault.clone(),
        borrowed_mint,
        borrowed_token_program,
        receipt.interest_paid,
        borrowed_decimals,
        &[&generate_market_seeds!(ctx.accounts.market)[..]],
    )?;
    let interest_vault_credit = token_account_info_credit(interest_vault_balance_before, interest_vault)?;
    let manager_fee_bps = ctx.accounts.market.config.manager_fee_bps;
    record_hlp_interest_credit(
        ctx.accounts.market.side_mut(borrowed_asset),
        interest_vault_credit,
        manager_fee_bps,
        ctx.accounts.futarchy_authority.revenue_share.interest_bps,
        ctx.accounts.futarchy_authority.protocol_auction_split,
    )?;
    Ok(())
}

fn rebalance_interest_transfer_accounts<'info>(
    ctx: &anchor_lang::context::Context<'_, '_, '_, 'info, Swap<'info>>,
    asset: MarketAsset,
) -> Result<(AccountInfo<'info>, AccountInfo<'info>, AccountInfo<'info>, u8)> {
    if ctx.accounts.market.asset_for_mint(ctx.accounts.asset_in_mint.key())? == asset {
        let token_program = token_program_for_mint(
            &ctx.accounts.asset_in_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        return Ok((
            ctx.accounts.reserve_in_vault.to_account_info(),
            ctx.accounts.asset_in_mint.to_account_info(),
            token_program,
            ctx.accounts.asset_in_mint.decimals,
        ));
    }
    if ctx.accounts.market.asset_for_mint(ctx.accounts.asset_out_mint.key())? == asset {
        let token_program = token_program_for_mint(
            &ctx.accounts.asset_out_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        return Ok((
            ctx.accounts.reserve_out_vault.to_account_info(),
            ctx.accounts.asset_out_mint.to_account_info(),
            token_program,
            ctx.accounts.asset_out_mint.decimals,
        ));
    }
    err!(ErrorCode::InvalidMint)
}

fn token_2022_mint_to_with_scratch<'info>(
    scratch: &mut TokenInstructionScratch,
    authority: AccountInfo<'info>,
    token_program: AccountInfo<'info>,
    mint: AccountInfo<'info>,
    destination: AccountInfo<'info>,
    amount: u64,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    scratch.instruction.program_id = *token_program.key;
    scratch.mint_to(*mint.key, *destination.key, *authority.key, amount);
    invoke_signed(
        &scratch.instruction,
        &[mint, destination, authority, token_program],
        signer_seeds,
    )
    .map_err(Into::into)
}

fn token_2022_burn_with_scratch<'info>(
    scratch: &mut TokenInstructionScratch,
    authority: AccountInfo<'info>,
    token_program: AccountInfo<'info>,
    mint: AccountInfo<'info>,
    source: AccountInfo<'info>,
    amount: u64,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    scratch.instruction.program_id = *token_program.key;
    scratch.burn(*source.key, *mint.key, *authority.key, amount);
    invoke_signed(
        &scratch.instruction,
        &[source, mint, authority, token_program],
        signer_seeds,
    )
    .map_err(Into::into)
}

fn token_transfer_checked_with_scratch<'info>(
    scratch: &mut TokenInstructionScratch,
    authority: AccountInfo<'info>,
    from_vault: AccountInfo<'info>,
    to_vault: AccountInfo<'info>,
    mint: AccountInfo<'info>,
    token_program: AccountInfo<'info>,
    amount: u64,
    mint_decimals: u8,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    if amount == 0 {
        return Ok(());
    }
    require!(
        *token_program.key == Token2022::id() || *token_program.key == Token::id(),
        ErrorCode::InvalidTokenProgram
    );
    scratch.transfer_checked(
        *from_vault.key,
        *mint.key,
        *to_vault.key,
        *authority.key,
        *token_program.key,
        amount,
        mint_decimals,
    );
    invoke_signed(
        &scratch.instruction,
        &[from_vault, mint, to_vault, authority, token_program],
        signer_seeds,
    )
    .map_err(Into::into)
}

fn receive_swap_inventory<'info>(
    ctx: &mut Context<'_, '_, '_, 'info, Swap<'info>>,
    exact_asset_in: u64,
) -> Result<u64> {
    let asset_in_token_program = token_program_for_mint(
        &ctx.accounts.asset_in_mint,
        &ctx.accounts.token_program,
        &ctx.accounts.token_2022_program,
    )?;
    transfer_from_user_to_vault(
        ctx.accounts.trader.to_account_info(),
        ctx.accounts.trader_asset_in_account.to_account_info(),
        ctx.accounts.reserve_in_vault.to_account_info(),
        ctx.accounts.asset_in_mint.to_account_info(),
        asset_in_token_program,
        exact_asset_in,
        ctx.accounts.asset_in_mint.decimals,
    )?;
    input_credit(ctx, exact_asset_in)
}

fn move_swap_fee<'info>(
    ctx: &mut Context<'_, '_, '_, 'info, Swap<'info>>,
    claimable_fee_debit: u64,
    scratch: &mut TokenInstructionScratch,
) -> Result<()> {
    if claimable_fee_debit == 0 {
        return Ok(());
    }
    let asset_in_token_program = token_program_for_mint(
        &ctx.accounts.asset_in_mint,
        &ctx.accounts.token_program,
        &ctx.accounts.token_2022_program,
    )?;
    token_transfer_checked_with_scratch(
        scratch,
        ctx.accounts.market.to_account_info(),
        ctx.accounts.reserve_in_vault.to_account_info(),
        ctx.accounts.fee_in_vault.to_account_info(),
        ctx.accounts.asset_in_mint.to_account_info(),
        asset_in_token_program,
        claimable_fee_debit,
        ctx.accounts.asset_in_mint.decimals,
        &[&generate_market_seeds!(ctx.accounts.market)[..]],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::solana_program::program_option::COption;
    use spl_token_2022::{
        extension::{
            transfer_fee::{TransferFee, TransferFeeAmount},
            BaseStateWithExtensionsMut, ExtensionType, StateWithExtensionsMut,
        },
        state::{Account as SplToken2022Account, AccountState},
    };

    #[test]
    fn token_2022_fee_credit_is_split_proportionally_without_losing_dust() {
        let fee = SwapFeeBreakdown {
            base_fee_debit: 60,
            distributed_surcharge_debit: 40,
            claimable_fee_debit: 100,
            ..SwapFeeBreakdown::default()
        };

        let credit = split_claimable_swap_fee_credit(&fee, 97).unwrap();

        assert_eq!(credit.base, 58);
        assert_eq!(credit.distributed_surcharge, 39);
        assert_eq!(credit.base + credit.distributed_surcharge, 97);
    }

    #[test]
    fn retained_surcharge_never_enters_claimable_fee_credit() {
        let fee = SwapFeeBreakdown {
            base_fee_debit: 30,
            dynamic_surcharge_debit: 70,
            retained_surcharge: 70,
            distributed_surcharge_debit: 0,
            claimable_fee_debit: 30,
            ..SwapFeeBreakdown::default()
        };

        let credit = split_claimable_swap_fee_credit(&fee, 29).unwrap();

        assert_eq!(credit.base, 29);
        assert_eq!(credit.distributed_surcharge, 0);
    }

    #[test]
    fn token_2022_hlp_interest_reads_net_credit_from_remaining_vault_data() {
        let transfer_fee = TransferFee {
            epoch: 0_u64.into(),
            maximum_fee: u64::MAX.into(),
            transfer_fee_basis_points: 300_u16.into(),
        };
        let gross_interest_paid = 10_000;
        let balance_before = 40_000;
        let actual_credit = transfer_fee.calculate_post_fee_amount(gross_interest_paid).unwrap();
        let balance_after = balance_before + actual_credit;
        let account_len =
            ExtensionType::try_calculate_account_len::<SplToken2022Account>(&[ExtensionType::TransferFeeAmount])
                .unwrap();
        let mut account_data = vec![0_u8; account_len];
        {
            let mut account =
                StateWithExtensionsMut::<SplToken2022Account>::unpack_uninitialized(&mut account_data).unwrap();
            account.init_extension::<TransferFeeAmount>(true).unwrap();
            account.base = SplToken2022Account {
                mint: Pubkey::new_unique(),
                owner: Pubkey::new_unique(),
                amount: balance_after,
                delegate: COption::None,
                state: AccountState::Initialized,
                is_native: COption::None,
                delegated_amount: 0,
                close_authority: COption::None,
            };
            account.pack_base();
            account.init_account_type().unwrap();
        }
        let account_key = Pubkey::new_unique();
        let token_program = spl_token_2022::ID;
        let mut lamports = 1;
        let account_info = AccountInfo::new(
            &account_key,
            false,
            true,
            &mut lamports,
            &mut account_data,
            &token_program,
            false,
            0,
        );

        assert_eq!(
            token_account_info_credit(balance_before, &account_info).unwrap(),
            actual_credit
        );
        assert_eq!(actual_credit, 9_700);
        assert!(actual_credit < gross_interest_paid);
    }
}
