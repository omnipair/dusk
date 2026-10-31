use anchor_lang::prelude::*;

use crate::{constants::*, errors::ErrorCode};

/// Maximum center amplification admitted by governance. The concentrated curve
/// enforces this through the nonzero full-range tail share.
pub const MAX_AMM_AMPLIFICATION_NAD: u64 = 2_000 * NAD;
pub const MIN_AMM_ADJUSTMENT_NAD: u64 = NAD / 1_000_000;
pub const MAX_AMM_ADJUSTMENT_NAD: u64 = NAD / 10;
pub const MAX_AMM_VOLATILITY_NAD: u64 = 10 * NAD;
/// Governance/arithmetic bound on signal sensitivity. Separate fee-share caps
/// bound the Huberized divergence marginal and the volatility debit.
pub const MAX_AMM_FEE_COEFFICIENT_NAD: u64 = 100 * NAD;
/// One nominal day at the protocol's 200 ms scheduled slot cadence.
pub const MAX_AMM_ADJUSTMENT_INTERVAL_SLOTS: u64 = MS_PER_DAY / TARGET_MS_PER_SLOT;
/// Fixed configuration extension room retained for future typed AMM controls.
///
/// Keep this wire-carried reserve compact: the complete `MarketConfig` is also
/// an initialize/update instruction argument, and 64 bytes made the
/// initialize transaction exceed Solana's 1,232-byte limit. Layout v1 keeps
/// this wire reserve for future configuration fields. Launch-fee configuration
/// consumes eleven bytes from the original 33-byte reserve and the launch
/// buy-size limiter consumes another twenty-one. The remaining byte is used
/// by the fee-denomination mode.
pub const AMM_CONFIG_RESERVED_BYTES: usize = 0;
pub const SWAP_FEE_COLLECT_INPUT_ASSET: u8 = 0;
pub const SWAP_FEE_COLLECT_BASE_ONLY: u8 = 1;
pub const SWAP_FEE_COLLECT_QUOTE_ONLY: u8 = 2;
pub const LAUNCH_FEE_DECAY_DISABLED: u8 = 0;
pub const LAUNCH_FEE_DECAY_LINEAR: u8 = 1;
pub const LAUNCH_FEE_DECAY_EXPONENTIAL: u8 = 2;
pub const LAUNCH_FEE_EXPONENTIAL_PERIODS: u64 = 16;
pub const MAX_LAUNCH_FEE_DURATION_SECONDS: u64 = 30 * 24 * 60 * 60;
pub const LAUNCH_RATE_LIMIT_ASSET_DISABLED: u8 = 0;
pub const LAUNCH_RATE_LIMIT_ASSET_BASE: u8 = 1;
pub const LAUNCH_RATE_LIMIT_ASSET_QUOTE: u8 = 2;
/// One serialized byte records that retained principal changed the exact
/// reserves after the last forward-target solve.
pub const AMM_RETENTION_TARGET_STALE_BYTES: usize = core::mem::size_of::<bool>();
/// A controller target is frozen when its full scheduled move is not yet
/// funded. Real swap-like operations retry this exact target; there is no
/// auxiliary instruction or keeper dependency.
pub const AMM_DEFERRED_CONTROLLER_TARGET_BYTES: usize = core::mem::size_of::<u8>()
    + 2 * core::mem::size_of::<u64>()
    + 4 * core::mem::size_of::<u128>()
    + core::mem::size_of::<bool>();
/// Layout v2 also binds launch graduation price/progress and the one-shot
/// initial-liquidity authority alongside the concentrated-curve state.
/// Pessimistic lending shapes are intentionally reconstructed only by
/// risk-sensitive operations instead of being persisted in every market.
/// The account-only expansion reserve is fully allocated to keep Anchor's
/// generated SBF deserializer inside Solana's 4 KiB stack frame.
/// Future account or configuration fields require another explicit layout
/// revision; all previously reserved bytes are now allocated.
pub const AMM_STATE_RESERVED_BYTES: usize = 0;

/// Protocol constants for the retained-surcharge safety budget.
pub const PROTECTED_LIQUIDITY_COVERAGE_BPS: u16 = 12_500;
pub const PROTECTED_LIQUIDITY_GUARD_BPS: u16 = 1;
pub const PROTECTED_LIQUIDITY_CAP_BPS: u16 = 100;
pub const PROTECTED_LIQUIDITY_HYSTERESIS_BPS: u16 = 1_000;

