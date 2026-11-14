use crate::transitions::amm::{geometric_mean_floor, split_claimable_fee_credit, SwapRequest};
use anchor_lang::prelude::*;
use anchor_spl::token_interface::Mint;

use super::{
    accounts::{require_supported_asset_mint, validate_lp_mint},
    leverage_collateral_fee, leverage_collateral_liquidation_fee, reconcile_live_hlp_supply,
    validate_hlp_authority_pdas,
};

use crate::{
    constants::*,
    errors::ErrorCode,
    state::{
        BorrowPosition, FlashAmmLiquidationQuote, FlashLiquidationQuote, FutarchyAuthority, LeveragePosition,
        LiquidationHealth, Market, MarketAsset,
    },
    token::{get_transfer_fee, get_transfer_fee_for_epoch, get_transfer_inverse_fee},
    transitions::{
        flash_liquidation::LiquidationPositionRef,
        liquidity::{
            current_hlp_curve_prices, current_hlp_entry_state_with_prices,
            prepare_concentrated_hlp_transition_at_current_state, HlpEntryDisposition, HlpFlashQuote,
        },
        MarketHealth,
    },
};

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
    /// Net deposit credit in the collateral vault. Capacity also accounts
    /// for the current fee when that collateral later exits the vault.
    pub collateral_amount: u64,
    pub projected_borrow_amount: Option<u64>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BorrowPositionCapacityKind {
    #[default]
    Borrow,
    Withdraw,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PreviewBorrowPositionCapacityArgs {
    pub capacity_kind: BorrowPositionCapacityKind,
    /// Net collateral credit (positive) or vault debit (negative), in raw mint units.
    pub collateral_change: i128,
    /// Additional debt to draw. None quotes the maximum additional draw.
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
        constraint = market.assert_current_version().is_ok() @ ErrorCode::InvalidVersion,
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
        constraint = market.assert_current_version().is_ok() @ ErrorCode::InvalidVersion,
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
        constraint = market.assert_current_version().is_ok() @ ErrorCode::InvalidVersion,
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
        constraint = market.assert_current_version().is_ok() @ ErrorCode::InvalidVersion,
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
        constraint = market.assert_current_version().is_ok() @ ErrorCode::InvalidVersion,
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
        constraint = market.assert_current_version().is_ok() @ ErrorCode::InvalidVersion,
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
            borrow_position.owner.as_ref(),
            borrow_position.position_id.as_ref(),
        ],
        bump = borrow_position.bump,
        constraint = borrow_position.market == market.key() @ ErrorCode::InvalidPositionMarket
    )]
    pub borrow_position: Box<Account<'info, BorrowPosition>>,

    /// Liquidation values each side's collateral after its mint's current
    /// transfer fee, so the preview reads both mints.
    #[account(address = market.base_side.asset_mint @ ErrorCode::InvalidMint)]
    pub base_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(address = market.quote_side.asset_mint @ ErrorCode::InvalidMint)]
    pub quote_mint: Box<InterfaceAccount<'info, Mint>>,
}

