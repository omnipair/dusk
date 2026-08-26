use anchor_lang::prelude::*;
use anchor_spl::token_interface::Mint;

use crate::instructions::accounts::require_supported_asset_mint;
use crate::instructions::{split_claimable_fee_credit, SwapRequest};
use crate::{
    constants::*,
    errors::ErrorCode,
    state::{BorrowPosition, FutarchyAuthority, Market, MarketAsset},
    token::{get_transfer_fee, get_transfer_inverse_fee},
    transitions::MarketHealth,
};

#[cfg(target_os = "solana")]
#[inline(always)]
fn debug_log_heap(tag: u64) {
    let cursor = unsafe { *(0x300000000 as *const u64) };
    let used = if cursor == 0 { 0 } else { 0x300008000_u64 - cursor };
    solana_program::log::sol_log_64(tag, cursor, used, 0, 0);
    solana_program::log::sol_log_compute_units();
}

#[cfg(not(target_os = "solana"))]
#[inline(always)]
fn debug_log_heap(_tag: u64) {}

// Most preview instructions update and return serialized market state. Swap
// preview is deliberately pure: all clock/ramp/hLP simulation runs on a clone
// so submitting a preview cannot alter fee routing or create a curve/Risk
// freshness mismatch.

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PreviewAddLiquidityArgs {
    pub base_deposit_amount: u64,
    pub quote_deposit_amount: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PreviewSwapArgs {
    pub exact_asset_in: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PreviewBorrowCapacityArgs {
    pub collateral_amount: u64,
    pub projected_borrow_amount: Option<u64>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PreviewHlpOrderTriggerArgs {
    pub target_asset: u8,
    pub hlp_amount: u64,
}

#[derive(Accounts)]
pub struct PreviewMarket<'info> {
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
}

#[derive(Accounts)]
pub struct PreviewHlpOrderTrigger<'info> {
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
}

#[derive(Accounts)]
pub struct PreviewAddLiquidity<'info> {
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

    pub base_mint: Box<InterfaceAccount<'info, Mint>>,
    pub quote_mint: Box<InterfaceAccount<'info, Mint>>,
}

#[derive(Accounts)]
pub struct PreviewSwap<'info> {
    #[account(
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

    pub asset_in_mint: Box<InterfaceAccount<'info, Mint>>,
    pub asset_out_mint: Box<InterfaceAccount<'info, Mint>>,
}

#[derive(Accounts)]
pub struct PreviewBorrowCapacity<'info> {
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

    pub collateral_asset_mint: Box<InterfaceAccount<'info, Mint>>,
    pub debt_asset_mint: Box<InterfaceAccount<'info, Mint>>,
}

#[derive(Accounts)]
pub struct PreviewBorrowPosition<'info> {
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
        seeds = [
            BORROW_POSITION_SEED_PREFIX,
            market.key().as_ref(),
            borrow_position.position_id.as_ref(),
        ],
        bump = borrow_position.bump,
        constraint = borrow_position.market == market.key() @ ErrorCode::InvalidPositionMarket
    )]
    pub borrow_position: Box<Account<'info, BorrowPosition>>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PreviewSide {
    pub live_reserve: u64,
    pub cash_reserve: u64,
    pub base_hlp_backing_inventory: u64,
    pub quote_hlp_backing_inventory: u64,
    pub ylp_supply: u64,
    pub ylp_exchange_rate_nad: u128,
    pub spot_price_nad: u64,
    pub price_ema_nad: u64,
    pub directional_price_ema_nad: u64,
    pub conservative_depth_nad: u128,
    pub borrow_index_nad: u128,
    pub rate_at_target_nad: u128,
    pub borrow_apr_nad: u128,
    pub utilization_bps: u64,
    pub fixed_debt: u128,
    pub isolated_debt: u128,
    pub hlp_funding_debt: u128,
    pub total_debt: u128,
    pub daily_borrow_limit: u64,
    pub daily_borrow_remaining: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default)]