/// AMM controls. One-times peak amplification with zero widths selects the
/// full-range CPMM branch of the same concentrated implementation.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, InitSpace, PartialEq, Eq)]
pub struct AmmConfig {
    pub peak_amplification_nad: u64,
    pub core_half_width_bps: u16,
    pub fade_width_bps: u16,
    pub center_ema_half_life_ms: u64,
    pub volatility_half_life_ms: u64,
    pub adjustment_threshold_nad: u64,
    pub adjustment_step_nad: u64,
    pub min_adjustment_interval_slots: u64,
    pub volatility_shock_cap_nad: u64,
    pub volatility_cap_nad: u64,
    pub divergence_fee_coefficient_nad: u64,
    pub volatility_fee_coefficient_nad: u64,
    /// Asset in which swap, toxicity, volatility, and retained-recenter fees
    /// are denominated. Lending and hLP funding interest remain in the
    /// borrowed asset and are not affected by this setting.
    pub swap_fee_collect_mode: u8,
    /// Share of the LP-owned swap fee which becomes ordinary reserve
    /// principal instead of a claimable fee liability. Zero disables native
    /// compounding; `BPS_DENOMINATOR` compounds the complete LP share.
    pub compounding_fee_bps: u16,
    /// Optional launch-only base fee. The premium above `swap_fee_bps`
    /// decays from `start_time` and is zero after the configured duration.
    pub launch_fee_start_bps: u16,
    pub launch_fee_duration_seconds: u64,
    pub launch_fee_decay_mode: u8,
    /// When all three values are zero, the launch fee follows the time
    /// schedule above. A fully nonzero tuple selects a price-milestone
    /// scheduler whose reference price is bound by the first liquidity seed.
    pub launch_market_price_step_bps: u16,
    pub launch_market_number_of_periods: u16,
    pub launch_market_reduction_factor_bps: u16,
    /// Optional launch buy-size limiter. The configured asset is the asset
    /// being bought, not the input asset. Each full/partial reference amount
    /// after the first adds `launch_rate_limit_increment_bps`, capped by
    /// `launch_rate_limit_max_fee_bps`.
    pub launch_rate_limit_asset: u8,
    pub launch_rate_limit_reference_nad: u64,
    pub launch_rate_limit_increment_bps: u16,
    pub launch_rate_limit_max_fee_bps: u16,
    pub launch_rate_limit_duration_seconds: u64,
    pub reserved: [u8; AMM_CONFIG_RESERVED_BYTES],
}