#[derive(Accounts)]
pub struct PreviewBorrowPositionCapacity<'info> {
    #[account(
        constraint = market.assert_current_version().is_ok() @ ErrorCode::InvalidVersion,
        seeds = [MARKET_V2_SEED_PREFIX, market.base_side.asset_mint.as_ref(),
            market.quote_side.asset_mint.as_ref(), market.params_hash.as_ref()],
        bump = market.bump
    )]
    pub market: Box<Account<'info, Market>>,
    #[account(
        seeds = [BORROW_POSITION_SEED_PREFIX, market.key().as_ref(), borrow_position.owner.as_ref(), borrow_position.position_id.as_ref()],
        bump = borrow_position.bump,
        constraint = borrow_position.market == market.key() @ ErrorCode::InvalidPositionMarket
    )]
    pub borrow_position: Box<Account<'info, BorrowPosition>>,
    pub collateral_asset_mint: Box<InterfaceAccount<'info, Mint>>,
    pub debt_asset_mint: Box<InterfaceAccount<'info, Mint>>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PreviewSide {
    pub live_reserve: u128,
    pub cash_reserve: u64,
    pub base_hlp_backing_inventory: u64,
    pub quote_hlp_backing_inventory: u64,
    pub srlp_supply: u64,
    pub srlp_exchange_rate_nad: u128,
    pub spot_price_nad: u64,
    pub price_ema_nad: u64,
    pub directional_price_ema_nad: u64,
    pub conservative_depth_nad: u128,
    pub borrow_index_nad: u128,
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
    pub srlp_amount: u64,
    pub srlp_supply: u64,
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
    /// Output reserve vault debit after Dusk trading fees, before the output mint transfer fee.
    pub amount_out: u64,
    /// Curve output before Dusk trading fees.
    pub gross_amount_out: u64,
    pub reserve_in_live_reserve: u128,
    pub reserve_out_live_reserve: u128,
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
    /// Concentrated range metadata. Zeroes mean the curve has no concentrated
    /// ranges: a CPMM curve, or an AMM that is not yet initialized.
    pub lower_range_price_nad: u64,
    pub upper_range_price_nad: u64,
    /// 0=lower tail, 1=lower shoulder, 2=inner band,
    /// 3=upper shoulder, 4=upper tail.
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
    /// Token-2022 fee withheld when the output vault transfers `amount_out`.
    pub output_transfer_fee: u64,
    /// Spendable output credited to the recipient; the value checked by `min_asset_out`.
    pub net_amount_out: u64,
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
    /// Maximum debt-token credit that the current liquidation policy permits.
    pub max_repay_amount: u64,
    pub liquidation_health: crate::state::LiquidationHealth,
    pub liquidation_rates: Option<crate::state::LiquidationRates>,
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

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct BorrowPositionCapacityPreview {
    pub owner: Pubkey,
    pub market: Pubkey,
    pub position_id: Pubkey,
    pub collateral_asset: MarketAsset,
    pub debt_asset: MarketAsset,
    pub current_collateral_amount: u64,
    pub collateral_amount: u64,
    pub existing_debt_amount: u128,
    pub max_borrow_amount: Option<u64>,
    pub max_withdraw_amount: Option<u64>,
    pub projected_borrow_amount: u64,
    pub projected_debt_amount: u128,
    pub collateral_value_nad: u128,
    pub max_cf_bps: u16,
    pub liquidation_cf_bps: u16,
    pub liquidation_debt_per_collateral_price_nad: u64,
    pub borrow_allowed: bool,
}

impl<'info> PreviewBorrowPositionCapacity<'info> {
    pub fn handle_preview(
        ctx: Context<Self>,
        args: PreviewBorrowPositionCapacityArgs,
    ) -> Result<BorrowPositionCapacityPreview> {
        require_supported_asset_mint(&ctx.accounts.collateral_asset_mint)?;
        require_supported_asset_mint(&ctx.accounts.debt_asset_mint)?;
        let market_key = ctx.accounts.market.key();
        // Both accounts are read-only: updates only affect this preview's
        // deserialized storage, even if someone submits the instruction.
        let market: &mut Market = &mut ctx.accounts.market;
        let position: &mut BorrowPosition = &mut ctx.accounts.borrow_position;
        let collateral_asset = market.asset_for_mint(ctx.accounts.collateral_asset_mint.key())?;
        let debt_asset = market.asset_for_mint(ctx.accounts.debt_asset_mint.key())?;
        require!(debt_asset == collateral_asset.opposite(), ErrorCode::InvalidMint);
        let borrow_capacity = args.capacity_kind == BorrowPositionCapacityKind::Borrow;
        require!(
            borrow_capacity || args.projected_borrow_amount.unwrap_or(0) == 0,
            ErrorCode::InvalidArgument
        );
        market.update()?;
        let current_collateral_amount = position.collateral(collateral_asset);
        let epoch = Clock::get()?.epoch;
        let change = u64::try_from(args.collateral_change.unsigned_abs()).map_err(|_| ErrorCode::MarketMathOverflow)?;
        if args.collateral_change > 0 {
            market.deposit_collateral_with_fee(
                position,
                collateral_asset,
                change,
                crate::instructions::leverage_collateral_fee(&ctx.accounts.collateral_asset_mint, epoch)?,
            )?;
        } else if args.collateral_change < 0 {
            market.withdraw_collateral_with_fee(
                position,
                collateral_asset,
                change,
                crate::instructions::leverage_collateral_admission_fee(&ctx.accounts.collateral_asset_mint, epoch)?,
                0,
            )?;
        }
        // Borrow and withdrawal capacity mirror execution, which admits
        // both at the worse of the effective and any scheduled fee.
        let fees = match collateral_asset {
            MarketAsset::Base => crate::instructions::lending_market_admission_fees(
                market,
                &ctx.accounts.collateral_asset_mint,
                &ctx.accounts.debt_asset_mint,
                epoch,
            )?,
            MarketAsset::Quote => crate::instructions::lending_market_admission_fees(
                market,
                &ctx.accounts.debt_asset_mint,
                &ctx.accounts.collateral_asset_mint,
                epoch,
            )?,
        };
        let quote = market.position_capacity_quote_with_market_fees(
            position,
            collateral_asset,
            args.projected_borrow_amount,
            borrow_capacity,
            Clock::get()?.slot,
            fees,
        )?;
        Ok(BorrowPositionCapacityPreview {
            owner: position.owner,
            market: market_key,
            position_id: position.position_id,
            collateral_asset,
            debt_asset,
            current_collateral_amount,
            collateral_amount: position.collateral(collateral_asset),
            existing_debt_amount: quote.existing_debt,
            max_borrow_amount: quote.max_borrow_amount,
            max_withdraw_amount: quote.max_withdraw_amount,
            projected_borrow_amount: quote.projected_borrow_amount,
            projected_debt_amount: quote.projected_debt_amount,
            collateral_value_nad: quote.collateral_value_nad,
            max_cf_bps: quote.max_cf_bps,
            liquidation_cf_bps: quote.liquidation_cf_bps,
            liquidation_debt_per_collateral_price_nad: quote.liquidation_price_nad,
            borrow_allowed: quote.borrow_allowed,
        })
    }
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
            liquidity_nad: geometric_mean_floor(
                market.normalize_amount(market.base_side.reserves.live_reserve, market.base_side.asset_decimals)?,
                market.normalize_amount(
                    market.quote_side.reserves.live_reserve,
                    market.quote_side.asset_decimals,
                )?,
            )?,
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

