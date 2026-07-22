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
    math::calculate_raw_amount_out,
    shared::{
        math::ceil_div,
        token::{get_transfer_fee, transfer_from_user_to_vault},
    },
    state::{FutarchyAuthority, HlpRebalanceReceipt, Market, MarketAsset, ProtocolAuctionSplit, SwapReceipt},
};

use crate::instructions::common::{require_supported_asset_mint, token_program_for_mint, validate_swap_accounts};

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
        self.market.refresh_risk()
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
        let charged_input = charge_fee(&mut ctx, reserve_credit)?;
        let current_slot = Clock::get()?.slot;
        let pre_quote_rebalance =
            maybe_rebalance_hlp_before_quote(&mut ctx.accounts.market, asset_in, charged_input.amount_in_after_fee)?;
        let amount_out = quote(&ctx.accounts.market, asset_in, charged_input.amount_in_after_fee)?;

        let swap_receipt = record_swap(
            &mut ctx.accounts.market,
            asset_in,
            &charged_input,
            amount_out,
            fee_config,
            pre_quote_rebalance.fee_eligible_ylp_supply,
        )?;
        let rebalance = maybe_rebalance_hlp_after_swap(&mut ctx.accounts.market, pre_quote_rebalance.receipts)?;
        validate_hlp_rebalance_accounts(&ctx.accounts.market, &rebalance, ctx.remaining_accounts)?;
        let received_credit = receive_input(&mut ctx, args.exact_asset_in)?;
        require_eq!(received_credit, reserve_credit, ErrorCode::BrokenInvariant);
        let h_lp_tokens_changed = apply_token_changes(&mut ctx, &rebalance, &mut token_scratch)?;
        move_swap_fee(&mut ctx, charged_input.total_fee, &mut token_scratch)?;
        settle_swap(&mut ctx, amount_out, args.min_asset_out, &mut token_scratch)?;
        emit_swap_events(
            &ctx,
            keys,
            asset_in,
            charged_input.reserve_credit,
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

struct ChargedSwapInput {
    reserve_credit: u64,
    total_fee: u64,
    fee_credit: u64,
    amount_in_after_fee: u64,
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

fn charge_fee<'info>(
    ctx: &mut Context<'_, '_, '_, 'info, Swap<'info>>,
    reserve_credit: u64,
) -> Result<ChargedSwapInput> {
    let total_fee = ceil_div(
        (reserve_credit as u128)
            .checked_mul(ctx.accounts.market.config.swap_fee_bps as u128)
            .ok_or(ErrorCode::FeeMathOverflow)?,
        BPS_DENOMINATOR as u128,
    )
    .ok_or(ErrorCode::FeeMathOverflow)?
    .min(reserve_credit as u128) as u64;

    let transfer_fee = get_transfer_fee(&ctx.accounts.asset_in_mint.to_account_info(), total_fee)?;
    let fee_credit = total_fee
        .checked_sub(transfer_fee)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let amount_in_after_fee = reserve_credit
        .checked_sub(total_fee)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    require!(amount_in_after_fee > 0, ErrorCode::InsufficientOutputAmount);

    Ok(ChargedSwapInput {
        reserve_credit,
        total_fee,
        fee_credit,
        amount_in_after_fee,
    })
}

fn maybe_rebalance_hlp_before_quote(
    market: &mut Market,
    asset_in: MarketAsset,
    amount_in_after_fee: u64,
) -> Result<PreQuoteHlpRebalance> {
    let (base, quote) = market.pre_solve_hlp_vaults_for_swap(asset_in, amount_in_after_fee)?;
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

fn quote(market: &Market, asset_in: MarketAsset, amount_in_after_fee: u64) -> Result<u64> {
    let (market_side_in, market_side_out) = market.swap_sides(asset_in);
    calculate_raw_amount_out(
        market_side_in.reserves.live_reserve,
        market_side_out.reserves.live_reserve,
        amount_in_after_fee,
    )
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
    asset_in: MarketAsset,
    input: &ChargedSwapInput,
    amount_out: u64,
    fee_config: SwapFeeConfig,
    fee_eligible_ylp_supply: u64,
) -> Result<SwapReceipt> {
    market.swap_reserves_with_fee_supply(
        asset_in,
        input.amount_in_after_fee,
        amount_out,
        input.fee_credit,
        fee_config.manager_fee_bps,
        fee_config.protocol_fee_bps,
        fee_config.protocol_auction_split,
        Some(fee_eligible_ylp_supply),
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
    } else {
        ctx.accounts.market.refresh_risk()?;
    }
    Ok(h_lp_tokens_changed)
}

fn emit_swap_events<'info>(
    ctx: &Context<'_, '_, '_, 'info, Swap<'info>>,
    keys: SwapKeys,
    asset_in: MarketAsset,
    reserve_credit: u64,
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
            reserve_credit,
            swap_receipt.amount_in_after_fee,
            swap_receipt.amount_out,
            swap_receipt.fee_credit,
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
        reserve_credit,
        swap_receipt.amount_in_after_fee,
        swap_receipt.amount_out,
        swap_receipt.fee_credit,
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
    let (borrowed_reserve_vault, borrowed_mint, borrowed_token_program, borrowed_decimals) =
        rebalance_interest_transfer_accounts(ctx, borrowed_asset)?;
    token_transfer_checked_with_scratch(
        scratch,
        ctx.accounts.market.to_account_info(),
        borrowed_reserve_vault,
        ctx.remaining_accounts[cursor + 2].clone(),
        borrowed_mint,
        borrowed_token_program,
        receipt.interest_paid,
        borrowed_decimals,
        &[&generate_market_seeds!(ctx.accounts.market)[..]],
    )?;
    let manager_fee_bps = ctx.accounts.market.config.manager_fee_bps;
    ctx.accounts.market.side_mut(borrowed_asset).record_interest_credit(
        receipt.interest_paid,
        manager_fee_bps,
        ctx.accounts.futarchy_authority.revenue_share.interest_bps,
        ctx.accounts.futarchy_authority.protocol_auction_split,
        0,
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
    total_fee: u64,
    scratch: &mut TokenInstructionScratch,
) -> Result<()> {
    if total_fee == 0 {
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
        total_fee,
        ctx.accounts.asset_in_mint.decimals,
        &[&generate_market_seeds!(ctx.accounts.market)[..]],
    )
}