impl Default for AmmConfig {
    fn default() -> Self {
        Self {
            peak_amplification_nad: NAD,
            core_half_width_bps: 0,
            fade_width_bps: 0,
            center_ema_half_life_ms: MIN_HALF_LIFE_MS,
            volatility_half_life_ms: MIN_HALF_LIFE_MS,
            adjustment_threshold_nad: 0,
            adjustment_step_nad: 0,
            min_adjustment_interval_slots: 0,
            volatility_shock_cap_nad: 0,
            volatility_cap_nad: 0,
            divergence_fee_coefficient_nad: 0,
            volatility_fee_coefficient_nad: 0,
            swap_fee_collect_mode: SWAP_FEE_COLLECT_INPUT_ASSET,
            compounding_fee_bps: 0,
            launch_fee_start_bps: 0,
            launch_fee_duration_seconds: 0,
            launch_fee_decay_mode: LAUNCH_FEE_DECAY_DISABLED,
            launch_market_price_step_bps: 0,
            launch_market_number_of_periods: 0,
            launch_market_reduction_factor_bps: 0,
            launch_rate_limit_asset: LAUNCH_RATE_LIMIT_ASSET_DISABLED,
            launch_rate_limit_reference_nad: 0,
            launch_rate_limit_increment_bps: 0,
            launch_rate_limit_max_fee_bps: 0,
            launch_rate_limit_duration_seconds: 0,
            reserved: [0; AMM_CONFIG_RESERVED_BYTES],
        }
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct DeferredControllerTarget {
    /// 0 = none, 2 = center move.
    pub kind: u8,
    pub center_price_nad: u64,
    pub required_nad: u128,
    pub evaluated_base_reserve_nad: u128,
    pub evaluated_quote_reserve_nad: u128,
    pub created_slot: u64,
    pub saturated: bool,
}

// Serialized concentrated-curve cache embedded in the Market PDA.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct ConcentratedCurveCache {
    pub math_revision: u8,
    pub peak_amplification_nad: u64,
    pub core_half_width_bps: u16,
    pub fade_width_bps: u16,
    pub tail_liquidity: u128,
    pub concentrated_liquidity: u128,
    pub core_lower_sqrt_price_nad: u128,
    pub core_upper_sqrt_price_nad: u128,
    pub outer_lower_sqrt_price_nad: u128,
    pub outer_upper_sqrt_price_nad: u128,
}

/// Embedded mutable state for the concentrated curve, internal signals, and
/// protected recenter liquidity.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, InitSpace, PartialEq, Eq)]
pub struct AmmState {
    pub initialized: bool,
    /// Concentrated CPMM-tail/band geometry. CPMM is represented by zero
    /// concentrated liquidity in this same cache.
    pub concentrated_curve_cache: ConcentratedCurveCache,
    pub center_price_nad: u64,
    pub price_ema_nad: u64,
    pub last_trade_price_nad: u64,
    pub last_observation_slot: u64,
    pub last_adjustment_slot: u64,
    /// Immutable launch price reference bound by the first fully-backed
    /// liquidity seed. Zero is allowed only before that seed.
    pub launch_reference_price_nad: u64,
    /// Fee-schedule progress already completed by a bootstrap adapter before
    /// the market graduates into GAMM.
    pub launch_fee_progress_offset: u16,
    pub volatility_accumulator_nad: u64,
    pub curve_depth_per_share_nad: u128,
    /// yLP principal floor protected from funded recenter/ramp impairment.
    pub protected_floor_per_share_nad: u128,
    /// Fresh protected-profit target that arms retained surcharge routing.
    /// This is a principal-budget target, never a cap on trader fees.
    pub retention_required_nad: u128,
    /// Hysteresis threshold below which retention remains armed.
    pub retention_stop_nad: u128,
    /// Maximum protected principal one controller target may request/spend.
    /// It does not clip divergence or volatility surcharge amounts.
    pub retention_hard_cap_nad: u128,
    /// When true, dynamic surcharge is locked in the non-quoteable protected
    /// recenter bucket; when false, the identical trader charge is routed to
    /// claimable yLP fee accounting.
    pub retain_dynamic_surcharge: bool,
    /// The requested protection target exceeded its principal-budget cap.
    pub retention_target_saturated: bool,
    /// The protected bucket changed after the last exact forward-target solve.
    /// While stale, retention stays on until a decision point refreshes the
    /// target or executes a funded recenter.
    pub retention_target_stale: bool,
    /// Exact unfunded controller target retried by later real operations.
    pub deferred_controller_target: DeferredControllerTarget,
    /// A distressed hLP cannot be reconstructed as positive one-sided equity.
    /// Each bit freezes only that vault's share/debt hedge during swaps.
    pub frozen_hlp_mask: u8,
    /// Fixed virtual-coordinate adjustment captured when the vault freezes.
    /// It preserves the pre-freeze executable price without treating the
    /// vault's negative equity as spendable token inventory.
    pub frozen_base_vault_base_offset_nad: i128,
    pub frozen_base_vault_quote_offset_nad: i128,
    pub frozen_quote_vault_base_offset_nad: i128,
    pub frozen_quote_vault_quote_offset_nad: i128,
    pub _reserved: [u8; AMM_STATE_RESERVED_BYTES],
}

impl Default for AmmState {
    fn default() -> Self {
        Self {
            initialized: false,
            concentrated_curve_cache: ConcentratedCurveCache::default(),
            center_price_nad: 0,
            price_ema_nad: 0,
            last_trade_price_nad: 0,
            last_observation_slot: 0,
            last_adjustment_slot: 0,
            launch_reference_price_nad: 0,
            launch_fee_progress_offset: 0,
            volatility_accumulator_nad: 0,
            curve_depth_per_share_nad: 0,
            protected_floor_per_share_nad: 0,
            retention_required_nad: 0,
            retention_stop_nad: 0,
            retention_hard_cap_nad: 0,
            retain_dynamic_surcharge: false,
            retention_target_saturated: false,
            retention_target_stale: false,
            deferred_controller_target: DeferredControllerTarget::default(),
            frozen_hlp_mask: 0,
            frozen_base_vault_base_offset_nad: 0,
            frozen_base_vault_quote_offset_nad: 0,
            frozen_quote_vault_base_offset_nad: 0,
            frozen_quote_vault_quote_offset_nad: 0,
            _reserved: [0; AMM_STATE_RESERVED_BYTES],
        }
    }
}

pub const DEFAULT_DAILY_BORROW_BPS: u16 = 2_000;
pub const MAX_DAILY_BORROW_BPS: u16 = 3_000;
pub const MIN_IRM_TARGET_UTILIZATION_BPS: u16 = 6_000;
pub const MAX_IRM_TARGET_UTILIZATION_BPS: u16 = 7_500;
pub const DEFAULT_IRM_TARGET_UTILIZATION_BPS: u16 = 7_000;
pub const MIN_IRM_CURVE_STEEPNESS_NAD: u64 = 2 * NAD;
pub const MAX_IRM_CURVE_STEEPNESS_NAD: u64 = 8 * NAD;
pub const DEFAULT_IRM_CURVE_STEEPNESS_NAD: u64 = 4 * NAD;
pub const MIN_IRM_ADJUSTMENT_SPEED_PER_YEAR: u64 = 1;
pub const MAX_IRM_ADJUSTMENT_SPEED_PER_YEAR: u64 = 50;
pub const DEFAULT_IRM_ADJUSTMENT_SPEED_PER_YEAR: u64 = 20;

/// Complete mutable fee surface. The fields remain embedded in their existing
/// `MarketConfig`/`AmmConfig` locations so this view can be used by typed
/// governance without duplicating fee state.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct FeeProfile {
    pub base_fee_bps: u16,
    pub divergence_fee_share_cap_bps: u16,
    pub volatility_fee_share_cap_bps: u16,
    pub divergence_fee_coefficient_nad: u64,
    pub volatility_fee_coefficient_nad: u64,
    pub volatility_half_life_ms: u64,
    pub volatility_shock_cap_nad: u64,
    pub volatility_accumulator_cap_nad: u64,
    pub swap_fee_collect_mode: u8,
    pub compounding_fee_bps: u16,
    pub launch_fee_start_bps: u16,
    pub launch_fee_duration_seconds: u64,
    pub launch_fee_decay_mode: u8,
    pub launch_market_price_step_bps: u16,
    pub launch_market_number_of_periods: u16,
    pub launch_market_reduction_factor_bps: u16,
    pub launch_rate_limit_asset: u8,
    pub launch_rate_limit_reference_nad: u64,
    pub launch_rate_limit_increment_bps: u16,
    pub launch_rate_limit_max_fee_bps: u16,
    pub launch_rate_limit_duration_seconds: u64,
}