        ctx.accounts.market.update_for_liquidity_change()?;
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
        // Validate the same reserve/share transition and curve finalization as
        // execution. Only the clock update above may persist: hypothetical
        // deposits, LP supply, and first-liquidity authority stay on this copy.
        let mut projected = Box::new(market.clone());
        let receipt = projected.add_liquidity(max_base_reserve_credit, max_quote_reserve_credit)?;
        projected.finalize_amm_transition_and_observe_risk(Clock::get()?.slot)?;
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
            srlp_amount: receipt.srlp_amount,
            srlp_supply: receipt.srlp_supply,
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
        let asset_in = quote_market.asset_for_mint(ctx.accounts.asset_in_mint.key())?;
        let asset_out = quote_market.asset_for_mint(ctx.accounts.asset_out_mint.key())?;
        require!(asset_out == asset_in.opposite(), ErrorCode::InvalidMint);

        let transfer_fee = get_transfer_fee(&ctx.accounts.asset_in_mint.to_account_info(), args.exact_asset_in)?;
        let reserve_credit = args
            .exact_asset_in
            .checked_sub(transfer_fee)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let mut prepared = SwapRequest {
            current_slot: slot,
            current_unix_timestamp: clock.unix_timestamp,
            asset_in,
            reserve_credit,
            protocol_fee_bps: ctx.accounts.futarchy_authority.revenue_share.swap_bps,
        }
        .prepare(quote_market)?;
        let quote = prepared.quote;
        let output_transfer_fee = get_transfer_fee_for_epoch(
            &ctx.accounts.asset_out_mint.to_account_info(),
            quote.amount_out,
            clock.epoch,
        )?;
        let net_amount_out = quote
            .amount_out
            .checked_sub(output_transfer_fee)
            .ok_or(ErrorCode::MarketMathOverflow)?;
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
            output_transfer_fee,
            net_amount_out,
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
        let fees = match collateral_asset {
            MarketAsset::Base => crate::instructions::lending_market_admission_fees(
                market,
                &ctx.accounts.collateral_asset_mint,
                &ctx.accounts.debt_asset_mint,
                Clock::get()?.epoch,
            )?,
            MarketAsset::Quote => crate::instructions::lending_market_admission_fees(
                market,
                &ctx.accounts.debt_asset_mint,
                &ctx.accounts.collateral_asset_mint,
                Clock::get()?.epoch,
            )?,
        };
        let quote = market.borrow_capacity_quote_with_market_fees(
            collateral_asset,
            args.collateral_amount,
            fees,
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
        let clock = Clock::get()?;
        let epoch = clock.epoch;
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
            base_debt: preview_position_debt_side(
                market,
                borrow_position,
                MarketAsset::Base,
                crate::instructions::leverage_collateral_liquidation_fee(&ctx.accounts.quote_mint, epoch)?,
            )?,
            quote_debt: preview_position_debt_side(
                market,
                borrow_position,
                MarketAsset::Quote,
                crate::instructions::leverage_collateral_liquidation_fee(&ctx.accounts.base_mint, epoch)?,
            )?,
        })
    }
}

