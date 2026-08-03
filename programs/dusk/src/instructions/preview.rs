use anchor_lang::prelude::*;
use anchor_spl::token_interface::Mint;

use crate::instructions::common::require_supported_asset_mint;
use crate::{
    constants::*,
    errors::ErrorCode,
    math::{
        denormalize_from_nad_floor, health_bps, instantaneous_rate_apr_nad, market_k_nad, market_liquidity_nad,
        normalize_to_nad, pessimistic_max_debt_on_curve_nad, utilization_bps, utilization_error_nad,
    },
    shared::{
        math::ceil_div,
        token::{get_transfer_fee, get_transfer_inverse_fee},
    },
    state::{
        market::{
            health::{max_cf_bps_from_liquidation_cf, DynamicBorrowTerms},
            transitions::liquidation::LiquidationPricing,
            AmmCurveParameters, AmmSwapQuote, SwapFeeBreakdown,
        },
        BorrowPosition, Debt, Market, MarketAsset, MarketHealth, Risk,
    },
};

// Most preview instructions retain their historical update-and-return
// behavior. Swap preview is deliberately pure: all clock/ramp/hLP simulation
// runs on a clone so submitting a preview cannot alter fee routing or create a
// curve/Risk freshness mismatch.

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
    pub reserved_liability: u64,
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
    /// Raw reserve-product telemetry. Lending risk uses CONCENTRATED Q, exposed under
    /// `amm.balanced_equivalent_q_nad`, rather than this CPMM-era diagnostic.
    pub reserve_product_k_nad: u128,
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
    pub invariant_d_nad: u128,
    pub invariant_d_high_nad: u128,
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
    pub applied_curve_parameters: AmmCurveParameters,
    pub desired_curve_parameters: AmmCurveParameters,
    pub target_curve_parameters: AmmCurveParameters,
    pub ramp_active: bool,
    pub ramp_start_curve_parameters: AmmCurveParameters,
    pub ramp_start_slot: u64,
    pub ramp_end_slot: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PreviewAddLiquidityArgs {
    pub base_deposit_amount: u64,
    pub quote_deposit_amount: u64,
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

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PreviewSwapArgs {
    pub exact_asset_in: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct SwapPreview {
    pub asset_in: MarketAsset,
    pub asset_out: MarketAsset,
    pub exact_asset_in: u64,
    pub transfer_fee: u64,
    /// Actual credit received by the reserve vault from the user transfer.
    pub reserve_credit: u64,
    /// Legacy alias for `base_fee_debit`.
    pub swap_fee_debit: u64,
    /// Legacy alias for `claimable_fee_credit`.
    pub fee_credit: u64,
    /// Legacy alias for `amount_in_for_quote`.
    pub amount_in_after_fee: u64,
    pub amount_out: u64,
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
    /// Legacy name for the invariant-preserving trade endpoint.
    pub end_price_nad: u64,
    /// Final pool marginal price after retained surcharge enters reserves.
    pub reserve_end_price_nad: u64,
    pub center_price_nad: u64,
    pub price_ema_nad: u64,
    pub decayed_volatility_nad: u64,
    pub post_success_volatility_nad: u64,
    pub retention_active: bool,
    pub retention_target_saturated: bool,
    pub protected_profit_per_share_nad: u128,
    pub projected_protected_profit_per_share_nad: u128,
    pub retention_required_nad: u128,
    pub retention_stop_nad: u128,
    pub retention_hard_cap_nad: u128,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PreviewBorrowCapacityArgs {
    pub collateral_amount: u64,
    pub projected_borrow_amount: Option<u64>,
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
        let slot = Clock::get()?.slot;
        Ok(MarketPreview {
            slot,
            base: preview_side(market, MarketAsset::Base, slot)?,
            quote: preview_side(market, MarketAsset::Quote, slot)?,
            reserve_product_k_nad: market_k_nad(&market.base_side, &market.quote_side)?,
            liquidity_nad: market_liquidity_nad(&market.base_side, &market.quote_side)?,
            health: market.market_health()?,
            amm: preview_amm(market, slot)?,
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

        let slot = Clock::get()?.slot;
        // Match executable spot on a heap-backed clone: accrue interest,
        // advance clock-driven signals, and release at most one stale-target
        // probe without persisting any preview side effect. Heavy ramp and
        // center maintenance remains exclusive to the permissionless crank.
        let mut quote_market = Box::new(Market::default());
        quote_market.as_mut().clone_from(&**ctx.accounts.market);
        quote_market.accrue_interest_to_slot(slot)?;
        if quote_market.base_side.reserves.live_reserve > 0 && quote_market.quote_side.reserves.live_reserve > 0 {
            quote_market.prepare_amm_for_swap(slot)?;
        }
        let asset_in = quote_market.asset_for_mint(ctx.accounts.asset_in_mint.key())?;
        let asset_out = quote_market.asset_for_mint(ctx.accounts.asset_out_mint.key())?;
        require!(asset_out == asset_in.opposite(), ErrorCode::InvalidMint);

        let transfer_fee = get_transfer_fee(&ctx.accounts.asset_in_mint.to_account_info(), args.exact_asset_in)?;
        let reserve_credit = args
            .exact_asset_in
            .checked_sub(transfer_fee)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        // A large CPMM swap can pre-position active hLP inventory before the
        // trader quote. Preview must run that same state-only pre-solve or it
        // advertises depth and min-out terms that execution never uses.
        let preliminary_inputs = quote_market.preliminary_swap_inputs(reserve_credit, slot)?;
        let (base_pre_solve, quote_pre_solve) = quote_market.pre_solve_hlp_vaults_for_swap_with_reserve_input(
            asset_in,
            preliminary_inputs.amount_in_for_quote,
            preliminary_inputs.reserve_input_credit,
        )?;
        let pre_solve_mutates_curve_inventory = [base_pre_solve, quote_pre_solve].iter().any(|receipt| {
            receipt.executed_delta != 0
                || receipt.ylp_mint_amount != 0
                || receipt.ylp_burn_amount != 0
                || receipt.debt_delta != 0
                || receipt.interest_paid != 0
        });
        if pre_solve_mutates_curve_inventory {
            quote_market.checkpoint_amm_neutral_inventory(slot)?;
        } else {
            quote_market.ensure_amm_initialized(slot)?;
        }
        let market: &Market = &quote_market;
        let quote = market.quote_amm_swap(asset_in, reserve_credit, slot)?;
        require_swap_preview_hlp_safe(market, &quote, slot)?;
        let claimable_fee_credit = if quote.fee.claimable_fee_debit == 0 {
            0
        } else {
            let claimable_transfer_fee = get_transfer_fee(
                &ctx.accounts.asset_in_mint.to_account_info(),
                quote.fee.claimable_fee_debit,
            )?;
            quote
                .fee
                .claimable_fee_debit
                .checked_sub(claimable_transfer_fee)
                .ok_or(ErrorCode::MarketMathOverflow)?
        };
        let (base_fee_credit, distributed_surcharge_credit) =
            split_claimable_fee_credit(&quote.fee, claimable_fee_credit)?;
        let projected_protected_profit_per_share_nad = projected_protected_profit_after_swap(market, &quote)?;
        let (market_side_in, market_side_out) = market.swap_sides(asset_in);
        Ok(SwapPreview {
            asset_in,
            asset_out,
            exact_asset_in: args.exact_asset_in,
            transfer_fee,
            reserve_credit,
            swap_fee_debit: quote.fee.base_fee_debit,
            fee_credit: claimable_fee_credit,
            amount_in_after_fee: quote.fee.amount_in_for_quote,
            amount_out: quote.amount_out,
            reserve_in_live_reserve: market_side_in
                .reserves
                .live_reserve
                .checked_add(quote.fee.reserve_input_credit)
                .ok_or(ErrorCode::ReserveOverflow)?,
            reserve_out_live_reserve: market_side_out
                .reserves
                .live_reserve
                .checked_sub(quote.amount_out)
                .ok_or(ErrorCode::ReserveUnderflow)?,
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
            end_price_nad: quote.end_price_nad,
            reserve_end_price_nad: quote.reserve_end_price_nad,
            center_price_nad: market.amm.center_price_nad,
            price_ema_nad: market.amm.price_ema_nad,
            decayed_volatility_nad: quote.decayed_volatility_nad,
            post_success_volatility_nad: quote.post_success_volatility_nad,
            retention_active: market.amm.retain_dynamic_surcharge,
            retention_target_saturated: market.amm.retention_target_saturated,
            protected_profit_per_share_nad: market.amm.spendable_protected_profit_nad(),
            projected_protected_profit_per_share_nad,
            retention_required_nad: market.amm.retention_required_nad,
            retention_stop_nad: market.amm.retention_stop_nad,
            retention_hard_cap_nad: market.amm.retention_hard_cap_nad,
        })
    }
}

/// Preview is read-only, so it runs the exact concentrated hLP admission guard
/// without checkpointing pending exposure. Successful execution calls the same
/// guard again through the deferred hLP checkpoint after applying reserves.
fn require_swap_preview_hlp_safe(market: &Market, quote: &AmmSwapQuote, slot: u64) -> Result<()> {
    if !market.has_active_hlp() || market.current_curve_parameters(slot).is_cpmm() {
        return Ok(());
    }
    let final_price_nad = u64::try_from(
        quote
            .reserve_endpoint_certificate()?
            .certified_evaluation()
            .marginal_price_nad,
    )
    .map_err(|_| ErrorCode::MarketMathOverflow)?;
    market.require_hlp_vaults_after_concentrated_swap_safe(quote.start_price_nad, final_price_nad)
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
        let preview_context = NewPositionPreviewContext::new(market, debt_asset, args.collateral_amount, &risk)?;
        let max_debt_by_health =
            max_new_position_debt_by_dynamic_health(&preview_context, debt_side.reserves.live_reserve)?;
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
            liquidation_debt_per_collateral_price_nad: liquidation_threshold_price_nad(
                args.collateral_amount,
                collateral_side.asset_decimals,
                projected_debt_amount,
                debt_side.asset_decimals,
                projected_terms.liquidation_cf_bps,
            )?,
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
    let fixed_debt = fixed_debt(market, asset)?;
    let isolated_debt = market.debt.isolated_debt(asset)?;
    let hlp_funding_debt = hlp_funding_debt(market, asset)?;
    let total_debt = fixed_debt
        .checked_add(isolated_debt)
        .and_then(|value| value.checked_add(hlp_funding_debt))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let utilization_bps = utilization_bps(total_debt, side.reserves.cash_reserve as u128)?;
    let utilization_error_nad = utilization_error_nad(utilization_bps, INTEREST_TARGET_UTILIZATION_BPS)?;
    let borrow_apr_nad =
        instantaneous_rate_apr_nad(rate_at_target_nad, utilization_error_nad, INTEREST_CURVE_STEEPNESS_NAD)?;
    let daily_borrow_limit = market.daily_limit_for_side(asset, market.config.max_daily_borrow_bps)?;
    let daily_borrow_remaining = daily_borrow_remaining(market, asset, slot)?;

    Ok(PreviewSide {
        live_reserve: side.reserves.live_reserve,
        cash_reserve: side.reserves.cash_reserve,
        reserved_liability: side.reserves.reserved_liability,
        ylp_supply: side.shares.ylp_supply,
        ylp_exchange_rate_nad: side.ylp_exchange_rate_nad()?,
        spot_price_nad: market.curve_price_for_asset_nad(asset, slot)?,
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

fn preview_amm(market: &Market, slot: u64) -> Result<PreviewAmm> {
    let state = &market.amm;
    let configured_target = market.config.amm.curve_parameters();
    let target_curve_parameters = if state.ramp.active {
        state.ramp.target
    } else {
        configured_target
    };
    let (executable_base_reserve, executable_quote_reserve, balanced_equivalent_q_nad) = if state.initialized {
        (
            market.curve_reserve(MarketAsset::Base)?,
            market.curve_reserve(MarketAsset::Quote)?,
            market.evaluate_current_curve(slot)?.balanced_equivalent_q,
        )
    } else {
        (0, 0, 0)
    };

    Ok(PreviewAmm {
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
        invariant_d_nad: state.invariant_d_nad,
        invariant_d_high_nad: state.invariant_d_high_nad,
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
        applied_curve_parameters: state.effective_curve_parameters(&market.config.amm, slot),
        desired_curve_parameters: state.desired_curve_parameters(&market.config.amm, slot),
        target_curve_parameters,
        ramp_active: state.ramp.active,
        ramp_start_curve_parameters: state.ramp.start,
        ramp_start_slot: state.ramp.start_slot,
        ramp_end_slot: state.ramp.end_slot,
    })
}

fn split_claimable_fee_credit(fee: &SwapFeeBreakdown, total_credit: u64) -> Result<(u64, u64)> {
    require_eq!(
        fee.base_fee_debit
            .checked_add(fee.distributed_surcharge_debit)
            .ok_or(ErrorCode::FeeMathOverflow)?,
        fee.claimable_fee_debit,
        ErrorCode::BrokenInvariant
    );
    require_gte!(fee.claimable_fee_debit, total_credit, ErrorCode::BrokenInvariant);
    if fee.claimable_fee_debit == 0 {
        return Ok((0, 0));
    }
    let base_credit = u64::try_from(
        (total_credit as u128)
            .checked_mul(fee.base_fee_debit as u128)
            .and_then(|value| value.checked_div(fee.claimable_fee_debit as u128))
            .ok_or(ErrorCode::FeeMathOverflow)?,
    )
    .map_err(|_| ErrorCode::FeeMathOverflow)?;
    let distributed_surcharge_credit = total_credit
        .checked_sub(base_credit)
        .ok_or(ErrorCode::FeeMathOverflow)?;
    Ok((base_credit, distributed_surcharge_credit))
}

fn projected_protected_profit_after_swap(market: &Market, quote: &AmmSwapQuote) -> Result<u128> {
    let protected_before = market.amm.spendable_protected_profit_nad();
    if quote.fee.retained_surcharge == 0 {
        return Ok(protected_before);
    }

    let post_trade_evaluation = quote.trade_endpoint_certificate()?.certified_evaluation();
    let post_trade_q = market.curve_q_per_share_nad(post_trade_evaluation.balanced_equivalent_q)?;
    let with_retained_evaluation = quote.reserve_endpoint_certificate()?.certified_evaluation();
    let with_retained_q = market.curve_q_per_share_nad(with_retained_evaluation.balanced_equivalent_q)?;
    require_gte!(with_retained_q, post_trade_q, ErrorCode::BrokenInvariant);
    protected_before
        .checked_add(with_retained_q - post_trade_q)
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

fn fixed_debt(market: &Market, asset: MarketAsset) -> Result<u128> {
    match asset {
        MarketAsset::Base => market.debt.fixed_base_debt(),
        MarketAsset::Quote => market.debt.fixed_quote_debt(),
    }
}

fn hlp_funding_debt(market: &Market, asset: MarketAsset) -> Result<u128> {
    let (shares, borrow_index_nad) = match asset {
        MarketAsset::Base => (market.quote_hlp_vault.debt_shares, market.debt.base_borrow_index_nad),
        MarketAsset::Quote => (market.base_hlp_vault.debt_shares, market.debt.quote_borrow_index_nad),
    };
    Debt::shares_to_debt(shares, borrow_index_nad)
}

fn daily_borrow_remaining(market: &Market, asset: MarketAsset, slot: u64) -> Result<u64> {
    let side = market.side(asset);
    let limit = market.daily_limit_for_side(asset, market.config.max_daily_borrow_bps)?;
    let mut limits = side.daily_limits;
    limits.decay_to_slot(slot)?;
    Ok(limit.saturating_sub(limits.borrowed_bucket))
}

struct NewPositionPreviewContext<'a> {
    market: &'a Market,
    debt_asset: MarketAsset,
    collateral_amount: u64,
    risk: &'a Risk,
    existing_total_debt_nad: u128,
    current_aggregate_contribution: u64,
    collateral_amount_nad: u128,
    collateral_virtual_reserve_nad: u128,
    debt_virtual_reserve_nad: u128,
}

impl<'a> NewPositionPreviewContext<'a> {
    fn new(market: &'a Market, debt_asset: MarketAsset, collateral_amount: u64, risk: &'a Risk) -> Result<Self> {
        let collateral_asset = debt_asset.opposite();
        let (collateral_virtual_reserve_nad, debt_virtual_reserve_nad) =
            market.pessimistic_virtual_reserves_nad(collateral_asset, risk, true)?;
        Ok(Self {
            market,
            debt_asset,
            collateral_amount,
            risk,
            existing_total_debt_nad: market.total_fixed_debt_nad(debt_asset)?,
            current_aggregate_contribution: match debt_asset {
                MarketAsset::Base => market.debt.global_health_quote_contribution_for_base_debt,
                MarketAsset::Quote => market.debt.global_health_base_contribution_for_quote_debt,
            },
            collateral_amount_nad: normalize_to_nad(
                collateral_amount as u128,
                market.side(collateral_asset).asset_decimals,
            )?,
            collateral_virtual_reserve_nad,
            debt_virtual_reserve_nad,
        })
    }

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
        let (effective_existing_debt_nad, projected_market_health_bps) =
            self.market.global_side_health_with_virtual_reserves(
                self.debt_asset,
                self.existing_total_debt_nad,
                projected_total_debt_nad,
                projected_aggregate,
                self.risk,
                self.collateral_virtual_reserve_nad,
                self.debt_virtual_reserve_nad,
            )?;
        let collateral_asset = self.debt_asset.opposite();
        let (curve, collateral_to_debt) = self.market.risk_curve_from_ordered_reserves(
            collateral_asset,
            self.risk,
            self.collateral_virtual_reserve_nad,
            self.debt_virtual_reserve_nad,
        )?;
        let terms = pessimistic_max_debt_on_curve_nad(
            self.collateral_amount_nad,
            curve,
            collateral_to_debt,
            effective_existing_debt_nad,
        )?;
        Ok((
            DynamicBorrowTerms {
                max_debt: denormalize_from_nad_floor(terms.max_debt_nad, debt_decimals)?,
                max_cf_bps: terms.max_cf_bps,
                liquidation_cf_bps: terms.liquidation_cf_bps,
                effective_existing_debt_nad,
                projected_market_health_bps,
            },
            contribution,
        ))
    }

    fn is_accepted(&self, projected_debt_amount: u64) -> Result<bool> {
        let (terms, _) = self.terms(projected_debt_amount)?;
        Ok(terms.max_debt >= projected_debt_amount
            && terms.projected_market_health_bps >= self.market.config.borrow_market_health_floor_bps as u64)
    }
}

fn max_new_position_debt_by_dynamic_health(context: &NewPositionPreviewContext<'_>, upper_bound: u64) -> Result<u64> {
    let current_health = context.market.market_health_from_risk(context.risk)?;
    if context.market.assert_market_health_snapshot(&current_health).is_err() {
        return Ok(0);
    }

    let mut low = 0_u64;
    let mut high = upper_bound;
    while low < high {
        let midpoint = low + (high - low) / 2 + 1;
        if context.is_accepted(midpoint)? {
            low = midpoint;
        } else {
            high = midpoint - 1;
        }
    }
    Ok(low)
}

fn liquidation_threshold_price_nad(
    collateral_amount: u64,
    collateral_decimals: u8,
    debt_amount: u64,
    debt_decimals: u8,
    liquidation_cf_bps: u16,
) -> Result<u64> {
    if collateral_amount == 0 || debt_amount == 0 || liquidation_cf_bps == 0 {
        return Ok(0);
    }
    let collateral_nad = normalize_to_nad(collateral_amount as u128, collateral_decimals)?;
    let debt_nad = normalize_to_nad(debt_amount as u128, debt_decimals)?;
    let price = ceil_div(
        debt_nad
            .checked_mul(BPS_DENOMINATOR as u128)
            .and_then(|value| value.checked_mul(NAD as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?,
        collateral_nad
            .checked_mul(liquidation_cf_bps as u128)
            .ok_or(ErrorCode::MarketMathOverflow)?,
    )
    .ok_or(ErrorCode::MarketMathOverflow)?;
    u64::try_from(price).map_err(|_| ErrorCode::MarketMathOverflow.into())
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
    use super::*;
    use proptest::prelude::*;

    fn preview_test_market(existing_base_debt: u64, aggregate_quote_contribution: u64) -> Market {
        let mut market = Market::default();
        market.base_side.asset_decimals = 0;
        market.quote_side.asset_decimals = 0;
        market.base_side.reserves.live_reserve = 1_000_000;
        market.base_side.reserves.cash_reserve = 1_000_000;
        market.quote_side.reserves.live_reserve = 1_000_000;
        market.quote_side.reserves.cash_reserve = 1_000_000;
        market.debt.base_borrow_index_nad = NAD as u128;
        market.debt.quote_borrow_index_nad = NAD as u128;
        market.debt.fixed_base_shares = existing_base_debt as u128;
        market.debt.global_health_quote_contribution_for_base_debt = aggregate_quote_contribution;
        market.config.global_health_contribution_cap_bps = 15_000;
        market.config.borrow_market_health_floor_bps = 11_000;
        market.risk = Risk {
            base_price_ema_nad: NAD,
            quote_price_ema_nad: NAD,
            directional_base_price_ema_nad: NAD,
            directional_quote_price_ema_nad: NAD,
            q_ema_nad: 1_000_000_u128 * NAD as u128,
            ..Risk::default()
        };
        market
    }

    #[test]
    fn dynamic_health_binary_search_matches_brute_force() {
        let market = preview_test_market(50_000, 75_000);
        let upper_bound = 5_000;
        let context = NewPositionPreviewContext::new(&market, MarketAsset::Base, 5_000, &market.risk).unwrap();
        let binary = max_new_position_debt_by_dynamic_health(&context, upper_bound).unwrap();
        let brute = (0..=upper_bound)
            .filter(|candidate| context.is_accepted(*candidate).unwrap())
            .max()
            .unwrap();

        assert_eq!(binary, brute);
    }

    #[test]
    fn token_2022_claimable_fee_preview_matches_execution_split() {
        let fee = SwapFeeBreakdown {
            base_fee_debit: 60,
            distributed_surcharge_debit: 40,
            claimable_fee_debit: 100,
            retained_surcharge: 25,
            ..SwapFeeBreakdown::default()
        };

        let (base_credit, distributed_credit) = split_claimable_fee_credit(&fee, 97).unwrap();

        assert_eq!(base_credit, 58);
        assert_eq!(distributed_credit, 39);
        assert_eq!(base_credit + distributed_credit, 97);
    }

    #[test]
    fn concentrated_swap_preview_and_execution_reject_the_same_stale_hlp_path() {
        let mut market = preview_test_market(0, 0);
        market.base_side.shares.ylp_supply = 1_000_000;
        market.quote_side.shares.ylp_supply = 1_000_000;
        market.config.settlement_divergence_bps = 1;
        market.config.amm.peak_depth_nad = 200 * NAD;
        market.config.amm.imbalance_scale_nad = NAD / 10;
        market.checkpoint_amm_neutral_inventory(1).unwrap();
        let reference = market.curve_marginal_price_nad(1).unwrap();
        market.base_hlp_vault.hlp_supply = 1;
        market.base_hlp_vault.cached_settlement_price_nad = reference as u128;

        let quote = market.quote_amm_swap(MarketAsset::Base, 50_000, 1).unwrap();
        let final_price = u64::try_from(
            quote
                .reserve_endpoint_certificate()
                .unwrap()
                .certified_evaluation()
                .marginal_price_nad,
        )
        .unwrap();
        let preview_error = require_swap_preview_hlp_safe(&market, &quote, 1).unwrap_err();
        let mut execution_market = market.clone();
        let execution_error = execution_market
            .defer_hlp_vaults_after_concentrated_swap(quote.start_price_nad, final_price)
            .unwrap_err();

        assert_eq!(preview_error, error!(ErrorCode::HlpSettlementUnavailable));
        assert_eq!(execution_error, preview_error);
    }

    proptest! {
        #[test]
        fn dynamic_health_acceptance_is_monotonic(
            existing_debt in 0_u64..100_000,
            existing_contribution_bps in 13_000_u64..=15_000,
            collateral_amount in 1_u64..500_000,
            lower_candidate in 0_u64..300_000,
            candidate_delta in 0_u64..300_000,
        ) {
            let aggregate_contribution = existing_debt
                .saturating_mul(existing_contribution_bps)
                / BPS_DENOMINATOR as u64;
            let market = preview_test_market(existing_debt, aggregate_contribution);
            let context = NewPositionPreviewContext::new(
                &market,
                MarketAsset::Base,
                collateral_amount,
                &market.risk,
            )
            .unwrap();
            let higher_candidate = lower_candidate.saturating_add(candidate_delta);
            let lower_accepted = context.is_accepted(lower_candidate).unwrap();
            let higher_accepted = context.is_accepted(higher_candidate).unwrap();

            let (cached_terms, cached_contribution) = context.terms(lower_candidate).unwrap();
            let projected_debt_nad = normalize_to_nad(lower_candidate as u128, 0).unwrap();
            let projected_aggregate = aggregate_contribution
                .checked_add(cached_contribution)
                .unwrap();
            let full_terms = market
                .dynamic_borrow_terms(
                    MarketAsset::Base,
                    collateral_amount,
                    existing_debt as u128 * NAD as u128,
                    existing_debt as u128 * NAD as u128 + projected_debt_nad,
                    projected_aggregate,
                    &market.risk,
                )
                .unwrap();

            prop_assert!(!higher_accepted || lower_accepted);
            prop_assert_eq!(cached_terms, full_terms);

            let upper_bound = 600_000;
            let maximum = max_new_position_debt_by_dynamic_health(&context, upper_bound).unwrap();
            if maximum > 0 {
                prop_assert!(context.is_accepted(maximum).unwrap());
            }
            if maximum < upper_bound {
                prop_assert!(!context.is_accepted(maximum + 1).unwrap());
            }
        }
    }
}