pub struct MarketPreview {
    pub slot: u64,
    pub base: PreviewSide,
    pub quote: PreviewSide,
    pub liquidity_nad: u128,
    pub health: MarketHealth,
    pub amm: PreviewAmm,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HlpOrderTriggerPreview {
    pub principal_nav_per_token_nad: u64,
    pub funding_apr_ema_nad: u128,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PreviewAmm {
    pub initialized: bool,
    pub executable_base_reserve: u64,
    pub executable_quote_reserve: u64,
    pub center_price_nad: u64,
    pub price_ema_nad: u64,
    pub last_trade_price_nad: u64,
    pub last_observation_slot: u64,
    pub last_adjustment_slot: u64,
    pub volatility_accumulator_nad: u64,
    pub decayed_volatility_nad: u64,
    pub curve_depth_nad: u128,
    pub curve_depth_per_share_nad: u128,
    pub protected_floor_per_share_nad: u128,
    pub protected_profit_per_share_nad: u128,
    pub retention_required_nad: u128,
    pub retention_stop_nad: u128,
    pub retention_hard_cap_nad: u128,
    pub retention_active: bool,
    pub retention_target_saturated: bool,
    pub retention_target_stale: bool,
    pub protected_recenter_base_reserve: u64,
    pub protected_recenter_quote_reserve: u64,
    pub peak_amplification_nad: u64,
    pub core_half_width_bps: u16,
    pub fade_width_bps: u16,
    pub lower_range_price_nad: u64,
    pub upper_range_price_nad: u64,
    pub concentrated_curve_branch: u8,
    pub ordinary_base_reserve_nad: u128,
    pub ordinary_quote_reserve_nad: u128,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct AddLiquidityPreview {
    pub requested_base_amount: u64,
    pub requested_quote_amount: u64,
    pub max_base_reserve_credit: u64,
    pub max_quote_reserve_credit: u64,
    pub base_transfer_amount: u64,
    pub quote_transfer_amount: u64,
    pub base_transfer_fee: u64,
    pub quote_transfer_fee: u64,
    pub base_reserve_credit: u64,
    pub quote_reserve_credit: u64,
    pub unused_base_amount: u64,
    pub unused_quote_amount: u64,
    pub ylp_amount: u64,
    pub ylp_supply: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct SwapPreview {
    pub asset_in: MarketAsset,
    pub asset_out: MarketAsset,
    pub exact_asset_in: u64,
    pub transfer_fee: u64,
    /// Actual credit received by the reserve vault from the user transfer.
    pub reserve_credit: u64,
    pub fee_asset: MarketAsset,
    pub amount_out: u64,
    pub gross_amount_out: u64,
    pub reserve_in_live_reserve: u64,
    pub reserve_out_live_reserve: u64,
    pub base_fee_debit: u64,
    pub divergence_surcharge_debit: u64,
    pub volatility_surcharge_debit: u64,
    pub dynamic_surcharge_debit: u64,
    pub total_fee_debit: u64,
    pub retained_surcharge: u64,
    pub distributed_surcharge_debit: u64,
    pub claimable_fee_debit: u64,
    /// LP-owned fee converted into reserve principal at the configured rate.
    pub compounded_fee_debit: u64,
    pub compounding_fee_bps: u16,
    pub amount_in_for_quote: u64,
    pub reserve_input_credit: u64,
    pub base_fee_credit: u64,
    pub distributed_surcharge_credit: u64,
    pub claimable_fee_credit: u64,
    pub base_fee_rate_nad: u64,
    pub divergence_fee_rate_nad: u64,
    pub volatility_fee_rate_nad: u64,
    pub total_fee_rate_nad: u64,
    pub start_price_nad: u64,
    /// Invariant-preserving executable trade endpoint.
    pub trade_end_price_nad: u64,
    /// Final executable marginal price. A protected retained surcharge is
    /// non-quoteable, so it does not change this value.
    pub reserve_end_price_nad: u64,
    pub center_price_nad: u64,
    pub price_ema_nad: u64,
    pub decayed_volatility_nad: u64,
    pub post_success_volatility_nad: u64,
    pub retention_active: bool,
    pub retention_target_saturated: bool,
    pub protected_recenter_base_reserve: u64,
    pub protected_recenter_quote_reserve: u64,
    pub protected_profit_per_share_nad: u128,
    pub projected_protected_profit_per_share_nad: u128,
    pub retention_required_nad: u128,
    pub retention_stop_nad: u128,
    pub retention_hard_cap_nad: u128,
    /// Concentrated range metadata. Zeroes denote the legacy curve during the
    /// temporary caller migration.
    pub lower_range_price_nad: u64,
    pub upper_range_price_nad: u64,
    /// 0=lower tail, 1=concentrated band, 2=upper tail.
    pub concentrated_curve_branch: u8,
    pub ordinary_base_reserve_nad: u128,
    pub ordinary_quote_reserve_nad: u128,
    pub final_spot_price_nad: u64,
    pub base_hlp_quote_debt_delta: i128,
    pub quote_hlp_base_debt_delta: i128,
    /// Funding recovery funded exclusively by the stressed hLP.
    pub hlp_recovery_target_asset: u8,
    pub hlp_recovery_funding_gap: u64,
    pub hlp_recovery_matched_input: u64,
    pub hlp_recovery_bonus_output: u64,
    pub hlp_recovery_discount_bps: u16,
    pub hlp_recovery_critical: bool,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct BorrowCapacityPreview {
    pub collateral_asset: MarketAsset,
    pub debt_asset: MarketAsset,
    pub collateral_amount: u64,
    pub collateral_value_nad: u128,
    pub max_debt_by_health: u64,
    pub max_debt_by_cash: u64,
    pub max_debt_by_daily_limit: u64,
    pub max_debt: u64,
    pub max_borrow_amount: u64,
    pub borrow_market_health_floor_bps: u16,
    pub global_health_contribution_cap_bps: u16,
    pub projected_borrow_amount: u64,
    pub projected_debt_amount: u64,
    pub projected_health_bps: u64,
    pub projected_global_market_health_bps: u64,
    pub projected_global_health_contribution: u64,
    pub projected_effective_existing_debt_nad: u128,
    pub max_cf_bps: u16,
    pub liquidation_cf_bps: u16,
    pub liquidation_debt_per_collateral_price_nad: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct PositionDebtSidePreview {
    pub debt_asset: MarketAsset,
    pub collateral_asset: MarketAsset,
    pub fixed_debt: u128,
    pub collateral_amount: u64,
    pub global_health_contribution: u64,
    pub collateral_value_nad: u128,
    pub health_bps: u64,
    pub max_cf_bps: u16,
    pub liquidation_cf_bps: u16,
    pub liquidation_reference_price_nad: u64,
    pub liquidation_health_bps: u64,
    pub is_liquidatable: bool,
    pub liquidation_incentive_bps: u16,
    pub insurance_funding_bps: u16,
    pub total_penalty_bps: u16,
    pub max_repay_amount: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct BorrowPositionPreview {
    pub owner: Pubkey,
    pub market: Pubkey,
    pub position_id: Pubkey,
    pub base_collateral: u64,
    pub quote_collateral: u64,
    pub global_health_base_contribution_for_quote_debt: u64,
    pub global_health_quote_contribution_for_base_debt: u64,
    pub base_liquidation_cf_bps: u16,
    pub quote_liquidation_cf_bps: u16,
    pub fixed_base_debt: u128,
    pub fixed_quote_debt: u128,
    pub base_debt: PositionDebtSidePreview,
    pub quote_debt: PositionDebtSidePreview,
}

impl<'info> PreviewMarket<'info> {
    pub fn handle_preview(ctx: Context<Self>) -> Result<MarketPreview> {
        ctx.accounts.market.update()?;
        let market: &Market = &ctx.accounts.market;
        let clock = Clock::get()?;
        let slot = clock.slot;
        let amm = {
            let state = &market.amm;
            let concentrated_parameters = market.config.amm.concentrated_curve_parameters()?;
            let metrics = market.amm_preview_metrics(0)?;

            PreviewAmm {
                initialized: state.initialized,
                executable_base_reserve: metrics.executable_base_reserve,
                executable_quote_reserve: metrics.executable_quote_reserve,
                center_price_nad: state.center_price_nad,
                price_ema_nad: state.price_ema_nad,
                last_trade_price_nad: state.last_trade_price_nad,
                last_observation_slot: state.last_observation_slot,
                last_adjustment_slot: state.last_adjustment_slot,
                volatility_accumulator_nad: state.volatility_accumulator_nad,
                decayed_volatility_nad: if state.initialized {
                    state.decayed_volatility(&market.config.amm, slot)?
                } else {
                    0
                },
                curve_depth_nad: metrics.curve_depth_nad,
                curve_depth_per_share_nad: state.curve_depth_per_share_nad,
                protected_floor_per_share_nad: state.protected_floor_per_share_nad,
                protected_profit_per_share_nad: state.spendable_protected_profit_nad(),
                retention_required_nad: state.retention_required_nad,
                retention_stop_nad: state.retention_stop_nad,
                retention_hard_cap_nad: state.retention_hard_cap_nad,
                retention_active: state.retain_dynamic_surcharge,
                retention_target_saturated: state.retention_target_saturated,
                retention_target_stale: state.retention_target_stale,
                protected_recenter_base_reserve: market.base_side.reserves.protected_recenter_reserve,
                protected_recenter_quote_reserve: market.quote_side.reserves.protected_recenter_reserve,
                peak_amplification_nad: concentrated_parameters.peak_amplification_nad,
                core_half_width_bps: concentrated_parameters.core_half_width_bps,
                fade_width_bps: concentrated_parameters.fade_width_bps,
                lower_range_price_nad: metrics.lower_range_price_nad,
                upper_range_price_nad: metrics.upper_range_price_nad,
                concentrated_curve_branch: metrics.concentrated_curve_branch,
                ordinary_base_reserve_nad: metrics.ordinary_base_reserve_nad,
                ordinary_quote_reserve_nad: metrics.ordinary_quote_reserve_nad,
            }
        };
        Ok(MarketPreview {
            slot,
            base: preview_side(market, MarketAsset::Base, slot)?,
            quote: preview_side(market, MarketAsset::Quote, slot)?,
            liquidity_nad: market.liquidity_nad()?,
            health: market.market_health()?,
            amm,
        })
    }
}

impl<'info> PreviewHlpOrderTrigger<'info> {
    pub fn handle_preview(ctx: Context<Self>, args: PreviewHlpOrderTriggerArgs) -> Result<HlpOrderTriggerPreview> {
        require!(args.hlp_amount > 0, ErrorCode::AmountZero);
        ctx.accounts.market.update()?;
        let target_asset = MarketAsset::try_from_code(args.target_asset)?;
        Ok(HlpOrderTriggerPreview {
            principal_nav_per_token_nad: ctx
                .accounts
                .market
                .hlp_principal_nav_per_token_nad(target_asset, args.hlp_amount)?,
            funding_apr_ema_nad: ctx.accounts.market.hlp_funding_apr_ema_nad(target_asset)?,
        })
    }
}

impl<'info> PreviewAddLiquidity<'info> {
    pub fn handle_preview(ctx: Context<Self>, args: PreviewAddLiquidityArgs) -> Result<AddLiquidityPreview> {
        require!(args.base_deposit_amount > 0, ErrorCode::AmountZero);
        require!(args.quote_deposit_amount > 0, ErrorCode::AmountZero);
        require_supported_asset_mint(&ctx.accounts.base_mint)?;
        require_supported_asset_mint(&ctx.accounts.quote_mint)?;

        ctx.accounts.market.update()?;
        let market: &Market = &ctx.accounts.market;
        require_keys_eq!(
            market.base_side.asset_mint,
            ctx.accounts.base_mint.key(),
            ErrorCode::InvalidMint
        );
        require_keys_eq!(
            market.quote_side.asset_mint,
            ctx.accounts.quote_mint.key(),
            ErrorCode::InvalidMint
        );

        let requested_base_amount = args.base_deposit_amount;
        let requested_quote_amount = args.quote_deposit_amount;
        let max_base_transfer_fee = get_transfer_fee(&ctx.accounts.base_mint.to_account_info(), requested_base_amount)?;
        let max_quote_transfer_fee =
            get_transfer_fee(&ctx.accounts.quote_mint.to_account_info(), requested_quote_amount)?;
        let max_base_reserve_credit = requested_base_amount
            .checked_sub(max_base_transfer_fee)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let max_quote_reserve_credit = requested_quote_amount
            .checked_sub(max_quote_transfer_fee)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let receipt = market.preview_add_liquidity(max_base_reserve_credit, max_quote_reserve_credit)?;
        let base_transfer_fee =
            get_transfer_inverse_fee(&ctx.accounts.base_mint.to_account_info(), receipt.base_reserve_credit)?;
        let quote_transfer_fee =
            get_transfer_inverse_fee(&ctx.accounts.quote_mint.to_account_info(), receipt.quote_reserve_credit)?;
        let base_transfer_amount = receipt
            .base_reserve_credit
            .checked_add(base_transfer_fee)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let quote_transfer_amount = receipt
            .quote_reserve_credit
            .checked_add(quote_transfer_fee)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require_gte!(requested_base_amount, base_transfer_amount, ErrorCode::SlippageExceeded);
        require_gte!(
            requested_quote_amount,
            quote_transfer_amount,
            ErrorCode::SlippageExceeded
        );

        Ok(AddLiquidityPreview {
            requested_base_amount,
            requested_quote_amount,
            max_base_reserve_credit,
            max_quote_reserve_credit,
            base_transfer_amount,
            quote_transfer_amount,
            base_transfer_fee,
            quote_transfer_fee,
            base_reserve_credit: receipt.base_reserve_credit,
            quote_reserve_credit: receipt.quote_reserve_credit,
            unused_base_amount: requested_base_amount
                .checked_sub(base_transfer_amount)
                .ok_or(ErrorCode::MarketMathOverflow)?,
            unused_quote_amount: requested_quote_amount
                .checked_sub(quote_transfer_amount)
                .ok_or(ErrorCode::MarketMathOverflow)?,
            ylp_amount: receipt.ylp_amount,
            ylp_supply: receipt.ylp_supply,
        })
    }
}

impl<'info> PreviewSwap<'info> {
    pub fn handle_preview(ctx: Context<Self>, args: PreviewSwapArgs) -> Result<SwapPreview> {
        require!(args.exact_asset_in > 0, ErrorCode::AmountZero);
        require_supported_asset_mint(&ctx.accounts.asset_in_mint)?;
        require_supported_asset_mint(&ctx.accounts.asset_out_mint)?;

        let clock = Clock::get()?;
        let slot = clock.slot;
        // This account is intentionally read-only, so Anchor will not persist
        // the in-memory simulation. Reusing its already-deserialized storage
        // avoids allocating a second full Market solely for preview.
        let quote_market: &mut Market = &mut ctx.accounts.market;
        debug_log_heap(1);
        let asset_in = quote_market.asset_for_mint(ctx.accounts.asset_in_mint.key())?;
        let asset_out = quote_market.asset_for_mint(ctx.accounts.asset_out_mint.key())?;
        require!(asset_out == asset_in.opposite(), ErrorCode::InvalidMint);

        let transfer_fee = get_transfer_fee(&ctx.accounts.asset_in_mint.to_account_info(), args.exact_asset_in)?;
        let reserve_credit = args
            .exact_asset_in
            .checked_sub(transfer_fee)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let prepared = SwapRequest {
            current_slot: slot,
            current_unix_timestamp: clock.unix_timestamp,
            asset_in,
            reserve_credit,
            protocol_fee_bps: ctx.accounts.futarchy_authority.revenue_share.swap_bps,
        }
        .prepare(quote_market)?;
        debug_log_heap(2);
        let quote = prepared.quote;
        let concentrated_debt_deltas = prepared
            .concentrated_transition
            .as_deref()
            .map(|transition| transition.debt_deltas())
            .unwrap_or((0, 0));
        // The one user-to-reserve transfer fee was already removed when
        // deriving `reserve_credit`. Claimable fees stay in that reserve vault,
        // so they must not pay a fictitious second Token-2022 transfer fee.
        let claimable_fee_credit = quote.fee.claimable_fee_debit;
        let (base_fee_credit, distributed_surcharge_credit) =
            split_claimable_fee_credit(&quote.fee, claimable_fee_credit)?;
        prepared.finalize_state(
            quote_market,
            slot,
            ctx.accounts.futarchy_authority.revenue_share.swap_bps,
            ctx.accounts.futarchy_authority.protocol_auction_split,
        )?;
        debug_log_heap(3);
        let projected_protected_profit_per_share_nad = quote_market.amm.spendable_protected_profit_nad();
        let market: &Market = quote_market;
        let concentrated_metadata = market.amm_preview_metrics(quote.reserve_end_price_nad)?;
        let (market_side_in, market_side_out) = market.swap_sides(asset_in);
        Ok(SwapPreview {
            asset_in,
            asset_out,
            exact_asset_in: args.exact_asset_in,
            transfer_fee,
            reserve_credit,
            fee_asset: MarketAsset::try_from_code(quote.fee.fee_asset)?,
            amount_out: quote.amount_out,
            gross_amount_out: quote.gross_amount_out,
            reserve_in_live_reserve: market_side_in.reserves.live_reserve,
            reserve_out_live_reserve: market_side_out.reserves.live_reserve,
            base_fee_debit: quote.fee.base_fee_debit,
            divergence_surcharge_debit: quote.fee.divergence_surcharge_debit,
            volatility_surcharge_debit: quote.fee.volatility_surcharge_debit,
            dynamic_surcharge_debit: quote.fee.dynamic_surcharge_debit,
            total_fee_debit: quote.fee.total_fee_debit,
            retained_surcharge: quote.fee.retained_surcharge,
            distributed_surcharge_debit: quote.fee.distributed_surcharge_debit,
            claimable_fee_debit: quote.fee.claimable_fee_debit,
            compounded_fee_debit: quote.fee.compounded_fee_debit,
            compounding_fee_bps: market.config.amm.compounding_fee_bps,
            amount_in_for_quote: quote.fee.amount_in_for_quote,
            reserve_input_credit: quote.fee.reserve_input_credit,
            base_fee_credit,
            distributed_surcharge_credit,
            claimable_fee_credit,
            base_fee_rate_nad: quote.fee.base_fee_rate_nad,
            divergence_fee_rate_nad: quote.fee.divergence_fee_rate_nad,
            volatility_fee_rate_nad: quote.fee.volatility_fee_rate_nad,
            total_fee_rate_nad: quote.fee.total_fee_rate_nad,
            start_price_nad: quote.start_price_nad,
            trade_end_price_nad: quote.end_price_nad,
            reserve_end_price_nad: quote.reserve_end_price_nad,
            center_price_nad: market.amm.center_price_nad,
            price_ema_nad: market.amm.price_ema_nad,
            decayed_volatility_nad: quote.decayed_volatility_nad,
            post_success_volatility_nad: quote.post_success_volatility_nad,
            retention_active: market.amm.retain_dynamic_surcharge,
            retention_target_saturated: market.amm.retention_target_saturated,
            protected_recenter_base_reserve: market.base_side.reserves.protected_recenter_reserve,
            protected_recenter_quote_reserve: market.quote_side.reserves.protected_recenter_reserve,
            protected_profit_per_share_nad: market.amm.spendable_protected_profit_nad(),
            projected_protected_profit_per_share_nad,
            retention_required_nad: market.amm.retention_required_nad,
            retention_stop_nad: market.amm.retention_stop_nad,
            retention_hard_cap_nad: market.amm.retention_hard_cap_nad,
            lower_range_price_nad: concentrated_metadata.lower_range_price_nad,
            upper_range_price_nad: concentrated_metadata.upper_range_price_nad,
            concentrated_curve_branch: concentrated_metadata.concentrated_curve_branch,
            ordinary_base_reserve_nad: concentrated_metadata.ordinary_base_reserve_nad,
            ordinary_quote_reserve_nad: concentrated_metadata.ordinary_quote_reserve_nad,
            final_spot_price_nad: concentrated_metadata.final_spot_price_nad,
            base_hlp_quote_debt_delta: concentrated_debt_deltas.0,
            quote_hlp_base_debt_delta: concentrated_debt_deltas.1,
            hlp_recovery_target_asset: quote.recovery.target_asset,
            hlp_recovery_funding_gap: quote.recovery.funding_gap,
            hlp_recovery_matched_input: quote.recovery.matched_input,
            hlp_recovery_bonus_output: quote.recovery.bonus_output,
            hlp_recovery_discount_bps: quote.recovery.discount_bps,
            hlp_recovery_critical: quote.recovery.critical,
        })
    }
}

impl<'info> PreviewBorrowCapacity<'info> {
    pub fn handle_preview(ctx: Context<Self>, args: PreviewBorrowCapacityArgs) -> Result<BorrowCapacityPreview> {
        require!(args.collateral_amount > 0, ErrorCode::AmountZero);
        require_supported_asset_mint(&ctx.accounts.collateral_asset_mint)?;
        require_supported_asset_mint(&ctx.accounts.debt_asset_mint)?;
        ctx.accounts.market.update()?;
        let market: &Market = &ctx.accounts.market;
        let collateral_asset = market.asset_for_mint(ctx.accounts.collateral_asset_mint.key())?;
        let debt_asset = market.asset_for_mint(ctx.accounts.debt_asset_mint.key())?;
        require!(debt_asset == collateral_asset.opposite(), ErrorCode::InvalidMint);
        let slot = Clock::get()?.slot;
        let quote = market.borrow_capacity_quote(
            collateral_asset,
            args.collateral_amount,
            args.projected_borrow_amount,
            slot,
        )?;

        Ok(BorrowCapacityPreview {
            collateral_asset,
            debt_asset,
            collateral_amount: args.collateral_amount,
            collateral_value_nad: quote.collateral_value_nad,
            max_debt_by_health: quote.max_debt_by_health,
            max_debt_by_cash: quote.max_debt_by_cash,
            max_debt_by_daily_limit: quote.max_debt_by_daily_limit,
            max_debt: quote.max_debt,
            max_borrow_amount: quote.max_debt,
            borrow_market_health_floor_bps: market.config.borrow_market_health_floor_bps,
            global_health_contribution_cap_bps: market.config.global_health_contribution_cap_bps,
            projected_borrow_amount: quote.projected_debt_amount,
            projected_debt_amount: quote.projected_debt_amount,
            projected_health_bps: quote.projected_health_bps,
            projected_global_market_health_bps: quote.projected_terms.projected_market_health_bps,
            projected_global_health_contribution: quote.projected_global_health_contribution,
            projected_effective_existing_debt_nad: quote.projected_terms.effective_existing_debt_nad,
            max_cf_bps: quote.projected_terms.max_cf_bps,
            liquidation_cf_bps: quote.projected_terms.liquidation_cf_bps,
            liquidation_debt_per_collateral_price_nad: quote.liquidation_debt_per_collateral_price_nad,
        })
    }
}

impl<'info> PreviewBorrowPosition<'info> {
    pub fn handle_preview(ctx: Context<Self>) -> Result<BorrowPositionPreview> {
        ctx.accounts.market.update()?;
        let market: &Market = &ctx.accounts.market;
        let borrow_position = &ctx.accounts.borrow_position;

        Ok(BorrowPositionPreview {
            owner: borrow_position.owner,
            market: borrow_position.market,
            position_id: borrow_position.position_id,
            base_collateral: borrow_position.base_collateral,
            quote_collateral: borrow_position.quote_collateral,
            global_health_base_contribution_for_quote_debt: borrow_position
                .global_health_base_contribution_for_quote_debt,
            global_health_quote_contribution_for_base_debt: borrow_position
                .global_health_quote_contribution_for_base_debt,
            base_liquidation_cf_bps: borrow_position.base_liquidation_cf_bps,
            quote_liquidation_cf_bps: borrow_position.quote_liquidation_cf_bps,
            fixed_base_debt: borrow_position.fixed_base_debt(&market.debt)?,
            fixed_quote_debt: borrow_position.fixed_quote_debt(&market.debt)?,
            base_debt: preview_position_debt_side(market, borrow_position, MarketAsset::Base)?,
            quote_debt: preview_position_debt_side(market, borrow_position, MarketAsset::Quote)?,
        })
    }
}

fn preview_side(market: &Market, asset: MarketAsset, slot: u64) -> Result<PreviewSide> {
    let side = market.side(asset);
    let (price_ema_nad, directional_price_ema_nad) = match asset {
        MarketAsset::Base => (
            market.risk.base_price_ema_nad,
            market.risk.directional_base_price_ema_nad,
        ),
        MarketAsset::Quote => (
            market.risk.quote_price_ema_nad,
            market.risk.directional_quote_price_ema_nad,
        ),
    };
    let lending = market.lending_side_preview(asset, slot)?;

    Ok(PreviewSide {
        live_reserve: side.reserves.live_reserve,
        cash_reserve: side.reserves.cash_reserve,
        base_hlp_backing_inventory: side.reserves.base_hlp_backing_inventory,
        quote_hlp_backing_inventory: side.reserves.quote_hlp_backing_inventory,
        ylp_supply: side.shares.ylp_supply,
        ylp_exchange_rate_nad: side.ylp_exchange_rate_nad()?,
        spot_price_nad: lending.spot_price_nad,
        price_ema_nad,
        directional_price_ema_nad,
        conservative_depth_nad: lending.conservative_depth_nad,
        borrow_index_nad: lending.borrow_index_nad,
        rate_at_target_nad: lending.rate_at_target_nad,
        borrow_apr_nad: lending.borrow_apr_nad,
        utilization_bps: lending.utilization_bps,
        fixed_debt: lending.fixed_debt,
        isolated_debt: lending.isolated_debt,
        hlp_funding_debt: lending.hlp_funding_debt,
        total_debt: lending.total_debt,
        daily_borrow_limit: lending.daily_borrow_limit,
        daily_borrow_remaining: lending.daily_borrow_remaining,
    })
}

fn preview_position_debt_side(
    market: &Market,
    borrow_position: &BorrowPosition,
    debt_asset: MarketAsset,
) -> Result<PositionDebtSidePreview> {
    let quote = market.position_debt_side_quote(borrow_position, debt_asset)?;

    Ok(PositionDebtSidePreview {
        debt_asset: quote.debt_asset,
        collateral_asset: quote.collateral_asset,
        fixed_debt: quote.fixed_debt,
        collateral_amount: quote.collateral_amount,
        global_health_contribution: quote.global_health_contribution,
        collateral_value_nad: quote.collateral_value_nad,
        health_bps: quote.health_bps,
        max_cf_bps: quote.max_cf_bps,
        liquidation_cf_bps: quote.liquidation_cf_bps,
        liquidation_reference_price_nad: quote.liquidation_reference_price_nad,
        liquidation_health_bps: quote.liquidation_health_bps,
        is_liquidatable: quote.is_liquidatable,
        liquidation_incentive_bps: quote.liquidation_incentive_bps,
        insurance_funding_bps: quote.insurance_funding_bps,
        total_penalty_bps: quote.total_penalty_bps,
        max_repay_amount: quote.max_repay_amount,
    })
}

#[cfg(test)]
mod tests {
    include!("../tests/instructions/preview.rs");
}