fn preview_side(market: &Market, asset: MarketAsset, slot: u64) -> Result<PreviewSide> {
    let side = market.side(asset);
    let price_ema_nad = match asset {
        MarketAsset::Base => market.risk.base_price_ema_nad,
        MarketAsset::Quote => market.risk.quote_price_ema_nad,
    };
    let directional_price_ema_nad = match asset {
        MarketAsset::Base => market.risk.directional_base_price_ema_nad,
        MarketAsset::Quote => market.risk.directional_quote_price_ema_nad,
    };
    if !market.amm.initialized
        && market.base_side.shares.srlp_supply == market.quote_side.shares.srlp_supply
        && market.base_side.shares.srlp_supply <= MIN_LIQUIDITY
    {
        return Ok(PreviewSide {
            live_reserve: side.reserves.live_reserve,
            cash_reserve: side.reserves.cash_reserve,
            srlp_supply: side.shares.srlp_supply,
            srlp_exchange_rate_nad: if side.shares.srlp_supply == 0 {
                0
            } else {
                side.srlp_exchange_rate_nad()?
            },
            borrow_index_nad: market.debt.borrow_index(asset),
            borrow_apr_nad: match asset {
                MarketAsset::Base => market.debt.base_borrow_apr_nad,
                MarketAsset::Quote => market.debt.quote_borrow_apr_nad,
            },
            price_ema_nad,
            directional_price_ema_nad,
            ..PreviewSide::default()
        });
    }
    let lending = market.lending_side_preview(asset, slot)?;
    let prices = market.side_prices_at(asset, market.current_base_price_nad()?)?;

    Ok(PreviewSide {
        live_reserve: side.reserves.live_reserve,
        cash_reserve: side.reserves.cash_reserve,
        base_hlp_backing_inventory: side.reserves.base_hlp_backing_inventory,
        quote_hlp_backing_inventory: side.reserves.quote_hlp_backing_inventory,
        srlp_supply: side.shares.srlp_supply,
        srlp_exchange_rate_nad: side.srlp_exchange_rate_nad()?,
        spot_price_nad: prices.spot_price_nad,
        price_ema_nad: prices.price_ema_nad,
        directional_price_ema_nad,
        conservative_depth_nad: lending.conservative_depth_nad,
        borrow_index_nad: lending.borrow_index_nad,
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
    collateral_fee: crate::transitions::LeverageCollateralFee,
) -> Result<PositionDebtSidePreview> {
    let quote = market.position_debt_side_quote(borrow_position, debt_asset, collateral_fee)?;

    let health = market.flash_liquidation_health(
        &crate::transitions::flash_liquidation::LiquidationPositionRef::Borrow(borrow_position),
        debt_asset,
        collateral_fee,
    )?;

    let eligible = health.eligible()?;
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
        is_liquidatable: eligible,
        max_repay_amount: quote.max_repay_amount,
        liquidation_health: health,
        liquidation_rates: if eligible { Some(health.rates()?) } else { None },
    })
}

// hLP admission previews

/// Every account is read-only. Admission checks and interest accrual affect
/// only deserialized memory, including when this instruction is submitted.
#[derive(Accounts)]
pub struct PreviewHlpDepositCapacity<'info> {
    #[account(constraint = market.assert_current_version().is_ok() @ ErrorCode::InvalidVersion)]
    pub market: Box<Account<'info, Market>>,
    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,
    pub base_mint: Box<InterfaceAccount<'info, Mint>>,
    pub quote_mint: Box<InterfaceAccount<'info, Mint>>,
    pub target_hlp_mint: Box<InterfaceAccount<'info, Mint>>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum HlpDepositStatus {
    Ready,
    NotStarted,
    ReduceOnly,
    NoLiquidity,
    RebalanceRequired,
    CashConstrained,
    Unhedgeable,
    NoFundingCapacity,
    NoNetDeposit,
    Frozen,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct HlpDepositCapacityPreview {
    pub market: Pubkey,
    pub target_asset: MarketAsset,
    pub hlp_mint: Pubkey,
    pub slot: u64,
    pub epoch: u64,
    pub status: HlpDepositStatus,
    /// Exact funding ceiling, NOT a promise that all smaller amounts execute.
    /// Actual deposits still enforce rounding, settlement, custody and slippage.
    /// Gross wallet debit, including the current epoch's transfer fee.
    pub funding_limit_gross: u64,
    /// Greatest net target reserve credit supported by opposite-asset funding.
    pub funding_limit_net: u64,
}