pub use crate::transitions::tranches::RiskRateConfig as IrmConfig;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct MarketConfig {
    pub swap_fee_bps: u16,
    pub divergence_fee_share_cap_bps: u16,
    pub volatility_fee_share_cap_bps: u16,
    pub target_hlp_leverage_bps: u16,
    pub settlement_divergence_bps: u16,
    pub ema_half_life_ms: u64,
    pub directional_ema_half_life_ms: u64,
    pub curve_depth_ema_half_life_ms: u64,
    pub max_daily_borrow_bps: u16,
    pub global_health_contribution_cap_bps: u16,
    pub borrow_market_health_floor_bps: u16,
    pub amm: AmmConfig,
    pub irm: IrmConfig,
    pub start_time: i64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace)]
pub struct Debt {
    pub fixed_base_shares: u128,
    pub fixed_quote_shares: u128,
    pub base_borrow_index_nad: u128,
    pub quote_borrow_index_nad: u128,
    /// Fractional index growth carried across checkpoints. Denominator is
    /// `NAD * MS_PER_YEAR`, so frequent checkpoints cannot discard interest.
    pub base_borrow_index_remainder: u128,
    pub quote_borrow_index_remainder: u128,
    pub base_rate_at_target_nad: u128,
    pub quote_rate_at_target_nad: u128,
    /// Fractional NAD APR carried by the exponential controller (scale 1e9).
    pub base_rate_fraction_e18: u64,
    pub quote_rate_fraction_e18: u64,
    pub global_health_base_contribution_for_quote_debt: u64,
    pub global_health_quote_contribution_for_base_debt: u64,
    pub base_last_accrual_slot: u64,
    pub quote_last_accrual_slot: u64,
    // Debt tracking (r_debt)
    /// Aggregate outstanding *principal* (borrowed token amount, excluding
    /// accrued interest) backing fixed margin debt on each side. Accrued
    /// interest is `fixed_*_debt - fixed_*_principal`; tracked so interest can
    /// be routed to the interest vault (non-compounding) instead of
    /// compounding into reserves. Principal is a raw token-atom balance and is
    /// therefore bounded by the corresponding `u64` reserve custody domain.
    pub fixed_base_principal: u64,
    pub fixed_quote_principal: u64,
    /// Aggregate isolated leverage debt. This debt contributes to utilization
    /// and interest, but is intentionally not utilized as normal margin debt.
    /// Shares remain `u128`; raw principal remains in the token account's
    /// `u64` amount domain.
    pub isolated_base_shares: u128,
    pub isolated_quote_shares: u128,
    pub isolated_base_principal: u64,
    pub isolated_quote_principal: u64,
    /// Gross collateral atoms in debt-bearing leverage positions. Counts all
    /// owners and namespaces; custody donations cannot create exposure credit.
    pub leverage_base_collateral: u64,
    pub leverage_quote_collateral: u64,
}

/// Reserve-cash funding of public borrower interest. The protocol portion is
/// held in the reserve vault until repayment, so cancellation can return it
/// to executable cash without a transfer or a second transfer fee.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct PublicInterestAdvance {
    pub outstanding_gross: u64,
    pub protocol_escrow: u64,
    /// Native repayment has already restored this cash; its token handler
    /// must exclude it from the new interest-vault transfer.
    pub repayment_credit_pending: u64,
    /// Frozen protocol allocation released by repayment, still held in the
    /// reserve vault until the same instruction completes token settlement.
    pub protocol_release_pending: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace)]
