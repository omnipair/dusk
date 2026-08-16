use anchor_lang::prelude::*;
use anchor_spl::token_interface::Mint;

use crate::instructions::accounts::require_supported_asset_mint;
use crate::instructions::{split_claimable_fee_credit, SwapRequest};
use crate::{
    constants::*,
    errors::ErrorCode,
    market::{max_cf_bps_from_liquidation_cf, DynamicBorrowTerms, LiquidationPricing},
    math::{
        ceil_div, geometric_mean_floor, health_bps, instantaneous_rate_apr_nad, normalize_to_nad, utilization_bps,
        utilization_error_nad,
    },
    state::{BorrowPosition, FutarchyAuthority, Market, MarketAsset, MarketHealth, Risk},
    token::{get_transfer_fee, get_transfer_inverse_fee},
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
    pub balanced_equivalent_q_nad: u128,
    pub q_per_share_nad: u128,
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
    pub range_width_nad: u64,
    pub concentrated_liquidity_share_nad: u64,
    pub lower_range_price_nad: u64,
    pub upper_range_price_nad: u64,
    pub explicit_curve_branch: u8,
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
    /// Explicit range metadata. Zeroes denote the legacy curve during the
    /// temporary caller migration.
    pub lower_range_price_nad: u64,
    pub upper_range_price_nad: u64,
    /// 0=lower tail, 1=concentrated band, 2=upper tail.
    pub explicit_curve_branch: u8,
    pub ordinary_base_reserve_nad: u128,
    pub ordinary_quote_reserve_nad: u128,
    pub final_spot_price_nad: u64,
    pub base_hlp_quote_debt_delta: i128,
    pub quote_hlp_base_debt_delta: i128,
    /// Yield-Basis-like recovery funded exclusively by the stressed hLP.
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
            let explicit_parameters = market.config.amm.explicit_curve_parameters()?;
            let (executable_base_reserve, executable_quote_reserve, balanced_equivalent_q_nad) = if state.initialized {
                let q_nad = state
                    .explicit_curve_cache
                    .tail_liquidity
                    .checked_add(state.explicit_curve_cache.concentrated_liquidity)
                    .ok_or(ErrorCode::InvariantOverflow)?;
                (
                    market.curve_reserve(MarketAsset::Base)?,
                    market.curve_reserve(MarketAsset::Quote)?,
                    q_nad,
                )
            } else {
                (0, 0, 0)
            };
            let explicit_metadata = if state.initialized {
                let geometry = state.explicit_curve_cache.geometry()?;
                let ordinary = market.integrated_curve_state_nad()?;
                let (lower, upper) = geometry.range_prices_nad()?.unwrap_or((0, 0));
                (
                    u64::try_from(lower).map_err(|_| ErrorCode::MarketMathOverflow)?,
                    u64::try_from(upper).map_err(|_| ErrorCode::MarketMathOverflow)?,
                    geometry
                        .branch(crate::math::ExplicitCurvePoint {
                            base_reserve: ordinary.ordinary_base,
                            quote_reserve: ordinary.ordinary_quote,
                        })
                        .code(),
                    ordinary.ordinary_base,
                    ordinary.ordinary_quote,
                )
            } else {
                (0, 0, 0, 0, 0)
            };

            PreviewAmm {
                initialized: state.initialized,
                executable_base_reserve,
                executable_quote_reserve,
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
                balanced_equivalent_q_nad,
                q_per_share_nad: state.q_per_share_nad,
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
                range_width_nad: explicit_parameters.range_width_nad,
                concentrated_liquidity_share_nad: explicit_parameters.concentrated_liquidity_share_nad,
                lower_range_price_nad: explicit_metadata.0,
                upper_range_price_nad: explicit_metadata.1,
                explicit_curve_branch: explicit_metadata.2,
                ordinary_base_reserve_nad: explicit_metadata.3,
                ordinary_quote_reserve_nad: explicit_metadata.4,
            }
        };
        Ok(MarketPreview {
            slot,
            base: preview_side(market, MarketAsset::Base, slot)?,
            quote: preview_side(market, MarketAsset::Quote, slot)?,
            liquidity_nad: geometric_mean_floor(
                normalize_to_nad(
                    market.base_side.reserves.live_reserve as u128,
                    market.base_side.asset_decimals,
                )?,
                normalize_to_nad(
                    market.quote_side.reserves.live_reserve as u128,
                    market.quote_side.asset_decimals,
                )?,
            )?,
            health: market.market_health()?,
            amm,
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
        }
        .prepare(quote_market)?;
        debug_log_heap(2);
        let quote = prepared.quote;
        let explicit_debt_deltas = prepared
            .explicit_transition
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
        let explicit_metadata = if let Some(geometry) = market.current_explicit_curve_geometry()? {
            let ordinary = market.integrated_curve_state_nad()?;
            let (lower, upper) = geometry.range_prices_nad()?.unwrap_or((0, 0));
            (
                u64::try_from(lower).map_err(|_| ErrorCode::MarketMathOverflow)?,
                u64::try_from(upper).map_err(|_| ErrorCode::MarketMathOverflow)?,
                geometry
                    .branch(crate::math::ExplicitCurvePoint {
                        base_reserve: ordinary.ordinary_base,
                        quote_reserve: ordinary.ordinary_quote,
                    })
                    .code(),
                ordinary.ordinary_base,
                ordinary.ordinary_quote,
                market
                    .current_explicit_spot_price_nad()?
                    .ok_or(ErrorCode::BrokenInvariant)?,
            )
        } else {
            (0, 0, 0, 0, 0, quote.reserve_end_price_nad)
        };
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
            lower_range_price_nad: explicit_metadata.0,
            upper_range_price_nad: explicit_metadata.1,
            explicit_curve_branch: explicit_metadata.2,
            ordinary_base_reserve_nad: explicit_metadata.3,
            ordinary_quote_reserve_nad: explicit_metadata.4,
            final_spot_price_nad: explicit_metadata.5,
            base_hlp_quote_debt_delta: explicit_debt_deltas.0,
            quote_hlp_base_debt_delta: explicit_debt_deltas.1,
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

        let collateral_side = market.side(collateral_asset);
        let debt_side = market.side(debt_asset);
        let risk = market.current_risk()?;
        let collateral_value_nad = market.collateral_value_nad(collateral_asset, args.collateral_amount, &risk)?;
        let max_debt_by_cash = debt_side.reserves.cash_reserve;
        let slot = Clock::get()?.slot;
        let max_debt_by_daily_limit = daily_borrow_remaining(market, debt_asset, slot)?;
        let preview_context = NewPositionPreviewContext {
            market,
            debt_asset,
            collateral_amount: args.collateral_amount,
            risk: &risk,
            existing_total_debt_nad: market.total_fixed_debt_nad(debt_asset)?,
            current_aggregate_contribution: match debt_asset {
                MarketAsset::Base => market.debt.global_health_quote_contribution_for_base_debt,
                MarketAsset::Quote => market.debt.global_health_base_contribution_for_quote_debt,
            },
        };
        let max_debt_by_health = {
            let current_health = market.market_health_from_risk(&risk)?;
            if market.assert_market_health_snapshot(&current_health).is_err() {
                0
            } else {
                let mut low = 0_u64;
                let mut high = debt_side.reserves.live_reserve;
                while low < high {
                    let midpoint = low + (high - low) / 2 + 1;
                    let (terms, _) = preview_context.terms(midpoint)?;
                    let accepted = terms.max_debt >= midpoint
                        && terms.projected_market_health_bps >= market.config.borrow_market_health_floor_bps as u64;
                    if accepted {
                        low = midpoint;
                    } else {
                        high = midpoint - 1;
                    }
                }
                low
            }
        };
        let max_debt = max_debt_by_health.min(max_debt_by_cash).min(max_debt_by_daily_limit);
        let max_borrow_amount = max_debt;
        let projected_borrow_amount = args.projected_borrow_amount.unwrap_or(max_borrow_amount);
        let projected_debt_amount = projected_borrow_amount;
        let (projected_terms, projected_global_health_contribution) = preview_context.terms(projected_debt_amount)?;
        let projected_debt_nad = normalize_to_nad(projected_debt_amount as u128, debt_side.asset_decimals)?;
        let projected_health_bps = if projected_debt_nad == 0 {
            u64::MAX
        } else {
            health_bps(collateral_value_nad, projected_debt_nad)?
        };

        Ok(BorrowCapacityPreview {
            collateral_asset,
            debt_asset,
            collateral_amount: args.collateral_amount,
            collateral_value_nad,
            max_debt_by_health,
            max_debt_by_cash,
            max_debt_by_daily_limit,
            max_debt,
            max_borrow_amount,
            borrow_market_health_floor_bps: market.config.borrow_market_health_floor_bps,
            global_health_contribution_cap_bps: market.config.global_health_contribution_cap_bps,
            projected_borrow_amount,
            projected_debt_amount,
            projected_health_bps,
            projected_global_market_health_bps: projected_terms.projected_market_health_bps,
            projected_global_health_contribution,
            projected_effective_existing_debt_nad: projected_terms.effective_existing_debt_nad,
            max_cf_bps: projected_terms.max_cf_bps,
            liquidation_cf_bps: projected_terms.liquidation_cf_bps,
            liquidation_debt_per_collateral_price_nad: if args.collateral_amount == 0
                || projected_debt_amount == 0
                || projected_terms.liquidation_cf_bps == 0
            {
                0
            } else {
                let collateral_nad = normalize_to_nad(args.collateral_amount as u128, collateral_side.asset_decimals)?;
                let debt_nad = normalize_to_nad(projected_debt_amount as u128, debt_side.asset_decimals)?;
                let price = ceil_div(
                    debt_nad
                        .checked_mul(BPS_DENOMINATOR as u128)
                        .and_then(|value| value.checked_mul(NAD as u128))
                        .ok_or(ErrorCode::MarketMathOverflow)?,
                    collateral_nad
                        .checked_mul(projected_terms.liquidation_cf_bps as u128)
                        .ok_or(ErrorCode::MarketMathOverflow)?,
                )
                .ok_or(ErrorCode::MarketMathOverflow)?;
                u64::try_from(price).map_err(|_| ErrorCode::MarketMathOverflow)?
            },
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
    let (base_depth, quote_depth) = market.conservative_risk_reserve_depths(&market.risk)?;
    let conservative_depth_nad = match asset {
        MarketAsset::Base => normalize_to_nad(base_depth as u128, side.asset_decimals)?,
        MarketAsset::Quote => normalize_to_nad(quote_depth as u128, side.asset_decimals)?,
    };
    let borrow_index_nad = market.debt.borrow_index(asset);
    let rate_at_target_nad = match asset {
        MarketAsset::Base => market.debt.base_rate_at_target_nad,
        MarketAsset::Quote => market.debt.quote_rate_at_target_nad,
    };
    let fixed_debt = match asset {
        MarketAsset::Base => market.debt.fixed_base_debt()?,
        MarketAsset::Quote => market.debt.fixed_quote_debt()?,
    };
    let isolated_debt = market.debt.isolated_debt(asset)?;
    let (hlp_debt_shares, hlp_borrow_index_nad) = match asset {
        MarketAsset::Base => (market.quote_hlp_vault.debt_shares, market.debt.base_borrow_index_nad),
        MarketAsset::Quote => (market.base_hlp_vault.debt_shares, market.debt.quote_borrow_index_nad),
    };
    let hlp_funding_debt = crate::state::Debt::shares_to_debt(hlp_debt_shares, hlp_borrow_index_nad)?;
    let total_debt = fixed_debt
        .checked_add(isolated_debt)
        .and_then(|value| value.checked_add(hlp_funding_debt))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let utilization_bps = utilization_bps(total_debt, side.reserves.cash_reserve as u128)?;
    let utilization_error_nad =
        utilization_error_nad(utilization_bps, market.config.irm.target_utilization_bps as u64)?;
    let borrow_apr_nad = instantaneous_rate_apr_nad(
        rate_at_target_nad,
        utilization_error_nad,
        market.config.irm.curve_steepness_nad as u128,
    )?;
    let daily_borrow_limit = market.daily_limit_for_side(asset, market.config.max_daily_borrow_bps)?;
    let daily_borrow_remaining = daily_borrow_remaining(market, asset, slot)?;

    Ok(PreviewSide {
        live_reserve: side.reserves.live_reserve,
        cash_reserve: side.reserves.cash_reserve,
        base_hlp_backing_inventory: side.reserves.base_hlp_backing_inventory,
        quote_hlp_backing_inventory: side.reserves.quote_hlp_backing_inventory,
        ylp_supply: side.shares.ylp_supply,
        ylp_exchange_rate_nad: side.ylp_exchange_rate_nad()?,
        spot_price_nad: {
            let base_price = market
                .current_explicit_spot_price_nad()?
                .ok_or(ErrorCode::BrokenInvariant)?;
            match asset {
                MarketAsset::Base => base_price,
                MarketAsset::Quote => {
                    require!(base_price > 0, ErrorCode::InvalidSettlementPrice);
                    let inverse = (NAD as u128)
                        .checked_mul(NAD as u128)
                        .and_then(|value| value.checked_div(base_price as u128))
                        .ok_or(ErrorCode::MarketMathOverflow)?;
                    u64::try_from(inverse).map_err(|_| ErrorCode::MarketMathOverflow)?
                }
            }
        },
        price_ema_nad,
        directional_price_ema_nad,
        conservative_depth_nad,
        borrow_index_nad,
        rate_at_target_nad,
        borrow_apr_nad,
        utilization_bps,
        fixed_debt,
        isolated_debt,
        hlp_funding_debt,
        total_debt,
        daily_borrow_limit,
        daily_borrow_remaining,
    })
}

fn daily_borrow_remaining(market: &Market, asset: MarketAsset, slot: u64) -> Result<u64> {
    let limit = market.daily_limit_for_side(asset, market.config.max_daily_borrow_bps)?;
    market.side(asset).daily_borrow_bucket.remaining(limit, slot)
}

struct NewPositionPreviewContext<'a> {
    market: &'a Market,
    debt_asset: MarketAsset,
    collateral_amount: u64,
    risk: &'a Risk,
    existing_total_debt_nad: u128,
    current_aggregate_contribution: u64,
}

impl<'a> NewPositionPreviewContext<'a> {
    fn terms(&self, projected_debt_amount: u64) -> Result<(DynamicBorrowTerms, u64)> {
        let debt_decimals = self.market.side(self.debt_asset).asset_decimals;
        let projected_debt_nad = normalize_to_nad(projected_debt_amount as u128, debt_decimals)?;
        let projected_total_debt_nad = self
            .existing_total_debt_nad
            .checked_add(projected_debt_nad)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let contribution = self.market.debt_capped_global_health_contribution(
            self.debt_asset,
            projected_debt_amount as u128,
            self.collateral_amount,
            self.risk,
        )?;
        let projected_aggregate = self
            .current_aggregate_contribution
            .checked_add(contribution)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let terms = self.market.dynamic_borrow_terms(
            self.debt_asset,
            self.collateral_amount,
            self.existing_total_debt_nad,
            projected_total_debt_nad,
            projected_aggregate,
            self.risk,
        )?;
        Ok((terms, contribution))
    }
}

fn preview_position_debt_side(
    market: &Market,
    borrow_position: &BorrowPosition,
    debt_asset: MarketAsset,
) -> Result<PositionDebtSidePreview> {
    let collateral_asset = debt_asset.opposite();
    let debt = match debt_asset {
        MarketAsset::Base => borrow_position.fixed_base_debt(&market.debt)?,
        MarketAsset::Quote => borrow_position.fixed_quote_debt(&market.debt)?,
    };
    let collateral_amount = borrow_position.collateral(collateral_asset);
    let global_health_contribution = borrow_position.global_health_contribution(debt_asset);
    let liquidation_cf_bps = borrow_position.liquidation_cf_bps(debt_asset);
    let risk = market.current_risk()?;
    let collateral_value_nad = market.collateral_value_nad(collateral_asset, collateral_amount, &risk)?;
    let health_bps = if debt == 0 {
        u64::MAX
    } else {
        let debt_side = market.side(debt_asset);
        health_bps(collateral_value_nad, normalize_to_nad(debt, debt_side.asset_decimals)?)?
    };
    let liquidation_reference_price_nad = if debt == 0 {
        0
    } else {
        market.liquidation_reference_price_nad(borrow_position, debt_asset)?
    };
    let pricing = LiquidationPricing::ReferencePrice {
        debt_per_collateral_price_nad: liquidation_reference_price_nad,
    };
    let liquidation_health_bps = if debt == 0 {
        u64::MAX
    } else {
        market.liquidation_health_bps_with_pricing(borrow_position, debt_asset, pricing)?
    };
    let terms = if debt == 0 {
        Default::default()
    } else {
        market.liquidation_terms_with_pricing(borrow_position, debt_asset, pricing)?
    };

    Ok(PositionDebtSidePreview {
        debt_asset,
        collateral_asset,
        fixed_debt: debt,
        collateral_amount,
        global_health_contribution,
        collateral_value_nad,
        health_bps,
        max_cf_bps: max_cf_bps_from_liquidation_cf(liquidation_cf_bps),
        liquidation_cf_bps,
        liquidation_reference_price_nad,
        liquidation_health_bps,
        is_liquidatable: market.is_position_liquidatable_with_risk(borrow_position, debt_asset, &risk)?,
        liquidation_incentive_bps: terms.liquidation_incentive_bps,
        insurance_funding_bps: terms.insurance_funding_bps,
        total_penalty_bps: terms.total_penalty_bps,
        max_repay_amount: terms.max_repay_amount,
    })
}

#[cfg(test)]
mod tests {
    include!("../tests/instructions/preview.rs");
}