impl<'info> PreviewHlpDepositCapacity<'info> {
    pub fn handle_preview(ctx: Context<Self>) -> Result<HlpDepositCapacityPreview> {
        let accounts = ctx.accounts;
        let market_key = accounts.market.key();
        validate_hlp_authority_pdas(
            &accounts.market,
            market_key,
            &accounts.futarchy_authority,
            accounts.futarchy_authority.key(),
        )?;
        require_keys_eq!(
            accounts.base_mint.key(),
            accounts.market.base_side.asset_mint,
            ErrorCode::InvalidMint
        );
        require_keys_eq!(
            accounts.quote_mint.key(),
            accounts.market.quote_side.asset_mint,
            ErrorCode::InvalidMint
        );
        require_supported_asset_mint(&accounts.base_mint)?;
        require_supported_asset_mint(&accounts.quote_mint)?;
        let target = accounts.market.asset_for_hlp_mint(accounts.target_hlp_mint.key())?;
        let target_mint = match target {
            MarketAsset::Base => &accounts.base_mint,
            MarketAsset::Quote => &accounts.quote_mint,
        };
        validate_lp_mint(&accounts.target_hlp_mint, market_key, target_mint.decimals)?;
        let clock = Clock::get()?;
        let (mut status, mut funding_limit_net) = preview_hlp_funding_limit(
            &mut accounts.market,
            target,
            accounts.target_hlp_mint.supply,
            accounts.futarchy_authority.global_reduce_only,
            &clock,
        )?;
        let funding_limit_gross = if funding_limit_net == 0 {
            0
        } else {
            // Net credit is monotone, including capped and 100% Token-2022
            // transfer fees. Inverse-fee helpers find the *smallest* gross
            // debit for a net value; this finds the exact upper boundary.
            gross_funding_limit(funding_limit_net, |amount| {
                get_transfer_fee_for_epoch(&target_mint.to_account_info(), amount, clock.epoch)
            })?
        };
        if status == HlpDepositStatus::Ready && funding_limit_gross == 0 {
            status = HlpDepositStatus::NoNetDeposit;
            funding_limit_net = 0;
        }
        Ok(HlpDepositCapacityPreview {
            market: market_key,
            target_asset: target,
            hlp_mint: accounts.target_hlp_mint.key(),
            slot: clock.slot,
            epoch: clock.epoch,
            status,
            funding_limit_gross,
            funding_limit_net,
        })
    }
}