pub struct Fees {
    pub junior: crate::transitions::tranches::TrancheIncome,
    pub public_interest_advance: PublicInterestAdvance,
    pub swap_fee_growth_index_q64: u128,
    pub interest_growth_index_q64: u128,
    /// Scaled fee entitlement not yet representable by the integer growth
    /// index. The corresponding whole-token backing already sits in
    /// `swap_fee_liability`; it must never be redistributed as unallocated
    /// revenue.
    pub swap_fee_growth_remainder_scaled: u64,
    /// Interest counterpart of `swap_fee_growth_remainder_scaled`.
    pub interest_growth_remainder_scaled: u64,
    /// Source-scoped Q64 carry for interest paid by hLP funding debt. Funding
    /// uses a non-hLP denominator, while public interest uses total yLP
    /// supply; sharing one carry across those denominators would eventually
    /// leak rounding entitlement between the two populations.
    pub hlp_funding_interest_growth_remainder_scaled: u64,
    /// Claimable swap fees physically held in the reserve vault but excluded
    /// from executable cash and live reserves.
    pub swap_fee_custody_balance: u64,
    pub interest_vault_balance: u64,
    pub swap_fee_liability: u64,
    pub interest_liability: u64,
    pub unallocated_swap_fee_liability: u64,
    pub unallocated_interest_liability: u64,
    pub swap_protocol_fee_liability: u64,
    pub swap_buyback_fee_liability: u64,
    pub interest_protocol_fee_liability: u64,
    pub interest_buyback_fee_liability: u64,
    pub referral_interest_liability: u64,
    /// Governance-approved reference market for fee-lane auctions. A default
    /// key permits only the sold market itself when it directly pairs the sold
    /// and accepted mints.
    pub fee_auction_reference_market: Pubkey,
    /// Governance-approved reference market for buyback-lane auctions. A
    /// default key has the same direct-market-only meaning as above.
    pub buyback_auction_reference_market: Pubkey,
    pub fee_swap_auction_epoch: ProtocolAuctionEpoch,
    pub fee_interest_auction_epoch: ProtocolAuctionEpoch,
    pub buyback_swap_auction_epoch: ProtocolAuctionEpoch,
    pub buyback_interest_auction_epoch: ProtocolAuctionEpoch,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq, InitSpace)]
pub struct ProtocolAuctionEpoch {
    pub start_slot: u64,
    /// Liability remaining immediately after the preceding fill. A larger
    /// current liability proves that new inventory arrived and starts a new
    /// epoch instead of inheriting an old floor price.
    pub tracked_inventory: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace)]
pub struct HlpVault {
    /// Collateral committed to one atomic flash recovery. Receipt supply is
    /// fixed until settlement; retail operations on other positions may run.
    pub flash_collateral_debit: u64,
    pub ylp_vault: Pubkey,
    pub ylp_shares: u64,
    /// hLP-owned live reserve depth that is not backed by reserve cash or
    /// normal cash-backed debt. This is the explicit synthetic live component
    /// in `r_virtual = r_cash + r_cash_backed_debt + r_hlp_live`.
    pub base_hlp_live_reserve: u64,
    pub quote_hlp_live_reserve: u64,
    /// Funding debt used by the hLP vault. It accrues interest and counts
    /// toward utilization, but is not same-side cash-backed reserve debt.
    pub debt_shares: u128,
    /// Raw borrowed token atoms; products and indexed shares stay `u128`.
    pub debt_principal: u64,
    pub hlp_supply: u64,
    pub residual_exposure: i128,
    pub base_swap_fee_growth_index_q64: u128,
    pub base_interest_growth_index_q64: u128,
    pub quote_swap_fee_growth_index_q64: u128,
    pub quote_interest_growth_index_q64: u128,
    pub base_swap_fee_checkpoint_q64: u128,
    pub base_interest_checkpoint_q64: u128,
    pub quote_swap_fee_checkpoint_q64: u128,
    pub quote_interest_checkpoint_q64: u128,
    /// Aggregate sub-atom yLP entitlement carried across hLP checkpoints.
    /// These are distinct from each holder YieldAccount remainder: this layer
    /// converts vault-owned yLP growth into hLP growth without double-flooring.
    pub base_swap_fee_remainder_q64: u64,
    pub base_interest_remainder_q64: u64,
    pub quote_swap_fee_remainder_q64: u64,
    pub quote_interest_remainder_q64: u64,
    /// Sub-index distribution carry for the second, yLP-to-hLP allocation
    /// layer. Whole-token backing represented here has already left the
    /// corresponding `unallocated_*` bucket.
    pub base_swap_fee_growth_remainder_scaled: u64,
    pub base_interest_growth_remainder_scaled: u64,
    pub quote_swap_fee_growth_remainder_scaled: u64,
    pub quote_interest_growth_remainder_scaled: u64,
    pub unallocated_base_swap_fee_amount: u64,
    pub unallocated_base_interest_amount: u64,
    pub unallocated_quote_swap_fee_amount: u64,
    pub unallocated_quote_interest_amount: u64,
    pub last_nav_nad: u128,
    /// Base quoted in quote NAD units for either target asset; zero means unset.
    pub cached_settlement_price_nad: u128,
    /// Smoothed APR of the opposite asset borrowed by this target-asset hLP.
    /// The fixed twelve-hour half-life gives Stop Rate orders stable semantics.
    pub funding_apr_ema_nad: u128,
    pub funding_apr_ema_last_slot: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace)]
pub struct DailyBorrowBucket {
    /// Gross principal lent out through the public borrow path. Internal hLP
    /// funding and isolated leverage do not consume this capacity. This is a
    /// 24-hour leaky/token bucket, not an exact trailing-window sum: it permits
    /// a full burst after idle and then refills at the configured daily rate.
    pub borrowed_bucket: u64,
    pub last_decay_slot: u64,
    /// Numerator remainder from `limit * elapsed_ms / MS_PER_DAY`. For a fixed
    /// absolute limit, carrying it makes refill independent of how often the
    /// bucket is checkpointed. The bps-derived absolute limit can still move
    /// when conservative market depth changes.
    pub decay_remainder_ms: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace, PartialEq, Eq)]
