use anchor_lang::prelude::*;
use anchor_spl::token_interface::Mint;

use crate::instructions::common::require_supported_asset_mint;
use crate::{
    constants::*,
    errors::ErrorCode,
    math::{
        calculate_raw_amount_out, denormalize_from_nad_floor, health_bps, instantaneous_rate_apr_nad, market_k_nad,
        market_liquidity_nad, market_spot_price_nad, normalize_to_nad, pessimistic_max_debt_nad, utilization_bps,
        utilization_error_nad,
    },
    shared::{
        math::ceil_div,
        token::{get_transfer_fee, get_transfer_inverse_fee},
    },
    state::{
        market::{
            health::{max_cf_bps_from_liquidation_cf, DynamicBorrowTerms},
            transitions::liquidation::LiquidationPricing,
        },
        BorrowPosition, Debt, FutarchyAuthority, Market, MarketAsset, MarketHealth, ReferralFeeQuote, Risk,
    },
};

// Preview instructions are intended for simulated transactions. The market is
// writable because previews run the same update hook as user-facing actions
// before returning typed data; submitting one only refreshes market accounting.

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

    #[account(seeds = [FUTARCHY_AUTHORITY_SEED_PREFIX], bump = futarchy_authority.bump)]
    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,

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
    pub k_nad: u128,
    pub liquidity_nad: u128,
    pub health: MarketHealth,
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
    pub reserve_credit: u64,
    pub swap_fee_debit: u64,
    pub fee_credit: u64,
    pub amount_in_after_fee: u64,
    pub amount_out: u64,
    pub reserve_in_live_reserve: u64,
    pub reserve_out_live_reserve: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PreviewBorrowCapacityArgs {
    pub collateral_amount: u64,
    pub projected_borrow_amount: Option<u64>,
    pub with_referral: bool,
    pub max_acceptable_referral_fee_bps: u16,
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
    pub referral_origination_fee_bps: u16,
    pub borrow_market_health_floor_bps: u16,
    pub global_health_contribution_cap_bps: u16,
    pub projected_borrow_amount: u64,
    pub projected_referral_fee_debit: u64,
    pub projected_referral_vault_credit: u64,
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
            k_nad: market_k_nad(&market.base_side, &market.quote_side)?,
            liquidity_nad: market_liquidity_nad(&market.base_side, &market.quote_side)?,
            health: market.market_health()?,
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

        ctx.accounts.market.update()?;
        let market: &Market = &ctx.accounts.market;
        let asset_in = market.asset_for_mint(ctx.accounts.asset_in_mint.key())?;
        let asset_out = market.asset_for_mint(ctx.accounts.asset_out_mint.key())?;
        require!(asset_out == asset_in.opposite(), ErrorCode::InvalidMint);

        let transfer_fee = get_transfer_fee(&ctx.accounts.asset_in_mint.to_account_info(), args.exact_asset_in)?;
        let reserve_credit = args
            .exact_asset_in
            .checked_sub(transfer_fee)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let swap_fee_debit = ceil_div(
            (reserve_credit as u128)
                .checked_mul(market.config.swap_fee_bps as u128)
                .ok_or(ErrorCode::FeeMathOverflow)?,
            BPS_DENOMINATOR as u128,
        )
        .ok_or(ErrorCode::FeeMathOverflow)?
        .min(reserve_credit as u128) as u64;
        let fee_transfer_fee = get_transfer_fee(&ctx.accounts.asset_in_mint.to_account_info(), swap_fee_debit)?;
        let fee_credit = swap_fee_debit
            .checked_sub(fee_transfer_fee)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let amount_in_after_fee = reserve_credit
            .checked_sub(swap_fee_debit)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require!(amount_in_after_fee > 0, ErrorCode::InsufficientOutputAmount);

        let (market_side_in, market_side_out) = market.swap_sides(asset_in);
        let amount_out = calculate_raw_amount_out(
            market_side_in.reserves.live_reserve,
            market_side_out.reserves.live_reserve,
            amount_in_after_fee,
        )?;
        Ok(SwapPreview {
            asset_in,
            asset_out,
            exact_asset_in: args.exact_asset_in,
            transfer_fee,
            reserve_credit,
            swap_fee_debit,
            fee_credit,
            amount_in_after_fee,
            amount_out,
            reserve_in_live_reserve: market_side_in
                .reserves
                .live_reserve
                .checked_add(amount_in_after_fee)
                .ok_or(ErrorCode::ReserveOverflow)?,
            reserve_out_live_reserve: market_side_out
                .reserves
                .live_reserve
                .checked_sub(amount_out)
                .ok_or(ErrorCode::ReserveUnderflow)?,
        })
    }
}