fn gross_funding_limit(limit: u64, fee: impl Fn(u64) -> Result<u64>) -> Result<u64> {
    let net = |gross: u64| -> Result<u64> {
        gross
            .checked_sub(fee(gross)?)
            .ok_or_else(|| error!(ErrorCode::MarketMathOverflow))
    };
    let mut low = 0_u64;
    let mut high = u64::MAX;
    while low < high {
        let mid = low + (high - low) / 2 + 1;
        if net(mid)? <= limit {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    Ok(if net(low)? == 0 { 0 } else { low })
}

fn preview_hlp_funding_limit(
    market: &mut Market,
    target: MarketAsset,
    live_supply: u64,
    global_reduce_only: bool,
    clock: &Clock,
) -> Result<(HlpDepositStatus, u64)> {
    market.assert_current_version()?;
    market.require_hlp_flash_idle(target)?;
    market.require_hlp_flash_idle(target.opposite())?;
    if clock.unix_timestamp < market.config.start_time {
        return Ok((HlpDepositStatus::NotStarted, 0));
    }
    if global_reduce_only || market.reduce_only {
        return Ok((HlpDepositStatus::ReduceOnly, 0));
    }
    market.accrue_interest_to_slot(clock.slot)?;
    reconcile_live_hlp_supply(market, target, live_supply, false)?;
    if market.base_side.reserves.live_reserve == 0 || market.quote_side.reserves.live_reserve == 0 {
        return Ok((HlpDepositStatus::NoLiquidity, 0));
    }
    // Entry applies a due, funded recenter first; preview the same state.
    market.prepare_amm_for_swap(clock.slot)?;
    market.advance_one_amm_controller_target(clock.slot)?;
    market.freeze_negative_hlp_vaults()?;
    if market.hlp_frozen(target) {
        return Ok((HlpDepositStatus::Frozen, 0));
    }
    if market.side(target.opposite()).reserves.cash_reserve == 0 {
        return Ok((HlpDepositStatus::NoFundingCapacity, 0));
    }
    // Entry settles the existing hedge atomically. Preview the same endpoint
    // before classifying readiness and calculating the remaining cash budget.
    prepare_concentrated_hlp_transition_at_current_state(market)?.consume(market)?;
    market.checkpoint_hlp_vaults()?;
    let prices = current_hlp_curve_prices(market)?;
    let entry = current_hlp_entry_state_with_prices(market, target, prices)?;
    let status = match entry.disposition {
        HlpEntryDisposition::Settled | HlpEntryDisposition::ControllerGranularityLimited => HlpDepositStatus::Ready,
        HlpEntryDisposition::Actionable => HlpDepositStatus::RebalanceRequired,
        HlpEntryDisposition::CashConstrained => HlpDepositStatus::CashConstrained,
        HlpEntryDisposition::Unhedgeable => HlpDepositStatus::Unhedgeable,
    };
    if status != HlpDepositStatus::Ready {
        return Ok((status, 0));
    }
    market.observe_current_risk(clock.slot)?;
    let target_reserve = market.side(target).reserves.live_reserve;
    let opposite_reserve = market.side(target.opposite()).reserves.live_reserve;
    if target_reserve == 0 || opposite_reserve == 0 {
        return Ok((HlpDepositStatus::NoLiquidity, 0));
    }
    let headroom = market.hlp_funding_headroom(target.opposite())?;
    // floor(deposit * opposite / target) <= headroom. Compare ratios first:
    // wider virtual reserves can overflow a u128 numerator even when the
    // final raw-token limit fits u64.
    let full_domain_fits = !crate::math::ratio_lte_full_width(
        u128::from(headroom) + 1,
        opposite_reserve,
        u64::MAX as u128,
        target_reserve,
    )?;
    let limit = if full_domain_fits {
        u64::MAX
    } else {
        let ceiling = crate::math::mul_div_ceil_u128(u128::from(headroom) + 1, target_reserve, opposite_reserve)?;
        u64::try_from(ceiling.saturating_sub(1)).map_err(|_| ErrorCode::MarketMathOverflow)?
    };
    if headroom == 0 || crate::math::mul_div_u128(u128::from(limit), opposite_reserve, target_reserve)? == 0 {
        return Ok((HlpDepositStatus::NoFundingCapacity, 0));
    }
    Ok((HlpDepositStatus::Ready, limit))
}

// Atomic liquidation previews

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct PreviewFlashLiquidationArgs {
    pub debt_asset: u8,
    pub max_repayment: u64,
    pub full: bool,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct PreviewFlashAmmLiquidationArgs {
    pub debt_asset: u8,
    pub max_repayment: u64,
    pub full: bool,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct PreviewEmergencyLiquidationArgs {
    pub debt_asset: u8,
    pub collateral_debit: u64,
    pub full: bool,
}

#[derive(Accounts)]
pub struct PreviewFlashLiquidation<'info> {
    #[account(constraint = market.assert_current_version().is_ok() @ ErrorCode::InvalidVersion, seeds = [MARKET_V2_SEED_PREFIX, market.base_side.asset_mint.as_ref(), market.quote_side.asset_mint.as_ref(), market.params_hash.as_ref()], bump = market.bump)]
    pub market: Box<Account<'info, Market>>,
    pub borrow_position: Option<Box<Account<'info, BorrowPosition>>>,
    pub leverage_position: Option<Box<Account<'info, LeveragePosition>>>,
    pub collateral_mint: Box<InterfaceAccount<'info, Mint>>,
    pub debt_mint: Box<InterfaceAccount<'info, Mint>>,
}

#[derive(Accounts)]
pub struct PreviewEmergencyLiquidation<'info> {
    pub preview: PreviewFlashLiquidation<'info>,
    #[account(seeds = [FUTARCHY_AUTHORITY_SEED_PREFIX], bump = futarchy_authority.bump)]
    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,
}

fn validate_position<'a>(
    market: &Account<Market>,
    borrow: &'a Option<Box<Account<BorrowPosition>>>,
    leverage: &'a Option<Box<Account<LeveragePosition>>>,
    asset: MarketAsset,
    collateral_mint: &InterfaceAccount<Mint>,
    debt_mint: &InterfaceAccount<Mint>,
) -> Result<LiquidationPositionRef<'a>> {
    market.assert_started()?;
    require_supported_asset_mint(collateral_mint)?;
    require_supported_asset_mint(debt_mint)?;
    require_keys_eq!(debt_mint.key(), market.side(asset).asset_mint, ErrorCode::InvalidMint);
    require_keys_eq!(
        collateral_mint.key(),
        market.side(asset.opposite()).asset_mint,
        ErrorCode::InvalidMint
    );
    let result = match (borrow, leverage) {
        (Some(p), None) => {
            require_keys_eq!(p.market, market.key(), ErrorCode::InvalidBorrowPosition);
            let key = Pubkey::create_program_address(
                &[
                    BORROW_POSITION_SEED_PREFIX,
                    p.market.as_ref(),
                    p.owner.as_ref(),
                    p.position_id.as_ref(),
                    &[p.bump],
                ],
                &crate::ID,
            )
            .map_err(|_| ErrorCode::InvalidBorrowPosition)?;
            require_keys_eq!(key, p.key(), ErrorCode::InvalidBorrowPosition);
            LiquidationPositionRef::Borrow(p)
        }
        (None, Some(p)) => {
            require_keys_eq!(p.market, market.key(), ErrorCode::InvalidLeveragePosition);
            require!(p.debt_asset()? == asset, ErrorCode::InvalidLeveragePosition);
            let key = Pubkey::create_program_address(
                &[
                    LEVERAGE_POSITION_SEED_PREFIX,
                    p.market.as_ref(),
                    p.owner.as_ref(),
                    p.namespace_authority.as_ref(),
                    p.position_id.as_ref(),
                    &[p.bump],
                ],
                &crate::ID,
            )
            .map_err(|_| ErrorCode::InvalidLeveragePosition)?;
            require_keys_eq!(key, p.key(), ErrorCode::InvalidLeveragePosition);
            LiquidationPositionRef::Leverage(p)
        }
        _ => return err!(ErrorCode::InvalidLiquidationSession),
    };
    result.require_idle()?;
    Ok(result)
}