pub struct InsuranceDrawWindow {
    /// `start_slot` may legitimately be zero in a test or newly started bank;
    /// keep initialization separate rather than using a sentinel slot.
    pub initialized: bool,
    /// Start of the nominal 24-hour window, measured in consensus slots.
    pub start_slot: u64,
    /// Available insurance at the start of the window, before any draws.
    pub opening_available: u64,
    /// Net token credits received after the window opened.
    pub credited: u64,
    /// Gross token amount debited by insurance draws in this window.
    pub drawn: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, InitSpace, PartialEq, Eq)]
pub struct Insurance {
    pub base_vault: Pubkey,
    pub quote_vault: Pubkey,
    pub base_available: u64,
    pub quote_available: u64,
    pub base_draw_window: InsuranceDrawWindow,
    pub quote_draw_window: InsuranceDrawWindow,
    /// Maximum share of a borrower/leverage principal shortfall covered by
    /// insurance. hLP funding claims use their own eligible-loss definition.
    pub principal_coverage_bps: u16,
    pub per_day_draw_bps: u16,
}

impl Default for Insurance {
    fn default() -> Self {
        Self {
            base_vault: Pubkey::default(),
            quote_vault: Pubkey::default(),
            base_available: 0,
            quote_available: 0,
            base_draw_window: InsuranceDrawWindow::default(),
            quote_draw_window: InsuranceDrawWindow::default(),
            principal_coverage_bps: crate::constants::MAX_INSURANCE_PRINCIPAL_COVERAGE_BPS,
            per_day_draw_bps: crate::constants::MAX_INSURANCE_DRAW_PER_DAY_BPS,
        }
    }
}

/// Token amounts stored in reserves, positions, and shares are raw atoms.
/// Internal curve quantities, risk depths, hLP NAV/exposure, and normalized
/// debt/collateral values use `max(9, base_side.asset_decimals,
/// quote_side.asset_decimals)` decimal places, including quantity fields with
/// the historical `_nad` suffix. Prices, rates, and per-share ratios always
/// retain nine-decimal NAD scaling. The quantity scale is immutable per market.
#[account]
#[derive(InitSpace, Default)]
pub struct Market {
    pub oracle: Box<super::OracleState>,
    pub version: u8,
    pub ylp_mint: Pubkey,
    pub base_side: Box<MarketSide>,
    pub quote_side: Box<MarketSide>,
    pub config: MarketConfig,
    pub amm: Box<AmmState>,
    pub debt: Debt,
    /// One shared junior capital budget across both reserve/debt books.
    pub tranches: crate::transitions::tranches::TrancheCapital,
    pub income: crate::state::LazyIncomeBooks,
    pub junior_exit_config: crate::transitions::tranches::JuniorExitConfig,
    pub junior_mint: Pubkey,
    pub base_hlp_vault: HlpVault,
    pub quote_hlp_vault: HlpVault,
    pub risk: Risk,
    pub insurance: Insurance,
    pub params_hash: [u8; 32],
    /// One-shot signer allowed to provide the first fully-backed Base/Quote
    /// seed. It is cleared permanently once yLP supply becomes nonzero.
    pub initial_liquidity_authority: Pubkey,
    /// External yLP burned into direct support or reusable governance balances. This is added back
    /// when computing direct-yLP eligibility; internal reserve-share supply is
    /// intentionally unchanged by governance locking.
    pub governance_locked_ylp: u64,
    /// Independent monotone revisions for fee, concentration, IRM, EMA,
    /// daily-borrow-limit, and center-controller parameter families.
    pub parameter_revisions: [u64; 8],
    /// Latest trader-visible marginal price committed by a curve mutation.
    pub last_marginal_observation_nad: u64,
    /// Monotone revision for executable-curve mutations.
    pub curve_revision: u64,
    /// Curve revision represented by the materialized lending-risk snapshot.
    pub risk_revision: u64,
    pub last_update_slot: u64,
    pub reduce_only: bool,
    pub bump: u8,
}

// Separate frames keep the largest fixed Borsh value off the caller's stack.
#[inline(never)]
fn reload_field<T: AnchorDeserialize>(target: &mut T, cursor: &mut &[u8]) -> Result<()> {
    *target = T::deserialize(cursor)?;
    Ok(())
}