impl<'info> PreviewBorrowCapacity<'info> {
    pub fn handle_preview(ctx: Context<Self>, args: PreviewBorrowCapacityArgs) -> Result<BorrowCapacityPreview> {
        require!(args.collateral_amount > 0, ErrorCode::AmountZero);
        require_supported_asset_mint(&ctx.accounts.collateral_asset_mint)?;
        require_supported_asset_mint(&ctx.accounts.debt_asset_mint)?;
        ctx.accounts.futarchy_authority.validate_referral_origination_fee()?;

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
        let referral_origination_fee_bps = if args.with_referral {
            ctx.accounts.futarchy_authority.referral_origination_fee_bps
        } else {
            0
        };
        let max_borrow_amount = max_requested_principal_for_gross_debt(
            max_debt,
            referral_origination_fee_bps,
            args.max_acceptable_referral_fee_bps,
            args.with_referral,
        )?;
        let projected_borrow_amount = args.projected_borrow_amount.unwrap_or(max_borrow_amount);
        let referral_quote = ReferralFeeQuote::new(
            projected_borrow_amount,
            referral_origination_fee_bps,
            args.max_acceptable_referral_fee_bps,
            args.with_referral,
        )?;
        let projected_debt_amount = referral_quote.gross_debt;
        let projected_referral_transfer_fee = get_transfer_fee(
            &ctx.accounts.debt_asset_mint.to_account_info(),
            referral_quote.fee_debit,
        )?;
        let projected_referral_vault_credit = referral_quote
            .fee_debit
            .checked_sub(projected_referral_transfer_fee)
            .ok_or(ErrorCode::MarketMathOverflow)?;
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
            referral_origination_fee_bps,
            borrow_market_health_floor_bps: market.config.borrow_market_health_floor_bps,
            global_health_contribution_cap_bps: market.config.global_health_contribution_cap_bps,
            projected_borrow_amount,
            projected_referral_fee_debit: referral_quote.fee_debit,
            projected_referral_vault_credit,
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
    let opposite_side = market.side(asset.opposite());
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
        spot_price_nad: market_spot_price_nad(side, opposite_side)?,
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
        let terms = pessimistic_max_debt_nad(
            self.collateral_amount_nad,
            self.collateral_virtual_reserve_nad,
            self.debt_virtual_reserve_nad,
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

fn max_requested_principal_for_gross_debt(
    max_gross_debt: u64,
    configured_fee_bps: u16,
    max_acceptable_fee_bps: u16,
    referred: bool,
) -> Result<u64> {
    let mut low = 0_u64;
    let mut high = max_gross_debt;
    while low < high {
        let midpoint = low + (high - low) / 2 + 1;
        if ReferralFeeQuote::new(midpoint, configured_fee_bps, max_acceptable_fee_bps, referred)?.gross_debt
            <= max_gross_debt
        {
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
        market.liquidation_reference_price_nad(debt_asset)?
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
            k_ema: (1_000_000_u128 * NAD as u128).pow(2),
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
    fn referral_capacity_binary_search_returns_largest_net_principal() {
        let maximum = max_requested_principal_for_gross_debt(100_000, 10, 10, true).unwrap();
        let quote = ReferralFeeQuote::new(maximum, 10, 10, true).unwrap();
        assert!(quote.gross_debt <= 100_000);
        assert!(ReferralFeeQuote::new(maximum + 1, 10, 10, true).unwrap().gross_debt > 100_000);
        assert_eq!(
            max_requested_principal_for_gross_debt(100_000, 10, 0, false).unwrap(),
            100_000
        );
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