impl PreviewEmergencyLiquidation<'_> {
    pub fn handle_flash_amm(
        ctx: Context<Self>,
        args: PreviewFlashAmmLiquidationArgs,
    ) -> Result<FlashAmmLiquidationQuote> {
        let clock = Clock::get()?;
        let a = &mut ctx.accounts.preview;
        let asset = MarketAsset::try_from_code(args.debt_asset)?;
        a.market.accrue_interest()?;
        a.market.update()?;
        let position = validate_position(
            &a.market,
            &a.borrow_position,
            &a.leverage_position,
            asset,
            &a.collateral_mint,
            &a.debt_mint,
        )?;
        let collateral_fee = leverage_collateral_fee(&a.collateral_mint, clock.epoch)?;
        let debt_fee = leverage_collateral_fee(&a.debt_mint, clock.epoch)?;
        let quote = a.market.quote_flash_liquidation(
            &position,
            asset,
            args.max_repayment,
            args.full,
            collateral_fee,
            leverage_collateral_liquidation_fee(&a.collateral_mint, clock.epoch)?,
            debt_fee,
            clock.unix_timestamp,
            clock.slot,
        )?;
        a.market.quote_flash_amm_liquidation(
            &position,
            asset,
            quote,
            collateral_fee,
            debt_fee,
            clock.unix_timestamp,
            clock.slot,
            ctx.accounts.futarchy_authority.revenue_share.swap_bps,
        )
    }

    pub fn handle(
        ctx: Context<Self>,
        args: PreviewEmergencyLiquidationArgs,
    ) -> Result<crate::transitions::amm_liquidation::EmergencyLiquidationQuote> {
        let clock = Clock::get()?;
        let a = &mut ctx.accounts.preview;
        let asset = MarketAsset::try_from_code(args.debt_asset)?;
        a.market.accrue_interest()?;
        a.market.update()?;
        validate_position(
            &a.market,
            &a.borrow_position,
            &a.leverage_position,
            asset,
            &a.collateral_mint,
            &a.debt_mint,
        )?;
        let position = match (&a.borrow_position, &a.leverage_position) {
            (Some(p), None) => LiquidationPositionRef::Borrow(p),
            (None, Some(p)) => LiquidationPositionRef::Leverage(p),
            _ => return err!(ErrorCode::InvalidLiquidationSession),
        };
        Ok(a.market
            .prepare_emergency_liquidation(
                &position,
                asset,
                args.collateral_debit,
                args.full,
                leverage_collateral_fee(&a.collateral_mint, clock.epoch)?,
                leverage_collateral_liquidation_fee(&a.collateral_mint, clock.epoch)?,
                leverage_collateral_fee(&a.debt_mint, clock.epoch)?,
                clock.unix_timestamp,
                clock.slot,
                ctx.accounts.futarchy_authority.revenue_share.swap_bps,
            )?
            .quote)
    }
}