impl Market {
    /// Refresh a previously decoded market after CPI, reusing its large boxed
    /// fields. A failed decode must abort the instruction: this copy may then
    /// contain a partial refresh. The caller must bind the account owner/key.
    #[inline(never)]
    pub fn reload_from_account_bytes(&mut self, data: &[u8]) -> Result<()> {
        use anchor_lang::Discriminator;
        require!(data.starts_with(Self::DISCRIMINATOR), ErrorCode::BrokenInvariant);
        let mut cursor = &data[Self::DISCRIMINATOR.len()..];
        reload_field(&mut *self.oracle, &mut cursor)?;
        reload_field(&mut self.version, &mut cursor)?;
        reload_field(&mut self.ylp_mint, &mut cursor)?;
        reload_field(&mut *self.base_side, &mut cursor)?;
        reload_field(&mut *self.quote_side, &mut cursor)?;
        reload_field(&mut self.config, &mut cursor)?;
        reload_field(&mut *self.amm, &mut cursor)?;
        reload_field(&mut self.debt, &mut cursor)?;
        reload_field(&mut self.tranches, &mut cursor)?;
        self.income.reload(&mut cursor)?;
        reload_field(&mut self.junior_exit_config, &mut cursor)?;
        reload_field(&mut self.junior_mint, &mut cursor)?;
        reload_field(&mut self.base_hlp_vault, &mut cursor)?;
        reload_field(&mut self.quote_hlp_vault, &mut cursor)?;
        reload_field(&mut self.risk, &mut cursor)?;
        reload_field(&mut self.insurance, &mut cursor)?;
        reload_field(&mut self.params_hash, &mut cursor)?;
        reload_field(&mut self.initial_liquidity_authority, &mut cursor)?;
        reload_field(&mut self.governance_locked_ylp, &mut cursor)?;
        reload_field(&mut self.parameter_revisions, &mut cursor)?;
        reload_field(&mut self.last_marginal_observation_nad, &mut cursor)?;
        reload_field(&mut self.curve_revision, &mut cursor)?;
        reload_field(&mut self.risk_revision, &mut cursor)?;
        reload_field(&mut self.last_update_slot, &mut cursor)?;
        reload_field(&mut self.reduce_only, &mut cursor)?;
        reload_field(&mut self.bump, &mut cursor)?;
        Ok(())
    }

    pub(crate) fn validate_mint_domain(
        base_asset_mint: Pubkey,
        quote_asset_mint: Pubkey,
        ylp_mint: Pubkey,
        base_hlp_mint: Pubkey,
        quote_hlp_mint: Pubkey,
    ) -> Result<()> {
        require_keys_neq!(base_asset_mint, quote_asset_mint, ErrorCode::InvalidMint);
        require_keys_neq!(ylp_mint, base_asset_mint, ErrorCode::InvalidLpMintKey);
        require_keys_neq!(ylp_mint, quote_asset_mint, ErrorCode::InvalidLpMintKey);
        require_keys_neq!(base_hlp_mint, base_asset_mint, ErrorCode::InvalidLpMintKey);
        require_keys_neq!(base_hlp_mint, quote_asset_mint, ErrorCode::InvalidLpMintKey);
        require_keys_neq!(base_hlp_mint, ylp_mint, ErrorCode::InvalidLpMintKey);
        require_keys_neq!(quote_hlp_mint, base_asset_mint, ErrorCode::InvalidLpMintKey);
        require_keys_neq!(quote_hlp_mint, quote_asset_mint, ErrorCode::InvalidLpMintKey);
        require_keys_neq!(quote_hlp_mint, ylp_mint, ErrorCode::InvalidLpMintKey);
        require_keys_neq!(quote_hlp_mint, base_hlp_mint, ErrorCode::InvalidLpMintKey);
        Ok(())
    }

    pub fn assert_current_version(&self) -> Result<()> {
        require_eq!(self.version, MARKET_LAYOUT_VERSION, ErrorCode::InvalidVersion);
        Ok(())
    }

    pub fn assert_started(&self) -> Result<()> {
        self.assert_started_at(Clock::get()?.unix_timestamp)
    }

    pub(crate) fn assert_started_at(&self, unix_timestamp: i64) -> Result<()> {
        self.assert_current_version()?;
        require!(unix_timestamp >= self.config.start_time, ErrorCode::MarketNotStarted);
        Ok(())
    }

    pub fn side(&self, market_asset: MarketAsset) -> &MarketSide {
        match market_asset {
            MarketAsset::Base => &self.base_side,
            MarketAsset::Quote => &self.quote_side,
        }
    }

    pub fn side_mut(&mut self, market_asset: MarketAsset) -> &mut MarketSide {
        match market_asset {
            MarketAsset::Base => &mut self.base_side,
            MarketAsset::Quote => &mut self.quote_side,
        }
    }

    pub fn asset_for_mint(&self, mint: Pubkey) -> Result<MarketAsset> {
        if mint == self.base_side.asset_mint {
            return Ok(MarketAsset::Base);
        }
        if mint == self.quote_side.asset_mint {
            return Ok(MarketAsset::Quote);
        }
        err!(ErrorCode::InvalidMint)
    }

    pub fn asset_for_hlp_mint(&self, mint: Pubkey) -> Result<MarketAsset> {
        if mint == self.base_side.hlp_mint {
            return Ok(MarketAsset::Base);
        }
        if mint == self.quote_side.hlp_mint {
            return Ok(MarketAsset::Quote);
        }
        err!(ErrorCode::InvalidLpMintKey)
    }