impl PreviewFlashLiquidation<'_> {
    pub fn handle_health(ctx: Context<Self>, debt_asset: u8) -> Result<LiquidationHealth> {
        let clock = Clock::get()?;
        let a = &mut *ctx.accounts;
        let asset = MarketAsset::try_from_code(debt_asset)?;
        a.market.accrue_interest()?;
        a.market.update()?;
        let position = validate_position(
            &a.market,
            &a.borrow_position,
            &a.leverage_position,
            asset,
            &a.collateral_mint,
            &a.debt_mint,
        )?;
        a.market.flash_liquidation_health(
            &position,
            asset,
            leverage_collateral_liquidation_fee(&a.collateral_mint, clock.epoch)?,
        )
    }

    pub fn handle(ctx: Context<Self>, args: PreviewFlashLiquidationArgs) -> Result<FlashLiquidationQuote> {
        let clock = Clock::get()?;
        let a = &mut *ctx.accounts;
        let asset = MarketAsset::try_from_code(args.debt_asset)?;
        // Risk and interest refresh only this disposable read-only snapshot.
        a.market.accrue_interest()?;
        a.market.update()?;
        let position = validate_position(
            &a.market,
            &a.borrow_position,
            &a.leverage_position,
            asset,
            &a.collateral_mint,
            &a.debt_mint,
        )?;
        a.market.quote_flash_liquidation(
            &position,
            asset,
            args.max_repayment,
            args.full,
            leverage_collateral_fee(&a.collateral_mint, clock.epoch)?,
            leverage_collateral_liquidation_fee(&a.collateral_mint, clock.epoch)?,
            leverage_collateral_fee(&a.debt_mint, clock.epoch)?,
            clock.unix_timestamp,
            clock.slot,
        )
    }
}

// Atomic hLP recovery preview

#[derive(Accounts)]
pub struct PreviewHlpFlash<'info> {
    #[account(constraint = market.assert_current_version().is_ok() @ ErrorCode::InvalidVersion)]
    pub market: Box<Account<'info, Market>>,
    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,
    pub target_mint: Box<InterfaceAccount<'info, Mint>>,
    pub borrowed_mint: Box<InterfaceAccount<'info, Mint>>,
}
impl PreviewHlpFlash<'_> {
    pub fn handle(ctx: Context<Self>, target_asset: u8) -> Result<HlpFlashQuote> {
        let a = ctx.accounts;
        let target = MarketAsset::try_from_code(target_asset)?;
        validate_hlp_authority_pdas(
            &a.market,
            a.market.key(),
            &a.futarchy_authority,
            a.futarchy_authority.key(),
        )?;
        a.market.assert_current_version()?;
        a.market.assert_started()?;
        require_keys_eq!(
            a.target_mint.key(),
            a.market.side(target).asset_mint,
            ErrorCode::InvalidMint
        );
        require_keys_eq!(
            a.borrowed_mint.key(),
            a.market.side(target.opposite()).asset_mint,
            ErrorCode::InvalidMint
        );
        require_supported_asset_mint(&a.target_mint)?;
        require_supported_asset_mint(&a.borrowed_mint)?;
        let clock = Clock::get()?;
        a.market.accrue_interest_to_slot(clock.slot)?;
        a.market.advance_amm_clock(clock.slot)?;
        a.market.quote_hlp_flash_recovery(
            target,
            leverage_collateral_fee(&a.target_mint, clock.epoch)?,
            leverage_collateral_fee(&a.borrowed_mint, clock.epoch)?,
        )
    }
}
#[cfg(test)]
mod tests {
    include!("../tests/instructions/preview.rs");
}