    pub fn swap_sides(&self, asset_in: MarketAsset) -> (&MarketSide, &MarketSide) {
        match asset_in {
            MarketAsset::Base => (&self.base_side, &self.quote_side),
            MarketAsset::Quote => (&self.quote_side, &self.base_side),
        }
    }

    pub fn swap_sides_mut(&mut self, asset_in: MarketAsset) -> (&mut MarketSide, &mut MarketSide) {
        match asset_in {
            MarketAsset::Base => (&mut self.base_side, &mut self.quote_side),
            MarketAsset::Quote => (&mut self.quote_side, &mut self.base_side),
        }
    }
}

#[macro_export]
macro_rules! generate_market_seeds {
    ($market:expr) => {
        [
            MARKET_V2_SEED_PREFIX,
            $market.base_side.asset_mint.as_ref(),
            $market.quote_side.asset_mint.as_ref(),
            $market.params_hash.as_ref(),
            &[$market.bump],
        ]
    };
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct Risk {
    pub base_price_ema_nad: u64,
    pub quote_price_ema_nad: u64,
    pub directional_base_price_ema_nad: u64,
    pub directional_quote_price_ema_nad: u64,
    pub cached_spot_base_price_nad: u64,
    pub cached_spot_quote_price_nad: u64,
    /// Last observed total active curve depth in market amount units (see Market).
    pub observed_curve_depth_nad: u128,
    /// EMA of total active curve depth in market amount units (see Market).
    pub curve_depth_ema_nad: u128,
    pub last_snapshot_slot: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace)]
pub struct ReserveShares {
    pub ylp_supply: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace)]
pub struct Reserves {
    /// Same-transaction collateral receivable. Included in backing, excluded
    /// from cash available to swaps, loans and withdrawals.
    pub flash_receivable: u64,
    // Virtual Reserves (r_virtual = r_cash + r_cash_backed_debt + r_hlp_live)
    pub live_reserve: u64,
    // Cash Reserves (r_cash)
    pub cash_reserve: u64,
    /// Physical reserve-vault atoms removed from executable AMM inventory by
    /// base-hLP deleveraging. They are conservation-only bookkeeping, excluded
    /// from hLP NAV and exit output, and return to executable cash pro rata as
    /// base hLP exits.
    pub base_hlp_backing_inventory: u64,
    /// Quote-hLP counterpart of `base_hlp_backing_inventory`; never a second
    /// hLP NAV or withdrawal claim.
    pub quote_hlp_backing_inventory: u64,
    /// Physical reserve-vault atoms retained from toxicity surcharge for a
    /// future protected recenter. They are custody-backed but excluded from
    /// executable cash/live reserves, yLP NAV, and every withdrawal claim.
    pub protected_recenter_reserve: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarketAsset {
    Base,
    Quote,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace)]
pub struct MarketSide {
    pub asset_mint: Pubkey,
    pub asset_decimals: u8,
    pub hlp_mint: Pubkey,
    pub reserve_vault: Pubkey,
    pub collateral_vault: Pubkey,
    pub interest_vault: Pubkey,
    pub reserves: Reserves,
    pub shares: ReserveShares,
    pub fees: Fees,
    pub daily_borrow_bucket: DailyBorrowBucket,
}

// Boxing changes only in-memory placement; Borsh retains the same wire layout.
impl anchor_lang::Space for Box<AmmState> {
    const INIT_SPACE: usize = AmmState::INIT_SPACE;
}
impl anchor_lang::Space for Box<MarketSide> {
    const INIT_SPACE: usize = MarketSide::INIT_SPACE;
}

#[cfg(test)]
mod reload_tests {
    use super::*;

    #[test]
    fn in_place_refresh_matches_canonical_decoder_and_reuses_boxes() {
        let mut bytes = Vec::new();
        Market::default().try_serialize(&mut bytes).unwrap();
        let mut target = Market::default();
        let pointers = (
            &*target.base_side as *const MarketSide,
            &*target.quote_side as *const MarketSide,
            &*target.amm as *const AmmState,
        );
        // Perturb every wire byte. Valid canonical encodings must refresh the
        // entire account, including fields not currently used by protection.
        for index in 8..bytes.len() {
            let original = bytes[index];
            bytes[index] = original.wrapping_add(1);
            if let Ok(expected) = Market::try_deserialize(&mut bytes.as_slice()) {
                target.reload_from_account_bytes(&bytes).unwrap();
                let mut actual_bytes = Vec::new();
                let mut expected_bytes = Vec::new();
                target.try_serialize(&mut actual_bytes).unwrap();
                expected.try_serialize(&mut expected_bytes).unwrap();
                assert_eq!(actual_bytes, expected_bytes, "wire byte {index}");
            }
            bytes[index] = original;
        }
        assert_eq!(
            pointers,
            (
                &*target.base_side as *const MarketSide,
                &*target.quote_side as *const MarketSide,
                &*target.amm as *const AmmState,
            )
        );
        assert!(target.reload_from_account_bytes(&bytes[..bytes.len() - 1]).is_err());
        bytes[0] ^= 1;
        assert!(target.reload_from_account_bytes(&bytes).is_err());
    }
}
