use anchor_lang::prelude::*;

#[cfg(test)]
use std::cell::{Cell, RefCell};

use crate::state::HlpVault;
use crate::{
    constants::{NAD, YIELD_GROWTH_SCALE_Q64},
    errors::ErrorCode,
    math::{
        concentrated_marginal_price_nad, denormalize_from_nad_ceil, denormalize_from_nad_floor,
        hlp_opposite_exposure_nad, mul_div_rem_u128, mul_div_u128, normalize_to_nad, ratio_lte_full_width,
        sqrt_ratio_nad, ConcentratedGuidanceCurve, ConcentratedInvariantSeed, ConcentratedPreparedCurve,
        ConcentratedSwapDirection, DynamicFeePreState, HlpInventoryValuesNad,
    },
    state::{ConcentrationParameters, Debt, DebtClearance, DebtRepaymentQuote, Market, MarketAsset},
};

use super::{
    amm::{CurveCheckpoint, CurveReservesNad, PreliminarySwapInputs},
    AmmSwapQuote,
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

/// Post-transition exposure is protocol dust only when it is no more than
/// 0.00001 target tokens and no more than one part per million of current hLP
/// NAV. Coarse assets and small vaults therefore fail closed rather than hide
/// a meaningful constrained gap.
const HLP_REBALANCE_DUST_MAX_NAD: u128 = 10_000;
const HLP_REBALANCE_DUST_NAV_DENOMINATOR: u128 = 1_000_000;
/// Joint predictive positioning bounds each active hLP's combined
/// target-numeraire change—principal NAV plus its operation-frozen public
/// borrow-interest claim—to one part per million. One raw target atom is the
/// floor for coarse assets whose accounting cannot represent that tolerance.
const HLP_CONCENTRATED_TRACKING_NAV_DENOMINATOR: u128 = 1_000_000;
/// One lifecycle-accounting seed, a possibly bounded and then canonicalized
/// center, independent axes, one lifecycle-accounting final plan, and one
/// exact authority. Only the exact authority can be accepted.
const HLP_CONCENTRATED_MAX_CANDIDATE_EVALUATIONS: u32 = 7;
const HLP_CONCENTRATED_MAX_AUTHORITATIVE_EVALUATIONS: u32 = 2;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SwapCashFloors {
    base: u64,
    quote: u64,
}

impl SwapCashFloors {
    fn set(&mut self, asset: MarketAsset, amount: u64) {
        match asset {
            MarketAsset::Base => self.base = amount,
            MarketAsset::Quote => self.quote = amount,
        }
    }

    fn for_asset(self, asset: MarketAsset) -> u64 {
        match asset {
            MarketAsset::Base => self.base,
            MarketAsset::Quote => self.quote,
        }
    }

    pub(crate) fn available(self, market: &Market) -> bool {
        market.base_side.reserves.cash_reserve >= self.base && market.quote_side.reserves.cash_reserve >= self.quote
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SwapCashPolicy {
    Spot,
    Borrow {
        asset: MarketAsset,
        amount: u64,
    },
    Decrease {
        debt_asset: MarketAsset,
        debt_shares: u128,
        debt_principal: u128,
    },
    Close {
        debt_asset: MarketAsset,
        debt_shares: u128,
        debt_principal: u128,
    },
    Liquidate {
        debt_asset: MarketAsset,
        debt_shares: u128,
        debt_principal: u128,
    },
}

impl SwapCashPolicy {
    pub(crate) fn floors(self, market: &Market, asset_in: MarketAsset, amount_out: u64) -> Result<SwapCashFloors> {
        let mut floors = SwapCashFloors::default();
        match self {
            Self::Spot => floors.set(asset_in.opposite(), amount_out),
            Self::Borrow { asset, amount } => {
                require!(asset == asset_in, ErrorCode::BrokenInvariant);
                floors.set(asset, amount);
                floors.set(asset_in.opposite(), amount_out);
            }
            Self::Decrease {
                debt_asset,
                debt_shares,
                debt_principal,
            } => {
                require!(debt_asset == asset_in.opposite(), ErrorCode::BrokenInvariant);
                let (_, interest) =
                    isolated_repayment_cash_and_interest(market, debt_asset, debt_shares, debt_principal, amount_out)?;
                floors.set(debt_asset, interest);
            }
            Self::Close {
                debt_asset,
                debt_shares,
                debt_principal,
            } => {
                require!(debt_asset == asset_in.opposite(), ErrorCode::BrokenInvariant);
                let (debt_cash, interest) =
                    isolated_repayment_cash_and_interest(market, debt_asset, debt_shares, debt_principal, u64::MAX)?;
                floors.set(
                    debt_asset,
                    interest
                        .checked_add(amount_out.saturating_sub(debt_cash))
                        .ok_or(ErrorCode::MarketMathOverflow)?,
                );
            }
            Self::Liquidate {
                debt_asset,
                debt_shares,
                debt_principal,
            } => {
                require!(debt_asset == asset_in.opposite(), ErrorCode::BrokenInvariant);
                if debt_shares == 0 {
                    require_eq!(debt_principal, 0, ErrorCode::BrokenInvariant);
                    return Ok(floors);
                }
                let full = market
                    .debt
                    .isolated_repayment_for_max(debt_asset, debt_shares, u64::MAX)?;
                let repay_credit = amount_out.min(full.cash_repaid);
                let repayment_basis = (full.cash_repaid as u128).max(debt_principal);
                let (_, interest) =
                    crate::math::realized_interest_split(repay_credit, repayment_basis, debt_principal)?;
                floors.set(
                    debt_asset,
                    interest
                        .checked_add(amount_out.saturating_sub(full.cash_repaid))
                        .ok_or(ErrorCode::MarketMathOverflow)?,
                );
            }
        }
        Ok(floors)
    }
}

fn isolated_repayment_cash_and_interest(
    market: &Market,
    debt_asset: MarketAsset,
    debt_shares: u128,
    debt_principal: u128,
    max_repay: u64,
) -> Result<(u64, u64)> {
    if max_repay == 0 {
        return Ok((0, 0));
    }
    let clearance = market
        .debt
        .isolated_clearance_for_max(debt_asset, debt_shares, debt_principal, max_repay)?;
    Ok((clearance.cash_repaid, clearance.interest_paid))
}

#[cfg(test)]
thread_local! {
    static CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS: Cell<u32> = const { Cell::new(0) };
    static CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS: Cell<u32> = const { Cell::new(0) };
    static VERIFY_COMPACT_HLP_GUIDANCE: Cell<bool> = const { Cell::new(false) };
    static HLP_DELEVERAGE_FULL_CAPACITY_EVALUATIONS: Cell<u32> = const { Cell::new(0) };
    static HLP_DELEVERAGE_CHEAP_REPAYMENT_EVALUATIONS: Cell<u32> = const { Cell::new(0) };
    static HLP_DELEVERAGE_LEGACY_CAPACITY_EVALUATIONS: Cell<u32> = const { Cell::new(0) };
    static HLP_COMPACT_GUIDANCE_CELLS: Cell<u32> = const { Cell::new(0) };
    static HLP_COMPACT_GUIDANCE_EXACT_IN_PROBES: Cell<u32> = const { Cell::new(0) };
    static HLP_COMPACT_GUIDANCE_EXACT_OUT_PROBES: Cell<u32> = const { Cell::new(0) };
    static SCRATCH_HLP_LIFECYCLE_RESULT: RefCell<Option<HlpCompactLifecycleResult>> = const { RefCell::new(None) };
}

#[cfg(test)]
struct VerifyCompactHlpGuidanceGuard {
    previous: bool,
}

#[cfg(test)]
impl VerifyCompactHlpGuidanceGuard {
    fn enable() -> Self {
        let previous = VERIFY_COMPACT_HLP_GUIDANCE.with(|enabled| enabled.replace(true));
        Self { previous }
    }
}

#[cfg(test)]
impl Drop for VerifyCompactHlpGuidanceGuard {
    fn drop(&mut self) {
        VERIFY_COMPACT_HLP_GUIDANCE.with(|enabled| enabled.set(self.previous));
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct HlpDeleverageCapProbeCounts {
    full_capacity: u32,
    cheap_repayment: u32,
    legacy_capacity: u32,
}

#[cfg(test)]
fn reset_hlp_deleverage_cap_probe_counts() {
    HLP_DELEVERAGE_FULL_CAPACITY_EVALUATIONS.with(|count| count.set(0));
    HLP_DELEVERAGE_CHEAP_REPAYMENT_EVALUATIONS.with(|count| count.set(0));
    HLP_DELEVERAGE_LEGACY_CAPACITY_EVALUATIONS.with(|count| count.set(0));
}

#[cfg(test)]
fn hlp_deleverage_cap_probe_counts() -> HlpDeleverageCapProbeCounts {
    HlpDeleverageCapProbeCounts {
        full_capacity: HLP_DELEVERAGE_FULL_CAPACITY_EVALUATIONS.with(Cell::get),
        cheap_repayment: HLP_DELEVERAGE_CHEAP_REPAYMENT_EVALUATIONS.with(Cell::get),
        legacy_capacity: HLP_DELEVERAGE_LEGACY_CAPACITY_EVALUATIONS.with(Cell::get),
    }
}

fn recognized_hlp_residual_exposure(actual_residual_nad: i128, nav_nad: u128) -> i128 {
    let tolerance_nad = HLP_REBALANCE_DUST_MAX_NAD.min(nav_nad / HLP_REBALANCE_DUST_NAV_DENOMINATOR);
    if actual_residual_nad.unsigned_abs() <= tolerance_nad {
        0
    } else {
        actual_residual_nad
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SingleSidedLiquidityReceipt {
    pub deposit_amount: u64,
    pub borrowed_amount: u64,
    pub ylp_amount: u64,
    pub hlp_amount: u64,
    pub hlp_supply: u64,
    pub target_amount_out: u64,
    pub debt_repaid: u64,
    pub interest_paid: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HlpRebalanceReceipt {
    pub target_asset: MarketAsset,
    pub ideal_delta: i128,
    pub executed_delta: i128,
    pub residual_exposure: i128,
    pub current_swap_fee_eligible_ylp_shares: u64,
    pub ylp_mint_amount: u64,
    pub ylp_burn_amount: u64,
    pub debt_delta: i128,
    pub interest_paid: u64,
    pub nav_nad: u128,
    pub(crate) tracking_start_nav_nad: i128,
    pub(crate) tracking_loss_budget_nad: u128,
    /// Frozen public-borrow interest only; hLP funding interest is ineligible.
    pub(crate) tracking_base_unrealized_interest: u64,
    /// Frozen public-borrow interest only; hLP funding interest is ineligible.
    pub(crate) tracking_quote_unrealized_interest: u64,
    pub(crate) tracking_start_ylp_shares: u64,
    pub(crate) tracking_start_ylp_supply: u64,
    pub(crate) tracking_retained_contribution_nad: i128,
    /// Internal controller signal: an explicit cash/collateral/debt/share cap
    /// clipped the requested preposition. Such a point is authoritative when
    /// all lifecycle guards pass, but cannot define a predictive derivative.
    pub(crate) preposition_capacity_bound: bool,
}

impl Default for HlpRebalanceReceipt {
    fn default() -> Self {
        Self {
            target_asset: MarketAsset::Base,
            ideal_delta: 0,
            executed_delta: 0,
            residual_exposure: 0,
            current_swap_fee_eligible_ylp_shares: 0,
            ylp_mint_amount: 0,
            ylp_burn_amount: 0,
            debt_delta: 0,
            interest_paid: 0,
            nav_nad: 0,
            tracking_start_nav_nad: 0,
            tracking_loss_budget_nad: 0,
            tracking_base_unrealized_interest: 0,
            tracking_quote_unrealized_interest: 0,
            tracking_start_ylp_shares: 0,
            tracking_start_ylp_supply: 0,
            tracking_retained_contribution_nad: 0,
            preposition_capacity_bound: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HlpTrackingReference {
    pub(crate) principal_nav_nad: i128,
    pub(crate) loss_budget_nad: u128,
    /// Frozen public-borrow interest only; never an hLP-funding claim.
    pub(crate) base_unrealized_interest: u64,
    /// Frozen public-borrow interest only; never an hLP-funding claim.
    pub(crate) quote_unrealized_interest: u64,
    pub(crate) start_ylp_shares: u64,
    pub(crate) start_ylp_supply: u64,
}

impl Market {
    pub fn deposit_single_sided(
        &mut self,
        target_asset: MarketAsset,
        deposit_amount: u64,
        min_hlp_amount: u64,
    ) -> Result<SingleSidedLiquidityReceipt> {
        let market = self;
        require!(deposit_amount > 0, ErrorCode::AmountZero);
        require_hlp_settlement_available(market, target_asset)?;
        let pre_prices = current_hlp_curve_prices(market)?;
        let pre_entry = current_hlp_entry_state_with_prices(market, target_asset, pre_prices)?;
        let (hlp_supply_before, settlement_reference_before) = match target_asset {
            MarketAsset::Base => (
                market.base_hlp_vault.hlp_supply,
                market.base_hlp_vault.cached_settlement_price_nad,
            ),
            MarketAsset::Quote => (
                market.quote_hlp_vault.hlp_supply,
                market.quote_hlp_vault.cached_settlement_price_nad,
            ),
        };
        require!(
            hlp_supply_before == 0 || pre_entry.disposition.admits_entry(),
            ErrorCode::HlpSettlementUnavailable
        );
        // An hLP owns ordinary yLP shares. Its two deposit legs must therefore
        // follow the executable reserve claims, not a 50/50 marginal-value
        // split (the two coincide only for CPMM).
        let target_reserve = market.curve_reserve(target_asset)?;
        let opposite_reserve = market.curve_reserve(target_asset.opposite())?;
        require!(
            target_reserve > 0 && opposite_reserve > 0,
            ErrorCode::InsufficientLiquidity
        );
        let borrowed_amount = u64::try_from(
            (deposit_amount as u128)
                .checked_mul(opposite_reserve as u128)
                .and_then(|value| value.checked_div(target_reserve as u128))
                .ok_or(ErrorCode::MarketMathOverflow)?,
        )
        .map_err(|_| ErrorCode::MarketMathOverflow)?;
        require!(borrowed_amount > 0, ErrorCode::InsufficientLiquidity);
        let debt_shares = require_hlp_borrow_headroom(market, target_asset.opposite(), borrowed_amount)?;
        checkpoint_hlp_yield_from_ylp(market, target_asset)?;

        let (ylp_amount, hlp_amount, hlp_supply, post_prices) = match target_asset {
            MarketAsset::Base => {
                let hlp_supply_before = market.base_hlp_vault.hlp_supply;
                let nav_before_nad = if hlp_supply_before == 0 {
                    0
                } else if market.base_hlp_vault.last_nav_nad > 0 {
                    market.base_hlp_vault.last_nav_nad
                } else {
                    hlp_nav_nad(market, MarketAsset::Base)?
                };
                let ylp_amount = ylp_for_live_reserve_deposit(market, deposit_amount, borrowed_amount)?;
                require!(ylp_amount > 0, ErrorCode::SlippageExceeded);
                market.base_side.credit_reserve(deposit_amount, true)?;
                market.quote_side.credit_reserve(borrowed_amount, false)?;
                market
                    .base_hlp_vault
                    .credit_hlp_live_reserve(MarketAsset::Quote, borrowed_amount)?;
                market.base_side.shares.mint(ylp_amount)?;
                market.quote_side.shares.mint(ylp_amount)?;
                market.base_hlp_vault.add_debt_shares(debt_shares)?;
                market.base_hlp_vault.add_debt_principal(borrowed_amount)?;
                market.base_hlp_vault.credit_ylp(ylp_amount)?;
                let current_prices = current_hlp_curve_prices(market)?;
                let current_nav_nad = hlp_nav_nad_with_prices(market, MarketAsset::Base, current_prices)?;
                let hlp_amount = if hlp_supply_before == 0 {
                    deposit_amount
                } else {
                    let delta_nav_nad = current_nav_nad
                        .checked_sub(nav_before_nad)
                        .ok_or(ErrorCode::MarketMathOverflow)?;
                    hlp_shares_for_delta_nav(
                        delta_nav_nad,
                        nav_before_nad.max(market.base_hlp_vault.last_nav_nad),
                        hlp_supply_before,
                    )?
                };
                market.base_hlp_vault.mint_hlp(hlp_amount)?;
                market.base_hlp_vault.last_nav_nad = current_nav_nad;
                (ylp_amount, hlp_amount, market.base_hlp_vault.hlp_supply, current_prices)
            }
            MarketAsset::Quote => {
                let hlp_supply_before = market.quote_hlp_vault.hlp_supply;
                let nav_before_nad = if hlp_supply_before == 0 {
                    0
                } else if market.quote_hlp_vault.last_nav_nad > 0 {
                    market.quote_hlp_vault.last_nav_nad
                } else {
                    hlp_nav_nad(market, MarketAsset::Quote)?
                };
                let ylp_amount = ylp_for_live_reserve_deposit(market, borrowed_amount, deposit_amount)?;
                require!(ylp_amount > 0, ErrorCode::SlippageExceeded);
                market.base_side.credit_reserve(borrowed_amount, false)?;
                market.quote_side.credit_reserve(deposit_amount, true)?;
                market
                    .quote_hlp_vault
                    .credit_hlp_live_reserve(MarketAsset::Base, borrowed_amount)?;
                market.base_side.shares.mint(ylp_amount)?;
                market.quote_side.shares.mint(ylp_amount)?;
                market.quote_hlp_vault.add_debt_shares(debt_shares)?;
                market.quote_hlp_vault.add_debt_principal(borrowed_amount)?;
                market.quote_hlp_vault.credit_ylp(ylp_amount)?;
                let current_prices = current_hlp_curve_prices(market)?;
                let current_nav_nad = hlp_nav_nad_with_prices(market, MarketAsset::Quote, current_prices)?;
                let hlp_amount = if hlp_supply_before == 0 {
                    deposit_amount
                } else {
                    let delta_nav_nad = current_nav_nad
                        .checked_sub(nav_before_nad)
                        .ok_or(ErrorCode::MarketMathOverflow)?;
                    hlp_shares_for_delta_nav(
                        delta_nav_nad,
                        nav_before_nad.max(market.quote_hlp_vault.last_nav_nad),
                        hlp_supply_before,
                    )?
                };
                market.quote_hlp_vault.mint_hlp(hlp_amount)?;
                market.quote_hlp_vault.last_nav_nad = current_nav_nad;
                (
                    ylp_amount,
                    hlp_amount,
                    market.quote_hlp_vault.hlp_supply,
                    current_prices,
                )
            }
        };
        require_gte!(hlp_amount, min_hlp_amount, ErrorCode::SlippageExceeded);
        let post_entry = current_hlp_entry_state_with_prices(market, target_asset, post_prices)?;
        require!(
            post_entry.disposition.admits_entry(),
            ErrorCode::HlpSettlementUnavailable
        );
        if hlp_supply_before > 0 {
            match pre_entry.disposition {
                HlpEntryDisposition::Settled => require!(
                    post_entry.disposition == HlpEntryDisposition::Settled,
                    ErrorCode::HlpSettlementUnavailable
                ),
                HlpEntryDisposition::ControllerGranularityLimited => {
                    let pre_residual = pre_entry.residual_exposure.unsigned_abs();
                    let post_residual = post_entry.residual_exposure.unsigned_abs();
                    require!(
                        post_entry.nav_nad > pre_entry.nav_nad && hlp_supply > hlp_supply_before,
                        ErrorCode::HlpSettlementUnavailable
                    );
                    require!(post_residual <= pre_residual, ErrorCode::HlpSettlementUnavailable);
                    require!(
                        post_entry.residual_exposure == 0
                            || post_entry.residual_exposure.is_negative() == pre_entry.residual_exposure.is_negative(),
                        ErrorCode::HlpSettlementUnavailable
                    );
                    require!(
                        ratio_lte_full_width(post_residual, post_entry.nav_nad, pre_residual, pre_entry.nav_nad,)?
                            && ratio_lte_full_width(
                                post_residual,
                                hlp_supply as u128,
                                pre_residual,
                                hlp_supply_before as u128,
                            )?
                            && ratio_lte_full_width(
                                pre_entry.nav_nad,
                                hlp_supply_before as u128,
                                post_entry.nav_nad,
                                hlp_supply as u128,
                            )?,
                        ErrorCode::HlpSettlementUnavailable
                    );
                }
                _ => return err!(ErrorCode::HlpSettlementUnavailable),
            }
        }
        let vault = match target_asset {
            MarketAsset::Base => &mut market.base_hlp_vault,
            MarketAsset::Quote => &mut market.quote_hlp_vault,
        };
        vault.last_nav_nad = post_entry.nav_nad;
        vault.residual_exposure = post_entry.residual_exposure;
        vault.cached_settlement_price_nad = if hlp_supply_before == 0 || post_entry.residual_exposure == 0 {
            post_prices.for_asset(target_asset)
        } else {
            settlement_reference_before
        };
        let health = market.market_health()?;
        market.assert_market_health_snapshot(&health)?;
        market.assert_virtual_reserve_invariant(MarketAsset::Base)?;
        market.assert_virtual_reserve_invariant(MarketAsset::Quote)?;
        Ok(SingleSidedLiquidityReceipt {
            deposit_amount,
            borrowed_amount,
            ylp_amount,
            hlp_amount,
            hlp_supply,
            target_amount_out: 0,
            debt_repaid: 0,
            interest_paid: 0,
        })
    }
    pub fn withdraw_single_sided(
        &mut self,
        target_asset: MarketAsset,
        hlp_amount: u64,
    ) -> Result<SingleSidedLiquidityReceipt> {
        let market = self;
        require!(hlp_amount > 0, ErrorCode::AmountZero);
        let residual_exposure = match target_asset {
            MarketAsset::Base => market.base_hlp_vault.residual_exposure,
            MarketAsset::Quote => market.quote_hlp_vault.residual_exposure,
        };
        let settlement_reference_before = match target_asset {
            MarketAsset::Base => market.base_hlp_vault.cached_settlement_price_nad,
            MarketAsset::Quote => market.quote_hlp_vault.cached_settlement_price_nad,
        };
        // Preserve the ordinary stale-price guard. An explicitly recorded
        // partial controller residual is different: an exit reduces or fully
        // retires that hedge, so trapping it behind the prior reference would
        // create a liveness failure.
        if residual_exposure == 0 {
            require_hlp_settlement_available(market, target_asset)?;
        }
        checkpoint_hlp_yield_from_ylp(market, target_asset)?;
        let receipt = match target_asset {
            MarketAsset::Base => {
                let supply = market.base_hlp_vault.hlp_supply;
                require_gte!(supply, hlp_amount, ErrorCode::InsufficientBalance);
                let ylp_amount = proportional(market.base_hlp_vault.ylp_shares, hlp_amount, supply)?;
                let quote_debt_shares = proportional_u128(market.base_hlp_vault.debt_shares, hlp_amount, supply)?;
                let base_out = ylp_live_underlying_amount(market, MarketAsset::Base, ylp_amount)?;
                let quote_redeemed = ylp_live_underlying_amount(market, MarketAsset::Quote, ylp_amount)?;
                let debt_repaid = Debt::aggregate_debt_reduction_for_shares(
                    market.base_hlp_vault.debt_shares,
                    quote_debt_shares,
                    market.debt.quote_borrow_index_nad,
                )?;
                let base_hlp_live_debit =
                    proportional(market.base_hlp_vault.base_hlp_live_reserve, hlp_amount, supply)?;
                let quote_hlp_live_debit =
                    proportional(market.base_hlp_vault.quote_hlp_live_reserve, hlp_amount, supply)?;
                let base_out =
                    settled_close_target_amount(market, MarketAsset::Base, base_out, quote_redeemed, debt_repaid)?;
                release_hlp_backing_inventory(market, MarketAsset::Base, hlp_amount, supply)?;
                let debt_clearance = market
                    .base_hlp_vault
                    .clear_debt_repay(quote_debt_shares, market.debt.quote_borrow_index_nad)?;
                let interest_paid = debt_clearance.interest_paid;
                market.base_side.debit_reserve(base_out, true)?;
                debit_hlp_live_reserve(market, MarketAsset::Base, MarketAsset::Base, base_hlp_live_debit)?;
                debit_hlp_live_reserve(market, MarketAsset::Base, MarketAsset::Quote, quote_hlp_live_debit)?;
                market.base_side.shares.burn(ylp_amount)?;
                market.quote_side.shares.burn(ylp_amount)?;
                market.base_side.assert_share_backing()?;
                market.quote_side.assert_share_backing()?;
                market.base_hlp_vault.debit_ylp(ylp_amount)?;
                debit_cash_for_hlp_interest(&mut market.quote_side, interest_paid)?;
                market.base_hlp_vault.burn_hlp(hlp_amount)?;
                if market.base_hlp_vault.hlp_supply == 0 {
                    require_eq!(
                        market.base_side.reserves.base_hlp_backing_inventory,
                        0,
                        ErrorCode::BrokenInvariant
                    );
                    require_eq!(
                        market.quote_side.reserves.base_hlp_backing_inventory,
                        0,
                        ErrorCode::BrokenInvariant
                    );
                    market.base_hlp_vault.last_nav_nad = 0;
                    market.base_hlp_vault.cached_settlement_price_nad = 0;
                } else {
                    let current_prices = current_hlp_curve_prices(market)?;
                    market.base_hlp_vault.last_nav_nad =
                        hlp_nav_nad_with_prices(market, MarketAsset::Base, current_prices)?;
                    if residual_exposure == 0 {
                        market.base_hlp_vault.cached_settlement_price_nad = current_prices.for_asset(MarketAsset::Base);
                    } else {
                        market.base_hlp_vault.cached_settlement_price_nad = settlement_reference_before;
                    }
                }
                SingleSidedLiquidityReceipt {
                    hlp_amount,
                    ylp_amount,
                    hlp_supply: market.base_hlp_vault.hlp_supply,
                    target_amount_out: base_out,
                    debt_repaid: debt_clearance.debt_reduced,
                    interest_paid,
                    ..SingleSidedLiquidityReceipt::default()
                }
            }
            MarketAsset::Quote => {
                let supply = market.quote_hlp_vault.hlp_supply;
                require_gte!(supply, hlp_amount, ErrorCode::InsufficientBalance);
                let ylp_amount = proportional(market.quote_hlp_vault.ylp_shares, hlp_amount, supply)?;
                let base_debt_shares = proportional_u128(market.quote_hlp_vault.debt_shares, hlp_amount, supply)?;
                let quote_out = ylp_live_underlying_amount(market, MarketAsset::Quote, ylp_amount)?;
                let base_redeemed = ylp_live_underlying_amount(market, MarketAsset::Base, ylp_amount)?;
                let debt_repaid = Debt::aggregate_debt_reduction_for_shares(
                    market.quote_hlp_vault.debt_shares,
                    base_debt_shares,
                    market.debt.base_borrow_index_nad,
                )?;
                let base_hlp_live_debit =
                    proportional(market.quote_hlp_vault.base_hlp_live_reserve, hlp_amount, supply)?;
                let quote_hlp_live_debit =
                    proportional(market.quote_hlp_vault.quote_hlp_live_reserve, hlp_amount, supply)?;
                let quote_out =
                    settled_close_target_amount(market, MarketAsset::Quote, quote_out, base_redeemed, debt_repaid)?;
                release_hlp_backing_inventory(market, MarketAsset::Quote, hlp_amount, supply)?;
                let debt_clearance = market
                    .quote_hlp_vault
                    .clear_debt_repay(base_debt_shares, market.debt.base_borrow_index_nad)?;
                let interest_paid = debt_clearance.interest_paid;
                market.quote_side.debit_reserve(quote_out, true)?;
                debit_hlp_live_reserve(market, MarketAsset::Quote, MarketAsset::Quote, quote_hlp_live_debit)?;
                debit_hlp_live_reserve(market, MarketAsset::Quote, MarketAsset::Base, base_hlp_live_debit)?;
                market.base_side.shares.burn(ylp_amount)?;
                market.quote_side.shares.burn(ylp_amount)?;
                market.base_side.assert_share_backing()?;
                market.quote_side.assert_share_backing()?;
                market.quote_hlp_vault.debit_ylp(ylp_amount)?;
                debit_cash_for_hlp_interest(&mut market.base_side, interest_paid)?;
                market.quote_hlp_vault.burn_hlp(hlp_amount)?;
                if market.quote_hlp_vault.hlp_supply == 0 {
                    require_eq!(
                        market.base_side.reserves.quote_hlp_backing_inventory,
                        0,
                        ErrorCode::BrokenInvariant
                    );
                    require_eq!(
                        market.quote_side.reserves.quote_hlp_backing_inventory,
                        0,
                        ErrorCode::BrokenInvariant
                    );
                    market.quote_hlp_vault.last_nav_nad = 0;
                    market.quote_hlp_vault.cached_settlement_price_nad = 0;
                } else {
                    let current_prices = current_hlp_curve_prices(market)?;
                    market.quote_hlp_vault.last_nav_nad =
                        hlp_nav_nad_with_prices(market, MarketAsset::Quote, current_prices)?;
                    if residual_exposure == 0 {
                        market.quote_hlp_vault.cached_settlement_price_nad =
                            current_prices.for_asset(MarketAsset::Quote);
                    } else {
                        market.quote_hlp_vault.cached_settlement_price_nad = settlement_reference_before;
                    }
                }
                SingleSidedLiquidityReceipt {
                    hlp_amount,
                    ylp_amount,
                    hlp_supply: market.quote_hlp_vault.hlp_supply,
                    target_amount_out: quote_out,
                    debt_repaid: debt_clearance.debt_reduced,
                    interest_paid,
                    ..SingleSidedLiquidityReceipt::default()
                }
            }
        };
        market.refresh_risk()?;
        let health = market.market_health()?;
        market.assert_market_health_snapshot(&health)?;
        market.assert_virtual_reserve_invariant(MarketAsset::Base)?;
        market.assert_virtual_reserve_invariant(MarketAsset::Quote)?;
        Ok(receipt)
    }
}

pub(crate) fn require_residual_hlp_swap_safe(
    market: &Market,
    target_asset: MarketAsset,
    start_prices: HlpCurvePrices,
    end_prices: HlpCurvePrices,
    residual_on_entry: bool,
) -> Result<()> {
    let vault = match target_asset {
        MarketAsset::Base => &market.base_hlp_vault,
        MarketAsset::Quote => &market.quote_hlp_vault,
    };
    // The settlement band is a recovery guard for an already-actionable
    // residual, not a pre-emptive trade-size limit on a settled hLP. A settled
    // vault is corrected from the actual post-trade state below. If that
    // maximum-safe correction leaves a residual, this unchanged settlement
    // reference then prevents later outward flow from compounding it.
    if vault.hlp_supply == 0
        || vault.cached_settlement_price_nad == 0
        || !residual_on_entry
        || vault.residual_exposure == 0
    {
        return Ok(());
    }
    let reference = vault.cached_settlement_price_nad;
    let start_divergence = absolute_difference(start_prices.for_asset(target_asset), reference);
    let end_divergence = absolute_difference(end_prices.for_asset(target_asset), reference);
    let max_divergence = reference
        .checked_mul(market.config.settlement_divergence_bps as u128)
        .and_then(|value| value.checked_div(crate::constants::BPS_DENOMINATOR as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    // Once outside the normal band, only a strictly restoring trade remains
    // executable. This avoids bricking recovery while preventing repeated
    // same-direction flow from accumulating unbounded stale hLP exposure.
    require!(
        end_divergence <= max_divergence || end_divergence < start_divergence,
        ErrorCode::HlpSettlementUnavailable
    );
    Ok(())
}

fn absolute_difference(first: u128, second: u128) -> u128 {
    first.max(second) - first.min(second)
}

pub(crate) fn checkpoint_pre_solve_fee_eligibility(market: &mut Market, receipt: &HlpRebalanceReceipt) -> Result<()> {
    if receipt.ylp_mint_amount == 0 && receipt.ylp_burn_amount == 0 {
        return Ok(());
    }
    checkpoint_hlp_yield_from_ylp_shares(
        market,
        receipt.target_asset,
        receipt.current_swap_fee_eligible_ylp_shares,
    )
}

pub(crate) fn combine_hlp_rebalance_receipts(
    pre: HlpRebalanceReceipt,
    post: HlpRebalanceReceipt,
) -> Result<HlpRebalanceReceipt> {
    require!(pre.target_asset == post.target_asset, ErrorCode::BrokenInvariant);
    let total_mint = pre
        .ylp_mint_amount
        .checked_add(post.ylp_mint_amount)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let total_burn = pre
        .ylp_burn_amount
        .checked_add(post.ylp_burn_amount)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let (ylp_mint_amount, ylp_burn_amount) = if total_mint >= total_burn {
        (total_mint - total_burn, 0)
    } else {
        (0, total_burn - total_mint)
    };
    Ok(HlpRebalanceReceipt {
        target_asset: pre.target_asset,
        ideal_delta: pre
            .ideal_delta
            .checked_add(post.ideal_delta)
            .ok_or(ErrorCode::MarketMathOverflow)?,
        executed_delta: pre
            .executed_delta
            .checked_add(post.executed_delta)
            .ok_or(ErrorCode::MarketMathOverflow)?,
        residual_exposure: post.residual_exposure,
        current_swap_fee_eligible_ylp_shares: 0,
        // Pre- and post-positioning have already changed state. Settle their
        // net token delta once so a direction reversal cannot issue both a
        // mint and a burn CPI for the same hLP side.
        ylp_mint_amount,
        ylp_burn_amount,
        debt_delta: pre
            .debt_delta
            .checked_add(post.debt_delta)
            .ok_or(ErrorCode::MarketMathOverflow)?,
        interest_paid: pre
            .interest_paid
            .checked_add(post.interest_paid)
            .ok_or(ErrorCode::MarketMathOverflow)?,
        nav_nad: post.nav_nad,
        tracking_start_nav_nad: pre.tracking_start_nav_nad,
        tracking_loss_budget_nad: pre.tracking_loss_budget_nad,
        tracking_base_unrealized_interest: pre.tracking_base_unrealized_interest,
        tracking_quote_unrealized_interest: pre.tracking_quote_unrealized_interest,
        tracking_start_ylp_shares: pre.tracking_start_ylp_shares,
        tracking_start_ylp_supply: pre.tracking_start_ylp_supply,
        tracking_retained_contribution_nad: pre.tracking_retained_contribution_nad,
        preposition_capacity_bound: pre.preposition_capacity_bound || post.preposition_capacity_bound,
    })
}

#[derive(Clone, Copy)]
struct ConcentratedHlpStart {
    active: bool,
    tracking: HlpTrackingReference,
    /// Operation-start inventory at the frozen curve mark. Candidate
    /// checkpointing changes only yield indexes, so these raw values are
    /// reusable; derive the proportional valuation lazily only when a side
    /// actually requests a nonzero preposition.
    inventory_values: HlpInventoryValuesNad,
    target_atom_nad: u128,
    economic_nav_nad: u128,
}

#[derive(Clone, Copy)]
struct ConcentratedHlpCandidate {
    base_receipt: HlpRebalanceReceipt,
    quote_receipt: HlpRebalanceReceipt,
    authoritative: Option<(CurveCheckpoint, AmmSwapQuote)>,
    base_principal_tracking_error_nad: i128,
    quote_principal_tracking_error_nad: i128,
    base_tracking_error_nad: i128,
    quote_tracking_error_nad: i128,
    base_trade_tracking_error_nad: i128,
    quote_trade_tracking_error_nad: i128,
    base_reserve_tracking_error_nad: i128,
    quote_reserve_tracking_error_nad: i128,
    base_endpoint_exposure_nad: i128,
    quote_endpoint_exposure_nad: i128,
    base_trade_endpoint_safe: bool,
    quote_trade_endpoint_safe: bool,
    reserve_endpoint_safe: bool,
    settlement_cash_available: bool,
    next_base_delta_nad: i128,
    next_quote_delta_nad: i128,
}

/// Quote coordinates projected for one concentrated hLP candidate. Keeping
/// the canonical/guidance quote machinery behind a non-inlined boundary keeps
/// its prepared-curve and successor-proof temporaries out of the candidate
/// evaluator's SBF stack frame.
#[derive(Clone, Copy)]
struct ConcentratedHlpProjectionCommon {
    amount_in_after_fee: u64,
    retained_surcharge: u64,
    amount_out: u64,
    start_price_nad: u64,
    end_price_nad: u64,
    endpoint_reserves: CurveReservesNad,
    reserve_endpoint_reserves: CurveReservesNad,
    reserve_end_price_nad: u64,
}

#[derive(Clone, Copy)]
enum ConcentratedHlpProjection {
    Guidance {
        common: ConcentratedHlpProjectionCommon,
        endpoints: HlpGuidanceEndpointCapability,
    },
    Authoritative {
        common: ConcentratedHlpProjectionCommon,
        start_checkpoint: CurveCheckpoint,
        quote: AmmSwapQuote,
    },
}

impl ConcentratedHlpProjection {
    const fn common(&self) -> ConcentratedHlpProjectionCommon {
        match self {
            Self::Guidance { common, .. } | Self::Authoritative { common, .. } => *common,
        }
    }

    const fn authoritative(&self) -> bool {
        matches!(self, Self::Authoritative { .. })
    }

    const fn authoritative_quote(&self) -> Option<(CurveCheckpoint, AmmSwapQuote)> {
        match self {
            Self::Authoritative {
                start_checkpoint,
                quote,
                ..
            } => Some((*start_checkpoint, *quote)),
            Self::Guidance { .. } => None,
        }
    }
}

/// Snapshot-bound, planner-only curve endpoints. Unlike `CurveCheckpoint`,
/// this capability deliberately carries same-invariant guidance states and
/// can never authorize or persist a swap.
#[derive(Clone, Copy)]
struct HlpGuidanceEndpointCapability {
    current_slot: u64,
    curve_revision: u64,
    center_price_nad: u64,
    parameters: ConcentrationParameters,
    retain_dynamic_surcharge: bool,
    trade_prepared: ConcentratedGuidanceCurve,
    reserve_prepared: ConcentratedGuidanceCurve,
}

impl HlpGuidanceEndpointCapability {
    fn require_identity(self, market: &Market) -> Result<()> {
        require_eq!(self.current_slot, curve_slot(market), ErrorCode::BrokenInvariant);
        require_eq!(self.curve_revision, market.curve_revision, ErrorCode::BrokenInvariant);
        require_eq!(
            self.center_price_nad,
            market.current_curve_center_price_nad()?,
            ErrorCode::BrokenInvariant
        );
        require!(
            self.parameters == market.current_curve_parameters(self.current_slot),
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            self.retain_dynamic_surcharge,
            market.amm.retain_dynamic_surcharge,
            ErrorCode::BrokenInvariant
        );
        Ok(())
    }

    fn trade_reserves(self) -> CurveReservesNad {
        CurveReservesNad {
            base: self.trade_prepared.base_reserve_nad(),
            quote: self.trade_prepared.quote_reserve_nad(),
        }
    }

    fn reserve_reserves(self) -> CurveReservesNad {
        CurveReservesNad {
            base: self.reserve_prepared.base_reserve_nad(),
            quote: self.reserve_prepared.quote_reserve_nad(),
        }
    }

    /// Canonically solves a fresh reserve state, then immediately erases the
    /// canonical capability.  The compact planner needs this only after a
    /// one-sided socialized loss, where yLP-supply homogeneity cannot predict
    /// the successor invariant.  No `ConcentratedPreparedCurve` or
    /// `CurveCheckpoint` can escape this boundary.
    #[cfg(test)]
    fn fresh_guidance_for_reserves(
        self,
        market: &Market,
        reserves: CurveReservesNad,
    ) -> Result<ConcentratedGuidanceCurve> {
        self.require_identity(market)?;
        let canonical = market.prepare_curve_for_reserves_nad(
            reserves,
            self.center_price_nad,
            self.current_slot,
        )?;
        canonical.prepare_guidance_successor_with_invariant(
            reserves.base,
            reserves.quote,
            canonical.invariant_d(),
        )
    }
}

/// Operation-bound basis for the test-only compact guidance planner.  The
/// only curve object it can carry is `ConcentratedGuidanceCurve`; no method
/// exposes a canonical prepared curve, checkpoint, quote, or persisted AMM
/// state.  Production `LifecycleProjected` remains unchanged until the full
/// compact differential is green.
#[cfg(test)]
#[derive(Clone, Copy)]
struct ConcentratedGuidanceBasis {
    start: ConcentratedGuidanceCurve,
    start_ylp_supply: u64,
    current_slot: u64,
    curve_revision: u64,
    center_price_nad: u64,
    parameters: ConcentrationParameters,
    asset_in: MarketAsset,
    reserve_credit: u64,
    pre_state: DynamicFeePreState,
    preliminary: PreliminarySwapInputs,
}

#[cfg(test)]
#[derive(Clone, Copy)]
struct HlpCompactGuidanceQuote {
    common: ConcentratedHlpProjectionCommon,
    trade: ConcentratedGuidanceCurve,
    reserve: ConcentratedGuidanceCurve,
    retain_dynamic_surcharge: bool,
    start_ylp_supply: u64,
}

#[cfg(test)]
impl ConcentratedGuidanceBasis {
    fn capture(market: &Market, context: &ConcentratedHlpSolveContext) -> Result<Self> {
        let reserves = market.curve_reserves_nad()?;
        let start = context.guidance_start_prepared.prepare_guidance_successor_with_invariant(
            reserves.base,
            reserves.quote,
            context.guidance_start_prepared.invariant_d(),
        )?;
        Ok(Self {
            start,
            start_ylp_supply: context.guidance_start_ylp_supply,
            current_slot: context.current_slot,
            curve_revision: market.curve_revision,
            center_price_nad: market.current_curve_center_price_nad()?,
            parameters: market.current_curve_parameters(context.current_slot),
            asset_in: context.asset_in,
            reserve_credit: context.reserve_credit,
            pre_state: context.pre_state,
            preliminary: context.preliminary,
        })
    }

    fn require_identity(self, market: &Market, context: &ConcentratedHlpSolveContext) -> Result<()> {
        require_eq!(self.current_slot, context.current_slot, ErrorCode::BrokenInvariant);
        require_eq!(self.curve_revision, market.curve_revision, ErrorCode::BrokenInvariant);
        require_eq!(
            self.center_price_nad,
            market.current_curve_center_price_nad()?,
            ErrorCode::BrokenInvariant
        );
        require!(
            self.parameters == market.current_curve_parameters(self.current_slot),
            ErrorCode::BrokenInvariant
        );
        require!(self.asset_in == context.asset_in, ErrorCode::BrokenInvariant);
        require_eq!(self.reserve_credit, context.reserve_credit, ErrorCode::BrokenInvariant);
        require!(self.pre_state == context.pre_state, ErrorCode::BrokenInvariant);
        require!(self.preliminary == context.preliminary, ErrorCode::BrokenInvariant);
        Ok(())
    }

    fn prepared_for(
        self,
        fixed: HlpPlannerStatic,
        state: HlpPlannerState,
    ) -> Result<ConcentratedGuidanceCurve> {
        let supply = state.base_side.ylp_supply;
        require_eq!(supply, state.quote_side.ylp_supply, ErrorCode::BrokenInvariant);
        require!(self.start_ylp_supply > 0 && supply > 0, ErrorCode::SupplyUnderflow);
        require_eq!(self.start_ylp_supply, fixed.start_ylp_supply, ErrorCode::BrokenInvariant);
        let scaled_invariant_d = mul_div_u128(
            self.start.invariant_d(),
            supply as u128,
            self.start_ylp_supply as u128,
        )?;
        let reserves = state.curve_reserves_nad(fixed)?;
        self.start
            .prepare_guidance_successor_with_invariant(reserves.base, reserves.quote, scaled_invariant_d)
    }

    fn quote_bounded(
        self,
        market: &Market,
        context: &ConcentratedHlpSolveContext,
        fixed: HlpPlannerStatic,
        state: HlpPlannerState,
        inventory_changed: bool,
    ) -> Result<HlpCompactGuidanceQuote> {
        self.require_identity(market, context)?;
        HLP_COMPACT_GUIDANCE_CELLS.with(|count| count.set(count.get().saturating_add(1)));
        let prepared = self.prepared_for(fixed, state)?;
        let reserves = state.curve_reserves_nad(fixed)?;
        let amount_in_after_fee = market.exact_swap_input_for_prepared_guidance(
            self.asset_in,
            self.reserve_credit,
            reserves,
            self.pre_state,
            self.preliminary,
            prepared,
        )?;
        let amount_in_nad = normalize_to_nad(
            amount_in_after_fee as u128,
            fixed.decimals(self.asset_in),
        )?;
        let direction = match self.asset_in {
            MarketAsset::Base => ConcentratedSwapDirection::BaseToQuote,
            MarketAsset::Quote => ConcentratedSwapDirection::QuoteToBase,
        };
        let probes_before = crate::math::residual_evaluations();
        let amount_out_nad = prepared.quote_bounded_exact_in(amount_in_nad, direction)?;
        let probes = crate::math::residual_evaluations().saturating_sub(probes_before);
        require!(probes <= 2, ErrorCode::BrokenInvariant);
        HLP_COMPACT_GUIDANCE_EXACT_IN_PROBES.with(|count| {
            count.set(count.get().saturating_add(u32::try_from(probes).unwrap_or(u32::MAX)))
        });
        let amount_out = denormalize_from_nad_floor(
            amount_out_nad,
            fixed.decimals(self.asset_in.opposite()),
        )?;
        require!(amount_out > 0, ErrorCode::InsufficientOutputAmount);
        let executable_output_nad = normalize_to_nad(
            amount_out as u128,
            fixed.decimals(self.asset_in.opposite()),
        )?;
        let endpoint_reserves = match self.asset_in {
            MarketAsset::Base => CurveReservesNad {
                base: reserves
                    .base
                    .checked_add(amount_in_nad)
                    .ok_or(ErrorCode::ReserveOverflow)?,
                quote: reserves
                    .quote
                    .checked_sub(executable_output_nad)
                    .ok_or(ErrorCode::ReserveUnderflow)?,
            },
            MarketAsset::Quote => CurveReservesNad {
                base: reserves
                    .base
                    .checked_sub(executable_output_nad)
                    .ok_or(ErrorCode::ReserveUnderflow)?,
                quote: reserves
                    .quote
                    .checked_add(amount_in_nad)
                    .ok_or(ErrorCode::ReserveOverflow)?,
            },
        };
        let trade = prepared.prepare_guidance_successor(endpoint_reserves.base, endpoint_reserves.quote)?;
        let retain_dynamic_surcharge = if inventory_changed {
            fixed.retain_dynamic_surcharge_after_inventory
        } else {
            fixed.retain_dynamic_surcharge_at_start
        };
        let reserve_input_credit = if retain_dynamic_surcharge {
            self.preliminary.reserve_input_credit
        } else {
            amount_in_after_fee
        };
        let retained_surcharge = reserve_input_credit
            .checked_sub(amount_in_after_fee)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        let mut reserve_endpoint_reserves = endpoint_reserves;
        if retained_surcharge > 0 {
            let retained_nad = normalize_to_nad(
                retained_surcharge as u128,
                fixed.decimals(self.asset_in),
            )?;
            match self.asset_in {
                MarketAsset::Base => {
                    reserve_endpoint_reserves.base = reserve_endpoint_reserves
                        .base
                        .checked_add(retained_nad)
                        .ok_or(ErrorCode::ReserveOverflow)?;
                }
                MarketAsset::Quote => {
                    reserve_endpoint_reserves.quote = reserve_endpoint_reserves
                        .quote
                        .checked_add(retained_nad)
                        .ok_or(ErrorCode::ReserveOverflow)?;
                }
            }
        }
        // Retained surcharge is a non-homogeneous reserve credit. Keep it
        // guidance-typed and same-D in this bounded predictor; the separate
        // exact authority remains the only canonical successor proof.
        let reserve = trade.prepare_guidance_successor(
            reserve_endpoint_reserves.base,
            reserve_endpoint_reserves.quote,
        )?;
        Ok(HlpCompactGuidanceQuote {
            common: ConcentratedHlpProjectionCommon {
                amount_in_after_fee,
                retained_surcharge,
                amount_out,
                start_price_nad: u64::try_from(prepared.marginal_price_nad()?)
                    .map_err(|_| ErrorCode::MarketMathOverflow)?,
                end_price_nad: u64::try_from(trade.marginal_price_nad()?)
                    .map_err(|_| ErrorCode::MarketMathOverflow)?,
                endpoint_reserves,
                reserve_endpoint_reserves,
                reserve_end_price_nad: u64::try_from(reserve.marginal_price_nad()?)
                    .map_err(|_| ErrorCode::MarketMathOverflow)?,
            },
            trade,
            reserve,
            retain_dynamic_surcharge,
            start_ylp_supply: state.base_side.ylp_supply,
        })
    }
}

#[derive(Clone, Copy)]
struct ConcentratedHlpSideTracking {
    principal_error_nad: i128,
    error_nad: i128,
    trade_error_nad: i128,
    reserve_error_nad: i128,
    retained_contribution_nad: i128,
    endpoint_exposure_nad: i128,
    trade_endpoint_safe: bool,
    reserve_endpoint_safe: bool,
}

#[derive(Clone, Copy, Default)]
struct HlpExactSampleRows {
    base_combined_nad: i128,
    base_trade_nad: i128,
    quote_combined_nad: i128,
    quote_trade_nad: i128,
}

impl HlpExactSampleRows {
    fn from_candidate(candidate: &ConcentratedHlpCandidate) -> Self {
        Self {
            base_combined_nad: candidate.base_tracking_error_nad,
            base_trade_nad: candidate.base_trade_tracking_error_nad,
            quote_combined_nad: candidate.quote_tracking_error_nad,
            quote_trade_nad: candidate.quote_trade_tracking_error_nad,
        }
    }

    const fn value(self, asset: MarketAsset, row: HlpExactControlRow) -> i128 {
        match (asset, row) {
            (MarketAsset::Base, HlpExactControlRow::Combined) => self.base_combined_nad,
            (MarketAsset::Base, HlpExactControlRow::Trade) => self.base_trade_nad,
            (MarketAsset::Quote, HlpExactControlRow::Combined) => self.quote_combined_nad,
            (MarketAsset::Quote, HlpExactControlRow::Trade) => self.quote_trade_nad,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct HlpFiniteDifferenceBasis {
    origin: HlpExactSampleRows,
    base_probe_delta_nad: i128,
    base_probe: HlpExactSampleRows,
    quote_probe_delta_nad: i128,
    quote_probe: HlpExactSampleRows,
    base_probe_recorded: bool,
    quote_probe_recorded: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HlpSolvePhase {
    ProjectSeed,
    PlanCenter,
    PlanBaseAxis,
    PlanQuoteAxis,
    PlanFinal,
    AuthorizeCorrected,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HlpCandidateEvaluationMode {
    LifecycleProjected,
    Authoritative,
}

impl HlpCandidateEvaluationMode {
    const fn authoritative(self) -> bool {
        matches!(self, Self::Authoritative)
    }

    const fn runs_lifecycle(self) -> bool {
        true
    }

    const fn exact_quote(self) -> bool {
        matches!(self, Self::Authoritative)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HlpExactControlRow {
    Combined,
    Trade,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct HlpPrepositionSignature {
    ylp_mint_amount: u64,
    ylp_burn_amount: u64,
    debt_delta: i128,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct HlpLifecycleTracking {
    base_principal_error_nad: i128,
    base_error_nad: i128,
    base_trade_error_nad: i128,
    base_reserve_error_nad: i128,
    base_retained_contribution_nad: i128,
    base_exposure_nad: i128,
    quote_principal_error_nad: i128,
    quote_error_nad: i128,
    quote_trade_error_nad: i128,
    quote_reserve_error_nad: i128,
    quote_retained_contribution_nad: i128,
    quote_exposure_nad: i128,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HlpLifecycleEndpoint {
    principal_nav_nad: i128,
    opposite_exposure_nad: i128,
}

#[derive(Clone, Copy)]
enum HlpLifecycleEndpointMode {
    Guidance(HlpGuidanceEndpointCapability),
    #[cfg(test)]
    CanonicalGuidance {
        trade: CurveCheckpoint,
        reserve: CurveCheckpoint,
    },
    Authoritative {
        trade: CurveCheckpoint,
        reserve: CurveCheckpoint,
    },
}

#[derive(Clone, Copy)]
struct HlpAuthoritativeLifecycleArgs {
    amount_in_after_fee: u64,
    retained_surcharge: u64,
    amount_out: u64,
    endpoints: HlpLifecycleEndpointMode,
    expected_trade_price_nad: u64,
    expected_reserve_price_nad: u64,
}

#[derive(Clone, Copy)]
struct HlpCandidatePreposition {
    base_receipt: HlpRebalanceReceipt,
    quote_receipt: HlpRebalanceReceipt,
    preliminary: PreliminarySwapInputs,
}

#[derive(Clone, Copy)]
struct ConcentratedHlpSolveContext {
    base_start: ConcentratedHlpStart,
    quote_start: ConcentratedHlpStart,
    frozen_prices: HlpCurvePrices,
    asset_in: MarketAsset,
    reserve_credit: u64,
    current_slot: u64,
    pre_state: DynamicFeePreState,
    preliminary: PreliminarySwapInputs,
    cash_policy: SwapCashPolicy,
    guidance_start_prepared: ConcentratedPreparedCurve,
    guidance_start_ylp_supply: u64,
}

/// Jointly pre-positions both hLP numeraires against one exact applied-curve
/// lifecycle endpoint. Candidate transitions use the real reserve, share, and
/// debt mutations in deterministic base-then-quote order; only a candidate
/// inside both vault-local combined tracking budgets can become the quoted
/// market state.
pub(crate) fn pre_solve_hlps_for_swap_joint(
    market: &mut Market,
    asset_in: MarketAsset,
    reserve_credit: u64,
    current_slot: u64,
    pre_state: DynamicFeePreState,
    preliminary: PreliminarySwapInputs,
    cash_policy: SwapCashPolicy,
) -> Result<(HlpRebalanceReceipt, HlpRebalanceReceipt, AmmSwapQuote)> {
    let mut snapshot = Box::<Market>::default();
    snapshot.as_mut().clone_from(market);
    let guidance_start_reserves = snapshot.curve_reserves_nad()?;
    let guidance_start_prepared = snapshot.prepare_curve_for_reserves_nad(
        guidance_start_reserves,
        snapshot.current_curve_center_price_nad()?,
        current_slot,
    )?;
    let frozen_prices = hlp_curve_prices_from_base_price_nad(guidance_start_prepared.marginal_price_nad()?)?;
    let base_start = concentrated_hlp_start(&snapshot, MarketAsset::Base, frozen_prices)?;
    let quote_start = concentrated_hlp_start(&snapshot, MarketAsset::Quote, frozen_prices)?;
    let context = ConcentratedHlpSolveContext {
        base_start,
        quote_start,
        frozen_prices,
        asset_in,
        reserve_credit,
        current_slot,
        pre_state,
        preliminary,
        cash_policy,
        guidance_start_prepared,
        guidance_start_ylp_supply: snapshot.base_side.shares.ylp_supply,
    };
    require!(
        context.base_start.active || context.quote_start.active,
        ErrorCode::HlpSettlementUnavailable
    );

    solve_concentrated_hlp_candidates(market, &snapshot, &context)
}

#[inline(never)]
fn solve_concentrated_hlp_candidates(
    market: &mut Market,
    snapshot: &Market,
    context: &ConcentratedHlpSolveContext,
) -> Result<(HlpRebalanceReceipt, HlpRebalanceReceipt, AmmSwapQuote)> {
    let result = solve_concentrated_hlp_candidates_inner(market, snapshot, context);
    if result.is_err() {
        market.clone_from(snapshot);
    }
    result
}

#[inline(never)]
fn solve_concentrated_hlp_candidates_inner(
    market: &mut Market,
    snapshot: &Market,
    context: &ConcentratedHlpSolveContext,
) -> Result<(HlpRebalanceReceipt, HlpRebalanceReceipt, AmmSwapQuote)> {
    debug_log_heap(10);
    // One solver-wide scratch market preserves each authoritative candidate's
    // preposition while its full lifecycle is evaluated. Reusing this box
    // avoids both an unsafe >4 KiB stack frame and per-candidate allocation.
    let mut lifecycle_scratch = Box::<Market>::default();
    // Projection payloads contain either two opaque prepared guidance curves
    // or a full exact quote/checkpoint bundle. Reuse one heap slot so neither
    // payload inflates the per-candidate SBF stack frame.
    let mut projection_scratch = Box::<Option<ConcentratedHlpProjection>>::default();
    let mut base_delta_nad = 0_i128;
    let mut quote_delta_nad = 0_i128;
    // All exact finite-difference samples must represent the same function.
    // Freeze the operation-start floor; any sample whose actual settlement
    // needs bind it is rejected rather than mixed into the derivative basis.
    let cash_floors = context.cash_policy.floors(snapshot, context.asset_in, 0)?;
    let mut candidate: Option<ConcentratedHlpCandidate>;
    let mut phase = HlpSolvePhase::ProjectSeed;
    let mut basis = HlpFiniteDifferenceBasis::default();
    let mut center_base_delta_nad = 0_i128;
    let mut center_quote_delta_nad = 0_i128;
    let mut base_row = HlpExactControlRow::Combined;
    let mut quote_row = HlpExactControlRow::Combined;
    let mut center_base_signature = HlpPrepositionSignature::default();
    let mut center_quote_signature = HlpPrepositionSignature::default();
    let mut center_canonicalized = false;
    let mut center_next_base_delta_nad = 0_i128;
    let mut center_next_quote_delta_nad = 0_i128;
    let mut base_axis_next_delta_nad = 0_i128;
    let mut final_from_reflected_guidance = false;
    let mut authoritative_evaluations = 0_u32;
    for evaluation_index in 0..HLP_CONCENTRATED_MAX_CANDIDATE_EVALUATIONS {
        let mode = match phase {
            HlpSolvePhase::ProjectSeed
            | HlpSolvePhase::PlanCenter
            | HlpSolvePhase::PlanBaseAxis
            | HlpSolvePhase::PlanQuoteAxis
            | HlpSolvePhase::PlanFinal => HlpCandidateEvaluationMode::LifecycleProjected,
            HlpSolvePhase::AuthorizeCorrected => HlpCandidateEvaluationMode::Authoritative,
        };
        if mode.authoritative() {
            authoritative_evaluations = authoritative_evaluations
                .checked_add(1)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            require_gte!(
                HLP_CONCENTRATED_MAX_AUTHORITATIVE_EVALUATIONS,
                authoritative_evaluations,
                ErrorCode::BrokenInvariant
            );
        }
        market.clone_from(snapshot);
        candidate = None;
        let evaluation = evaluate_concentrated_hlp_candidate(
            market,
            lifecycle_scratch.as_mut(),
            snapshot,
            context,
            cash_floors,
            base_delta_nad,
            quote_delta_nad,
            mode,
            projection_scratch.as_mut(),
            &mut candidate,
        );
        debug_log_heap(20 + u64::from(evaluation_index));
        if evaluation.is_err() {
            market.clone_from(snapshot);
            if phase == HlpSolvePhase::ProjectSeed {
                // The same-D endpoint mark is guidance only and has a
                // narrower arithmetic domain than the canonical successor.
                // Never let its failure reject a candidate that the exact
                // quote may safely authorize.
                base_delta_nad = 0;
                quote_delta_nad = 0;
                phase = HlpSolvePhase::AuthorizeCorrected;
                continue;
            }
            return err!(ErrorCode::HlpSettlementUnavailable);
        }
        let candidate = candidate.ok_or(ErrorCode::BrokenInvariant)?;
        let base_lifecycle_safe = concentrated_hlp_candidate_components_are_safe(
            context.base_start,
            candidate.base_principal_tracking_error_nad,
            candidate.base_tracking_error_nad,
            candidate.base_endpoint_exposure_nad,
        );
        let quote_lifecycle_safe = concentrated_hlp_candidate_components_are_safe(
            context.quote_start,
            candidate.quote_principal_tracking_error_nad,
            candidate.quote_tracking_error_nad,
            candidate.quote_endpoint_exposure_nad,
        );
        let base_safe = base_lifecycle_safe && candidate.base_trade_endpoint_safe;
        let quote_safe = quote_lifecycle_safe && candidate.quote_trade_endpoint_safe;
        let fully_safe = base_safe && quote_safe && candidate.reserve_endpoint_safe;
        if mode.authoritative() && candidate.settlement_cash_available && fully_safe {
            let (start_checkpoint, quote) = candidate.authoritative.ok_or(ErrorCode::BrokenInvariant)?;
            if candidate.base_receipt.ylp_mint_amount != 0
                || candidate.base_receipt.ylp_burn_amount != 0
                || candidate.quote_receipt.ylp_mint_amount != 0
                || candidate.quote_receipt.ylp_burn_amount != 0
            {
                if market
                    .checkpoint_amm_neutral_inventory_from_quote(start_checkpoint, context.current_slot)
                    .is_err()
                {
                    market.clone_from(snapshot);
                    return err!(ErrorCode::HlpSettlementUnavailable);
                }
            }
            return Ok((candidate.base_receipt, candidate.quote_receipt, quote));
        }
        match phase {
            HlpSolvePhase::ProjectSeed => {
                if fully_safe {
                    phase = HlpSolvePhase::PlanFinal;
                    continue;
                }
                center_base_delta_nad = if context.base_start.active {
                    candidate.next_base_delta_nad
                } else {
                    0
                };
                center_quote_delta_nad = if context.quote_start.active {
                    candidate.next_quote_delta_nad
                } else {
                    0
                };
                base_delta_nad = center_base_delta_nad;
                quote_delta_nad = center_quote_delta_nad;
                phase = HlpSolvePhase::PlanCenter;
            }
            HlpSolvePhase::PlanCenter => {
                let center_capacity_bound = (context.base_start.active
                    && candidate.base_receipt.preposition_capacity_bound)
                    || (context.quote_start.active && candidate.quote_receipt.preposition_capacity_bound);
                if center_capacity_bound && !center_canonicalized {
                    let canonical_base = if candidate.base_receipt.preposition_capacity_bound {
                        hlp_preposition_coordinate_from_debt(snapshot, context.frozen_prices, candidate.base_receipt)?
                    } else {
                        center_base_delta_nad
                    };
                    let canonical_quote = if candidate.quote_receipt.preposition_capacity_bound {
                        hlp_preposition_coordinate_from_debt(snapshot, context.frozen_prices, candidate.quote_receipt)?
                    } else {
                        center_quote_delta_nad
                    };
                    require!(
                        (canonical_base, canonical_quote) != (center_base_delta_nad, center_quote_delta_nad),
                        ErrorCode::HlpSettlementUnavailable
                    );
                    center_base_delta_nad = canonical_base;
                    center_quote_delta_nad = canonical_quote;
                    base_delta_nad = canonical_base;
                    quote_delta_nad = canonical_quote;
                    center_canonicalized = true;
                    continue;
                }
                if fully_safe {
                    phase = HlpSolvePhase::PlanFinal;
                    continue;
                }
                require!(
                    candidate.settlement_cash_available && (!center_capacity_bound || center_canonicalized),
                    ErrorCode::HlpSettlementUnavailable
                );
                base_row = hlp_exact_control_row(&candidate, MarketAsset::Base, context.base_start)
                    .ok_or(ErrorCode::HlpSettlementUnavailable)?;
                quote_row = hlp_exact_control_row(&candidate, MarketAsset::Quote, context.quote_start)
                    .ok_or(ErrorCode::HlpSettlementUnavailable)?;
                basis.origin = HlpExactSampleRows::from_candidate(&candidate);
                let origin_base_error_nad = basis.origin.value(MarketAsset::Base, base_row);
                let origin_quote_error_nad = basis.origin.value(MarketAsset::Quote, quote_row);
                center_base_signature = hlp_preposition_signature(candidate.base_receipt);
                center_quote_signature = hlp_preposition_signature(candidate.quote_receipt);
                center_next_base_delta_nad = candidate.next_base_delta_nad;
                center_next_quote_delta_nad = candidate.next_quote_delta_nad;
                basis.base_probe_delta_nad =
                    hlp_exact_axis_probe_delta(center_base_delta_nad, context.base_start, origin_base_error_nad)
                        .unwrap_or(0);
                basis.quote_probe_delta_nad =
                    hlp_exact_axis_probe_delta(center_quote_delta_nad, context.quote_start, origin_quote_error_nad)
                        .unwrap_or(0);
                if context.base_start.active {
                    require!(basis.base_probe_delta_nad != 0, ErrorCode::HlpSettlementUnavailable);
                    base_delta_nad = center_base_delta_nad
                        .checked_add(basis.base_probe_delta_nad)
                        .ok_or(ErrorCode::MarketMathOverflow)?;
                    quote_delta_nad = center_quote_delta_nad;
                    phase = HlpSolvePhase::PlanBaseAxis;
                } else {
                    require!(basis.quote_probe_delta_nad != 0, ErrorCode::HlpSettlementUnavailable);
                    base_delta_nad = center_base_delta_nad;
                    quote_delta_nad = center_quote_delta_nad
                        .checked_add(basis.quote_probe_delta_nad)
                        .ok_or(ErrorCode::MarketMathOverflow)?;
                    phase = HlpSolvePhase::PlanQuoteAxis;
                }
            }
            HlpSolvePhase::PlanBaseAxis => {
                require!(
                    candidate.settlement_cash_available
                        && hlp_exact_derivative_sample_is_unbound(&candidate, context)
                        && hlp_preposition_signature(candidate.base_receipt) != center_base_signature,
                    ErrorCode::HlpSettlementUnavailable
                );
                basis.base_probe = HlpExactSampleRows::from_candidate(&candidate);
                basis.base_probe_recorded = true;
                base_axis_next_delta_nad = candidate.next_base_delta_nad;
                if context.quote_start.active {
                    require!(basis.quote_probe_delta_nad != 0, ErrorCode::HlpSettlementUnavailable);
                    base_delta_nad = center_base_delta_nad;
                    quote_delta_nad = center_quote_delta_nad
                        .checked_add(basis.quote_probe_delta_nad)
                        .ok_or(ErrorCode::MarketMathOverflow)?;
                    phase = HlpSolvePhase::PlanQuoteAxis;
                } else {
                    let origin_base_error = basis.origin.value(MarketAsset::Base, base_row);
                    let origin_quote_error = basis.origin.value(MarketAsset::Quote, quote_row);
                    let boundary_base_error = hlp_initial_active_set_residual(origin_base_error, context.base_start)?;
                    let boundary_quote_error =
                        hlp_initial_active_set_residual(origin_quote_error, context.quote_start)?;
                    let (base_step, quote_step) = basis
                        .solve_step(
                            context,
                            center_base_delta_nad,
                            center_quote_delta_nad,
                            base_row,
                            quote_row,
                            origin_base_error,
                            origin_quote_error,
                        )
                        .or_else(|| {
                            basis.solve_step(
                                context,
                                center_base_delta_nad,
                                center_quote_delta_nad,
                                base_row,
                                quote_row,
                                boundary_base_error,
                                boundary_quote_error,
                            )
                        })
                        .ok_or(ErrorCode::HlpSettlementUnavailable)?;
                    base_delta_nad = center_base_delta_nad
                        .checked_add(base_step)
                        .ok_or(ErrorCode::MarketMathOverflow)?;
                    quote_delta_nad = center_quote_delta_nad
                        .checked_add(quote_step)
                        .ok_or(ErrorCode::MarketMathOverflow)?;
                    require!(
                        basis
                            .candidate_is_distinct(
                                center_base_delta_nad,
                                center_quote_delta_nad,
                                base_delta_nad,
                                quote_delta_nad,
                                center_base_delta_nad,
                                center_quote_delta_nad,
                            )
                            .unwrap_or(false)
                            && hlp_coordinate_within_center_trust(
                                center_base_delta_nad,
                                base_delta_nad,
                                context.base_start,
                            )
                            && hlp_coordinate_within_center_trust(
                                center_quote_delta_nad,
                                quote_delta_nad,
                                context.quote_start,
                            ),
                        ErrorCode::HlpSettlementUnavailable
                    );
                    phase = HlpSolvePhase::PlanFinal;
                }
            }
            HlpSolvePhase::PlanQuoteAxis => {
                require!(
                    candidate.settlement_cash_available
                        && hlp_exact_derivative_sample_is_unbound(&candidate, context)
                        && hlp_preposition_signature(candidate.quote_receipt) != center_quote_signature,
                    ErrorCode::HlpSettlementUnavailable
                );
                basis.quote_probe = HlpExactSampleRows::from_candidate(&candidate);
                basis.quote_probe_recorded = true;
                let origin_base_error = basis.origin.value(MarketAsset::Base, base_row);
                let origin_quote_error = basis.origin.value(MarketAsset::Quote, quote_row);
                let boundary_base_error = hlp_initial_active_set_residual(origin_base_error, context.base_start)?;
                let boundary_quote_error = hlp_initial_active_set_residual(origin_quote_error, context.quote_start)?;
                let zero_step = basis.solve_step(
                    context,
                    center_base_delta_nad,
                    center_quote_delta_nad,
                    base_row,
                    quote_row,
                    origin_base_error,
                    origin_quote_error,
                );
                let reflected_step = zero_step
                    .is_none()
                    .then(|| {
                        hlp_reflected_guidance_step(
                            context,
                            center_base_delta_nad,
                            center_quote_delta_nad,
                            center_next_base_delta_nad,
                            center_next_quote_delta_nad,
                            base_axis_next_delta_nad,
                            candidate.next_quote_delta_nad,
                        )
                    })
                    .flatten();
                final_from_reflected_guidance = reflected_step.is_some();
                let (base_step, quote_step) = zero_step
                    .or(reflected_step)
                    .or_else(|| {
                        basis.solve_step(
                            context,
                            center_base_delta_nad,
                            center_quote_delta_nad,
                            base_row,
                            quote_row,
                            boundary_base_error,
                            boundary_quote_error,
                        )
                    })
                    .ok_or(ErrorCode::HlpSettlementUnavailable)?;
                base_delta_nad = center_base_delta_nad
                    .checked_add(base_step)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                quote_delta_nad = center_quote_delta_nad
                    .checked_add(quote_step)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                require!(
                    basis
                        .candidate_is_distinct(
                            center_base_delta_nad,
                            center_quote_delta_nad,
                            base_delta_nad,
                            quote_delta_nad,
                            center_base_delta_nad,
                            center_quote_delta_nad,
                        )
                        .unwrap_or(false)
                        && hlp_coordinate_within_center_trust(
                            center_base_delta_nad,
                            base_delta_nad,
                            context.base_start,
                        )
                        && hlp_coordinate_within_center_trust(
                            center_quote_delta_nad,
                            quote_delta_nad,
                            context.quote_start,
                        ),
                    ErrorCode::HlpSettlementUnavailable
                );
                phase = HlpSolvePhase::PlanFinal;
            }
            HlpSolvePhase::PlanFinal => {
                if fully_safe {
                    phase = HlpSolvePhase::AuthorizeCorrected;
                    continue;
                }
                require!(
                    candidate.settlement_cash_available
                        && candidate.reserve_endpoint_safe
                        && hlp_exact_derivative_sample_is_unbound(&candidate, context),
                    ErrorCode::HlpSettlementUnavailable
                );
                let retry_base_row = hlp_exact_control_row(&candidate, MarketAsset::Base, context.base_start)
                    .ok_or(ErrorCode::HlpSettlementUnavailable)?;
                let retry_quote_row = hlp_exact_control_row(&candidate, MarketAsset::Quote, context.quote_start)
                    .ok_or(ErrorCode::HlpSettlementUnavailable)?;
                let current_base_error_nad = hlp_exact_control_value(&candidate, MarketAsset::Base, retry_base_row);
                let current_quote_error_nad = hlp_exact_control_value(&candidate, MarketAsset::Quote, retry_quote_row);
                let (mut solved_base_delta_nad, mut solved_quote_delta_nad) = basis
                    .solve_broyden_candidate(
                        context,
                        center_base_delta_nad,
                        center_quote_delta_nad,
                        base_delta_nad,
                        quote_delta_nad,
                        retry_base_row,
                        retry_quote_row,
                        current_base_error_nad,
                        current_quote_error_nad,
                    )
                    .ok_or(ErrorCode::HlpSettlementUnavailable)?;
                if final_from_reflected_guidance && (base_safe != quote_safe) {
                    if !base_safe {
                        solved_base_delta_nad = hlp_adjacent_share_phase_coordinate(
                            market,
                            snapshot,
                            context,
                            cash_floors,
                            candidate.base_receipt,
                            base_delta_nad,
                            quote_delta_nad,
                            solved_base_delta_nad,
                        )?;
                        solved_quote_delta_nad = quote_delta_nad;
                    } else {
                        solved_quote_delta_nad = hlp_adjacent_share_phase_coordinate(
                            market,
                            snapshot,
                            context,
                            cash_floors,
                            candidate.quote_receipt,
                            base_delta_nad,
                            quote_delta_nad,
                            solved_quote_delta_nad,
                        )?;
                        solved_base_delta_nad = base_delta_nad;
                    }
                }
                base_delta_nad = hlp_force_adjacent_atom_if_needed(
                    snapshot,
                    context.frozen_prices,
                    context.base_start,
                    candidate.base_receipt,
                    base_delta_nad,
                    solved_base_delta_nad,
                    current_base_error_nad,
                )?;
                quote_delta_nad = hlp_force_adjacent_atom_if_needed(
                    snapshot,
                    context.frozen_prices,
                    context.quote_start,
                    candidate.quote_receipt,
                    quote_delta_nad,
                    solved_quote_delta_nad,
                    current_quote_error_nad,
                )?;
                phase = HlpSolvePhase::AuthorizeCorrected;
            }
            HlpSolvePhase::AuthorizeCorrected => break,
        }
    }

    market.clone_from(snapshot);
    err!(ErrorCode::HlpSettlementUnavailable)
}

fn hlp_preposition_signature(receipt: HlpRebalanceReceipt) -> HlpPrepositionSignature {
    HlpPrepositionSignature {
        ylp_mint_amount: receipt.ylp_mint_amount,
        ylp_burn_amount: receipt.ylp_burn_amount,
        debt_delta: receipt.debt_delta,
    }
}

fn hlp_preposition_coordinate_from_debt(
    market: &Market,
    prices: HlpCurvePrices,
    receipt: HlpRebalanceReceipt,
) -> Result<i128> {
    let value = asset_value_in_target_nad_with_prices(
        market,
        prices,
        receipt.target_asset.opposite(),
        u64::try_from(receipt.debt_delta.unsigned_abs()).map_err(|_| ErrorCode::MarketMathOverflow)?,
        receipt.target_asset,
    )?;
    let value = i128::try_from(value).map_err(|_| ErrorCode::MarketMathOverflow)?;
    if receipt.debt_delta < 0 {
        value.checked_neg().ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    } else {
        Ok(value)
    }
}

fn hlp_force_adjacent_atom_if_needed(
    market: &Market,
    prices: HlpCurvePrices,
    start: ConcentratedHlpStart,
    receipt: HlpRebalanceReceipt,
    current_delta_nad: i128,
    proposed_delta_nad: i128,
    current_error_nad: i128,
) -> Result<i128> {
    if !start.active
        || start.tracking.loss_budget_nad > start.target_atom_nad
        || proposed_delta_nad == current_delta_nad
        || receipt.debt_delta == 0
        || current_error_nad.unsigned_abs() <= start.tracking.loss_budget_nad
    {
        return Ok(proposed_delta_nad);
    }
    let canonical_current = hlp_preposition_coordinate_from_debt(market, prices, receipt)?;
    let debt_magnitude = receipt.debt_delta.unsigned_abs();
    let coordinate_atom = canonical_current
        .unsigned_abs()
        .checked_add(debt_magnitude.checked_sub(1).ok_or(ErrorCode::MarketMathOverflow)?)
        .ok_or(ErrorCode::MarketMathOverflow)?
        .checked_div(debt_magnitude)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    if proposed_delta_nad.abs_diff(current_delta_nad) >= coordinate_atom {
        return Ok(proposed_delta_nad);
    }
    let direction = if proposed_delta_nad > current_delta_nad { 1 } else { -1 };
    let mut adjacent = receipt;
    adjacent.debt_delta = adjacent
        .debt_delta
        .checked_add(direction)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    hlp_preposition_coordinate_from_debt(market, prices, adjacent)
}

fn hlp_adjacent_share_phase_coordinate(
    market: &mut Market,
    snapshot: &Market,
    context: &ConcentratedHlpSolveContext,
    cash_floors: SwapCashFloors,
    receipt: HlpRebalanceReceipt,
    current_base_delta_nad: i128,
    current_quote_delta_nad: i128,
    proposed_delta_nad: i128,
) -> Result<i128> {
    let current_delta_nad = match receipt.target_asset {
        MarketAsset::Base => current_base_delta_nad,
        MarketAsset::Quote => current_quote_delta_nad,
    };
    if proposed_delta_nad == current_delta_nad || receipt.debt_delta == 0 {
        return Ok(proposed_delta_nad);
    }
    let current_ylp = receipt
        .ylp_mint_amount
        .checked_add(receipt.ylp_burn_amount)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    if current_ylp == 0 {
        return Ok(proposed_delta_nad);
    }
    let debt_magnitude = receipt.debt_delta.unsigned_abs();
    let period = debt_magnitude
        .checked_add(
            u128::from(current_ylp)
                .checked_sub(1)
                .ok_or(ErrorCode::MarketMathOverflow)?,
        )
        .ok_or(ErrorCode::MarketMathOverflow)?
        .checked_div(u128::from(current_ylp))
        .ok_or(ErrorCode::MarketMathOverflow)?
        .checked_add(2)
        .ok_or(ErrorCode::MarketMathOverflow)?
        .min(32);
    let direction = if proposed_delta_nad > current_delta_nad {
        1_i128
    } else {
        -1_i128
    };
    for raw_step in 1..=period {
        let mut adjacent = receipt;
        adjacent.debt_delta = adjacent
            .debt_delta
            .checked_add(
                direction
                    .checked_mul(i128::try_from(raw_step).map_err(|_| ErrorCode::MarketMathOverflow)?)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
            )
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let coordinate = hlp_preposition_coordinate_from_debt(snapshot, context.frozen_prices, adjacent)?;
        let (base_delta_nad, quote_delta_nad) = match receipt.target_asset {
            MarketAsset::Base => (coordinate, current_quote_delta_nad),
            MarketAsset::Quote => (current_base_delta_nad, coordinate),
        };
        market.clone_from(snapshot);
        let preposition =
            apply_hlp_candidate_preposition(market, context, cash_floors, base_delta_nad, quote_delta_nad)?;
        let adjacent_receipt = match receipt.target_asset {
            MarketAsset::Base => preposition.base_receipt,
            MarketAsset::Quote => preposition.quote_receipt,
        };
        let adjacent_ylp = adjacent_receipt
            .ylp_mint_amount
            .checked_add(adjacent_receipt.ylp_burn_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        if adjacent_ylp != current_ylp {
            market.clone_from(snapshot);
            return Ok(coordinate);
        }
    }
    market.clone_from(snapshot);
    Ok(proposed_delta_nad)
}

fn hlp_exact_derivative_sample_is_unbound(
    candidate: &ConcentratedHlpCandidate,
    context: &ConcentratedHlpSolveContext,
) -> bool {
    (!context.base_start.active || !candidate.base_receipt.preposition_capacity_bound)
        && (!context.quote_start.active || !candidate.quote_receipt.preposition_capacity_bound)
}

fn hlp_exact_control_row(
    candidate: &ConcentratedHlpCandidate,
    asset: MarketAsset,
    start: ConcentratedHlpStart,
) -> Option<HlpExactControlRow> {
    if !start.active {
        return Some(HlpExactControlRow::Combined);
    }
    let (combined, trade, reserve) = match asset {
        MarketAsset::Base => (
            candidate.base_tracking_error_nad,
            candidate.base_trade_tracking_error_nad,
            candidate.base_reserve_tracking_error_nad,
        ),
        MarketAsset::Quote => (
            candidate.quote_tracking_error_nad,
            candidate.quote_trade_tracking_error_nad,
            candidate.quote_reserve_tracking_error_nad,
        ),
    };
    let lifecycle_safe = combined.unsigned_abs() <= start.tracking.loss_budget_nad;
    let trade_safe = concentrated_hlp_trade_endpoint_is_safe(start, trade);
    let reserve_safe = concentrated_hlp_reserve_is_safe(start, trade, reserve);
    if lifecycle_safe && trade_safe && !reserve_safe {
        return None;
    }
    match (lifecycle_safe, trade_safe) {
        (true, true) | (false, true) => Some(HlpExactControlRow::Combined),
        (true, false) => Some(HlpExactControlRow::Trade),
        (false, false) => {
            if combined == 0 || trade == 0 || (combined < 0) != (trade < 0) {
                None
            } else if combined.unsigned_abs() >= trade.unsigned_abs() {
                Some(HlpExactControlRow::Combined)
            } else {
                Some(HlpExactControlRow::Trade)
            }
        }
    }
}

fn hlp_exact_control_value(candidate: &ConcentratedHlpCandidate, asset: MarketAsset, row: HlpExactControlRow) -> i128 {
    match (asset, row) {
        (MarketAsset::Base, HlpExactControlRow::Combined) => candidate.base_tracking_error_nad,
        (MarketAsset::Base, HlpExactControlRow::Trade) => candidate.base_trade_tracking_error_nad,
        (MarketAsset::Quote, HlpExactControlRow::Combined) => candidate.quote_tracking_error_nad,
        (MarketAsset::Quote, HlpExactControlRow::Trade) => candidate.quote_trade_tracking_error_nad,
    }
}

/// Builds one scale-aware exact finite-difference axis around the
/// authoritative center. The residual-to-economic-NAV ratio selects a local
/// square-root scale, bounded between 1/256 and 1/4 of the center coordinate.
/// The probe always moves inward; it remains guidance only, and the sampled
/// state must still prove an unbound yLP/debt mutation before entering J.
fn hlp_exact_axis_probe_delta(
    center_delta_nad: i128,
    start: ConcentratedHlpStart,
    origin_error_nad: i128,
) -> Option<i128> {
    if !start.active {
        return Some(0);
    }
    let center_magnitude = center_delta_nad.unsigned_abs();
    if center_magnitude == 0 {
        return None;
    }
    let minimum_span = start.target_atom_nad.max(start.tracking.loss_budget_nad);
    let scaled_span = if start.economic_nav_nad == 0 {
        center_magnitude.checked_add(63)?.checked_div(64)?
    } else {
        let ratio_nad = mul_div_u128(
            origin_error_nad.unsigned_abs().max(start.tracking.loss_budget_nad),
            NAD as u128,
            start.economic_nav_nad,
        )
        .ok()?;
        let q_nad = sqrt_ratio_nad(ratio_nad).ok()?;
        mul_div_u128(center_magnitude, q_nad, NAD as u128).ok()?
    };
    let minimum_center_span = center_magnitude.checked_add(255)?.checked_div(256)?;
    let quarter_center = center_magnitude.checked_add(3)?.checked_div(4)?;
    if minimum_span > quarter_center {
        return None;
    }
    let magnitude = scaled_span
        .max(minimum_center_span)
        .max(minimum_span)
        .min(quarter_center)
        .min(center_magnitude.saturating_sub(1));
    if magnitude == 0 || magnitude >= center_magnitude {
        return None;
    }
    if center_delta_nad > 0 {
        if magnitude == 1_u128.checked_shl(127)? {
            Some(i128::MIN)
        } else {
            i128::try_from(magnitude).ok()?.checked_neg()
        }
    } else {
        i128::try_from(magnitude).ok()
    }
}

fn hlp_coordinate_within_center_trust(
    center_delta_nad: i128,
    candidate_delta_nad: i128,
    start: ConcentratedHlpStart,
) -> bool {
    if !start.active {
        return candidate_delta_nad == 0;
    }
    if center_delta_nad == 0 || candidate_delta_nad == 0 || (center_delta_nad < 0) != (candidate_delta_nad < 0) {
        return false;
    }
    let Some(radius) = center_delta_nad
        .unsigned_abs()
        .checked_add(3)
        .and_then(|value| value.checked_div(4))
    else {
        return false;
    };
    candidate_delta_nad.abs_diff(center_delta_nad) <= radius
}

fn scale_hlp_error_row(
    origin: i128,
    base_probe: i128,
    quote_probe: i128,
    residual: i128,
) -> Option<(i128, i128, i128)> {
    // Subtract before scaling so a small, real derivative is not erased by
    // truncating two large, nearby endpoint values independently.
    let base_response = base_probe.checked_sub(origin)?;
    let quote_response = quote_probe.checked_sub(origin)?;
    let maximum = base_response
        .unsigned_abs()
        .max(quote_response.unsigned_abs())
        .max(residual.unsigned_abs());
    let significant_bits = u128::BITS.saturating_sub(maximum.leading_zeros());
    let shift = significant_bits.saturating_sub(60);
    let divisor = 1_i128.checked_shl(shift)?;
    Some((
        base_response.checked_div(divisor)?,
        quote_response.checked_div(divisor)?,
        residual.checked_div(divisor)?,
    ))
}

fn signed_mul_div_i128(first: i128, second: i128, denominator: i128) -> Option<i128> {
    if denominator == 0 {
        return None;
    }
    let magnitude = mul_div_u128(first.unsigned_abs(), second.unsigned_abs(), denominator.unsigned_abs()).ok()?;
    let negative = (first < 0) ^ (second < 0) ^ (denominator < 0);
    if negative {
        if magnitude == 1_u128.checked_shl(127)? {
            Some(i128::MIN)
        } else {
            i128::try_from(magnitude).ok()?.checked_neg()
        }
    } else {
        i128::try_from(magnitude).ok()
    }
}

/// Full-width signed multiplication/division rounded to nearest, with exact
/// half-way cases rounded away from zero. Candidate selection may use this
/// approximation, but only a fresh authoritative lifecycle can accept it.
fn signed_mul_div_round_i128(first: i128, second: i128, denominator: i128) -> Option<i128> {
    if denominator == 0 {
        return None;
    }
    let denominator_magnitude = denominator.unsigned_abs();
    let (mut magnitude, remainder) =
        mul_div_rem_u128(first.unsigned_abs(), second.unsigned_abs(), denominator_magnitude).ok()?;
    if remainder >= denominator_magnitude.checked_sub(remainder)? {
        magnitude = magnitude.checked_add(1)?;
    }
    let negative = (first < 0) ^ (second < 0) ^ (denominator < 0);
    if negative {
        if magnitude == 1_u128.checked_shl(127)? {
            Some(i128::MIN)
        } else {
            i128::try_from(magnitude).ok()?.checked_neg()
        }
    } else {
        i128::try_from(magnitude).ok()
    }
}

fn scale_hlp_broyden_row(
    origin: i128,
    base_probe: i128,
    quote_probe: i128,
    current: i128,
) -> Option<(i128, i128, i128, i128, i128)> {
    let base_response = base_probe.checked_sub(origin)?;
    let quote_response = quote_probe.checked_sub(origin)?;
    // Preserve the exact observed secant before scaling. Scaling `origin`
    // and `current` independently can truncate a small real response to zero
    // and break the Broyden identity.
    let observed_response = current.checked_sub(origin)?;
    let maximum = base_response
        .unsigned_abs()
        .max(quote_response.unsigned_abs())
        .max(observed_response.unsigned_abs())
        .max(current.unsigned_abs());
    let significant_bits = u128::BITS.saturating_sub(maximum.leading_zeros());
    let shift = significant_bits.saturating_sub(60);
    let divisor = 1_i128.checked_shl(shift)?;
    Some((
        base_response.checked_div(divisor)?,
        quote_response.checked_div(divisor)?,
        observed_response.checked_div(divisor)?,
        current.checked_div(divisor)?,
        divisor,
    ))
}

fn hlp_tracking_boundary_target(current: i128, budget: u128, target_atom_nad: u128) -> Option<i128> {
    // Fine-grained markets retain the zero-residual canonical plan. Boundary
    // targeting is only needed when one raw target atom itself defines the
    // tolerance and a zero solve can skip over the feasible discrete cell.
    if budget > target_atom_nad {
        return Some(0);
    }
    if current.unsigned_abs() <= budget {
        return Some(current);
    }
    // Leave a small deterministic rounding margin because the continuous
    // secant lands on a raw debt/yLP atom, not at the exact real-valued row.
    let margin = budget.checked_div(16)?.max(1);
    let budget = i128::try_from(budget.checked_sub(margin)?).ok()?;
    if current < 0 {
        budget.checked_neg()
    } else {
        Some(budget)
    }
}

fn hlp_initial_active_set_residual(current: i128, start: ConcentratedHlpStart) -> Result<i128> {
    if start.tracking.loss_budget_nad > start.target_atom_nad {
        return Ok(current);
    }
    let target = if current.unsigned_abs() <= start.tracking.loss_budget_nad {
        current
    } else {
        let budget = i128::try_from(start.tracking.loss_budget_nad).map_err(|_| ErrorCode::MarketMathOverflow)?;
        if current < 0 {
            budget.checked_neg().ok_or(ErrorCode::MarketMathOverflow)?
        } else {
            budget
        }
    };
    current
        .checked_sub(target)
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

fn hlp_reflected_guidance_step(
    context: &ConcentratedHlpSolveContext,
    center_base_delta_nad: i128,
    center_quote_delta_nad: i128,
    center_next_base_delta_nad: i128,
    center_next_quote_delta_nad: i128,
    base_axis_next_delta_nad: i128,
    quote_axis_next_delta_nad: i128,
) -> Option<(i128, i128)> {
    let base_guidance = if center_next_base_delta_nad != center_base_delta_nad {
        center_next_base_delta_nad
    } else {
        base_axis_next_delta_nad
    };
    let quote_guidance = if center_next_quote_delta_nad != center_quote_delta_nad {
        center_next_quote_delta_nad
    } else {
        quote_axis_next_delta_nad
    };
    let base_step = if context.base_start.active {
        base_guidance.checked_sub(center_base_delta_nad)?.checked_mul(2)?
    } else {
        0
    };
    let quote_step = if context.quote_start.active {
        quote_guidance.checked_sub(center_quote_delta_nad)?.checked_mul(2)?
    } else {
        0
    };
    ((base_step != 0 || !context.base_start.active) && (quote_step != 0 || !context.quote_start.active))
        .then_some((base_step, quote_step))
}

impl HlpFiniteDifferenceBasis {
    fn candidate_is_distinct(
        self,
        center_base_delta_nad: i128,
        center_quote_delta_nad: i128,
        candidate_base_delta_nad: i128,
        candidate_quote_delta_nad: i128,
        extra_base_delta_nad: i128,
        extra_quote_delta_nad: i128,
    ) -> Option<bool> {
        let base_axis = center_base_delta_nad.checked_add(self.base_probe_delta_nad)?;
        let quote_axis = center_quote_delta_nad.checked_add(self.quote_probe_delta_nad)?;
        let candidate = (candidate_base_delta_nad, candidate_quote_delta_nad);
        Some(
            candidate != (center_base_delta_nad, center_quote_delta_nad)
                && candidate != (base_axis, center_quote_delta_nad)
                && candidate != (center_base_delta_nad, quote_axis)
                && candidate != (extra_base_delta_nad, extra_quote_delta_nad),
        )
    }

    fn trusted_step(
        self,
        asset: MarketAsset,
        step: Option<i128>,
        context: &ConcentratedHlpSolveContext,
        center_delta_nad: i128,
    ) -> Option<i128> {
        let step = step?;
        let start = match asset {
            MarketAsset::Base => context.base_start,
            MarketAsset::Quote => context.quote_start,
        };
        let candidate_delta_nad = center_delta_nad.checked_add(step)?;
        hlp_coordinate_within_center_trust(center_delta_nad, candidate_delta_nad, start).then_some(step)
    }

    #[inline(never)]
    fn solve_step(
        self,
        context: &ConcentratedHlpSolveContext,
        center_base_delta_nad: i128,
        center_quote_delta_nad: i128,
        base_row: HlpExactControlRow,
        quote_row: HlpExactControlRow,
        base_residual_nad: i128,
        quote_residual_nad: i128,
    ) -> Option<(i128, i128)> {
        let origin_base_error_nad = self.origin.value(MarketAsset::Base, base_row);
        let origin_quote_error_nad = self.origin.value(MarketAsset::Quote, quote_row);
        match (context.base_start.active, context.quote_start.active) {
            (true, true) => {
                if !self.base_probe_recorded || !self.quote_probe_recorded {
                    return None;
                }
                let (a, c, base_residual) = scale_hlp_error_row(
                    origin_base_error_nad,
                    self.base_probe.value(MarketAsset::Base, base_row),
                    self.quote_probe.value(MarketAsset::Base, base_row),
                    base_residual_nad,
                )?;
                let (b, d, quote_residual) = scale_hlp_error_row(
                    origin_quote_error_nad,
                    self.base_probe.value(MarketAsset::Quote, quote_row),
                    self.quote_probe.value(MarketAsset::Quote, quote_row),
                    quote_residual_nad,
                )?;
                let ad = a.checked_mul(d)?;
                let bc = b.checked_mul(c)?;
                let determinant = ad.checked_sub(bc)?;
                let determinant_scale = a
                    .unsigned_abs()
                    .max(c.unsigned_abs())
                    .checked_mul(b.unsigned_abs().max(d.unsigned_abs()))?;
                let minimum_determinant = determinant_scale.checked_div(1_024)?.max(1);
                if determinant.unsigned_abs() < minimum_determinant {
                    return None;
                }
                let base_numerator = c
                    .checked_mul(quote_residual)?
                    .checked_sub(d.checked_mul(base_residual)?)?;
                let quote_numerator = b
                    .checked_mul(base_residual)?
                    .checked_sub(a.checked_mul(quote_residual)?)?;
                let base_step = signed_mul_div_i128(self.base_probe_delta_nad, base_numerator, determinant)?;
                let quote_step = signed_mul_div_i128(self.quote_probe_delta_nad, quote_numerator, determinant)?;
                Some((
                    self.trusted_step(MarketAsset::Base, Some(base_step), context, center_base_delta_nad)?,
                    self.trusted_step(MarketAsset::Quote, Some(quote_step), context, center_quote_delta_nad)?,
                ))
            }
            (true, false) => {
                if !self.base_probe_recorded {
                    return None;
                }
                let (response, _, residual) = scale_hlp_error_row(
                    origin_base_error_nad,
                    self.base_probe.value(MarketAsset::Base, base_row),
                    origin_base_error_nad,
                    base_residual_nad,
                )?;
                let step = signed_mul_div_i128(self.base_probe_delta_nad, residual.checked_neg()?, response)?;
                Some((
                    self.trusted_step(MarketAsset::Base, Some(step), context, center_base_delta_nad)?,
                    0,
                ))
            }
            (false, true) => {
                if !self.quote_probe_recorded {
                    return None;
                }
                let (_, response, residual) = scale_hlp_error_row(
                    origin_quote_error_nad,
                    origin_quote_error_nad,
                    self.quote_probe.value(MarketAsset::Quote, quote_row),
                    quote_residual_nad,
                )?;
                let step = signed_mul_div_i128(self.quote_probe_delta_nad, residual.checked_neg()?, response)?;
                Some((
                    0,
                    self.trusted_step(MarketAsset::Quote, Some(step), context, center_quote_delta_nad)?,
                ))
            }
            (false, false) => None,
        }
    }

    /// Moves only the control belonging to the most-normalized violated row,
    /// and targets the admissible tracking boundary instead of the zero. This
    /// avoids a two-axis continuous correction hopping across a feasible raw
    /// debt/yLP atom on coarse markets.
    #[allow(clippy::too_many_arguments)]
    fn solve_dominant_boundary_candidate(
        self,
        context: &ConcentratedHlpSolveContext,
        center_base_delta_nad: i128,
        center_quote_delta_nad: i128,
        current_base_delta_nad: i128,
        current_quote_delta_nad: i128,
        base_row: HlpExactControlRow,
        quote_row: HlpExactControlRow,
        current_base_error_nad: i128,
        current_quote_error_nad: i128,
    ) -> Option<(i128, i128)> {
        let base_budget = context.base_start.tracking.loss_budget_nad;
        let quote_budget = context.quote_start.tracking.loss_budget_nad;
        let base_excess = current_base_error_nad.unsigned_abs().saturating_sub(base_budget);
        let quote_excess = current_quote_error_nad.unsigned_abs().saturating_sub(quote_budget);
        let move_base = match (context.base_start.active, context.quote_start.active) {
            (true, false) => true,
            (false, true) => false,
            (true, true) => base_excess.saturating_mul(quote_budget) >= quote_excess.saturating_mul(base_budget),
            (false, false) => return None,
        };

        if move_base {
            if !self.base_probe_recorded {
                return None;
            }
            let response = self
                .base_probe
                .value(MarketAsset::Base, base_row)
                .checked_sub(self.origin.value(MarketAsset::Base, base_row))?;
            let budget = i128::try_from(base_budget).ok()?;
            let target = if current_base_error_nad < 0 {
                budget.checked_neg()?
            } else {
                budget
            };
            let correction = signed_mul_div_round_i128(
                self.base_probe_delta_nad,
                target.checked_sub(current_base_error_nad)?,
                response,
            )?;
            let next_base = current_base_delta_nad.checked_add(correction)?;
            (self.candidate_is_distinct(
                center_base_delta_nad,
                center_quote_delta_nad,
                next_base,
                current_quote_delta_nad,
                current_base_delta_nad,
                current_quote_delta_nad,
            )? && hlp_coordinate_within_center_trust(center_base_delta_nad, next_base, context.base_start))
            .then_some((next_base, current_quote_delta_nad))
        } else {
            if !self.quote_probe_recorded {
                return None;
            }
            let response = self
                .quote_probe
                .value(MarketAsset::Quote, quote_row)
                .checked_sub(self.origin.value(MarketAsset::Quote, quote_row))?;
            let budget = i128::try_from(quote_budget).ok()?;
            let target = if current_quote_error_nad < 0 {
                budget.checked_neg()?
            } else {
                budget
            };
            let correction = signed_mul_div_round_i128(
                self.quote_probe_delta_nad,
                target.checked_sub(current_quote_error_nad)?,
                response,
            )?;
            let next_quote = current_quote_delta_nad.checked_add(correction)?;
            (self.candidate_is_distinct(
                center_base_delta_nad,
                center_quote_delta_nad,
                current_base_delta_nad,
                next_quote,
                current_base_delta_nad,
                current_quote_delta_nad,
            )? && hlp_coordinate_within_center_trust(center_quote_delta_nad, next_quote, context.quote_start))
            .then_some((current_base_delta_nad, next_quote))
        }
    }

    /// Applies one normalized good-Broyden rank-one update using the exact
    /// center/axis samples and one exact unsafe candidate. Coordinates are
    /// normalized by their signed probe spans, so unequal hLP numeraires do
    /// not bias the local Jacobian. The returned point remains guidance only.
    #[allow(clippy::too_many_arguments)]
    #[inline(never)]
    fn solve_broyden_candidate(
        self,
        context: &ConcentratedHlpSolveContext,
        center_base_delta_nad: i128,
        center_quote_delta_nad: i128,
        current_base_delta_nad: i128,
        current_quote_delta_nad: i128,
        base_row: HlpExactControlRow,
        quote_row: HlpExactControlRow,
        current_base_error_nad: i128,
        current_quote_error_nad: i128,
    ) -> Option<(i128, i128)> {
        const COORDINATE_SCALE_Q32: i128 = 1_i128 << 32;
        match (context.base_start.active, context.quote_start.active) {
            (true, true) => {
                if !self.base_probe_recorded || !self.quote_probe_recorded {
                    return None;
                }
                let (mut a, mut c, base_observed, _base_current, base_divisor) = scale_hlp_broyden_row(
                    self.origin.value(MarketAsset::Base, base_row),
                    self.base_probe.value(MarketAsset::Base, base_row),
                    self.quote_probe.value(MarketAsset::Base, base_row),
                    current_base_error_nad,
                )?;
                let (mut b, mut d, quote_observed, _quote_current, quote_divisor) = scale_hlp_broyden_row(
                    self.origin.value(MarketAsset::Quote, quote_row),
                    self.base_probe.value(MarketAsset::Quote, quote_row),
                    self.quote_probe.value(MarketAsset::Quote, quote_row),
                    current_quote_error_nad,
                )?;
                let base_displacement = current_base_delta_nad.checked_sub(center_base_delta_nad)?;
                let quote_displacement = current_quote_delta_nad.checked_sub(center_quote_delta_nad)?;
                let base_coordinate =
                    signed_mul_div_round_i128(base_displacement, COORDINATE_SCALE_Q32, self.base_probe_delta_nad)?;
                let quote_coordinate =
                    signed_mul_div_round_i128(quote_displacement, COORDINATE_SCALE_Q32, self.quote_probe_delta_nad)?;
                let coordinate_norm = base_coordinate
                    .checked_mul(base_coordinate)?
                    .checked_add(quote_coordinate.checked_mul(quote_coordinate)?)?;
                if coordinate_norm == 0 {
                    return None;
                }
                let predicted_base_numerator = a
                    .checked_mul(base_coordinate)?
                    .checked_add(c.checked_mul(quote_coordinate)?)?;
                let predicted_quote_numerator = b
                    .checked_mul(base_coordinate)?
                    .checked_add(d.checked_mul(quote_coordinate)?)?;
                let predicted_base = signed_mul_div_round_i128(predicted_base_numerator, 1, COORDINATE_SCALE_Q32)?;
                let predicted_quote = signed_mul_div_round_i128(predicted_quote_numerator, 1, COORDINATE_SCALE_Q32)?;
                let base_defect = base_observed.checked_sub(predicted_base)?;
                let quote_defect = quote_observed.checked_sub(predicted_quote)?;
                let base_weight = base_coordinate.checked_mul(COORDINATE_SCALE_Q32)?;
                let quote_weight = quote_coordinate.checked_mul(COORDINATE_SCALE_Q32)?;
                a = a.checked_add(signed_mul_div_round_i128(base_defect, base_weight, coordinate_norm)?)?;
                c = c.checked_add(signed_mul_div_round_i128(base_defect, quote_weight, coordinate_norm)?)?;
                b = b.checked_add(signed_mul_div_round_i128(quote_defect, base_weight, coordinate_norm)?)?;
                d = d.checked_add(signed_mul_div_round_i128(quote_defect, quote_weight, coordinate_norm)?)?;

                let base_target = hlp_tracking_boundary_target(
                    current_base_error_nad,
                    context.base_start.tracking.loss_budget_nad,
                    context.base_start.target_atom_nad,
                )?;
                let quote_target = hlp_tracking_boundary_target(
                    current_quote_error_nad,
                    context.quote_start.tracking.loss_budget_nad,
                    context.quote_start.target_atom_nad,
                )?;
                let base_current = current_base_error_nad
                    .checked_sub(base_target)?
                    .checked_div(base_divisor)?;
                let quote_current = current_quote_error_nad
                    .checked_sub(quote_target)?
                    .checked_div(quote_divisor)?;

                let determinant = a.checked_mul(d)?.checked_sub(b.checked_mul(c)?)?;
                let determinant_scale = a
                    .unsigned_abs()
                    .max(c.unsigned_abs())
                    .checked_mul(b.unsigned_abs().max(d.unsigned_abs()))?;
                let minimum_determinant = determinant_scale.checked_div(1_024)?.max(1);
                if determinant.unsigned_abs() < minimum_determinant {
                    return None;
                }
                let base_numerator = c
                    .checked_mul(quote_current)?
                    .checked_sub(d.checked_mul(base_current)?)?;
                let quote_numerator = b
                    .checked_mul(base_current)?
                    .checked_sub(a.checked_mul(quote_current)?)?;
                let base_correction =
                    signed_mul_div_round_i128(self.base_probe_delta_nad, base_numerator, determinant)?;
                let quote_correction =
                    signed_mul_div_round_i128(self.quote_probe_delta_nad, quote_numerator, determinant)?;
                let next_base = current_base_delta_nad.checked_add(base_correction)?;
                let next_quote = current_quote_delta_nad.checked_add(quote_correction)?;
                (self.candidate_is_distinct(
                    center_base_delta_nad,
                    center_quote_delta_nad,
                    next_base,
                    next_quote,
                    current_base_delta_nad,
                    current_quote_delta_nad,
                )? && hlp_coordinate_within_center_trust(center_base_delta_nad, next_base, context.base_start)
                    && hlp_coordinate_within_center_trust(center_quote_delta_nad, next_quote, context.quote_start))
                .then_some((next_base, next_quote))
            }
            (true, false) => {
                if !self.base_probe_recorded {
                    return None;
                }
                let (_, _, observed, _, divisor) = scale_hlp_broyden_row(
                    self.origin.value(MarketAsset::Base, base_row),
                    self.base_probe.value(MarketAsset::Base, base_row),
                    self.origin.value(MarketAsset::Base, base_row),
                    current_base_error_nad,
                )?;
                let displacement = current_base_delta_nad.checked_sub(center_base_delta_nad)?;
                let coordinate =
                    signed_mul_div_round_i128(displacement, COORDINATE_SCALE_Q32, self.base_probe_delta_nad)?;
                let updated_response = signed_mul_div_round_i128(observed, COORDINATE_SCALE_Q32, coordinate)?;
                let target = hlp_tracking_boundary_target(
                    current_base_error_nad,
                    context.base_start.tracking.loss_budget_nad,
                    context.base_start.target_atom_nad,
                )?;
                let current = current_base_error_nad.checked_sub(target)?.checked_div(divisor)?;
                let correction =
                    signed_mul_div_round_i128(self.base_probe_delta_nad, current.checked_neg()?, updated_response)?;
                let next_base = current_base_delta_nad.checked_add(correction)?;
                (self.candidate_is_distinct(
                    center_base_delta_nad,
                    center_quote_delta_nad,
                    next_base,
                    0,
                    current_base_delta_nad,
                    current_quote_delta_nad,
                )? && hlp_coordinate_within_center_trust(center_base_delta_nad, next_base, context.base_start))
                .then_some((next_base, 0))
            }
            (false, true) => {
                if !self.quote_probe_recorded {
                    return None;
                }
                let (_, _, observed, _, divisor) = scale_hlp_broyden_row(
                    self.origin.value(MarketAsset::Quote, quote_row),
                    self.origin.value(MarketAsset::Quote, quote_row),
                    self.quote_probe.value(MarketAsset::Quote, quote_row),
                    current_quote_error_nad,
                )?;
                let displacement = current_quote_delta_nad.checked_sub(center_quote_delta_nad)?;
                let coordinate =
                    signed_mul_div_round_i128(displacement, COORDINATE_SCALE_Q32, self.quote_probe_delta_nad)?;
                let updated_response = signed_mul_div_round_i128(observed, COORDINATE_SCALE_Q32, coordinate)?;
                let target = hlp_tracking_boundary_target(
                    current_quote_error_nad,
                    context.quote_start.tracking.loss_budget_nad,
                    context.quote_start.target_atom_nad,
                )?;
                let current = current_quote_error_nad.checked_sub(target)?.checked_div(divisor)?;
                let correction =
                    signed_mul_div_round_i128(self.quote_probe_delta_nad, current.checked_neg()?, updated_response)?;
                let next_quote = current_quote_delta_nad.checked_add(correction)?;
                (self.candidate_is_distinct(
                    center_base_delta_nad,
                    center_quote_delta_nad,
                    0,
                    next_quote,
                    current_base_delta_nad,
                    current_quote_delta_nad,
                )? && hlp_coordinate_within_center_trust(center_quote_delta_nad, next_quote, context.quote_start))
                .then_some((0, next_quote))
            }
            (false, false) => None,
        }
    }
}

fn concentrated_hlp_start(
    market: &Market,
    target_asset: MarketAsset,
    prices: HlpCurvePrices,
) -> Result<ConcentratedHlpStart> {
    let active = match target_asset {
        MarketAsset::Base => market.base_hlp_vault.hlp_supply > 0 || market.base_hlp_vault.residual_exposure != 0,
        MarketAsset::Quote => market.quote_hlp_vault.hlp_supply > 0 || market.quote_hlp_vault.residual_exposure != 0,
    };
    if !active {
        return Ok(ConcentratedHlpStart {
            active: false,
            tracking: HlpTrackingReference::default(),
            inventory_values: HlpInventoryValuesNad::default(),
            target_atom_nad: 0,
            economic_nav_nad: 0,
        });
    }
    let values = current_hlp_inventory_values_nad_with_prices(market, target_asset, prices)?;
    let collateral = values
        .target_inventory_value_nad
        .checked_add(values.opposite_inventory_value_nad)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let principal_nav_nad = signed_value_difference(collateral, values.debt_value_nad)?;
    let base_unrealized_interest =
        u64::try_from(market.unrealized_interest(MarketAsset::Base)?).map_err(|_| ErrorCode::MarketMathOverflow)?;
    let quote_unrealized_interest =
        u64::try_from(market.unrealized_interest(MarketAsset::Quote)?).map_err(|_| ErrorCode::MarketMathOverflow)?;
    let start_ylp_shares = match target_asset {
        MarketAsset::Base => market.base_hlp_vault.ylp_shares,
        MarketAsset::Quote => market.quote_hlp_vault.ylp_shares,
    };
    let start_ylp_supply = market.base_side.shares.ylp_supply;
    require_eq!(
        start_ylp_supply,
        market.quote_side.shares.ylp_supply,
        ErrorCode::BrokenInvariant
    );
    let (start_base_interest_claim, start_quote_interest_claim) = hlp_interest_claims_for_shares(
        base_unrealized_interest,
        quote_unrealized_interest,
        start_ylp_shares,
        start_ylp_supply,
    )?;
    let start_interest_claim_value_nad = asset_value_in_target_nad_with_prices(
        market,
        prices,
        MarketAsset::Base,
        start_base_interest_claim,
        target_asset,
    )?
    .checked_add(asset_value_in_target_nad_with_prices(
        market,
        prices,
        MarketAsset::Quote,
        start_quote_interest_claim,
        target_asset,
    )?)
    .ok_or(ErrorCode::MarketMathOverflow)?;
    let economic_nav_nad = principal_nav_nad
        .checked_add(i128::try_from(start_interest_claim_value_nad).map_err(|_| ErrorCode::MarketMathOverflow)?)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let target_atom_nad = normalize_to_nad(1, market.side(target_asset).asset_decimals)?;
    require!(target_atom_nad > 0, ErrorCode::MarketMathOverflow);
    let tracking_budget_nad = target_atom_nad.max(
        if economic_nav_nad > 0 {
            economic_nav_nad as u128
        } else {
            0
        } / HLP_CONCENTRATED_TRACKING_NAV_DENOMINATOR,
    );
    Ok(ConcentratedHlpStart {
        active,
        tracking: HlpTrackingReference {
            principal_nav_nad,
            loss_budget_nad: tracking_budget_nad,
            base_unrealized_interest,
            quote_unrealized_interest,
            start_ylp_shares,
            start_ylp_supply,
        },
        inventory_values: values,
        target_atom_nad,
        economic_nav_nad: u128::try_from(economic_nav_nad).unwrap_or(0),
    })
}

fn concentrated_hlp_candidate_components_are_safe(
    start: ConcentratedHlpStart,
    _principal_tracking_error_nad: i128,
    tracking_error_nad: i128,
    _endpoint_exposure_nad: i128,
) -> bool {
    !start.active || tracking_error_nad.unsigned_abs() <= start.tracking.loss_budget_nad
}

fn concentrated_hlp_trade_endpoint_is_safe(start: ConcentratedHlpStart, tracking_error_nad: i128) -> bool {
    !start.active
        || i128::try_from(start.tracking.loss_budget_nad)
            .ok()
            .and_then(i128::checked_neg)
            .map(|minimum| tracking_error_nad >= minimum)
            .unwrap_or(false)
}

#[cfg(test)]
fn concentrated_hlp_candidate_is_safe(
    start: ConcentratedHlpStart,
    tracking_error_nad: i128,
    endpoint_exposure_nad: i128,
) -> bool {
    concentrated_hlp_candidate_components_are_safe(start, tracking_error_nad, tracking_error_nad, endpoint_exposure_nad)
}

fn concentrated_hlp_reserve_components_are_safe(
    start: ConcentratedHlpStart,
    _trade_principal_tracking_error_nad: i128,
    _reserve_principal_tracking_error_nad: i128,
    trade_tracking_error_nad: i128,
    reserve_tracking_error_nad: i128,
) -> bool {
    !start.active
        || reserve_tracking_error_nad >= trade_tracking_error_nad.saturating_sub(start.target_atom_nad as i128)
}

fn concentrated_hlp_reserve_is_safe(
    start: ConcentratedHlpStart,
    trade_tracking_error_nad: i128,
    reserve_tracking_error_nad: i128,
) -> bool {
    concentrated_hlp_reserve_components_are_safe(
        start,
        trade_tracking_error_nad,
        reserve_tracking_error_nad,
        trade_tracking_error_nad,
        reserve_tracking_error_nad,
    )
}

#[inline(never)]
fn project_concentrated_hlp_candidate(
    market: &Market,
    context: &ConcentratedHlpSolveContext,
    preliminary: PreliminarySwapInputs,
    authoritative: bool,
) -> Result<ConcentratedHlpProjection> {
    if authoritative {
        let mut start_checkpoint = None;
        let quote = market.quote_amm_swap_for_reserves_nad_with_start(
            context.asset_in,
            context.reserve_credit,
            context.current_slot,
            market.curve_reserves_nad()?,
            context.pre_state,
            preliminary,
            Some(&mut start_checkpoint),
        )?;
        Ok(ConcentratedHlpProjection::Authoritative {
            common: ConcentratedHlpProjectionCommon {
                amount_in_after_fee: quote.fee.amount_in_for_quote,
                retained_surcharge: quote.fee.retained_surcharge,
                amount_out: quote.amount_out,
                start_price_nad: quote.start_price_nad,
                end_price_nad: quote.end_price_nad,
                endpoint_reserves: quote.trade_endpoint()?.reserves,
                reserve_endpoint_reserves: quote.reserve_endpoint()?.reserves,
                reserve_end_price_nad: quote.reserve_end_price_nad,
            },
            start_checkpoint: start_checkpoint.ok_or(ErrorCode::BrokenInvariant)?,
            quote,
        })
    } else {
        let reserves = market.curve_reserves_nad()?;
        let candidate_ylp_supply = market.base_side.shares.ylp_supply;
        require_eq!(
            candidate_ylp_supply,
            market.quote_side.shares.ylp_supply,
            ErrorCode::BrokenInvariant
        );
        require!(
            context.guidance_start_ylp_supply > 0 && candidate_ylp_supply > 0,
            ErrorCode::SupplyUnderflow
        );
        // Every planner preposition mints or burns the two yLP legs
        // proportionally. Scale the already-proved operation-start invariant
        // with supply and reuse its geometry; raw rounding is guidance-only
        // and the final authoritative quote still solves canonically.
        let scaled_invariant_d = mul_div_u128(
            context.guidance_start_prepared.invariant_d(),
            candidate_ylp_supply as u128,
            context.guidance_start_ylp_supply as u128,
        )?;
        let prepared = context
            .guidance_start_prepared
            .prepare_guidance_successor_with_invariant(reserves.base, reserves.quote, scaled_invariant_d)?;
        let guidance_executable_input = market.exact_swap_input_for_prepared_guidance(
            context.asset_in,
            context.reserve_credit,
            reserves,
            context.pre_state,
            preliminary,
            prepared,
        )?;
        let quote = market.quote_curve_guidance_exact_in_for_prepared_nad(
            context.asset_in,
            guidance_executable_input,
            prepared,
        )?;
        let scratch_reserve_input_credit = if market.amm.retain_dynamic_surcharge {
            preliminary.reserve_input_credit
        } else {
            guidance_executable_input
        };
        let retained_surcharge = scratch_reserve_input_credit
            .checked_sub(guidance_executable_input)
            .ok_or(ErrorCode::FeeMathOverflow)?;
        let mut reserve_endpoint_reserves = quote.endpoint_reserves;
        if market.amm.retain_dynamic_surcharge {
            let retained_surcharge = scratch_reserve_input_credit
                .checked_sub(guidance_executable_input)
                .ok_or(ErrorCode::FeeMathOverflow)?;
            let retained_nad =
                normalize_to_nad(retained_surcharge as u128, market.side(context.asset_in).asset_decimals)?;
            match context.asset_in {
                MarketAsset::Base => {
                    reserve_endpoint_reserves.base = reserve_endpoint_reserves
                        .base
                        .checked_add(retained_nad)
                        .ok_or(ErrorCode::ReserveOverflow)?;
                }
                MarketAsset::Quote => {
                    reserve_endpoint_reserves.quote = reserve_endpoint_reserves
                        .quote
                        .checked_add(retained_nad)
                        .ok_or(ErrorCode::ReserveOverflow)?;
                }
            }
        }
        let reserve_prepared = if reserve_endpoint_reserves == quote.endpoint_reserves {
            quote.endpoint_prepared
        } else {
            prepared.prepare_hint_successor(reserve_endpoint_reserves.base, reserve_endpoint_reserves.quote)?
        };
        let reserve_end_price_nad =
            u64::try_from(reserve_prepared.marginal_price_nad()?).map_err(|_| ErrorCode::MarketMathOverflow)?;
        Ok(ConcentratedHlpProjection::Guidance {
            common: ConcentratedHlpProjectionCommon {
                amount_in_after_fee: guidance_executable_input,
                retained_surcharge,
                amount_out: quote.amount_out,
                start_price_nad: quote.start_price_nad,
                end_price_nad: quote.end_price_nad,
                endpoint_reserves: quote.endpoint_reserves,
                reserve_endpoint_reserves,
                reserve_end_price_nad,
            },
            endpoints: HlpGuidanceEndpointCapability {
                current_slot: context.current_slot,
                curve_revision: market.curve_revision,
                center_price_nad: market.current_curve_center_price_nad()?,
                parameters: market.current_curve_parameters(context.current_slot),
                retain_dynamic_surcharge: market.amm.retain_dynamic_surcharge,
                trade_prepared: quote.endpoint_prepared,
                reserve_prepared,
            },
        })
    }
}

#[allow(clippy::too_many_arguments)]
#[inline(never)]
fn project_concentrated_hlp_side_tracking(
    market: &Market,
    target_asset: MarketAsset,
    start: ConcentratedHlpStart,
    endpoint_base: u64,
    endpoint_quote: u64,
    endpoint_prices: HlpCurvePrices,
    reserve_endpoint_base: u64,
    reserve_endpoint_quote: u64,
    reserve_endpoint_prices: HlpCurvePrices,
) -> Result<ConcentratedHlpSideTracking> {
    if !start.active {
        return Ok(ConcentratedHlpSideTracking {
            principal_error_nad: 0,
            error_nad: 0,
            trade_error_nad: 0,
            reserve_error_nad: 0,
            retained_contribution_nad: 0,
            endpoint_exposure_nad: 0,
            trade_endpoint_safe: true,
            reserve_endpoint_safe: true,
        });
    }

    let endpoint = concentrated_hlp_endpoint(market, target_asset, endpoint_base, endpoint_quote, endpoint_prices)?;
    let (principal_error_nad, _, error_nad) =
        hlp_tracking_deltas_nad(market, target_asset, endpoint_prices, endpoint.nav_nad, start.tracking)?;
    let reserve_endpoint_nav_nad = concentrated_hlp_endpoint(
        market,
        target_asset,
        reserve_endpoint_base,
        reserve_endpoint_quote,
        reserve_endpoint_prices,
    )?
    .nav_nad;
    let (reserve_principal_error_nad, _, reserve_error_nad) = hlp_tracking_deltas_nad(
        market,
        target_asset,
        reserve_endpoint_prices,
        reserve_endpoint_nav_nad,
        start.tracking,
    )?;
    let trade_at_reserve_mark_nav_nad = concentrated_hlp_endpoint(
        market,
        target_asset,
        endpoint_base,
        endpoint_quote,
        reserve_endpoint_prices,
    )?
    .nav_nad;
    let trade_at_reserve_mark_error_nad = hlp_tracking_deltas_nad(
        market,
        target_asset,
        reserve_endpoint_prices,
        trade_at_reserve_mark_nav_nad,
        start.tracking,
    )?
    .2;
    let retained_contribution_nad = reserve_error_nad
        .checked_sub(trade_at_reserve_mark_error_nad)
        .ok_or(ErrorCode::MarketMathOverflow)?;

    Ok(ConcentratedHlpSideTracking {
        principal_error_nad,
        error_nad,
        trade_error_nad: error_nad,
        reserve_error_nad,
        retained_contribution_nad,
        endpoint_exposure_nad: endpoint.opposite_exposure_nad,
        trade_endpoint_safe: concentrated_hlp_trade_endpoint_is_safe(start, error_nad),
        reserve_endpoint_safe: concentrated_hlp_reserve_components_are_safe(
            start,
            principal_error_nad,
            reserve_principal_error_nad,
            error_nad,
            reserve_error_nad,
        ),
    })
}

const fn neutral_concentrated_hlp_side_tracking() -> ConcentratedHlpSideTracking {
    ConcentratedHlpSideTracking {
        principal_error_nad: 0,
        error_nad: 0,
        trade_error_nad: 0,
        reserve_error_nad: 0,
        retained_contribution_nad: 0,
        endpoint_exposure_nad: 0,
        trade_endpoint_safe: true,
        reserve_endpoint_safe: true,
    }
}

#[inline(never)]
fn evaluate_concentrated_hlp_candidate(
    market: &mut Market,
    lifecycle_scratch: &mut Market,
    original: &Market,
    context: &ConcentratedHlpSolveContext,
    cash_floors: SwapCashFloors,
    base_delta_nad: i128,
    quote_delta_nad: i128,
    mode: HlpCandidateEvaluationMode,
    projection_out: &mut Option<ConcentratedHlpProjection>,
    candidate_out: &mut Option<ConcentratedHlpCandidate>,
) -> Result<()> {
    debug_log_heap(100);
    #[cfg(test)]
    CONCENTRATED_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(|count| count.set(count.get() + 1));
    #[cfg(test)]
    if mode.authoritative() {
        CONCENTRATED_PRE_SOLVE_AUTHORITATIVE_EVALUATIONS.with(|count| count.set(count.get() + 1));
    }
    let mut preposition =
        apply_hlp_candidate_preposition(market, context, cash_floors, base_delta_nad, quote_delta_nad)?;
    debug_log_heap(101);
    let preliminary = preposition.preliminary;
    *projection_out = Some(project_concentrated_hlp_candidate(
        market,
        context,
        preliminary,
        mode.exact_quote(),
    )?);
    let projection = projection_out.as_ref().ok_or(ErrorCode::BrokenInvariant)?;
    let projection_common = projection.common();
    require_eq!(
        mode.authoritative(),
        projection.authoritative(),
        ErrorCode::BrokenInvariant
    );
    debug_log_heap(102);
    let required_cash_floors = context
        .cash_policy
        .floors(market, context.asset_in, projection_common.amount_out)?;
    let settlement_cash_available = required_cash_floors.available(market);
    let start_prices = hlp_curve_prices_from_base_price_nad(projection_common.start_price_nad as u128)?;
    refresh_hlp_candidate_preposition(
        market,
        context,
        &mut preposition,
        base_delta_nad,
        quote_delta_nad,
        start_prices,
    )?;
    debug_log_heap(103);
    let mut base_tracking = neutral_concentrated_hlp_side_tracking();
    let mut quote_tracking = neutral_concentrated_hlp_side_tracking();
    let mut reserve_endpoint_safe = true;
    let mut next_base_delta_nad = base_delta_nad;
    let mut next_quote_delta_nad = quote_delta_nad;
    let mut base_endpoint_exposure_nad = 0_i128;
    let mut quote_endpoint_exposure_nad = 0_i128;
    if mode.runs_lifecycle() {
        let lifecycle_args = hlp_lifecycle_args_for_projection(market, context, projection)?;
        #[cfg(test)]
        let lifecycle = if !mode.authoritative() && VERIFY_COMPACT_HLP_GUIDANCE.with(Cell::get) {
            let compact = compact_hlp_lifecycle_tracking(market, context, &lifecycle_args)?;
            let full = scratch_authoritative_result_preserving_preposition(
                lifecycle_scratch,
                market,
                context,
                &lifecycle_args,
            )?;
            assert_eq!(
                compact, full,
                "compact hLP guidance lifecycle diverged from the fixed-endpoint full lifecycle"
            );

            // Keep the pre-existing canonical endpoint differential after the
            // stronger compact/full equality. This comparison is deliberately
            // limited to the control rows because the compact contract is
            // bound to the opaque fixed-D guidance endpoints above.
            let reference_args = full_guidance_reference_args(market, context, projection)?;
            let reference = scratch_authoritative_result_preserving_preposition(
                lifecycle_scratch,
                market,
                context,
                &reference_args,
            )?;
            assert_eq!(
                full.tracking, reference.tracking,
                "fixed-endpoint full guidance rows diverged from canonical endpoint rows"
            );
            full.tracking
        } else {
            scratch_authoritative_preserving_preposition(lifecycle_scratch, market, context, &lifecycle_args)?
        };
        #[cfg(not(test))]
        let lifecycle =
            scratch_authoritative_preserving_preposition(lifecycle_scratch, market, context, &lifecycle_args)?;
        debug_log_heap(110);
        debug_log_heap(111);
        base_tracking.principal_error_nad = lifecycle.base_principal_error_nad;
        base_tracking.error_nad = lifecycle.base_error_nad;
        base_tracking.trade_error_nad = lifecycle.base_trade_error_nad;
        base_tracking.reserve_error_nad = lifecycle.base_reserve_error_nad;
        base_tracking.retained_contribution_nad = lifecycle.base_retained_contribution_nad;
        base_tracking.trade_endpoint_safe =
            concentrated_hlp_trade_endpoint_is_safe(context.base_start, base_tracking.trade_error_nad);
        base_endpoint_exposure_nad = lifecycle.base_exposure_nad;
        quote_tracking.principal_error_nad = lifecycle.quote_principal_error_nad;
        quote_tracking.error_nad = lifecycle.quote_error_nad;
        quote_tracking.trade_error_nad = lifecycle.quote_trade_error_nad;
        quote_tracking.reserve_error_nad = lifecycle.quote_reserve_error_nad;
        quote_tracking.retained_contribution_nad = lifecycle.quote_retained_contribution_nad;
        quote_tracking.trade_endpoint_safe =
            concentrated_hlp_trade_endpoint_is_safe(context.quote_start, quote_tracking.trade_error_nad);
        reserve_endpoint_safe = concentrated_hlp_reserve_is_safe(
            context.base_start,
            base_tracking.trade_error_nad,
            base_tracking.reserve_error_nad,
        ) && concentrated_hlp_reserve_is_safe(
            context.quote_start,
            quote_tracking.trade_error_nad,
            quote_tracking.reserve_error_nad,
        );
        quote_endpoint_exposure_nad = lifecycle.quote_exposure_nad;
        // The authoritative lifecycle ran on a stack-local copy, so the
        // accepted market is already the exact prepositioned state. Keeping
        // it in place avoids repeating the same reserve/debt mutations and
        // concentrated checkpoints solely to reconstruct that state.
        debug_log_heap(114);
    } else {
        // Projection-only guidance can never authorize a transition. Carry
        // its leg-aware endpoint exposure into the bounded root finder; every
        // lifecycle mutation and replay remains authoritative-only.
        base_endpoint_exposure_nad = base_tracking.endpoint_exposure_nad;
        quote_endpoint_exposure_nad = quote_tracking.endpoint_exposure_nad;
    }
    if mode.runs_lifecycle() {
        base_tracking.error_nad = base_tracking
            .error_nad
            .checked_sub(base_tracking.retained_contribution_nad)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        quote_tracking.error_nad = quote_tracking
            .error_nad
            .checked_sub(quote_tracking.retained_contribution_nad)
            .ok_or(ErrorCode::MarketMathOverflow)?;
    }
    if !mode.authoritative() {
        let endpoint_base = denormalize_from_nad_floor(
            projection_common.endpoint_reserves.base,
            market.base_side.asset_decimals,
        )?;
        let endpoint_quote = denormalize_from_nad_floor(
            projection_common.endpoint_reserves.quote,
            market.quote_side.asset_decimals,
        )?;
        let endpoint_prices = hlp_curve_prices_from_base_price_nad(projection_common.end_price_nad as u128)?;
        let base_needs_hint = context.base_start.active
            && !(concentrated_hlp_candidate_components_are_safe(
                context.base_start,
                base_tracking.principal_error_nad,
                base_tracking.error_nad,
                base_endpoint_exposure_nad,
            ) && base_tracking.trade_endpoint_safe
                && concentrated_hlp_reserve_is_safe(
                    context.base_start,
                    base_tracking.trade_error_nad,
                    base_tracking.reserve_error_nad,
                ));
        let quote_needs_hint = context.quote_start.active
            && !(concentrated_hlp_candidate_components_are_safe(
                context.quote_start,
                quote_tracking.principal_error_nad,
                quote_tracking.error_nad,
                quote_endpoint_exposure_nad,
            ) && quote_tracking.trade_endpoint_safe
                && concentrated_hlp_reserve_is_safe(
                    context.quote_start,
                    quote_tracking.trade_error_nad,
                    quote_tracking.reserve_error_nad,
                ));
        if base_needs_hint {
            next_base_delta_nad = concentrated_hlp_needed_delta(
                original,
                market,
                MarketAsset::Base,
                start_prices,
                endpoint_prices,
                endpoint_base,
                endpoint_quote,
                context.base_start.tracking.principal_nav_nad,
            )?;
        }
        if quote_needs_hint {
            next_quote_delta_nad = concentrated_hlp_needed_delta(
                original,
                market,
                MarketAsset::Quote,
                start_prices,
                endpoint_prices,
                endpoint_base,
                endpoint_quote,
                context.quote_start.tracking.principal_nav_nad,
            )?;
        }
    }
    preposition.base_receipt.tracking_retained_contribution_nad = base_tracking.retained_contribution_nad;
    preposition.quote_receipt.tracking_retained_contribution_nad = quote_tracking.retained_contribution_nad;
    *candidate_out = Some(ConcentratedHlpCandidate {
        base_receipt: preposition.base_receipt,
        quote_receipt: preposition.quote_receipt,
        authoritative: projection.authoritative_quote(),
        base_principal_tracking_error_nad: base_tracking.principal_error_nad,
        quote_principal_tracking_error_nad: quote_tracking.principal_error_nad,
        base_tracking_error_nad: base_tracking.error_nad,
        quote_tracking_error_nad: quote_tracking.error_nad,
        base_trade_tracking_error_nad: base_tracking.trade_error_nad,
        quote_trade_tracking_error_nad: quote_tracking.trade_error_nad,
        base_reserve_tracking_error_nad: base_tracking.reserve_error_nad,
        quote_reserve_tracking_error_nad: quote_tracking.reserve_error_nad,
        base_endpoint_exposure_nad,
        quote_endpoint_exposure_nad,
        base_trade_endpoint_safe: base_tracking.trade_endpoint_safe,
        quote_trade_endpoint_safe: quote_tracking.trade_endpoint_safe,
        reserve_endpoint_safe,
        settlement_cash_available,
        next_base_delta_nad,
        next_quote_delta_nad,
    });
    debug_log_heap(130);
    Ok(())
}

/// Selects the endpoint capability used by the shared lifecycle accounting.
/// Guidance carries a private prepared-curve capability; only the exact quote
/// carries canonical `CurveCheckpoint`s and can become authoritative.
fn hlp_lifecycle_args_for_projection(
    _market: &Market,
    _context: &ConcentratedHlpSolveContext,
    projection: &ConcentratedHlpProjection,
) -> Result<HlpAuthoritativeLifecycleArgs> {
    let common = projection.common();
    let endpoints = match projection {
        ConcentratedHlpProjection::Authoritative { quote, .. } => HlpLifecycleEndpointMode::Authoritative {
            trade: quote.trade_endpoint()?,
            reserve: quote.reserve_endpoint()?,
        },
        ConcentratedHlpProjection::Guidance { endpoints, .. } => HlpLifecycleEndpointMode::Guidance(*endpoints),
    };
    Ok(HlpAuthoritativeLifecycleArgs {
        amount_in_after_fee: common.amount_in_after_fee,
        retained_surcharge: common.retained_surcharge,
        amount_out: common.amount_out,
        endpoints,
        expected_trade_price_nad: common.end_price_nad,
        expected_reserve_price_nad: common.reserve_end_price_nad,
    })
}

#[cfg(test)]
fn full_guidance_reference_args(
    market: &Market,
    context: &ConcentratedHlpSolveContext,
    projection: &ConcentratedHlpProjection,
) -> Result<HlpAuthoritativeLifecycleArgs> {
    let common = projection.common();
    require!(!projection.authoritative(), ErrorCode::BrokenInvariant);
    let center_price_nad = market.current_curve_center_price_nad()?;
    let trade_prepared =
        market.prepare_curve_for_reserves_nad(common.endpoint_reserves, center_price_nad, context.current_slot)?;
    let trade = market.checkpoint_for_prepared_curve(trade_prepared, context.current_slot)?;
    let reserve = if common.reserve_endpoint_reserves == common.endpoint_reserves {
        trade
    } else {
        let reserve_prepared = market.prepare_curve_for_reserves_nad(
            common.reserve_endpoint_reserves,
            center_price_nad,
            context.current_slot,
        )?;
        market.checkpoint_for_prepared_curve(reserve_prepared, context.current_slot)?
    };
    Ok(HlpAuthoritativeLifecycleArgs {
        amount_in_after_fee: common.amount_in_after_fee,
        retained_surcharge: common.retained_surcharge,
        amount_out: common.amount_out,
        endpoints: HlpLifecycleEndpointMode::CanonicalGuidance { trade, reserve },
        expected_trade_price_nad: common.end_price_nad,
        expected_reserve_price_nad: common.reserve_end_price_nad,
    })
}

/// Runs the full authorization lifecycle without consuming the candidate's
/// already-prepositioned market. The returned candidate keeps that exact
/// state; only the temporary copy advances through swap settlement and the
/// post-trade hLP rebalance used by the authorization guards.
#[inline(never)]
fn scratch_authoritative_preserving_preposition(
    lifecycle_market: &mut Market,
    prepositioned: &Market,
    context: &ConcentratedHlpSolveContext,
    args: &HlpAuthoritativeLifecycleArgs,
) -> Result<HlpLifecycleTracking> {
    lifecycle_market.clone_from(prepositioned);
    scratch_authoritative_hlp_lifecycle_tracking(
        lifecycle_market,
        context,
        args.amount_in_after_fee,
        args.retained_surcharge,
        args.amount_out,
        args.endpoints,
        args.expected_trade_price_nad,
        args.expected_reserve_price_nad,
    )
}

#[cfg(test)]
fn scratch_authoritative_result_preserving_preposition(
    lifecycle_market: &mut Market,
    prepositioned: &Market,
    context: &ConcentratedHlpSolveContext,
    args: &HlpAuthoritativeLifecycleArgs,
) -> Result<HlpCompactLifecycleResult> {
    lifecycle_market.clone_from(prepositioned);
    SCRATCH_HLP_LIFECYCLE_RESULT.with(|result| {
        result.borrow_mut().take();
    });
    let tracking = scratch_authoritative_hlp_lifecycle_tracking(
        lifecycle_market,
        context,
        args.amount_in_after_fee,
        args.retained_surcharge,
        args.amount_out,
        args.endpoints,
        args.expected_trade_price_nad,
        args.expected_reserve_price_nad,
    )?;
    let result = SCRATCH_HLP_LIFECYCLE_RESULT
        .with(|result| result.borrow_mut().take())
        .ok_or(ErrorCode::BrokenInvariant)?;
    require!(result.tracking == tracking, ErrorCode::BrokenInvariant);
    Ok(result)
}

fn apply_hlp_candidate_preposition(
    market: &mut Market,
    context: &ConcentratedHlpSolveContext,
    cash_floors: SwapCashFloors,
    base_delta_nad: i128,
    quote_delta_nad: i128,
) -> Result<HlpCandidatePreposition> {
    debug_log_heap(300);
    if base_delta_nad != 0 || quote_delta_nad != 0 {
        checkpoint_hlp_yield_from_ylp_pair(market, context.base_start.active, context.quote_start.active)?;
    }
    debug_log_heap(301);
    // Value both plans from the same state, then execute base before quote.
    let base_valuation = if base_delta_nad != 0 {
        Some(hlp_valuation_from_values(
            context.base_start.inventory_values,
            context.frozen_prices,
        )?)
    } else {
        None
    };
    let quote_valuation = if quote_delta_nad != 0 {
        Some(hlp_valuation_from_values(
            context.quote_start.inventory_values,
            context.frozen_prices,
        )?)
    } else {
        None
    };
    debug_log_heap(302);
    let base_receipt = if context.base_start.active {
        apply_concentrated_hlp_pre_adjustment(market, MarketAsset::Base, base_delta_nad, base_valuation, cash_floors)?
    } else {
        empty_hlp_rebalance_receipt(MarketAsset::Base)
    };
    debug_log_heap(303);
    let quote_receipt = if context.quote_start.active {
        apply_concentrated_hlp_pre_adjustment(
            market,
            MarketAsset::Quote,
            quote_delta_nad,
            quote_valuation,
            cash_floors,
        )?
    } else {
        empty_hlp_rebalance_receipt(MarketAsset::Quote)
    };
    debug_log_heap(304);
    let mutates_curve_inventory = base_receipt.ylp_mint_amount != 0
        || base_receipt.ylp_burn_amount != 0
        || quote_receipt.ylp_mint_amount != 0
        || quote_receipt.ylp_burn_amount != 0;
    if mutates_curve_inventory {
        market.defer_amm_retention_target()?;
    }
    // Fee inputs are frozen before hLP positioning. Inventory mutations and
    // deferring the retention target do not change any input used by this
    // calculation, so every candidate reuses the identity-bound preliminary.
    let preliminary = context.preliminary;
    debug_log_heap(305);
    Ok(HlpCandidatePreposition {
        base_receipt,
        quote_receipt,
        preliminary,
    })
}

fn refresh_hlp_candidate_preposition(
    market: &mut Market,
    context: &ConcentratedHlpSolveContext,
    preposition: &mut HlpCandidatePreposition,
    base_delta_nad: i128,
    quote_delta_nad: i128,
    start_prices: HlpCurvePrices,
) -> Result<()> {
    debug_log_heap(310);
    if base_delta_nad != 0 || quote_delta_nad != 0 {
        if context.base_start.active {
            preposition.base_receipt =
                refresh_hlp_after_rebalance(market, MarketAsset::Base, preposition.base_receipt, start_prices)?;
        }
        debug_log_heap(311);
        if context.quote_start.active {
            preposition.quote_receipt =
                refresh_hlp_after_rebalance(market, MarketAsset::Quote, preposition.quote_receipt, start_prices)?;
        }
        debug_log_heap(312);
    }
    if context.base_start.active {
        stamp_hlp_tracking_reference(&mut preposition.base_receipt, context.base_start.tracking);
    }
    if context.quote_start.active {
        stamp_hlp_tracking_reference(&mut preposition.quote_receipt, context.quote_start.tracking);
    }
    debug_log_heap(313);
    Ok(())
}

fn apply_concentrated_hlp_pre_adjustment(
    market: &mut Market,
    target_asset: MarketAsset,
    requested_delta_nad: i128,
    valuation: Option<HlpValuation>,
    cash_floors: SwapCashFloors,
) -> Result<HlpRebalanceReceipt> {
    if requested_delta_nad == 0 {
        return Ok(empty_hlp_rebalance_receipt(target_asset));
    }
    let valuation = valuation.ok_or(ErrorCode::HlpSettlementUnavailable)?;
    require!(
        valuation.proportional_hedge_available,
        ErrorCode::HlpSettlementUnavailable
    );
    let ylp_shares_before = match target_asset {
        MarketAsset::Base => market.base_hlp_vault.ylp_shares,
        MarketAsset::Quote => market.quote_hlp_vault.ylp_shares,
    };
    let mut plan = if requested_delta_nad > 0 {
        plan_leverage_up_proportional_with_cash_floors(
            market,
            target_asset,
            requested_delta_nad,
            valuation,
            cash_floors,
        )?
    } else {
        plan_deleverage_proportional_with_cash_floors(
            market,
            target_asset,
            requested_delta_nad,
            valuation,
            cash_floors,
        )?
    };
    let current_swap_fee_eligible_ylp_shares = if plan.ylp_mint_amount() > 0 {
        ylp_shares_before
    } else {
        ylp_shares_before
            .checked_sub(plan.ylp_burn_amount())
            .ok_or(ErrorCode::SupplyUnderflow)?
    };
    plan.set_current_swap_fee_eligible_ylp_shares(current_swap_fee_eligible_ylp_shares);
    apply_hlp_rebalance_plan(market, plan)
}

fn scratch_hlp_lifecycle_tracking(
    market: &mut Market,
    context: &ConcentratedHlpSolveContext,
    endpoint_prices: HlpCurvePrices,
    reserve_input_credit: u64,
    amount_out: u64,
) -> Result<HlpLifecycleTracking> {
    scratch_commit_swap_cash_policy(market, context, reserve_input_credit, amount_out)?;

    let base_valuation = context
        .base_start
        .active
        .then(|| current_hlp_valuation_with_prices(market, MarketAsset::Base, endpoint_prices))
        .transpose()?;
    let quote_valuation = context
        .quote_start
        .active
        .then(|| current_hlp_valuation_with_prices(market, MarketAsset::Quote, endpoint_prices))
        .transpose()?;
    let base_post = base_valuation
        .map(|valuation| rebalance_hlp_from_valuation(market, MarketAsset::Base, valuation))
        .transpose()?
        .unwrap_or_else(|| empty_hlp_rebalance_receipt(MarketAsset::Base));
    let quote_post = quote_valuation
        .map(|valuation| rebalance_hlp_from_valuation(market, MarketAsset::Quote, valuation))
        .transpose()?
        .unwrap_or_else(|| empty_hlp_rebalance_receipt(MarketAsset::Quote));
    let inventory_changed = base_post.ylp_mint_amount != 0
        || base_post.ylp_burn_amount != 0
        || quote_post.ylp_mint_amount != 0
        || quote_post.ylp_burn_amount != 0;
    let final_prices = if inventory_changed {
        current_hlp_curve_prices(market)?
    } else {
        endpoint_prices
    };

    let base = hlp_lifecycle_tracking_for_asset(market, MarketAsset::Base, final_prices, context.base_start)?;
    let quote = hlp_lifecycle_tracking_for_asset(market, MarketAsset::Quote, final_prices, context.quote_start)?;
    Ok(HlpLifecycleTracking {
        base_principal_error_nad: base.0,
        base_error_nad: base.2,
        base_trade_error_nad: 0,
        base_reserve_error_nad: 0,
        base_retained_contribution_nad: 0,
        base_exposure_nad: base.3,
        quote_principal_error_nad: quote.0,
        quote_error_nad: quote.2,
        quote_trade_error_nad: 0,
        quote_reserve_error_nad: 0,
        quote_retained_contribution_nad: 0,
        quote_exposure_nad: quote.3,
    })
}

fn scratch_authoritative_hlp_lifecycle_tracking(
    market: &mut Market,
    context: &ConcentratedHlpSolveContext,
    amount_in_after_fee: u64,
    retained_surcharge: u64,
    amount_out: u64,
    endpoints: HlpLifecycleEndpointMode,
    expected_trade_price_nad: u64,
    expected_reserve_price_nad: u64,
) -> Result<HlpLifecycleTracking> {
    debug_log_heap(200);
    let guidance_rebalance_start = match endpoints {
        HlpLifecycleEndpointMode::Guidance(guidance) => Some(guidance.reserve_prepared),
        _ => None,
    };
    let transition = market.apply_leverage_lifecycle_transition(
        context.cash_policy,
        context.asset_in,
        amount_in_after_fee,
        amount_out,
    )?;
    debug_log_heap(201);
    let debt_asset = match context.cash_policy {
        SwapCashPolicy::Decrease { debt_asset, .. }
        | SwapCashPolicy::Close { debt_asset, .. }
        | SwapCashPolicy::Liquidate { debt_asset, .. } => Some(debt_asset),
        _ => None,
    };
    let base_endpoint_start =
        lifecycle_tracking_start_after_transition(market, context.base_start, transition, debt_asset, 0)?;
    let quote_endpoint_start =
        lifecycle_tracking_start_after_transition(market, context.quote_start, transition, debt_asset, 0)?;
    debug_log_heap(202);
    let trade_base = market.curve_reserve(MarketAsset::Base)?;
    let trade_quote = market.curve_reserve(MarketAsset::Quote)?;
    let reserve_reserves = match endpoints {
        HlpLifecycleEndpointMode::Authoritative { trade, reserve } => {
            require!(
                market.curve_reserves_nad()? == trade.reserves,
                ErrorCode::BrokenInvariant
            );
            market.checkpoint_leverage_lifecycle_inventory_from_quote(
                context.asset_in,
                retained_surcharge,
                context.current_slot,
                trade,
                reserve,
            )?;
            reserve.reserves
        }
        #[cfg(test)]
        HlpLifecycleEndpointMode::CanonicalGuidance { trade, reserve } => {
            require!(
                market.curve_reserves_nad()? == trade.reserves,
                ErrorCode::BrokenInvariant
            );
            market.checkpoint_leverage_lifecycle_inventory(
                context.asset_in,
                retained_surcharge,
                context.current_slot,
            )?;
            reserve.reserves
        }
        HlpLifecycleEndpointMode::Guidance(guidance) => {
            guidance.require_identity(market)?;
            let trade_reserves = guidance.trade_reserves();
            let reserve_reserves = guidance.reserve_reserves();
            require!(
                market.curve_reserves_nad()? == trade_reserves,
                ErrorCode::BrokenInvariant
            );
            market.ensure_amm_initialized(context.current_slot)?;
            require!(market.amm.initialized, ErrorCode::BrokenInvariant);
            let trade_evaluation = guidance.trade_prepared.evaluation()?;
            let trade_q_per_share_nad = market.curve_q_per_share_nad(trade_evaluation.balanced_equivalent_q)?;
            market.amm.commit_invariant(trade_evaluation.invariant_d)?;
            market.amm.checkpoint_neutral_liquidity(trade_q_per_share_nad);
            if retained_surcharge > 0 {
                market
                    .side_mut(context.asset_in)
                    .credit_reserve(retained_surcharge, true)?;
                require!(
                    market.curve_reserves_nad()? == reserve_reserves,
                    ErrorCode::BrokenInvariant
                );
                let reserve_evaluation = guidance.reserve_prepared.evaluation()?;
                let reserve_q_per_share_nad = market.curve_q_per_share_nad(reserve_evaluation.balanced_equivalent_q)?;
                market.amm.commit_invariant(reserve_evaluation.invariant_d)?;
                market.amm.checkpoint_retained_surcharge(reserve_q_per_share_nad)?;
            } else {
                require!(trade_reserves == reserve_reserves, ErrorCode::BrokenInvariant);
            }
            reserve_reserves
        }
    };
    debug_log_heap(203);
    require!(
        market.curve_reserves_nad()? == reserve_reserves,
        ErrorCode::BrokenInvariant
    );
    let trade_prices = hlp_curve_prices_from_base_price_nad(expected_trade_price_nad as u128)?;
    let reserve_prices = hlp_curve_prices_from_base_price_nad(expected_reserve_price_nad as u128)?;
    let base_trade_endpoint =
        concentrated_hlp_endpoint(market, MarketAsset::Base, trade_base, trade_quote, trade_prices)?;
    let quote_trade_endpoint =
        concentrated_hlp_endpoint(market, MarketAsset::Quote, trade_base, trade_quote, trade_prices)?;
    let base_trade = hlp_tracking_deltas_nad(
        market,
        MarketAsset::Base,
        trade_prices,
        base_trade_endpoint.nav_nad,
        base_endpoint_start.tracking,
    )?;
    let quote_trade = hlp_tracking_deltas_nad(
        market,
        MarketAsset::Quote,
        trade_prices,
        quote_trade_endpoint.nav_nad,
        quote_endpoint_start.tracking,
    )?;
    let base_reserve =
        hlp_lifecycle_tracking_for_asset(market, MarketAsset::Base, reserve_prices, base_endpoint_start)?;
    let quote_reserve =
        hlp_lifecycle_tracking_for_asset(market, MarketAsset::Quote, reserve_prices, quote_endpoint_start)?;
    let base_trade_at_reserve_mark =
        concentrated_hlp_endpoint(market, MarketAsset::Base, trade_base, trade_quote, reserve_prices)?;
    let quote_trade_at_reserve_mark =
        concentrated_hlp_endpoint(market, MarketAsset::Quote, trade_base, trade_quote, reserve_prices)?;
    let base_trade_at_reserve_mark_error = hlp_tracking_deltas_nad(
        market,
        MarketAsset::Base,
        reserve_prices,
        base_trade_at_reserve_mark.nav_nad,
        base_endpoint_start.tracking,
    )?
    .2;
    let quote_trade_at_reserve_mark_error = hlp_tracking_deltas_nad(
        market,
        MarketAsset::Quote,
        reserve_prices,
        quote_trade_at_reserve_mark.nav_nad,
        quote_endpoint_start.tracking,
    )?
    .2;
    debug_log_heap(204);
    let base_retained_contribution_nad = base_reserve
        .2
        .checked_sub(base_trade_at_reserve_mark_error)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let quote_retained_contribution_nad = quote_reserve
        .2
        .checked_sub(quote_trade_at_reserve_mark_error)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    debug_log_heap(205);
    let rebase = debt_asset
        .map(|asset| market.apply_leverage_socialized_loss(asset, transition, context.current_slot))
        .transpose()?
        .unwrap_or_default();
    let base_start = lifecycle_tracking_start_after_transition(
        market,
        context.base_start,
        transition,
        debt_asset,
        rebase.base_nav_delta_nad,
    )?;
    let quote_start = lifecycle_tracking_start_after_transition(
        market,
        context.quote_start,
        transition,
        debt_asset,
        rebase.quote_nav_delta_nad,
    )?;
    debug_log_heap(206);
    // The authoritative quote already proved the exact reserve-end mark.
    // Socialized principal loss is the only mutation between that checkpoint
    // and the joint rebalance that changes curve geometry; otherwise avoid a
    // second concentrated marginal-price solve for the identical reserves.
    let rebalance_start_price_nad = (transition.socialized_principal_loss == 0).then_some(expected_reserve_price_nad);
    let (_base_post_receipt, _quote_post_receipt, _, final_prices, base_endpoint, quote_endpoint) =
        if transition.socialized_principal_loss == 0 {
        if let Some(start) = guidance_rebalance_start {
            rebalance_hlps_after_swap_joint_with_curve_mode(
                market,
                rebalance_start_price_nad,
                HlpPostRebalanceCurveMode::Guidance {
                    start,
                    current_slot: context.current_slot,
                },
            )?
        } else {
            rebalance_hlps_after_swap_joint(market, context.current_slot, rebalance_start_price_nad)?
        }
        } else {
        // A one-sided socialized-loss reserve mutation is not homogeneous
        // in yLP supply. Keep the canonical post-loss solve for this rare
        // guidance sample instead of reusing a scaled planner invariant.
        rebalance_hlps_after_swap_joint(market, context.current_slot, rebalance_start_price_nad)?
        };
    debug_log_heap(207);
    let base =
        hlp_lifecycle_tracking_from_endpoint(market, MarketAsset::Base, final_prices, base_endpoint, base_start)?;
    let quote =
        hlp_lifecycle_tracking_from_endpoint(market, MarketAsset::Quote, final_prices, quote_endpoint, quote_start)?;
    debug_log_heap(208);
    let tracking = HlpLifecycleTracking {
        base_principal_error_nad: base.0,
        base_error_nad: base.2,
        base_trade_error_nad: base_trade.2,
        base_reserve_error_nad: base_reserve.2,
        base_retained_contribution_nad,
        base_exposure_nad: base.3,
        quote_principal_error_nad: quote.0,
        quote_error_nad: quote.2,
        quote_trade_error_nad: quote_trade.2,
        quote_reserve_error_nad: quote_reserve.2,
        quote_retained_contribution_nad,
        quote_exposure_nad: quote.3,
    };
    #[cfg(test)]
    SCRATCH_HLP_LIFECYCLE_RESULT.with(|result| {
        result.replace(Some(HlpCompactLifecycleResult {
            state: HlpPlannerState::capture(market),
            tracking,
            base_post_receipt: _base_post_receipt,
            quote_post_receipt: _quote_post_receipt,
            transition,
        }));
    });
    Ok(tracking)
}

fn lifecycle_tracking_start_after_transition(
    market: &Market,
    mut start: ConcentratedHlpStart,
    transition: crate::market::LeverageLifecycleTransition,
    removed_interest_asset: Option<MarketAsset>,
    socialized_nav_delta_nad: i128,
) -> Result<ConcentratedHlpStart> {
    if !start.active {
        return Ok(start);
    }
    if let (Some(asset), amount) = (removed_interest_asset, transition.removed_unrealized_interest) {
        if amount > 0 {
            match asset {
                MarketAsset::Base => {
                    start.tracking.base_unrealized_interest = start
                        .tracking
                        .base_unrealized_interest
                        .checked_sub(amount)
                        .ok_or(ErrorCode::MarketMathOverflow)?;
                }
                MarketAsset::Quote => {
                    start.tracking.quote_unrealized_interest = start
                        .tracking
                        .quote_unrealized_interest
                        .checked_sub(amount)
                        .ok_or(ErrorCode::MarketMathOverflow)?;
                }
            }
        }
    }
    start.tracking.base_unrealized_interest = start
        .tracking
        .base_unrealized_interest
        .min(u64::try_from(market.unrealized_interest(MarketAsset::Base)?).map_err(|_| ErrorCode::MarketMathOverflow)?);
    start.tracking.quote_unrealized_interest = start.tracking.quote_unrealized_interest.min(
        u64::try_from(market.unrealized_interest(MarketAsset::Quote)?).map_err(|_| ErrorCode::MarketMathOverflow)?,
    );
    start.tracking.principal_nav_nad = start
        .tracking
        .principal_nav_nad
        .checked_add(socialized_nav_delta_nad)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    Ok(start)
}

fn scratch_commit_swap_cash_policy(
    market: &mut Market,
    context: &ConcentratedHlpSolveContext,
    reserve_input_credit: u64,
    amount_out: u64,
) -> Result<()> {
    let floors = context.cash_policy.floors(market, context.asset_in, amount_out)?;
    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        let side = market.side_mut(asset);
        side.reserves.cash_reserve = side
            .reserves
            .cash_reserve
            .checked_sub(floors.for_asset(asset))
            .ok_or(ErrorCode::CashReserveUnderflow)?;
    }
    let side_in = market.side_mut(context.asset_in);
    side_in.reserves.live_reserve = side_in
        .reserves
        .live_reserve
        .checked_add(reserve_input_credit)
        .ok_or(ErrorCode::ReserveOverflow)?;
    side_in.reserves.cash_reserve = side_in
        .reserves
        .cash_reserve
        .checked_add(reserve_input_credit)
        .ok_or(ErrorCode::ReserveOverflow)?;
    let side_out = market.side_mut(context.asset_in.opposite());
    side_out.reserves.live_reserve = side_out
        .reserves
        .live_reserve
        .checked_sub(amount_out)
        .ok_or(ErrorCode::ReserveUnderflow)?;
    Ok(())
}

fn hlp_lifecycle_tracking_for_asset(
    market: &Market,
    target_asset: MarketAsset,
    prices: HlpCurvePrices,
    start: ConcentratedHlpStart,
) -> Result<(i128, i128, i128, i128)> {
    if !start.active {
        return Ok((0, 0, 0, 0));
    }
    let values = current_hlp_inventory_values_nad_with_prices(market, target_asset, prices)?;
    let collateral = values
        .target_inventory_value_nad
        .checked_add(values.opposite_inventory_value_nad)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let principal_nav_nad = signed_value_difference(collateral, values.debt_value_nad)?;
    let (principal, interest, combined) =
        hlp_tracking_deltas_nad(market, target_asset, prices, principal_nav_nad, start.tracking)?;
    Ok((principal, interest, combined, hlp_opposite_exposure_nad(values)?))
}

fn hlp_lifecycle_endpoint_from_values(values: HlpInventoryValuesNad) -> Result<HlpLifecycleEndpoint> {
    let collateral = values
        .target_inventory_value_nad
        .checked_add(values.opposite_inventory_value_nad)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    Ok(HlpLifecycleEndpoint {
        principal_nav_nad: signed_value_difference(collateral, values.debt_value_nad)?,
        opposite_exposure_nad: hlp_opposite_exposure_nad(values)?,
    })
}

fn hlp_lifecycle_tracking_from_endpoint(
    market: &Market,
    target_asset: MarketAsset,
    prices: HlpCurvePrices,
    endpoint: HlpLifecycleEndpoint,
    start: ConcentratedHlpStart,
) -> Result<(i128, i128, i128, i128)> {
    if !start.active {
        return Ok((0, 0, 0, 0));
    }
    let (principal, interest, combined) =
        hlp_tracking_deltas_nad(market, target_asset, prices, endpoint.principal_nav_nad, start.tracking)?;
    Ok((principal, interest, combined, endpoint.opposite_exposure_nad))
}

#[derive(Clone, Copy, Default)]
struct ConcentratedHlpEndpoint {
    nav_nad: i128,
    opposite_exposure_nad: i128,
}

fn concentrated_hlp_endpoint(
    market: &Market,
    target_asset: MarketAsset,
    endpoint_base: u64,
    endpoint_quote: u64,
    prices: HlpCurvePrices,
) -> Result<ConcentratedHlpEndpoint> {
    let active = match target_asset {
        MarketAsset::Base => market.base_hlp_vault.hlp_supply > 0 || market.base_hlp_vault.residual_exposure != 0,
        MarketAsset::Quote => market.quote_hlp_vault.hlp_supply > 0 || market.quote_hlp_vault.residual_exposure != 0,
    };
    if !active {
        return Ok(ConcentratedHlpEndpoint::default());
    }
    let supply = market.base_side.shares.ylp_supply;
    require_eq!(supply, market.quote_side.shares.ylp_supply, ErrorCode::BrokenInvariant);
    require!(supply > 0, ErrorCode::SupplyUnderflow);
    let shares = match target_asset {
        MarketAsset::Base => market.base_hlp_vault.ylp_shares,
        MarketAsset::Quote => market.quote_hlp_vault.ylp_shares,
    };
    let base_claim = u64::try_from(mul_div_u128(endpoint_base as u128, shares as u128, supply as u128)?)
        .map_err(|_| ErrorCode::MarketMathOverflow)?;
    let quote_claim = u64::try_from(mul_div_u128(endpoint_quote as u128, shares as u128, supply as u128)?)
        .map_err(|_| ErrorCode::MarketMathOverflow)?;
    let opposite_asset = target_asset.opposite();
    let (target_claim, opposite_claim) = match target_asset {
        MarketAsset::Base => (base_claim, quote_claim),
        MarketAsset::Quote => (quote_claim, base_claim),
    };
    let values = HlpInventoryValuesNad {
        target_inventory_value_nad: asset_value_in_target_nad_with_prices(
            market,
            prices,
            target_asset,
            target_claim,
            target_asset,
        )?,
        opposite_inventory_value_nad: asset_value_in_target_nad_with_prices(
            market,
            prices,
            opposite_asset,
            opposite_claim,
            target_asset,
        )?,
        debt_value_nad: asset_value_in_target_nad_with_prices(
            market,
            prices,
            opposite_asset,
            hlp_debt_amount(market, target_asset)?,
            target_asset,
        )?,
    };
    let collateral = values
        .target_inventory_value_nad
        .checked_add(values.opposite_inventory_value_nad)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    Ok(ConcentratedHlpEndpoint {
        nav_nad: signed_value_difference(collateral, values.debt_value_nad)?,
        opposite_exposure_nad: hlp_opposite_exposure_nad(values)?,
    })
}

#[allow(clippy::too_many_arguments)]
fn concentrated_hlp_needed_delta(
    original: &Market,
    candidate: &Market,
    target_asset: MarketAsset,
    start_prices: HlpCurvePrices,
    endpoint_prices: HlpCurvePrices,
    endpoint_base: u64,
    endpoint_quote: u64,
    start_nav_nad: i128,
) -> Result<i128> {
    let supply = candidate.base_side.shares.ylp_supply;
    require_eq!(
        supply,
        candidate.quote_side.shares.ylp_supply,
        ErrorCode::BrokenInvariant
    );
    require!(supply > 0, ErrorCode::SupplyUnderflow);
    let original_shares = match target_asset {
        MarketAsset::Base => original.base_hlp_vault.ylp_shares,
        MarketAsset::Quote => original.quote_hlp_vault.ylp_shares,
    };
    let existing_base_claim = u64::try_from(mul_div_u128(
        endpoint_base as u128,
        original_shares as u128,
        supply as u128,
    )?)
    .map_err(|_| ErrorCode::MarketMathOverflow)?;
    let existing_quote_claim = u64::try_from(mul_div_u128(
        endpoint_quote as u128,
        original_shares as u128,
        supply as u128,
    )?)
    .map_err(|_| ErrorCode::MarketMathOverflow)?;
    let existing_collateral = concentrated_pool_value_nad(
        original,
        target_asset,
        endpoint_prices,
        existing_base_claim,
        existing_quote_claim,
    )?;
    let original_debt = asset_value_in_target_nad_with_prices(
        original,
        endpoint_prices,
        target_asset.opposite(),
        hlp_debt_amount(original, target_asset)?,
        target_asset,
    )?;
    let existing_error = signed_value_difference(existing_collateral, original_debt)?
        .checked_sub(start_nav_nad)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    if existing_error == 0 {
        return Ok(0);
    }

    let start_pool_value = concentrated_pool_value_nad(
        candidate,
        target_asset,
        start_prices,
        candidate.curve_reserve(MarketAsset::Base)?,
        candidate.curve_reserve(MarketAsset::Quote)?,
    )?;
    let end_pool_value =
        concentrated_pool_value_nad(candidate, target_asset, endpoint_prices, endpoint_base, endpoint_quote)?;
    let opposite_start_price = start_prices.for_asset(target_asset.opposite());
    let opposite_end_price = endpoint_prices.for_asset(target_asset.opposite());
    require!(
        start_pool_value > 0 && opposite_start_price > 0,
        ErrorCode::InvalidSettlementPrice
    );
    concentrated_hlp_payoff_adjustment(
        existing_error,
        start_pool_value,
        end_pool_value,
        opposite_start_price,
        opposite_end_price,
    )
}

fn concentrated_hlp_payoff_adjustment(
    existing_error: i128,
    start_pool_value: u128,
    end_pool_value: u128,
    opposite_start_price: u128,
    opposite_end_price: u128,
) -> Result<i128> {
    require!(
        start_pool_value > 0 && opposite_start_price > 0,
        ErrorCode::InvalidSettlementPrice
    );
    if existing_error == 0 {
        return Ok(0);
    }
    // One unit of self-financing proportional liquidity has endpoint payoff
    // k = C1/C0 - q1/q0 in the target numeraire. Compare those exact ratios
    // without cross-products, then evaluate k at Q64 precision. The accepted
    // raw candidate is always checked against the exact endpoint NAV, so this
    // bounded projection can guide the solver without making large valid
    // reserves fail on an intermediate u128 product.
    let pool_lte_debt = ratio_lte_full_width(
        end_pool_value,
        start_pool_value,
        opposite_end_price,
        opposite_start_price,
    )?;
    let debt_lte_pool = ratio_lte_full_width(
        opposite_end_price,
        opposite_start_price,
        end_pool_value,
        start_pool_value,
    )?;
    require!(!(pool_lte_debt && debt_lte_pool), ErrorCode::HlpSettlementUnavailable);
    let payoff_positive = debt_lte_pool;
    let pool_ratio_q64 = mul_div_u128(end_pool_value, YIELD_GROWTH_SCALE_Q64, start_pool_value)?;
    let debt_ratio_q64 = mul_div_u128(opposite_end_price, YIELD_GROWTH_SCALE_Q64, opposite_start_price)?;
    let payoff_magnitude_q64 = pool_ratio_q64.abs_diff(debt_ratio_q64);
    require!(payoff_magnitude_q64 > 0, ErrorCode::HlpSettlementUnavailable);
    let (mut magnitude, remainder) = mul_div_rem_u128(
        existing_error.unsigned_abs(),
        YIELD_GROWTH_SCALE_Q64,
        payoff_magnitude_q64,
    )?;
    if remainder >= payoff_magnitude_q64 - remainder {
        magnitude = magnitude.checked_add(1).ok_or(ErrorCode::MarketMathOverflow)?;
    }
    let magnitude = i128::try_from(magnitude).map_err(|_| ErrorCode::MarketMathOverflow)?;
    if existing_error.is_positive() == payoff_positive {
        magnitude
            .checked_neg()
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    } else {
        Ok(magnitude)
    }
}

fn concentrated_pool_value_nad(
    market: &Market,
    target_asset: MarketAsset,
    prices: HlpCurvePrices,
    base_amount: u64,
    quote_amount: u64,
) -> Result<u128> {
    asset_value_in_target_nad_with_prices(market, prices, MarketAsset::Base, base_amount, target_asset)?
        .checked_add(asset_value_in_target_nad_with_prices(
            market,
            prices,
            MarketAsset::Quote,
            quote_amount,
            target_asset,
        )?)
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

fn signed_value_difference(collateral_nad: u128, debt_nad: u128) -> Result<i128> {
    if collateral_nad >= debt_nad {
        i128::try_from(collateral_nad - debt_nad).map_err(|_| ErrorCode::MarketMathOverflow.into())
    } else {
        i128::try_from(debt_nad - collateral_nad)
            .map_err(|_| ErrorCode::MarketMathOverflow)?
            .checked_neg()
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    }
}

pub(crate) fn empty_hlp_rebalance_receipt(target_asset: MarketAsset) -> HlpRebalanceReceipt {
    HlpRebalanceReceipt {
        target_asset,
        ..HlpRebalanceReceipt::default()
    }
}

#[cfg(test)]
fn deposit_base_hlp(
    market: &mut Market,
    base_deposit: u64,
    quote_borrow: u64,
) -> Result<(u64, u64, u64, HlpCurvePrices)> {
    let debt_shares = require_hlp_borrow_headroom(market, MarketAsset::Quote, quote_borrow)?;
    let hlp_supply_before = market.base_hlp_vault.hlp_supply;
    let nav_before_nad = if hlp_supply_before == 0 {
        0
    } else if market.base_hlp_vault.last_nav_nad > 0 {
        market.base_hlp_vault.last_nav_nad
    } else {
        hlp_nav_nad(market, MarketAsset::Base)?
    };
    let ylp_amount = ylp_for_live_reserve_deposit(market, base_deposit, quote_borrow)?;
    require!(ylp_amount > 0, ErrorCode::SlippageExceeded);
    market.base_side.credit_reserve(base_deposit, true)?;
    market.quote_side.credit_reserve(quote_borrow, false)?;
    market
        .base_hlp_vault
        .credit_hlp_live_reserve(MarketAsset::Quote, quote_borrow)?;
    market.base_side.shares.mint(ylp_amount)?;
    market.quote_side.shares.mint(ylp_amount)?;
    market.base_hlp_vault.add_debt_shares(debt_shares)?;
    market.base_hlp_vault.add_debt_principal(quote_borrow)?;
    market.base_hlp_vault.credit_ylp(ylp_amount)?;
    let current_prices = current_hlp_curve_prices(market)?;
    let current_nav_nad = hlp_nav_nad_with_prices(market, MarketAsset::Base, current_prices)?;
    let hlp_amount = if hlp_supply_before == 0 {
        base_deposit
    } else {
        let delta_nav_nad = current_nav_nad
            .checked_sub(nav_before_nad)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        hlp_shares_for_delta_nav(
            delta_nav_nad,
            nav_before_nad.max(market.base_hlp_vault.last_nav_nad),
            hlp_supply_before,
        )?
    };
    market.base_hlp_vault.mint_hlp(hlp_amount)?;
    market.base_hlp_vault.last_nav_nad = current_nav_nad;
    Ok((ylp_amount, hlp_amount, market.base_hlp_vault.hlp_supply, current_prices))
}

#[cfg(test)]
fn deposit_quote_hlp(
    market: &mut Market,
    quote_deposit: u64,
    base_borrow: u64,
) -> Result<(u64, u64, u64, HlpCurvePrices)> {
    let debt_shares = require_hlp_borrow_headroom(market, MarketAsset::Base, base_borrow)?;
    let hlp_supply_before = market.quote_hlp_vault.hlp_supply;
    let nav_before_nad = if hlp_supply_before == 0 {
        0
    } else if market.quote_hlp_vault.last_nav_nad > 0 {
        market.quote_hlp_vault.last_nav_nad
    } else {
        hlp_nav_nad(market, MarketAsset::Quote)?
    };
    let ylp_amount = ylp_for_live_reserve_deposit(market, base_borrow, quote_deposit)?;
    require!(ylp_amount > 0, ErrorCode::SlippageExceeded);
    market.base_side.credit_reserve(base_borrow, false)?;
    market.quote_side.credit_reserve(quote_deposit, true)?;
    market
        .quote_hlp_vault
        .credit_hlp_live_reserve(MarketAsset::Base, base_borrow)?;
    market.base_side.shares.mint(ylp_amount)?;
    market.quote_side.shares.mint(ylp_amount)?;
    market.quote_hlp_vault.add_debt_shares(debt_shares)?;
    market.quote_hlp_vault.add_debt_principal(base_borrow)?;
    market.quote_hlp_vault.credit_ylp(ylp_amount)?;
    let current_prices = current_hlp_curve_prices(market)?;
    let current_nav_nad = hlp_nav_nad_with_prices(market, MarketAsset::Quote, current_prices)?;
    let hlp_amount = if hlp_supply_before == 0 {
        quote_deposit
    } else {
        let delta_nav_nad = current_nav_nad
            .checked_sub(nav_before_nad)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        hlp_shares_for_delta_nav(
            delta_nav_nad,
            nav_before_nad.max(market.quote_hlp_vault.last_nav_nad),
            hlp_supply_before,
        )?
    };
    market.quote_hlp_vault.mint_hlp(hlp_amount)?;
    market.quote_hlp_vault.last_nav_nad = current_nav_nad;
    Ok((
        ylp_amount,
        hlp_amount,
        market.quote_hlp_vault.hlp_supply,
        current_prices,
    ))
}

fn debit_cash_for_hlp_interest(borrowed_side: &mut crate::state::MarketSide, interest_paid: u64) -> Result<()> {
    if interest_paid == 0 {
        return Ok(());
    }
    borrowed_side.reserves.live_reserve = borrowed_side
        .reserves
        .live_reserve
        .checked_sub(interest_paid)
        .ok_or(ErrorCode::ReserveUnderflow)?;
    borrowed_side.reserves.cash_reserve = borrowed_side
        .reserves
        .cash_reserve
        .checked_sub(interest_paid)
        .ok_or(ErrorCode::CashReserveUnderflow)?;
    Ok(())
}

#[cfg(test)]
fn credit_hlp_live_reserve(
    market: &mut Market,
    target_asset: MarketAsset,
    reserve_asset: MarketAsset,
    amount: u64,
) -> Result<()> {
    if amount == 0 {
        return Ok(());
    }
    market.side_mut(reserve_asset).credit_reserve(amount, false)?;
    match target_asset {
        MarketAsset::Base => market.base_hlp_vault.credit_hlp_live_reserve(reserve_asset, amount),
        MarketAsset::Quote => market.quote_hlp_vault.credit_hlp_live_reserve(reserve_asset, amount),
    }
}

fn debit_hlp_live_reserve(
    market: &mut Market,
    target_asset: MarketAsset,
    reserve_asset: MarketAsset,
    amount: u64,
) -> Result<()> {
    if amount == 0 {
        return Ok(());
    }
    market.side_mut(reserve_asset).debit_reserve(amount, false)?;
    match target_asset {
        MarketAsset::Base => market.base_hlp_vault.debit_hlp_live_reserve(reserve_asset, amount),
        MarketAsset::Quote => market.quote_hlp_vault.debit_hlp_live_reserve(reserve_asset, amount),
    }
}

#[cfg(test)]
fn debit_hlp_rebalance_reserve(
    market: &mut Market,
    target_asset: MarketAsset,
    reserve_asset: MarketAsset,
    amount: u64,
    interest_paid: u64,
) -> Result<()> {
    let plan = plan_hlp_rebalance_reserve(market, target_asset, reserve_asset, amount, interest_paid)?;
    let mut post = capture_hlp_rebalance_state(market, target_asset);
    apply_hlp_reserve_debit_to_state(
        &mut post,
        target_asset,
        plan.reserve_asset,
        plan.reserve_debit,
        plan.cash_debit,
        plan.interest_paid,
    )?;
    commit_hlp_rebalance_state(market, target_asset, post);
    Ok(())
}

fn plan_hlp_rebalance_reserve(
    market: &Market,
    target_asset: MarketAsset,
    reserve_asset: MarketAsset,
    amount: u64,
    interest_paid: u64,
) -> Result<HlpReserveDebitPlan> {
    if amount == 0 {
        require_eq!(interest_paid, 0, ErrorCode::HlpSettlementUnavailable);
        return Ok(HlpReserveDebitPlan {
            reserve_asset,
            reserve_debit: 0,
            cash_debit: 0,
            interest_paid: 0,
        });
    }
    // Funding interest is paid from the payer's borrowed-asset yLP leg, not
    // as an additional debit from shared live reserves. This keeps the yLP
    // exchange rate proportional across the burn and prevents either hLP from
    // subsidizing the payer through its remaining yLP claim.
    require_gte!(amount, interest_paid, ErrorCode::HlpSettlementUnavailable);
    let hlp_live_available = match target_asset {
        MarketAsset::Base => market.base_hlp_vault.hlp_live_reserve(reserve_asset),
        MarketAsset::Quote => market.quote_hlp_vault.hlp_live_reserve(reserve_asset),
    };
    let cash_debit = interest_paid.max(amount.saturating_sub(hlp_live_available));
    let hlp_live_debit = amount.checked_sub(cash_debit).ok_or(ErrorCode::MarketMathOverflow)?;
    require_gte!(hlp_live_available, hlp_live_debit, ErrorCode::BrokenInvariant);
    Ok(HlpReserveDebitPlan {
        reserve_asset,
        reserve_debit: amount,
        cash_debit,
        interest_paid,
    })
}

#[cfg(test)]
fn debit_hlp_deleverage_reserve_legs(
    market: &mut Market,
    target_asset: MarketAsset,
    target_leg_amount: u64,
    borrowed_leg_amount: u64,
    interest_paid: u64,
) -> Result<(u64, u64)> {
    let plan = plan_hlp_deleverage_reserve_legs(
        market,
        target_asset,
        target_leg_amount,
        borrowed_leg_amount,
        interest_paid,
    )?;
    let mut post = capture_hlp_rebalance_state(market, target_asset);
    let borrowed_asset = target_asset.opposite();
    let base_interest_paid = if borrowed_asset == MarketAsset::Base {
        interest_paid
    } else {
        0
    };
    let quote_interest_paid = if borrowed_asset == MarketAsset::Quote {
        interest_paid
    } else {
        0
    };
    apply_hlp_reserve_debit_to_state(
        &mut post,
        target_asset,
        MarketAsset::Base,
        plan.base_reserve_debit,
        plan.base_cash_debit,
        base_interest_paid,
    )?;
    apply_hlp_reserve_debit_to_state(
        &mut post,
        target_asset,
        MarketAsset::Quote,
        plan.quote_reserve_debit,
        plan.quote_cash_debit,
        quote_interest_paid,
    )?;
    commit_hlp_rebalance_state(market, target_asset, post);
    let (target_reserve_debit, borrowed_reserve_debit) = match target_asset {
        MarketAsset::Base => (plan.base_reserve_debit, plan.quote_reserve_debit),
        MarketAsset::Quote => (plan.quote_reserve_debit, plan.base_reserve_debit),
    };
    Ok((target_reserve_debit, borrowed_reserve_debit))
}

fn plan_hlp_deleverage_reserve_legs(
    market: &Market,
    target_asset: MarketAsset,
    target_leg_amount: u64,
    borrowed_leg_amount: u64,
    interest_paid: u64,
) -> Result<HlpDeleverageReservePlan> {
    let borrowed_asset = target_asset.opposite();
    let (target_reserve_debit, borrowed_reserve_debit, exact_out_checkpoint) = if interest_paid > borrowed_leg_amount {
        // Burn the proportional A/B yLP claim, but retain the exact target
        // input required to buy the borrowed-asset interest shortfall. The
        // resulting reserve deltas are the baseline burn plus one fee-free
        // exact-out curve settlement: target -(A-T), borrowed -I.
        let settlement = plan_settled_close_target_amount(
            market,
            target_asset,
            target_leg_amount,
            borrowed_leg_amount,
            interest_paid,
        )?;
        let checkpoint = settlement.exact_out_checkpoint.ok_or(ErrorCode::BrokenInvariant)?;
        (settlement.target_amount, interest_paid, Some(checkpoint))
    } else {
        (target_leg_amount, borrowed_leg_amount, None)
    };
    let (base_reserve_debit, quote_reserve_debit) = match target_asset {
        MarketAsset::Base => (target_reserve_debit, borrowed_reserve_debit),
        MarketAsset::Quote => (borrowed_reserve_debit, target_reserve_debit),
    };
    let base_interest_paid = if borrowed_asset == MarketAsset::Base {
        interest_paid
    } else {
        0
    };
    let quote_interest_paid = if borrowed_asset == MarketAsset::Quote {
        interest_paid
    } else {
        0
    };
    let base = plan_hlp_rebalance_reserve(
        market,
        target_asset,
        MarketAsset::Base,
        base_reserve_debit,
        base_interest_paid,
    )?;
    let quote = plan_hlp_rebalance_reserve(
        market,
        target_asset,
        MarketAsset::Quote,
        quote_reserve_debit,
        quote_interest_paid,
    )?;
    Ok(HlpDeleverageReservePlan {
        base_reserve_debit,
        quote_reserve_debit,
        base_cash_debit: base.cash_debit,
        quote_cash_debit: quote.cash_debit,
        exact_out_checkpoint,
    })
}

fn release_hlp_backing_inventory(
    market: &mut Market,
    target_asset: MarketAsset,
    hlp_amount: u64,
    hlp_supply: u64,
) -> Result<()> {
    for reserve_asset in [MarketAsset::Base, MarketAsset::Quote] {
        let inventory = market.side(reserve_asset).reserves.hlp_backing_inventory(target_asset);
        let release = if hlp_amount == hlp_supply {
            inventory
        } else {
            proportional(inventory, hlp_amount, hlp_supply)?
        };
        if release > 0 {
            let side = market.side_mut(reserve_asset);
            side.reserves.debit_hlp_backing_inventory(target_asset, release)?;
            side.credit_reserve(release, true)?;
        }
    }
    Ok(())
}

fn settled_close_target_amount(
    market: &Market,
    target_asset: MarketAsset,
    target_redeemed: u64,
    borrowed_redeemed: u64,
    debt_repaid: u64,
) -> Result<u64> {
    Ok(
        plan_settled_close_target_amount(market, target_asset, target_redeemed, borrowed_redeemed, debt_repaid)?
            .target_amount,
    )
}

fn curve_reserves_after_entitlement(
    market: &Market,
    target_asset: MarketAsset,
    target_redeemed: u64,
    borrowed_redeemed: u64,
) -> Result<(CurveReservesNad, CurveReservesNad)> {
    let borrowed_asset = target_asset.opposite();
    let start = market.curve_reserves_nad()?;
    let mut post = start;
    let target_redeemed_nad = normalize_to_nad(target_redeemed as u128, market.side(target_asset).asset_decimals)?;
    let borrowed_redeemed_nad =
        normalize_to_nad(borrowed_redeemed as u128, market.side(borrowed_asset).asset_decimals)?;
    match target_asset {
        MarketAsset::Base => {
            post.base = post
                .base
                .checked_sub(target_redeemed_nad)
                .ok_or(ErrorCode::ReserveUnderflow)?;
            post.quote = post
                .quote
                .checked_sub(borrowed_redeemed_nad)
                .ok_or(ErrorCode::ReserveUnderflow)?;
        }
        MarketAsset::Quote => {
            post.quote = post
                .quote
                .checked_sub(target_redeemed_nad)
                .ok_or(ErrorCode::ReserveUnderflow)?;
            post.base = post
                .base
                .checked_sub(borrowed_redeemed_nad)
                .ok_or(ErrorCode::ReserveUnderflow)?;
        }
    }
    Ok((start, post))
}

fn plan_settled_close_target_amount(
    market: &Market,
    target_asset: MarketAsset,
    target_redeemed: u64,
    borrowed_redeemed: u64,
    debt_repaid: u64,
) -> Result<SettledCloseTargetPlan> {
    if borrowed_redeemed == debt_repaid {
        return Ok(SettledCloseTargetPlan {
            target_amount: target_redeemed,
            exact_out_checkpoint: None,
        });
    }

    let borrowed_asset = target_asset.opposite();
    if borrowed_redeemed > debt_repaid {
        // Price the settlement conversion against the executable curve after
        // redeeming the ordinary yLP claim. Global yLP entitlement is always
        // based on live reserves, including accrued-but-unpaid lending
        // interest.
        let (_, reserves) = curve_reserves_after_entitlement(market, target_asset, target_redeemed, borrowed_redeemed)?;
        let surplus_borrowed = borrowed_redeemed
            .checked_sub(debt_repaid)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let slot = curve_slot(market);
        let prepared =
            market.prepare_curve_for_reserves_nad(reserves, market.current_curve_center_price_nad()?, slot)?;
        let target_from_surplus = market
            .quote_curve_exact_in_for_prepared_nad(borrowed_asset, surplus_borrowed, prepared, slot)?
            .amount_out;
        return Ok(SettledCloseTargetPlan {
            target_amount: target_redeemed
                .checked_add(target_from_surplus)
                .ok_or(ErrorCode::MarketMathOverflow)?,
            exact_out_checkpoint: None,
        });
    }

    let (target_amount, checkpoint) =
        HlpExactOutSettlementCheckpoint::plan(market, target_asset, target_redeemed, borrowed_redeemed, debt_repaid)?;
    Ok(SettledCloseTargetPlan {
        target_amount,
        exact_out_checkpoint: Some(checkpoint),
    })
}

#[cfg(test)]
pub(crate) fn rebalance_one_hlp(
    market: &mut Market,
    target_asset: MarketAsset,
    _current_slot: u64,
) -> Result<HlpRebalanceReceipt> {
    checkpoint_hlp_yield_from_ylp(market, target_asset)?;
    let valuation = current_hlp_valuation(market, target_asset)?;
    let receipt = rebalance_hlp_from_valuation(market, target_asset, valuation)?;
    let post_prices = current_hlp_curve_prices(market)?;
    refresh_hlp_after_rebalance(market, target_asset, receipt, post_prices)
}

/// An active-hLP post-trade endpoint is one joint state for CPMM and
/// concentration alike: value both numeraires before either transition, apply
/// the canonical base-then-quote order, then refresh both from the same exact
/// final curve price.
pub(crate) fn rebalance_hlps_after_swap_joint(
    market: &mut Market,
    current_slot: u64,
    start_price_nad: Option<u64>,
) -> Result<(
    HlpRebalanceReceipt,
    HlpRebalanceReceipt,
    Option<crate::math::ConcentratedEvaluation>,
    HlpCurvePrices,
    HlpLifecycleEndpoint,
    HlpLifecycleEndpoint,
)> {
    rebalance_hlps_after_swap_joint_with_curve_mode(
        market,
        start_price_nad,
        HlpPostRebalanceCurveMode::Canonical { current_slot },
    )
}

#[derive(Clone, Copy)]
enum HlpPostRebalanceCurveMode {
    Canonical {
        current_slot: u64,
    },
    Guidance {
        start: ConcentratedGuidanceCurve,
        current_slot: u64,
    },
}

fn rebalance_hlps_after_swap_joint_with_curve_mode(
    market: &mut Market,
    start_price_nad: Option<u64>,
    curve_mode: HlpPostRebalanceCurveMode,
) -> Result<(
    HlpRebalanceReceipt,
    HlpRebalanceReceipt,
    Option<crate::math::ConcentratedEvaluation>,
    HlpCurvePrices,
    HlpLifecycleEndpoint,
    HlpLifecycleEndpoint,
)> {
    debug_log_heap(400);
    let base_active = market.base_hlp_vault.hlp_supply > 0 || market.base_hlp_vault.residual_exposure != 0;
    let quote_active = market.quote_hlp_vault.hlp_supply > 0 || market.quote_hlp_vault.residual_exposure != 0;
    if matches!(curve_mode, HlpPostRebalanceCurveMode::Canonical { .. }) {
        checkpoint_hlp_yield_from_ylp_pair(market, true, true)?;
    }
    debug_log_heap(401);
    let prices = match start_price_nad {
        Some(price) => hlp_curve_prices_from_base_price_nad(price as u128)?,
        None => current_hlp_curve_prices(market)?,
    };
    debug_log_heap(402);
    let (base_values, quote_values) =
        current_hlp_inventory_values_pair_nad_with_prices(market, prices, base_active, quote_active)?;
    let base_valuation = base_active
        .then(|| hlp_valuation_from_values(base_values, prices))
        .transpose()?;
    let quote_valuation = quote_active
        .then(|| hlp_valuation_from_values(quote_values, prices))
        .transpose()?;
    debug_log_heap(403);

    let start_ylp_supply = market.base_side.shares.ylp_supply;
    require_eq!(
        start_ylp_supply,
        market.quote_side.shares.ylp_supply,
        ErrorCode::BrokenInvariant
    );
    let (base, quote) = match curve_mode {
        HlpPostRebalanceCurveMode::Canonical { .. } => {
            plan_and_apply_hlp_rebalance_pair(market, base_valuation, quote_valuation)?
        }
        HlpPostRebalanceCurveMode::Guidance { .. } => {
            plan_and_apply_hlp_rebalance_pair_guidance(market, base_valuation, quote_valuation)?
        }
    };
    let inventory_changed = base.ylp_mint_amount != 0
        || base.ylp_burn_amount != 0
        || quote.ylp_mint_amount != 0
        || quote.ylp_burn_amount != 0;
    let final_evaluation = if inventory_changed {
        Some(match curve_mode {
            HlpPostRebalanceCurveMode::Canonical { current_slot } => {
                market.checkpoint_amm_neutral_inventory(current_slot)?
            }
            HlpPostRebalanceCurveMode::Guidance { start, current_slot } => {
                let final_ylp_supply = market.base_side.shares.ylp_supply;
                require_eq!(
                    final_ylp_supply,
                    market.quote_side.shares.ylp_supply,
                    ErrorCode::BrokenInvariant
                );
                require!(start_ylp_supply > 0 && final_ylp_supply > 0, ErrorCode::SupplyUnderflow);
                let invariant_d =
                    mul_div_u128(start.invariant_d(), final_ylp_supply as u128, start_ylp_supply as u128)?;
                let reserves = market.curve_reserves_nad()?;
                let final_prepared =
                    start.prepare_guidance_successor_with_invariant(reserves.base, reserves.quote, invariant_d)?;
                let evaluation = final_prepared.evaluation()?;
                market.ensure_amm_initialized(current_slot)?;
                let q_per_share_nad = market.curve_q_per_share_nad(evaluation.balanced_equivalent_q)?;
                market.amm.commit_invariant(evaluation.invariant_d)?;
                market.amm.checkpoint_neutral_liquidity(q_per_share_nad);
                evaluation
            }
        })
    } else {
        None
    };
    debug_log_heap(406);
    let final_prices = match final_evaluation {
        Some(evaluation) => hlp_curve_prices_from_base_price_nad(evaluation.marginal_price_nad)?,
        None => prices,
    };
    let (base_final_values, quote_final_values) =
        current_hlp_inventory_values_pair_nad_with_prices(market, final_prices, base_active, quote_active)?;
    let base = if base_active {
        refresh_hlp_after_rebalance_from_valuation(
            market,
            MarketAsset::Base,
            base,
            hlp_valuation_from_values(base_final_values, final_prices)?,
        )?
    } else {
        base
    };
    debug_log_heap(407);
    let quote = if quote_active {
        refresh_hlp_after_rebalance_from_valuation(
            market,
            MarketAsset::Quote,
            quote,
            hlp_valuation_from_values(quote_final_values, final_prices)?,
        )?
    } else {
        quote
    };
    debug_log_heap(408);
    Ok((
        base,
        quote,
        final_evaluation,
        final_prices,
        hlp_lifecycle_endpoint_from_values(base_final_values)?,
        hlp_lifecycle_endpoint_from_values(quote_final_values)?,
    ))
}

pub(crate) fn require_hlp_end_to_end_tracking(
    market: &Market,
    receipt: HlpRebalanceReceipt,
    final_prices: HlpCurvePrices,
) -> Result<()> {
    if receipt.tracking_loss_budget_nad == 0 {
        return Ok(());
    }
    let tracking_delta_nad = hlp_end_to_end_tracking_delta(market, receipt, final_prices)?;
    require!(
        tracking_delta_nad.unsigned_abs() <= receipt.tracking_loss_budget_nad,
        ErrorCode::HlpSettlementUnavailable
    );
    Ok(())
}

pub(crate) fn hlp_end_to_end_tracking_delta(
    market: &Market,
    receipt: HlpRebalanceReceipt,
    final_prices: HlpCurvePrices,
) -> Result<i128> {
    let tracking = HlpTrackingReference {
        principal_nav_nad: receipt.tracking_start_nav_nad,
        loss_budget_nad: receipt.tracking_loss_budget_nad,
        base_unrealized_interest: receipt.tracking_base_unrealized_interest,
        quote_unrealized_interest: receipt.tracking_quote_unrealized_interest,
        start_ylp_shares: receipt.tracking_start_ylp_shares,
        start_ylp_supply: receipt.tracking_start_ylp_supply,
    };
    let values = current_hlp_inventory_values_nad_with_prices(market, receipt.target_asset, final_prices)?;
    let collateral = values
        .target_inventory_value_nad
        .checked_add(values.opposite_inventory_value_nad)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let final_principal_nav_nad = signed_value_difference(collateral, values.debt_value_nad)?;
    let (_, _, tracking_delta_nad) = hlp_tracking_deltas_nad(
        market,
        receipt.target_asset,
        final_prices,
        final_principal_nav_nad,
        tracking,
    )?;
    tracking_delta_nad
        .checked_sub(receipt.tracking_retained_contribution_nad)
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

#[cfg(test)]
pub(crate) fn current_hlp_signed_navs(market: &Market) -> Result<(i128, i128)> {
    let prices = current_hlp_curve_prices(market)?;
    current_hlp_signed_navs_with_prices(market, prices)
}

/// Values both active hLP principals from a caller-proved executable price.
/// This is the same accounting path as `current_hlp_signed_navs`; it merely
/// avoids solving the identical curve again when an identity-bound endpoint
/// already carries the marginal price.
pub(crate) fn current_hlp_signed_navs_with_prices(market: &Market, prices: HlpCurvePrices) -> Result<(i128, i128)> {
    Ok((
        concentrated_hlp_start(market, MarketAsset::Base, prices)?
            .tracking
            .principal_nav_nad,
        concentrated_hlp_start(market, MarketAsset::Quote, prices)?
            .tracking
            .principal_nav_nad,
    ))
}

pub(crate) fn rebase_hlp_tracking_for_socialized_loss(
    receipt: &mut HlpRebalanceReceipt,
    nav_before_nad: i128,
    nav_after_nad: i128,
) -> Result<()> {
    if receipt.tracking_loss_budget_nad == 0 {
        return Ok(());
    }
    let authorized_delta = nav_after_nad
        .checked_sub(nav_before_nad)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    receipt.tracking_start_nav_nad = receipt
        .tracking_start_nav_nad
        .checked_add(authorized_delta)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    Ok(())
}

fn rebalance_hlp_from_valuation(
    market: &mut Market,
    target_asset: MarketAsset,
    valuation: HlpValuation,
) -> Result<HlpRebalanceReceipt> {
    let plan = plan_hlp_rebalance_from_valuation(market, target_asset, valuation)?;
    apply_hlp_rebalance_plan(market, plan)
}

fn plan_hlp_rebalance_from_valuation(
    market: &Market,
    target_asset: MarketAsset,
    valuation: HlpValuation,
) -> Result<HlpRebalancePlan> {
    let ideal_delta = recognized_hlp_residual_exposure(valuation.ideal_delta, valuation.nav_nad);
    Ok(if !valuation.proportional_hedge_available && ideal_delta != 0 {
        // No finite proportional liquidity change can neutralize opposite
        // exposure when the target-side yLP claim has rounded to zero. Keep a
        // fail-closed residual signal without mutating reserves; importantly,
        // this vault-local condition must not make generic market updates fail.
        plan_hlp_noop(
            market,
            target_asset,
            ideal_delta,
            valuation,
            false,
            HlpRebalanceNoopReason::Unhedgeable,
        )
    } else if ideal_delta > 0 {
        plan_leverage_up_proportional_with_cash_floors(
            market,
            target_asset,
            ideal_delta,
            valuation,
            SwapCashFloors::default(),
        )?
    } else if ideal_delta < 0 {
        plan_deleverage_proportional_with_cash_floors(
            market,
            target_asset,
            ideal_delta,
            valuation,
            SwapCashFloors::default(),
        )?
    } else {
        plan_hlp_noop(
            market,
            target_asset,
            0,
            valuation,
            false,
            HlpRebalanceNoopReason::Settled,
        )
    })
}

#[cfg(test)]
fn current_hlp_ideal_delta(market: &Market, target_asset: MarketAsset) -> Result<i128> {
    current_hlp_valuation(market, target_asset).map(|valuation| valuation.ideal_delta)
}

#[cfg(test)]
fn current_hlp_valuation(market: &Market, target_asset: MarketAsset) -> Result<HlpValuation> {
    let prices = current_hlp_curve_prices(market)?;
    current_hlp_valuation_with_prices(market, target_asset, prices)
}

fn current_hlp_valuation_with_prices(
    market: &Market,
    target_asset: MarketAsset,
    prices: HlpCurvePrices,
) -> Result<HlpValuation> {
    let values = current_hlp_inventory_values_nad_with_prices(market, target_asset, prices)?;
    hlp_valuation_from_values(values, prices)
}

fn hlp_valuation_from_values(values: HlpInventoryValuesNad, prices: HlpCurvePrices) -> Result<HlpValuation> {
    let collateral = values
        .target_inventory_value_nad
        .checked_add(values.opposite_inventory_value_nad)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let debt = values.debt_value_nad;
    // An aggregate hLP can become underwater or have its target-side yLP claim
    // round to zero. Those are vault-local fail-closed states, not reasons to
    // brick swaps, withdrawals from the other vault, or the global AMM
    // controller. A zero NAV keeps new deposits gated by nonzero residual exposure.
    let nav_nad = collateral.saturating_sub(debt);
    let ideal_delta = if values.target_inventory_value_nad == 0 {
        // There is no finite proportional-liquidity solution in this
        // degenerate coordinate. Persist the signed opposite exposure as an
        // explicit nonzero residual signal instead of throwing a denominator
        // error from a generic checkpoint.
        hlp_opposite_exposure_nad(values)?
    } else {
        let exposure = hlp_opposite_exposure_nad(values)?;
        if exposure == 0 {
            0
        } else {
            let opposite_magnitude = mul_div_u128(
                exposure.unsigned_abs(),
                values.opposite_inventory_value_nad,
                values.target_inventory_value_nad,
            )?;
            let total_magnitude = exposure
                .unsigned_abs()
                .checked_add(opposite_magnitude)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            if exposure < 0 {
                if total_magnitude == 1_u128 << 127 {
                    i128::MIN
                } else {
                    i128::try_from(total_magnitude)
                        .map_err(|_| ErrorCode::MarketMathOverflow)?
                        .checked_neg()
                        .ok_or(ErrorCode::MarketMathOverflow)?
                }
            } else {
                i128::try_from(total_magnitude).map_err(|_| ErrorCode::MarketMathOverflow)?
            }
        }
    };
    Ok(HlpValuation {
        ideal_delta,
        nav_nad,
        values,
        prices,
        proportional_hedge_available: values.target_inventory_value_nad > 0,
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ProportionalRebalanceAmounts {
    target_leg_amount: u64,
    borrowed_leg_amount: u64,
    debt_amount: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct HlpValuation {
    ideal_delta: i128,
    nav_nad: u128,
    values: HlpInventoryValuesNad,
    prices: HlpCurvePrices,
    /// Derived status, recomputed from actual yLP claims at every checkpoint.
    /// False means no finite proportional hedge exists in this coordinate.
    proportional_hedge_available: bool,
}

/// Candidate-only economic state used by the compact hLP lifecycle planner.
///
/// Keep immutable conversion/debt-index inputs in `HlpPlannerStatic`; the two
/// reusable state values (`start` and `work`) are exactly 256 bytes each.  No
/// yield checkpoints, settlement references, or AMM authority can be carried
/// through this representation.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct HlpPlannerSide {
    pub(super) live_reserve: u64,
    pub(super) cash_reserve: u64,
    pub(super) base_hlp_backing_inventory: u64,
    pub(super) quote_hlp_backing_inventory: u64,
    pub(super) ylp_supply: u64,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct HlpPlannerVault {
    pub(super) debt_shares: u128,
    pub(super) residual_exposure: i128,
    pub(super) ylp_shares: u64,
    pub(super) base_hlp_live_reserve: u64,
    pub(super) quote_hlp_live_reserve: u64,
    pub(super) debt_principal: u64,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct HlpPlannerDebt {
    pub(super) isolated_base_shares: u128,
    pub(super) isolated_quote_shares: u128,
    pub(super) isolated_base_principal: u64,
    pub(super) isolated_quote_principal: u64,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct HlpPlannerState {
    pub(super) base_side: HlpPlannerSide,
    pub(super) quote_side: HlpPlannerSide,
    pub(super) base_vault: HlpPlannerVault,
    pub(super) quote_vault: HlpPlannerVault,
    pub(super) debt: HlpPlannerDebt,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct HlpPlannerStatic {
    pub(super) base_borrow_index_nad: u128,
    pub(super) quote_borrow_index_nad: u128,
    pub(super) base_fixed_public_interest: u128,
    pub(super) quote_fixed_public_interest: u128,
    pub(super) start_ylp_supply: u64,
    pub(super) base_decimals: u8,
    pub(super) quote_decimals: u8,
    pub(super) base_has_hlp_supply: bool,
    pub(super) quote_has_hlp_supply: bool,
    pub(super) retain_dynamic_surcharge_at_start: bool,
    pub(super) retain_dynamic_surcharge_after_inventory: bool,
}

#[cfg(test)]
const _: [(); 256] = [(); core::mem::size_of::<HlpPlannerState>()];

#[cfg(test)]
const _: [(); 80] = [(); core::mem::size_of::<HlpPlannerStatic>()];

#[cfg(test)]
impl HlpPlannerStatic {
    pub(super) fn capture(market: &Market) -> Result<Self> {
        require_eq!(
            market.base_side.shares.ylp_supply,
            market.quote_side.shares.ylp_supply,
            ErrorCode::BrokenInvariant
        );
        let base_fixed_debt = market.debt.fixed_base_debt()?;
        let quote_fixed_debt = market.debt.fixed_quote_debt()?;
        let base_fixed_public_interest = base_fixed_debt
            .checked_sub(u128::from(market.debt.fixed_base_principal).min(base_fixed_debt))
            .ok_or(ErrorCode::DebtMathOverflow)?;
        let quote_fixed_public_interest = quote_fixed_debt
            .checked_sub(u128::from(market.debt.fixed_quote_principal).min(quote_fixed_debt))
            .ok_or(ErrorCode::DebtMathOverflow)?;

        // `defer_amm_retention_target` depends only on the frozen AMM/config
        // state and on whether inventory changed, not on the candidate size.
        // Compute its route once; compact candidates select it only after both
        // Base and Quote preposition legs have been derived.
        let mut deferred = Market::default();
        deferred.clone_from(market);
        deferred.defer_amm_retention_target()?;

        Ok(Self {
            base_borrow_index_nad: market.debt.base_borrow_index_nad,
            quote_borrow_index_nad: market.debt.quote_borrow_index_nad,
            base_fixed_public_interest,
            quote_fixed_public_interest,
            start_ylp_supply: market.base_side.shares.ylp_supply,
            base_decimals: market.base_side.asset_decimals,
            quote_decimals: market.quote_side.asset_decimals,
            base_has_hlp_supply: market.base_hlp_vault.hlp_supply > 0,
            quote_has_hlp_supply: market.quote_hlp_vault.hlp_supply > 0,
            retain_dynamic_surcharge_at_start: market.amm.retain_dynamic_surcharge,
            retain_dynamic_surcharge_after_inventory: deferred.amm.retain_dynamic_surcharge,
        })
    }

    pub(super) const fn borrow_index(self, asset: MarketAsset) -> u128 {
        match asset {
            MarketAsset::Base => self.base_borrow_index_nad,
            MarketAsset::Quote => self.quote_borrow_index_nad,
        }
    }

    const fn decimals(self, asset: MarketAsset) -> u8 {
        match asset {
            MarketAsset::Base => self.base_decimals,
            MarketAsset::Quote => self.quote_decimals,
        }
    }

    const fn fixed_public_interest(self, asset: MarketAsset) -> u128 {
        match asset {
            MarketAsset::Base => self.base_fixed_public_interest,
            MarketAsset::Quote => self.quote_fixed_public_interest,
        }
    }

    const fn has_hlp_supply(self, asset: MarketAsset) -> bool {
        match asset {
            MarketAsset::Base => self.base_has_hlp_supply,
            MarketAsset::Quote => self.quote_has_hlp_supply,
        }
    }
}

#[cfg(test)]
impl HlpPlannerState {
    pub(super) fn capture(market: &Market) -> Self {
        let side = |asset: MarketAsset| {
            let side = market.side(asset);
            HlpPlannerSide {
                live_reserve: side.reserves.live_reserve,
                cash_reserve: side.reserves.cash_reserve,
                base_hlp_backing_inventory: side.reserves.base_hlp_backing_inventory,
                quote_hlp_backing_inventory: side.reserves.quote_hlp_backing_inventory,
                ylp_supply: side.shares.ylp_supply,
            }
        };
        let vault = |asset: MarketAsset| {
            let vault = match asset {
                MarketAsset::Base => &market.base_hlp_vault,
                MarketAsset::Quote => &market.quote_hlp_vault,
            };
            HlpPlannerVault {
                debt_shares: vault.debt_shares,
                residual_exposure: vault.residual_exposure,
                ylp_shares: vault.ylp_shares,
                base_hlp_live_reserve: vault.base_hlp_live_reserve,
                quote_hlp_live_reserve: vault.quote_hlp_live_reserve,
                debt_principal: vault.debt_principal,
            }
        };
        Self {
            base_side: side(MarketAsset::Base),
            quote_side: side(MarketAsset::Quote),
            base_vault: vault(MarketAsset::Base),
            quote_vault: vault(MarketAsset::Quote),
            debt: HlpPlannerDebt {
                isolated_base_shares: market.debt.isolated_base_shares,
                isolated_quote_shares: market.debt.isolated_quote_shares,
                isolated_base_principal: market.debt.isolated_base_principal,
                isolated_quote_principal: market.debt.isolated_quote_principal,
            },
        }
    }

    pub(super) const fn side(self, asset: MarketAsset) -> HlpPlannerSide {
        match asset {
            MarketAsset::Base => self.base_side,
            MarketAsset::Quote => self.quote_side,
        }
    }

    pub(super) fn side_mut(&mut self, asset: MarketAsset) -> &mut HlpPlannerSide {
        match asset {
            MarketAsset::Base => &mut self.base_side,
            MarketAsset::Quote => &mut self.quote_side,
        }
    }

    pub(super) const fn vault(self, target_asset: MarketAsset) -> HlpPlannerVault {
        match target_asset {
            MarketAsset::Base => self.base_vault,
            MarketAsset::Quote => self.quote_vault,
        }
    }

    pub(super) fn vault_mut(&mut self, target_asset: MarketAsset) -> &mut HlpPlannerVault {
        match target_asset {
            MarketAsset::Base => &mut self.base_vault,
            MarketAsset::Quote => &mut self.quote_vault,
        }
    }

    pub(super) fn unrealized_interest(self, fixed: HlpPlannerStatic, asset: MarketAsset) -> Result<u128> {
        let (shares, principal) = match asset {
            MarketAsset::Base => (self.debt.isolated_base_shares, self.debt.isolated_base_principal),
            MarketAsset::Quote => (self.debt.isolated_quote_shares, self.debt.isolated_quote_principal),
        };
        let isolated_debt = Debt::shares_to_debt(shares, fixed.borrow_index(asset))?;
        let isolated_interest = isolated_debt
            .checked_sub(u128::from(principal).min(isolated_debt))
            .ok_or(ErrorCode::DebtMathOverflow)?;
        fixed
            .fixed_public_interest(asset)
            .checked_add(isolated_interest)
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    }

    pub(super) fn curve_reserve(self, fixed: HlpPlannerStatic, asset: MarketAsset) -> Result<u64> {
        let reserve = u128::from(self.side(asset).live_reserve)
            .checked_sub(self.unrealized_interest(fixed, asset)?)
            .ok_or(ErrorCode::BrokenInvariant)?;
        u64::try_from(reserve).map_err(|_| ErrorCode::MarketMathOverflow.into())
    }

    pub(super) fn curve_reserves_nad(self, fixed: HlpPlannerStatic) -> Result<CurveReservesNad> {
        Ok(CurveReservesNad {
            base: normalize_to_nad(
                self.curve_reserve(fixed, MarketAsset::Base)? as u128,
                fixed.decimals(MarketAsset::Base),
            )?,
            quote: normalize_to_nad(
                self.curve_reserve(fixed, MarketAsset::Quote)? as u128,
                fixed.decimals(MarketAsset::Quote),
            )?,
        })
    }

    pub(super) const fn active(self, fixed: HlpPlannerStatic, target_asset: MarketAsset) -> bool {
        fixed.has_hlp_supply(target_asset) || self.vault(target_asset).residual_exposure != 0
    }
}

/// The complete mutable footprint of one individual-hLP actuation. Planning
/// binds to this compact checkpoint; apply derives the successor from the
/// semantic plan and commits it only after every checked transition succeeds.
/// Yield checkpoints and pair ordering remain explicit outer barriers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct HlpRebalanceState {
    target_ylp_shares: u64,
    target_debt_shares: u128,
    target_debt_principal: u64,
    target_base_hlp_live_reserve: u64,
    target_quote_hlp_live_reserve: u64,
    base_ylp_supply: u64,
    quote_ylp_supply: u64,
    base_live_reserve: u64,
    base_cash_reserve: u64,
    base_target_backing_inventory: u64,
    quote_live_reserve: u64,
    quote_cash_reserve: u64,
    quote_target_backing_inventory: u64,
    borrow_index_nad: u128,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HlpRebalancePlanCommon {
    start: HlpRebalanceState,
    target_asset: MarketAsset,
    ideal_delta_nad: i128,
    nav_nad: u128,
    capacity_bound: bool,
    current_swap_fee_eligible_ylp_shares: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HlpRebalanceNoopReason {
    Settled,
    Unhedgeable,
    CapacityOrGranularity,
}

/// Proof-independent exact-out facts consumed by the single Stage1 state
/// transition.  Authoritative and guidance constructors remain separate and
/// sealed; only these scalar consequences are shared by the pure kernel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HlpExactOutSettlementFacts {
    start_curve_reserves_nad: CurveReservesNad,
    post_entitlement_curve_reserves_nad: CurveReservesNad,
    successor_curve_reserves_nad: CurveReservesNad,
    borrowed_shortfall: u64,
    selected_input_nad: u128,
    target_retained: u64,
}

/// Opaque capability for the canonical fee-free target-side exact-out
/// conversion used when accrued interest exceeds the borrowed yLP leg. Its
/// private seal and fields prevent apply callers from supplying a
/// self-consistent but non-canonical bracket/input tuple.
mod hlp_exact_out_settlement {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Seal;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) struct Checkpoint {
        current_slot: u64,
        curve_revision: u64,
        center_price_nad: u64,
        parameters: ConcentrationParameters,
        retain_dynamic_surcharge: bool,
        start_curve_reserves_nad: CurveReservesNad,
        post_entitlement_curve_reserves_nad: CurveReservesNad,
        successor_curve_reserves_nad: CurveReservesNad,
        start_invariant_d: u128,
        borrowed_shortfall: u64,
        selected_input_nad: u128,
        target_retained: u64,
        _seal: Seal,
    }

    impl Checkpoint {
        /// The only constructor. It performs the canonical prepare, exact-out
        /// bracket selection, and raw ceil conversion before sealing the
        /// identity-bound result.
        pub(super) fn plan(
            market: &Market,
            target_asset: MarketAsset,
            target_redeemed: u64,
            borrowed_redeemed: u64,
            debt_repaid: u64,
        ) -> Result<(u64, Self)> {
            let borrowed_asset = target_asset.opposite();
            let (start_curve_reserves_nad, post_entitlement_curve_reserves_nad) =
                curve_reserves_after_entitlement(market, target_asset, target_redeemed, borrowed_redeemed)?;
            let borrowed_shortfall = debt_repaid
                .checked_sub(borrowed_redeemed)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            require!(borrowed_shortfall > 0, ErrorCode::AmountZero);
            let slot = curve_slot(market);
            let center_price_nad = market.current_curve_center_price_nad()?;
            let prepared =
                market.prepare_curve_for_reserves_nad(post_entitlement_curve_reserves_nad, center_price_nad, slot)?;
            let amount_out_nad =
                normalize_to_nad(borrowed_shortfall as u128, market.side(borrowed_asset).asset_decimals)?;
            let direction = match target_asset {
                MarketAsset::Base => ConcentratedSwapDirection::BaseToQuote,
                MarketAsset::Quote => ConcentratedSwapDirection::QuoteToBase,
            };
            let selected_input_nad = prepared.quote_exact_out_input_bracket(amount_out_nad, direction)?.1;
            let target_retained =
                denormalize_from_nad_ceil(selected_input_nad, market.side(target_asset).asset_decimals)?;
            require_gte!(target_redeemed, target_retained, ErrorCode::HlpSettlementUnavailable);
            let target_amount = target_redeemed
                .checked_sub(target_retained)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            let (base_reserve_debit, quote_reserve_debit) = match target_asset {
                MarketAsset::Base => (target_amount, debt_repaid),
                MarketAsset::Quote => (debt_repaid, target_amount),
            };
            let successor_curve_reserves_nad = curve_reserves_after_raw_debits(
                market,
                start_curve_reserves_nad,
                base_reserve_debit,
                quote_reserve_debit,
            )?;
            Ok((
                target_amount,
                Self {
                    current_slot: slot,
                    curve_revision: market.curve_revision,
                    center_price_nad,
                    parameters: market.current_curve_parameters(slot),
                    retain_dynamic_surcharge: market.amm.retain_dynamic_surcharge,
                    start_curve_reserves_nad,
                    post_entitlement_curve_reserves_nad,
                    successor_curve_reserves_nad,
                    start_invariant_d: prepared.invariant_d(),
                    borrowed_shortfall,
                    selected_input_nad,
                    target_retained,
                    _seal: Seal,
                },
            ))
        }

        pub(super) fn require_start(self, market: &Market) -> Result<()> {
            require_eq!(self.current_slot, curve_slot(market), ErrorCode::BrokenInvariant);
            require_eq!(self.curve_revision, market.curve_revision, ErrorCode::BrokenInvariant);
            require_eq!(
                self.center_price_nad,
                market.current_curve_center_price_nad()?,
                ErrorCode::BrokenInvariant
            );
            require!(
                self.parameters == market.current_curve_parameters(self.current_slot),
                ErrorCode::BrokenInvariant
            );
            require_eq!(
                self.retain_dynamic_surcharge,
                market.amm.retain_dynamic_surcharge,
                ErrorCode::BrokenInvariant
            );
            require!(
                self.start_curve_reserves_nad == market.curve_reserves_nad()?,
                ErrorCode::BrokenInvariant
            );
            require!(self.start_invariant_d > 0, ErrorCode::BrokenInvariant);
            Ok(())
        }

        pub(super) const fn current_slot(self) -> u64 {
            self.current_slot
        }

        pub(super) const fn center_price_nad(self) -> u64 {
            self.center_price_nad
        }

        pub(super) const fn start_curve_reserves_nad(self) -> CurveReservesNad {
            self.start_curve_reserves_nad
        }

        pub(super) const fn post_entitlement_curve_reserves_nad(self) -> CurveReservesNad {
            self.post_entitlement_curve_reserves_nad
        }

        pub(super) const fn successor_curve_reserves_nad(self) -> CurveReservesNad {
            self.successor_curve_reserves_nad
        }

        pub(super) const fn start_invariant_d(self) -> u128 {
            self.start_invariant_d
        }

        pub(super) const fn borrowed_shortfall(self) -> u64 {
            self.borrowed_shortfall
        }

        pub(super) const fn selected_input_nad(self) -> u128 {
            self.selected_input_nad
        }

        pub(super) const fn target_retained(self) -> u64 {
            self.target_retained
        }

        pub(super) const fn facts(self) -> HlpExactOutSettlementFacts {
            HlpExactOutSettlementFacts {
                start_curve_reserves_nad: self.start_curve_reserves_nad,
                post_entitlement_curve_reserves_nad: self.post_entitlement_curve_reserves_nad,
                successor_curve_reserves_nad: self.successor_curve_reserves_nad,
                borrowed_shortfall: self.borrowed_shortfall,
                selected_input_nad: self.selected_input_nad,
                target_retained: self.target_retained,
            }
        }
    }
}

use hlp_exact_out_settlement::Checkpoint as HlpExactOutSettlementCheckpoint;

/// Planner-only exact-out evidence.  It contains no curve checkpoint or
/// canonical prepared curve and cannot be passed to the authoritative Market
/// applier.  Construction is limited to an opaque guidance curve.
#[cfg(test)]
mod hlp_guidance_exact_out_settlement {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Seal;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) struct Proof {
        facts: HlpExactOutSettlementFacts,
        _seal: Seal,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) enum ProbeMode {
        ExactReference,
        Bounded,
    }

    impl Proof {
        #[allow(clippy::too_many_arguments)]
        pub(super) fn plan(
            fixed: HlpPlannerStatic,
            state: HlpPlannerState,
            anchor: ConcentratedGuidanceCurve,
            anchor_ylp_supply: u64,
            target_asset: MarketAsset,
            target_redeemed: u64,
            borrowed_redeemed: u64,
            debt_repaid: u64,
            post_ylp_supply: u64,
            mode: ProbeMode,
        ) -> Result<(u64, Self)> {
            let borrowed_asset = target_asset.opposite();
            let start_curve_reserves_nad = state.curve_reserves_nad(fixed)?;
            let (base_redeemed, quote_redeemed) = match target_asset {
                MarketAsset::Base => (target_redeemed, borrowed_redeemed),
                MarketAsset::Quote => (borrowed_redeemed, target_redeemed),
            };
            let post_entitlement_curve_reserves_nad = curve_reserves_after_raw_debits_with_decimals(
                start_curve_reserves_nad,
                base_redeemed,
                quote_redeemed,
                fixed.base_decimals,
                fixed.quote_decimals,
            )?;
            let borrowed_shortfall = debt_repaid
                .checked_sub(borrowed_redeemed)
                .filter(|shortfall| *shortfall > 0)
                .ok_or(ErrorCode::AmountZero)?;
            require!(
                anchor_ylp_supply > 0 && post_ylp_supply > 0,
                ErrorCode::SupplyUnderflow
            );
            let prepared = match mode {
                ProbeMode::ExactReference => anchor.prepare_hint_successor(
                    post_entitlement_curve_reserves_nad.base,
                    post_entitlement_curve_reserves_nad.quote,
                )?,
                ProbeMode::Bounded => {
                    let invariant_d = mul_div_u128(
                        anchor.invariant_d(),
                        post_ylp_supply as u128,
                        anchor_ylp_supply as u128,
                    )?;
                    anchor.prepare_guidance_successor_with_invariant(
                        post_entitlement_curve_reserves_nad.base,
                        post_entitlement_curve_reserves_nad.quote,
                        invariant_d,
                    )?
                }
            };
            let amount_out_nad = normalize_to_nad(
                borrowed_shortfall as u128,
                fixed.decimals(borrowed_asset),
            )?;
            let direction = match target_asset {
                MarketAsset::Base => ConcentratedSwapDirection::BaseToQuote,
                MarketAsset::Quote => ConcentratedSwapDirection::QuoteToBase,
            };
            let selected_input_nad = match mode {
                ProbeMode::ExactReference => prepared.quote_exact_out_input_bracket(amount_out_nad, direction)?.1,
                ProbeMode::Bounded => {
                    let probes_before = crate::math::residual_evaluations();
                    let input = prepared.quote_bounded_exact_out_input(amount_out_nad, direction)?;
                    let probes = crate::math::residual_evaluations().saturating_sub(probes_before);
                    require!(probes <= 2, ErrorCode::BrokenInvariant);
                    HLP_COMPACT_GUIDANCE_EXACT_OUT_PROBES.with(|count| {
                        count.set(count.get().saturating_add(u32::try_from(probes).unwrap_or(u32::MAX)))
                    });
                    input
                }
            };
            let target_retained = denormalize_from_nad_ceil(
                selected_input_nad,
                fixed.decimals(target_asset),
            )?;
            require_gte!(target_redeemed, target_retained, ErrorCode::HlpSettlementUnavailable);
            let target_amount = target_redeemed
                .checked_sub(target_retained)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            let (base_reserve_debit, quote_reserve_debit) = match target_asset {
                MarketAsset::Base => (target_amount, debt_repaid),
                MarketAsset::Quote => (debt_repaid, target_amount),
            };
            let successor_curve_reserves_nad = curve_reserves_after_raw_debits_with_decimals(
                start_curve_reserves_nad,
                base_reserve_debit,
                quote_reserve_debit,
                fixed.base_decimals,
                fixed.quote_decimals,
            )?;
            Ok((
                target_amount,
                Self {
                    facts: HlpExactOutSettlementFacts {
                        start_curve_reserves_nad,
                        post_entitlement_curve_reserves_nad,
                        successor_curve_reserves_nad,
                        borrowed_shortfall,
                        selected_input_nad,
                        target_retained,
                    },
                    _seal: Seal,
                },
            ))
        }

        pub(super) const fn facts(self) -> HlpExactOutSettlementFacts {
            self.facts
        }
    }
}

#[cfg(test)]
use hlp_guidance_exact_out_settlement::{ProbeMode as HlpGuidanceSettlementProbeMode, Proof as HlpGuidanceSettlementProof};

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HlpCompactRebalancePlan {
    plan: HlpRebalancePlan,
    guidance_settlement: Option<HlpGuidanceSettlementProof>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HlpDeleverageReservePlan {
    base_reserve_debit: u64,
    quote_reserve_debit: u64,
    base_cash_debit: u64,
    quote_cash_debit: u64,
    exact_out_checkpoint: Option<HlpExactOutSettlementCheckpoint>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HlpReserveDebitPlan {
    reserve_asset: MarketAsset,
    reserve_debit: u64,
    cash_debit: u64,
    interest_paid: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SettledCloseTargetPlan {
    target_amount: u64,
    exact_out_checkpoint: Option<HlpExactOutSettlementCheckpoint>,
}

/// A fixed-size, single-source individual-hLP transition. The receipt alone is
/// deliberately insufficient to execute: reserve classification, debt-share
/// clearance, and exact-out identity all live in the tagged plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HlpRebalancePlan {
    Noop {
        common: HlpRebalancePlanCommon,
        reason: HlpRebalanceNoopReason,
    },
    LeverageUp {
        common: HlpRebalancePlanCommon,
        base_leg_amount: u64,
        quote_leg_amount: u64,
        ylp_mint_amount: u64,
        debt_shares_added: u128,
        debt_principal_added: u64,
    },
    Deleverage {
        common: HlpRebalancePlanCommon,
        ylp_burn_amount: u64,
        base_entitlement_amount: u64,
        quote_entitlement_amount: u64,
        base_reserve_debit: u64,
        quote_reserve_debit: u64,
        base_cash_debit: u64,
        quote_cash_debit: u64,
        debt_repayment: DebtRepaymentQuote,
        debt_clearance: DebtClearance,
        interest_paid: u64,
        exact_out_checkpoint: Option<HlpExactOutSettlementCheckpoint>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HlpRebalancePairLegPlan {
    Inactive { target_asset: MarketAsset },
    Active(HlpRebalancePlan),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HlpRebalancePairStart {
    base: HlpRebalanceState,
    quote: HlpRebalanceState,
    base_active: bool,
    quote_active: bool,
}

/// A complete Base-then-Quote individual-hLP actuation. Both valuations are
/// frozen before either leg is planned. The Quote plan is constructed against
/// the exact Base successor, so an I>B settlement proof is consumed at the
/// same intermediate state that produced it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HlpRebalancePairPlan {
    start: HlpRebalancePairStart,
    base: HlpRebalancePairLegPlan,
    quote: HlpRebalancePairLegPlan,
}

impl HlpRebalancePlan {
    const fn common(&self) -> &HlpRebalancePlanCommon {
        match self {
            Self::Noop { common, .. } | Self::LeverageUp { common, .. } | Self::Deleverage { common, .. } => common,
        }
    }

    fn common_mut(&mut self) -> &mut HlpRebalancePlanCommon {
        match self {
            Self::Noop { common, .. } | Self::LeverageUp { common, .. } | Self::Deleverage { common, .. } => common,
        }
    }

    const fn ylp_mint_amount(&self) -> u64 {
        match self {
            Self::LeverageUp { ylp_mint_amount, .. } => *ylp_mint_amount,
            _ => 0,
        }
    }

    const fn ylp_burn_amount(&self) -> u64 {
        match self {
            Self::Deleverage { ylp_burn_amount, .. } => *ylp_burn_amount,
            _ => 0,
        }
    }

    fn set_current_swap_fee_eligible_ylp_shares(&mut self, shares: u64) {
        self.common_mut().current_swap_fee_eligible_ylp_shares = shares;
    }

    fn receipt(self) -> HlpRebalanceReceipt {
        let common = *self.common();
        let (ylp_mint_amount, ylp_burn_amount, debt_delta, interest_paid) = match self {
            Self::Noop { .. } => (0, 0, 0, 0),
            Self::LeverageUp {
                ylp_mint_amount,
                debt_principal_added,
                ..
            } => (ylp_mint_amount, 0, debt_principal_added as i128, 0),
            Self::Deleverage {
                ylp_burn_amount,
                debt_clearance,
                interest_paid,
                ..
            } => (
                0,
                ylp_burn_amount,
                -(debt_clearance.debt_reduced as i128),
                interest_paid,
            ),
        };
        HlpRebalanceReceipt {
            target_asset: common.target_asset,
            ideal_delta: common.ideal_delta_nad,
            current_swap_fee_eligible_ylp_shares: common.current_swap_fee_eligible_ylp_shares,
            ylp_mint_amount,
            ylp_burn_amount,
            debt_delta,
            interest_paid,
            nav_nad: common.nav_nad,
            preposition_capacity_bound: common.capacity_bound,
            ..HlpRebalanceReceipt::default()
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HlpCurvePrices {
    base_in_quote_nad: u128,
    quote_in_base_nad: u128,
}

impl HlpCurvePrices {
    const fn for_asset(self, asset: MarketAsset) -> u128 {
        match asset {
            MarketAsset::Base => self.base_in_quote_nad,
            MarketAsset::Quote => self.quote_in_base_nad,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HlpEntryDisposition {
    Settled,
    /// The production controller's uncapped ideal adjustment disappears at a
    /// raw-token or yLP-share conversion. The signed residual remains stored;
    /// this is a controller-resolution state, not protocol dust.
    ControllerGranularityLimited,
    Actionable,
    CashConstrained,
    Unhedgeable,
}

impl HlpEntryDisposition {
    pub(crate) const fn admits_entry(self) -> bool {
        matches!(self, Self::Settled | Self::ControllerGranularityLimited)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HlpEntryState {
    pub(crate) disposition: HlpEntryDisposition,
    residual_exposure: i128,
    nav_nad: u128,
}

pub(crate) fn current_hlp_entry_state_with_prices(
    market: &Market,
    target_asset: MarketAsset,
    prices: HlpCurvePrices,
) -> Result<HlpEntryState> {
    let valuation = current_hlp_valuation_with_prices(market, target_asset, prices)?;
    let residual_exposure = recognized_hlp_residual_exposure(valuation.ideal_delta, valuation.nav_nad);
    let disposition = if residual_exposure == 0 {
        HlpEntryDisposition::Settled
    } else if !valuation.proportional_hedge_available || valuation.nav_nad == 0 {
        HlpEntryDisposition::Unhedgeable
    } else if residual_exposure > 0 {
        'entry: {
            // Inspect the uncapped ideal plan before looking at cash.
            let amounts = proportional_rebalance_amounts(market, target_asset, residual_exposure, valuation)?;
            if !complete_rebalance_amounts(amounts) {
                break 'entry HlpEntryDisposition::ControllerGranularityLimited;
            }
            let (base_leg, quote_leg) = match target_asset {
                MarketAsset::Base => (amounts.target_leg_amount, amounts.borrowed_leg_amount),
                MarketAsset::Quote => (amounts.borrowed_leg_amount, amounts.target_leg_amount),
            };
            if ylp_for_live_reserve_deposit(market, base_leg, quote_leg)? == 0 {
                break 'entry HlpEntryDisposition::ControllerGranularityLimited;
            }
            if market.hlp_funding_headroom(target_asset.opposite())? < amounts.debt_amount {
                break 'entry HlpEntryDisposition::CashConstrained;
            }
            HlpEntryDisposition::Actionable
        }
    } else {
        'entry: {
            let (borrow_index, debt_shares, debt_principal, vault_ylp) = match target_asset {
                MarketAsset::Base => (
                    market.debt.quote_borrow_index_nad,
                    market.base_hlp_vault.debt_shares,
                    market.base_hlp_vault.debt_principal,
                    market.base_hlp_vault.ylp_shares,
                ),
                MarketAsset::Quote => (
                    market.debt.base_borrow_index_nad,
                    market.quote_hlp_vault.debt_shares,
                    market.quote_hlp_vault.debt_principal,
                    market.quote_hlp_vault.ylp_shares,
                ),
            };
            if debt_shares == 0 || vault_ylp == 0 || valuation.values.debt_value_nad == 0 {
                break 'entry HlpEntryDisposition::Unhedgeable;
            }
            let collateral_value_nad = valuation
                .values
                .target_inventory_value_nad
                .checked_add(valuation.values.opposite_inventory_value_nad)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            let requested_delta_nad = residual_exposure.unsigned_abs();
            if collateral_value_nad.min(valuation.values.debt_value_nad) < requested_delta_nad
                || requested_delta_nad == 0
            {
                break 'entry HlpEntryDisposition::Unhedgeable;
            }
            let feasible_delta = -i128::try_from(requested_delta_nad).map_err(|_| ErrorCode::MarketMathOverflow)?;
            let amounts = proportional_rebalance_amounts(market, target_asset, feasible_delta, valuation)?;
            if !complete_rebalance_amounts(amounts) {
                break 'entry HlpEntryDisposition::ControllerGranularityLimited;
            }
            let (base_leg, quote_leg) = match target_asset {
                MarketAsset::Base => (amounts.target_leg_amount, amounts.borrowed_leg_amount),
                MarketAsset::Quote => (amounts.borrowed_leg_amount, amounts.target_leg_amount),
            };
            let base_burn = ylp_shares_for_live_reserve_amount(market, MarketAsset::Base, base_leg)?;
            let quote_burn = ylp_shares_for_live_reserve_amount(market, MarketAsset::Quote, quote_leg)?;
            let ylp_burn = base_burn.min(quote_burn).min(vault_ylp);
            if ylp_burn == 0 {
                break 'entry HlpEntryDisposition::ControllerGranularityLimited;
            }
            let base_out = ylp_live_underlying_amount(market, MarketAsset::Base, ylp_burn)?;
            let quote_out = ylp_live_underlying_amount(market, MarketAsset::Quote, ylp_burn)?;
            if base_out == 0 || quote_out == 0 {
                break 'entry HlpEntryDisposition::ControllerGranularityLimited;
            }
            let (target_out, borrowed_out) = match target_asset {
                MarketAsset::Base => (base_out, quote_out),
                MarketAsset::Quote => (quote_out, base_out),
            };
            let borrowed_asset = target_asset.opposite();
            let removed_value_nad = asset_value_in_target_nad_with_prices(
                market,
                valuation.prices,
                target_asset,
                target_out,
                target_asset,
            )?
            .checked_add(asset_value_in_target_nad_with_prices(
                market,
                valuation.prices,
                borrowed_asset,
                borrowed_out,
                target_asset,
            )?)
            .ok_or(ErrorCode::MarketMathOverflow)?;
            let current_debt_nadless = Debt::shares_to_debt(debt_shares, borrow_index)?;
            let current_debt = u64::try_from(current_debt_nadless).unwrap_or(u64::MAX);
            let repay = raw_amount_from_target_value_nad_with_prices(
                market,
                valuation.prices,
                borrowed_asset,
                target_asset,
                removed_value_nad,
            )?
            .min(current_debt);
            if repay == 0 {
                break 'entry HlpEntryDisposition::ControllerGranularityLimited;
            }
            let (_, interest_paid) =
                crate::math::realized_interest_split(repay, current_debt_nadless, u128::from(debt_principal))?;
            if market.side(borrowed_asset).reserves.cash_reserve < interest_paid {
                break 'entry HlpEntryDisposition::CashConstrained;
            }
            HlpEntryDisposition::Actionable
        }
    };
    Ok(HlpEntryState {
        disposition,
        residual_exposure,
        nav_nad: valuation.nav_nad,
    })
}

const fn complete_rebalance_amounts(amounts: ProportionalRebalanceAmounts) -> bool {
    amounts.target_leg_amount > 0 && amounts.borrowed_leg_amount > 0 && amounts.debt_amount > 0
}

fn capture_hlp_rebalance_state(market: &Market, target_asset: MarketAsset) -> HlpRebalanceState {
    let vault = match target_asset {
        MarketAsset::Base => &market.base_hlp_vault,
        MarketAsset::Quote => &market.quote_hlp_vault,
    };
    HlpRebalanceState {
        target_ylp_shares: vault.ylp_shares,
        target_debt_shares: vault.debt_shares,
        target_debt_principal: vault.debt_principal,
        target_base_hlp_live_reserve: vault.base_hlp_live_reserve,
        target_quote_hlp_live_reserve: vault.quote_hlp_live_reserve,
        base_ylp_supply: market.base_side.shares.ylp_supply,
        quote_ylp_supply: market.quote_side.shares.ylp_supply,
        base_live_reserve: market.base_side.reserves.live_reserve,
        base_cash_reserve: market.base_side.reserves.cash_reserve,
        base_target_backing_inventory: market.base_side.reserves.hlp_backing_inventory(target_asset),
        quote_live_reserve: market.quote_side.reserves.live_reserve,
        quote_cash_reserve: market.quote_side.reserves.cash_reserve,
        quote_target_backing_inventory: market.quote_side.reserves.hlp_backing_inventory(target_asset),
        borrow_index_nad: market.debt.borrow_index(target_asset.opposite()),
    }
}

#[cfg(test)]
fn capture_hlp_rebalance_state_from_planner(
    fixed: HlpPlannerStatic,
    state: HlpPlannerState,
    target_asset: MarketAsset,
) -> HlpRebalanceState {
    let vault = state.vault(target_asset);
    HlpRebalanceState {
        target_ylp_shares: vault.ylp_shares,
        target_debt_shares: vault.debt_shares,
        target_debt_principal: vault.debt_principal,
        target_base_hlp_live_reserve: vault.base_hlp_live_reserve,
        target_quote_hlp_live_reserve: vault.quote_hlp_live_reserve,
        base_ylp_supply: state.base_side.ylp_supply,
        quote_ylp_supply: state.quote_side.ylp_supply,
        base_live_reserve: state.base_side.live_reserve,
        base_cash_reserve: state.base_side.cash_reserve,
        base_target_backing_inventory: match target_asset {
            MarketAsset::Base => state.base_side.base_hlp_backing_inventory,
            MarketAsset::Quote => state.base_side.quote_hlp_backing_inventory,
        },
        quote_live_reserve: state.quote_side.live_reserve,
        quote_cash_reserve: state.quote_side.cash_reserve,
        quote_target_backing_inventory: match target_asset {
            MarketAsset::Base => state.quote_side.base_hlp_backing_inventory,
            MarketAsset::Quote => state.quote_side.quote_hlp_backing_inventory,
        },
        borrow_index_nad: fixed.borrow_index(target_asset.opposite()),
    }
}

fn hlp_rebalance_plan_common(
    market: &Market,
    target_asset: MarketAsset,
    ideal_delta_nad: i128,
    nav_nad: u128,
    capacity_bound: bool,
) -> HlpRebalancePlanCommon {
    HlpRebalancePlanCommon {
        start: capture_hlp_rebalance_state(market, target_asset),
        target_asset,
        ideal_delta_nad,
        nav_nad,
        capacity_bound,
        current_swap_fee_eligible_ylp_shares: 0,
    }
}

fn plan_hlp_noop(
    market: &Market,
    target_asset: MarketAsset,
    ideal_delta_nad: i128,
    valuation: HlpValuation,
    capacity_bound: bool,
    reason: HlpRebalanceNoopReason,
) -> HlpRebalancePlan {
    HlpRebalancePlan::Noop {
        common: hlp_rebalance_plan_common(market, target_asset, ideal_delta_nad, valuation.nav_nad, capacity_bound),
        reason,
    }
}

fn require_hlp_exact_out_checkpoint_start(market: &Market, checkpoint: HlpExactOutSettlementCheckpoint) -> Result<()> {
    checkpoint.require_start(market)
}

fn apply_hlp_reserve_debit_to_state(
    state: &mut HlpRebalanceState,
    target_asset: MarketAsset,
    reserve_asset: MarketAsset,
    reserve_debit: u64,
    cash_debit: u64,
    interest_paid: u64,
) -> Result<()> {
    require_gte!(reserve_debit, interest_paid, ErrorCode::HlpSettlementUnavailable);
    require_gte!(reserve_debit, cash_debit, ErrorCode::BrokenInvariant);
    require_gte!(cash_debit, interest_paid, ErrorCode::BrokenInvariant);
    let synthetic_debit = reserve_debit
        .checked_sub(cash_debit)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let backing_credit = cash_debit
        .checked_sub(interest_paid)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    match reserve_asset {
        MarketAsset::Base => {
            state.base_live_reserve = state
                .base_live_reserve
                .checked_sub(reserve_debit)
                .ok_or(ErrorCode::ReserveUnderflow)?;
            state.base_cash_reserve = state
                .base_cash_reserve
                .checked_sub(cash_debit)
                .ok_or(ErrorCode::CashReserveUnderflow)?;
            state.target_base_hlp_live_reserve = state
                .target_base_hlp_live_reserve
                .checked_sub(synthetic_debit)
                .ok_or(ErrorCode::ReserveUnderflow)?;
            state.base_target_backing_inventory = state
                .base_target_backing_inventory
                .checked_add(backing_credit)
                .ok_or(ErrorCode::MarketMathOverflow)?;
        }
        MarketAsset::Quote => {
            state.quote_live_reserve = state
                .quote_live_reserve
                .checked_sub(reserve_debit)
                .ok_or(ErrorCode::ReserveUnderflow)?;
            state.quote_cash_reserve = state
                .quote_cash_reserve
                .checked_sub(cash_debit)
                .ok_or(ErrorCode::CashReserveUnderflow)?;
            state.target_quote_hlp_live_reserve = state
                .target_quote_hlp_live_reserve
                .checked_sub(synthetic_debit)
                .ok_or(ErrorCode::ReserveUnderflow)?;
            state.quote_target_backing_inventory = state
                .quote_target_backing_inventory
                .checked_add(backing_credit)
                .ok_or(ErrorCode::MarketMathOverflow)?;
        }
    }
    let borrowed_asset = target_asset.opposite();
    require!(
        interest_paid == 0 || reserve_asset == borrowed_asset,
        ErrorCode::BrokenInvariant
    );
    Ok(())
}

fn ylp_for_live_reserve_deposit_from_state(
    state: HlpRebalanceState,
    base_amount: u64,
    quote_amount: u64,
) -> Result<u64> {
    require_eq!(
        state.base_ylp_supply,
        state.quote_ylp_supply,
        ErrorCode::BrokenInvariant
    );
    require!(state.base_ylp_supply > 0, ErrorCode::SupplyUnderflow);
    require!(
        state.base_live_reserve > 0 && state.quote_live_reserve > 0,
        ErrorCode::InsufficientLiquidity
    );
    let base_ylp = mul_div_u128(
        base_amount as u128,
        state.base_ylp_supply as u128,
        state.base_live_reserve as u128,
    )?;
    let quote_ylp = mul_div_u128(
        quote_amount as u128,
        state.quote_ylp_supply as u128,
        state.quote_live_reserve as u128,
    )?;
    u64::try_from(base_ylp.min(quote_ylp)).map_err(|_| ErrorCode::LiquidityConversionOverflow.into())
}

fn ylp_live_underlying_amount_from_state(state: HlpRebalanceState, asset: MarketAsset, ylp_amount: u64) -> Result<u64> {
    let (live_reserve, supply) = match asset {
        MarketAsset::Base => (state.base_live_reserve, state.base_ylp_supply),
        MarketAsset::Quote => (state.quote_live_reserve, state.quote_ylp_supply),
    };
    ylp_live_underlying_amount_from_values(live_reserve, supply, ylp_amount)
}

fn ylp_live_underlying_amount_from_values(live_reserve: u64, supply: u64, ylp_amount: u64) -> Result<u64> {
    require!(ylp_amount > 0, ErrorCode::AmountZero);
    require_gte!(supply, ylp_amount, ErrorCode::InsufficientBalance);
    let amount = mul_div_u128(ylp_amount as u128, live_reserve as u128, supply as u128)?;
    u64::try_from(amount).map_err(|_| ErrorCode::MarketMathOverflow.into())
}

fn hlp_vault_from_rebalance_state(state: HlpRebalanceState) -> HlpVault {
    HlpVault {
        ylp_shares: state.target_ylp_shares,
        debt_shares: state.target_debt_shares,
        debt_principal: state.target_debt_principal,
        base_hlp_live_reserve: state.target_base_hlp_live_reserve,
        quote_hlp_live_reserve: state.target_quote_hlp_live_reserve,
        ..HlpVault::default()
    }
}

fn curve_reserves_after_raw_debits_with_decimals(
    start: CurveReservesNad,
    base_debit: u64,
    quote_debit: u64,
    base_decimals: u8,
    quote_decimals: u8,
) -> Result<CurveReservesNad> {
    Ok(CurveReservesNad {
        base: start
            .base
            .checked_sub(normalize_to_nad(base_debit as u128, base_decimals)?)
            .ok_or(ErrorCode::ReserveUnderflow)?,
        quote: start
            .quote
            .checked_sub(normalize_to_nad(quote_debit as u128, quote_decimals)?)
            .ok_or(ErrorCode::ReserveUnderflow)?,
    })
}

fn derive_hlp_rebalance_post_state(market: &Market, plan: &HlpRebalancePlan) -> Result<HlpRebalanceState> {
    let common = *plan.common();
    require!(
        capture_hlp_rebalance_state(market, common.target_asset) == common.start,
        ErrorCode::BrokenInvariant
    );
    if let HlpRebalancePlan::Deleverage {
        exact_out_checkpoint: Some(checkpoint),
        ..
    } = *plan
    {
        require_hlp_exact_out_checkpoint_start(market, checkpoint)?;
    }
    derive_hlp_rebalance_post_state_from_values(plan, market.base_side.asset_decimals, market.quote_side.asset_decimals)
}

/// Single-source fixed-value Stage1 kernel. Exact Market and compact guidance
/// adapters separately validate their sealed settlement capability, then use
/// this identical reserve/share/debt transition.
fn derive_hlp_rebalance_post_state_from_values(
    plan: &HlpRebalancePlan,
    base_decimals: u8,
    quote_decimals: u8,
) -> Result<HlpRebalanceState> {
    derive_hlp_rebalance_post_state_from_values_with_guidance(
        plan,
        base_decimals,
        quote_decimals,
        None,
    )
}

fn derive_hlp_rebalance_post_state_from_values_with_guidance(
    plan: &HlpRebalancePlan,
    base_decimals: u8,
    quote_decimals: u8,
    #[cfg(test)] guidance_settlement: Option<HlpExactOutSettlementFacts>,
    #[cfg(not(test))] _guidance_settlement: Option<HlpExactOutSettlementFacts>,
) -> Result<HlpRebalanceState> {
    let common = *plan.common();
    let mut post = common.start;
    match *plan {
        HlpRebalancePlan::Noop { .. } => {}
        HlpRebalancePlan::LeverageUp {
            base_leg_amount,
            quote_leg_amount,
            ylp_mint_amount,
            debt_shares_added,
            debt_principal_added,
            ..
        } => {
            require!(
                base_leg_amount > 0
                    && quote_leg_amount > 0
                    && ylp_mint_amount > 0
                    && debt_shares_added > 0
                    && debt_principal_added > 0,
                ErrorCode::BrokenInvariant
            );
            require_eq!(
                ylp_mint_amount,
                ylp_for_live_reserve_deposit_from_state(common.start, base_leg_amount, quote_leg_amount)?,
                ErrorCode::BrokenInvariant
            );
            require_eq!(
                debt_shares_added,
                Debt::debt_to_shares(debt_principal_added, common.start.borrow_index_nad)?,
                ErrorCode::BrokenInvariant
            );
            post.base_live_reserve = post
                .base_live_reserve
                .checked_add(base_leg_amount)
                .ok_or(ErrorCode::ReserveOverflow)?;
            post.quote_live_reserve = post
                .quote_live_reserve
                .checked_add(quote_leg_amount)
                .ok_or(ErrorCode::ReserveOverflow)?;
            post.target_base_hlp_live_reserve = post
                .target_base_hlp_live_reserve
                .checked_add(base_leg_amount)
                .ok_or(ErrorCode::ReserveOverflow)?;
            post.target_quote_hlp_live_reserve = post
                .target_quote_hlp_live_reserve
                .checked_add(quote_leg_amount)
                .ok_or(ErrorCode::ReserveOverflow)?;
            post.base_ylp_supply = post
                .base_ylp_supply
                .checked_add(ylp_mint_amount)
                .ok_or(ErrorCode::SupplyOverflow)?;
            post.quote_ylp_supply = post
                .quote_ylp_supply
                .checked_add(ylp_mint_amount)
                .ok_or(ErrorCode::SupplyOverflow)?;
            post.target_debt_shares = post
                .target_debt_shares
                .checked_add(debt_shares_added)
                .ok_or(ErrorCode::DebtShareMathOverflow)?;
            post.target_debt_principal = post
                .target_debt_principal
                .checked_add(debt_principal_added)
                .ok_or(ErrorCode::DebtMathOverflow)?;
            post.target_ylp_shares = post
                .target_ylp_shares
                .checked_add(ylp_mint_amount)
                .ok_or(ErrorCode::SupplyOverflow)?;
        }
        HlpRebalancePlan::Deleverage {
            ylp_burn_amount,
            base_entitlement_amount,
            quote_entitlement_amount,
            base_reserve_debit,
            quote_reserve_debit,
            base_cash_debit,
            quote_cash_debit,
            debt_repayment,
            debt_clearance,
            interest_paid,
            exact_out_checkpoint,
            ..
        } => {
            require!(ylp_burn_amount > 0, ErrorCode::BrokenInvariant);
            require_eq!(
                base_entitlement_amount,
                ylp_live_underlying_amount_from_state(common.start, MarketAsset::Base, ylp_burn_amount)?,
                ErrorCode::BrokenInvariant
            );
            require_eq!(
                quote_entitlement_amount,
                ylp_live_underlying_amount_from_state(common.start, MarketAsset::Quote, ylp_burn_amount)?,
                ErrorCode::BrokenInvariant
            );
            let borrowed_asset = common.target_asset.opposite();
            let (target_entitlement, borrowed_entitlement, target_reserve_debit, borrowed_reserve_debit) =
                match common.target_asset {
                    MarketAsset::Base => (
                        base_entitlement_amount,
                        quote_entitlement_amount,
                        base_reserve_debit,
                        quote_reserve_debit,
                    ),
                    MarketAsset::Quote => (
                        quote_entitlement_amount,
                        base_entitlement_amount,
                        quote_reserve_debit,
                        base_reserve_debit,
                    ),
                };
            let canonical_settlement = exact_out_checkpoint.map(HlpExactOutSettlementCheckpoint::facts);
            #[cfg(test)]
            require!(
                canonical_settlement.is_none() || guidance_settlement.is_none(),
                ErrorCode::BrokenInvariant
            );
            #[cfg(test)]
            let settlement = canonical_settlement.or(guidance_settlement);
            #[cfg(not(test))]
            let settlement = canonical_settlement;
            match settlement {
                Some(settlement) => {
                    require!(interest_paid > borrowed_entitlement, ErrorCode::BrokenInvariant);
                    require_eq!(
                        settlement.borrowed_shortfall,
                        interest_paid
                            .checked_sub(borrowed_entitlement)
                            .ok_or(ErrorCode::MarketMathOverflow)?,
                        ErrorCode::BrokenInvariant
                    );
                    require_eq!(borrowed_reserve_debit, interest_paid, ErrorCode::BrokenInvariant);
                    require_eq!(
                        settlement.target_retained,
                        target_entitlement
                            .checked_sub(target_reserve_debit)
                            .ok_or(ErrorCode::MarketMathOverflow)?,
                        ErrorCode::BrokenInvariant
                    );
                    let expected_post_entitlement = curve_reserves_after_raw_debits_with_decimals(
                        settlement.start_curve_reserves_nad,
                        base_entitlement_amount,
                        quote_entitlement_amount,
                        base_decimals,
                        quote_decimals,
                    )?;
                    require!(
                        expected_post_entitlement == settlement.post_entitlement_curve_reserves_nad,
                        ErrorCode::BrokenInvariant
                    );
                    let expected_successor = curve_reserves_after_raw_debits_with_decimals(
                        settlement.start_curve_reserves_nad,
                        base_reserve_debit,
                        quote_reserve_debit,
                        base_decimals,
                        quote_decimals,
                    )?;
                    require!(
                        expected_successor == settlement.successor_curve_reserves_nad,
                        ErrorCode::BrokenInvariant
                    );
                    let selected_input_raw_nad = normalize_to_nad(
                        settlement.target_retained as u128,
                        match common.target_asset {
                            MarketAsset::Base => base_decimals,
                            MarketAsset::Quote => quote_decimals,
                        },
                    )?;
                    require_gte!(
                        selected_input_raw_nad,
                        settlement.selected_input_nad,
                        ErrorCode::BrokenInvariant
                    );
                }
                None => {
                    require_gte!(borrowed_entitlement, interest_paid, ErrorCode::BrokenInvariant);
                    require_eq!(target_reserve_debit, target_entitlement, ErrorCode::BrokenInvariant);
                    require_eq!(borrowed_reserve_debit, borrowed_entitlement, ErrorCode::BrokenInvariant);
                }
            }

            let base_interest = if borrowed_asset == MarketAsset::Base {
                interest_paid
            } else {
                0
            };
            let quote_interest = if borrowed_asset == MarketAsset::Quote {
                interest_paid
            } else {
                0
            };
            let expected_base_cash =
                base_interest.max(base_reserve_debit.saturating_sub(post.target_base_hlp_live_reserve));
            let expected_quote_cash =
                quote_interest.max(quote_reserve_debit.saturating_sub(post.target_quote_hlp_live_reserve));
            require_eq!(base_cash_debit, expected_base_cash, ErrorCode::BrokenInvariant);
            require_eq!(quote_cash_debit, expected_quote_cash, ErrorCode::BrokenInvariant);
            apply_hlp_reserve_debit_to_state(
                &mut post,
                common.target_asset,
                MarketAsset::Base,
                base_reserve_debit,
                base_cash_debit,
                base_interest,
            )?;
            apply_hlp_reserve_debit_to_state(
                &mut post,
                common.target_asset,
                MarketAsset::Quote,
                quote_reserve_debit,
                quote_cash_debit,
                quote_interest,
            )?;

            post.base_ylp_supply = post
                .base_ylp_supply
                .checked_sub(ylp_burn_amount)
                .ok_or(ErrorCode::SupplyUnderflow)?;
            post.quote_ylp_supply = post
                .quote_ylp_supply
                .checked_sub(ylp_burn_amount)
                .ok_or(ErrorCode::SupplyUnderflow)?;
            if post.base_ylp_supply == 0 {
                require_eq!(post.base_live_reserve, 0, ErrorCode::BrokenInvariant);
            }
            if post.quote_ylp_supply == 0 {
                require_eq!(post.quote_live_reserve, 0, ErrorCode::BrokenInvariant);
            }

            let starting_vault = hlp_vault_from_rebalance_state(common.start);
            let current_repayment =
                starting_vault.repayment_for_max(debt_repayment.cash_repaid, common.start.borrow_index_nad)?;
            require!(current_repayment == debt_repayment, ErrorCode::BrokenInvariant);
            let mut vault = starting_vault;
            let current_clearance =
                vault.clear_debt_repay(debt_repayment.shares_to_burn, common.start.borrow_index_nad)?;
            require!(current_clearance == debt_clearance, ErrorCode::BrokenInvariant);
            require_eq!(
                current_clearance.interest_paid,
                interest_paid,
                ErrorCode::BrokenInvariant
            );
            vault.debit_ylp(ylp_burn_amount)?;
            post.target_debt_shares = vault.debt_shares;
            post.target_debt_principal = vault.debt_principal;
            post.target_ylp_shares = vault.ylp_shares;
        }
    }
    Ok(post)
}

fn curve_reserves_after_raw_debits(
    market: &Market,
    start: CurveReservesNad,
    base_debit: u64,
    quote_debit: u64,
) -> Result<CurveReservesNad> {
    Ok(CurveReservesNad {
        base: start
            .base
            .checked_sub(normalize_to_nad(base_debit as u128, market.base_side.asset_decimals)?)
            .ok_or(ErrorCode::ReserveUnderflow)?,
        quote: start
            .quote
            .checked_sub(normalize_to_nad(quote_debit as u128, market.quote_side.asset_decimals)?)
            .ok_or(ErrorCode::ReserveUnderflow)?,
    })
}

fn commit_hlp_rebalance_state(market: &mut Market, target_asset: MarketAsset, post: HlpRebalanceState) {
    market.base_side.reserves.live_reserve = post.base_live_reserve;
    market.base_side.reserves.cash_reserve = post.base_cash_reserve;
    market.quote_side.reserves.live_reserve = post.quote_live_reserve;
    market.quote_side.reserves.cash_reserve = post.quote_cash_reserve;
    market.base_side.shares.ylp_supply = post.base_ylp_supply;
    market.quote_side.shares.ylp_supply = post.quote_ylp_supply;
    match target_asset {
        MarketAsset::Base => {
            market.base_side.reserves.base_hlp_backing_inventory = post.base_target_backing_inventory;
            market.quote_side.reserves.base_hlp_backing_inventory = post.quote_target_backing_inventory;
            market.base_hlp_vault.ylp_shares = post.target_ylp_shares;
            market.base_hlp_vault.debt_shares = post.target_debt_shares;
            market.base_hlp_vault.debt_principal = post.target_debt_principal;
            market.base_hlp_vault.base_hlp_live_reserve = post.target_base_hlp_live_reserve;
            market.base_hlp_vault.quote_hlp_live_reserve = post.target_quote_hlp_live_reserve;
        }
        MarketAsset::Quote => {
            market.base_side.reserves.quote_hlp_backing_inventory = post.base_target_backing_inventory;
            market.quote_side.reserves.quote_hlp_backing_inventory = post.quote_target_backing_inventory;
            market.quote_hlp_vault.ylp_shares = post.target_ylp_shares;
            market.quote_hlp_vault.debt_shares = post.target_debt_shares;
            market.quote_hlp_vault.debt_principal = post.target_debt_principal;
            market.quote_hlp_vault.base_hlp_live_reserve = post.target_base_hlp_live_reserve;
            market.quote_hlp_vault.quote_hlp_live_reserve = post.target_quote_hlp_live_reserve;
        }
    }
}

#[cfg(test)]
fn commit_hlp_rebalance_planner_state(state: &mut HlpPlannerState, target_asset: MarketAsset, post: HlpRebalanceState) {
    state.base_side.live_reserve = post.base_live_reserve;
    state.base_side.cash_reserve = post.base_cash_reserve;
    state.base_side.ylp_supply = post.base_ylp_supply;
    state.quote_side.live_reserve = post.quote_live_reserve;
    state.quote_side.cash_reserve = post.quote_cash_reserve;
    state.quote_side.ylp_supply = post.quote_ylp_supply;
    match target_asset {
        MarketAsset::Base => {
            state.base_side.base_hlp_backing_inventory = post.base_target_backing_inventory;
            state.quote_side.base_hlp_backing_inventory = post.quote_target_backing_inventory;
        }
        MarketAsset::Quote => {
            state.base_side.quote_hlp_backing_inventory = post.base_target_backing_inventory;
            state.quote_side.quote_hlp_backing_inventory = post.quote_target_backing_inventory;
        }
    }
    let vault = state.vault_mut(target_asset);
    vault.ylp_shares = post.target_ylp_shares;
    vault.debt_shares = post.target_debt_shares;
    vault.debt_principal = post.target_debt_principal;
    vault.base_hlp_live_reserve = post.target_base_hlp_live_reserve;
    vault.quote_hlp_live_reserve = post.target_quote_hlp_live_reserve;
}

#[cfg(test)]
fn apply_hlp_rebalance_plan_to_planner_state(
    fixed: HlpPlannerStatic,
    state: &mut HlpPlannerState,
    plan: HlpRebalancePlan,
) -> Result<HlpRebalanceReceipt> {
    let target_asset = plan.common().target_asset;
    require!(
        capture_hlp_rebalance_state_from_planner(fixed, *state, target_asset) == plan.common().start,
        ErrorCode::BrokenInvariant
    );
    // Canonical exact-out plans are sealed at construction and their Market
    // identity is validated by the authoritative wrapper before this shared
    // fixed-value kernel runs. Guidance will supply a distinct sealed proof at
    // the later provider seam; it cannot construct this plan variant.
    let post = derive_hlp_rebalance_post_state_from_values(&plan, fixed.base_decimals, fixed.quote_decimals)?;
    let receipt = plan.receipt();
    commit_hlp_rebalance_planner_state(state, target_asset, post);
    Ok(receipt)
}

#[cfg(test)]
fn apply_compact_hlp_rebalance_plan_to_planner_state(
    fixed: HlpPlannerStatic,
    state: &mut HlpPlannerState,
    compact: HlpCompactRebalancePlan,
) -> Result<HlpRebalanceReceipt> {
    let target_asset = compact.plan.common().target_asset;
    require!(
        capture_hlp_rebalance_state_from_planner(fixed, *state, target_asset)
            == compact.plan.common().start,
        ErrorCode::BrokenInvariant
    );
    if let HlpRebalancePlan::Deleverage {
        exact_out_checkpoint,
        ..
    } = compact.plan
    {
        require!(exact_out_checkpoint.is_none(), ErrorCode::BrokenInvariant);
        if let Some(proof) = compact.guidance_settlement {
            require!(
                proof.facts().start_curve_reserves_nad == state.curve_reserves_nad(fixed)?,
                ErrorCode::BrokenInvariant
            );
        }
    } else {
        require!(compact.guidance_settlement.is_none(), ErrorCode::BrokenInvariant);
    }
    let post = derive_hlp_rebalance_post_state_from_values_with_guidance(
        &compact.plan,
        fixed.base_decimals,
        fixed.quote_decimals,
        compact.guidance_settlement.map(HlpGuidanceSettlementProof::facts),
    )?;
    let receipt = compact.plan.receipt();
    commit_hlp_rebalance_planner_state(state, target_asset, post);
    Ok(receipt)
}

fn apply_hlp_rebalance_plan(market: &mut Market, plan: HlpRebalancePlan) -> Result<HlpRebalanceReceipt> {
    #[cfg(test)]
    let planner_expected = {
        let fixed = HlpPlannerStatic::capture(market)?;
        let mut state = HlpPlannerState::capture(market);
        let receipt = apply_hlp_rebalance_plan_to_planner_state(fixed, &mut state, plan)?;
        (state, receipt)
    };
    let post = derive_hlp_rebalance_post_state(market, &plan)?;
    let receipt = plan.receipt();
    commit_hlp_rebalance_state(market, receipt.target_asset, post);
    #[cfg(test)]
    {
        require!(
            planner_expected.0 == HlpPlannerState::capture(market),
            ErrorCode::BrokenInvariant
        );
        require!(planner_expected.1 == receipt, ErrorCode::BrokenInvariant);
    }
    match plan {
        HlpRebalancePlan::LeverageUp { .. } => {
            debug_log_heap(503);
            debug_log_heap(504);
        }
        HlpRebalancePlan::Deleverage { .. } => debug_log_heap(604),
        HlpRebalancePlan::Noop { .. } => {}
    }
    Ok(receipt)
}

fn capture_hlp_rebalance_pair_start(market: &Market) -> HlpRebalancePairStart {
    HlpRebalancePairStart {
        base: capture_hlp_rebalance_state(market, MarketAsset::Base),
        quote: capture_hlp_rebalance_state(market, MarketAsset::Quote),
        base_active: market.base_hlp_vault.hlp_supply > 0 || market.base_hlp_vault.residual_exposure != 0,
        quote_active: market.quote_hlp_vault.hlp_supply > 0 || market.quote_hlp_vault.residual_exposure != 0,
    }
}

fn restore_hlp_rebalance_pair_start(market: &mut Market, start: HlpRebalancePairStart) {
    commit_hlp_rebalance_state(market, MarketAsset::Base, start.base);
    commit_hlp_rebalance_state(market, MarketAsset::Quote, start.quote);
}

fn plan_hlp_rebalance_pair_leg(
    market: &Market,
    target_asset: MarketAsset,
    valuation: Option<HlpValuation>,
) -> Result<HlpRebalancePairLegPlan> {
    valuation.map_or_else(
        || Ok(HlpRebalancePairLegPlan::Inactive { target_asset }),
        |valuation| {
            plan_hlp_rebalance_from_valuation(market, target_asset, valuation).map(HlpRebalancePairLegPlan::Active)
        },
    )
}

/// Build both legs without leaving planning mutations behind. The Base
/// successor is committed only long enough to construct Quote against the
/// canonical intermediate state, then the compact pair checkpoint restores
/// the caller before the complete pair is applied atomically.
fn plan_hlp_rebalance_pair(
    market: &mut Market,
    base_valuation: Option<HlpValuation>,
    quote_valuation: Option<HlpValuation>,
) -> Result<HlpRebalancePairPlan> {
    let start = capture_hlp_rebalance_pair_start(market);
    require!(
        base_valuation.is_some() == start.base_active && quote_valuation.is_some() == start.quote_active,
        ErrorCode::BrokenInvariant
    );
    let base = plan_hlp_rebalance_pair_leg(market, MarketAsset::Base, base_valuation)?;
    let quote_result = (|| {
        if let HlpRebalancePairLegPlan::Active(base_plan) = base {
            let base_post = derive_hlp_rebalance_post_state(market, &base_plan)?;
            commit_hlp_rebalance_state(market, MarketAsset::Base, base_post);
        }
        plan_hlp_rebalance_pair_leg(market, MarketAsset::Quote, quote_valuation)
    })();
    restore_hlp_rebalance_pair_start(market, start);
    let quote = quote_result?;
    Ok(HlpRebalancePairPlan { start, base, quote })
}

/// Keeps the fixed-size pair plan out of the surrounding lifecycle frame.
/// The planner still constructs Quote from the temporary Base successor, and
/// the applier still preflights both legs before their canonical commits.
#[inline(never)]
fn plan_and_apply_hlp_rebalance_pair(
    market: &mut Market,
    base_valuation: Option<HlpValuation>,
    quote_valuation: Option<HlpValuation>,
) -> Result<(HlpRebalanceReceipt, HlpRebalanceReceipt)> {
    let plan = plan_hlp_rebalance_pair(market, base_valuation, quote_valuation)?;
    apply_hlp_rebalance_pair_plan(market, plan)
}

/// Planner-only pair application on a disposable guidance scratch market.
/// Each leg still uses the single-source Stage1 plan/derive algebra and Quote
/// is still planned from Base's exact successor, but no atomic restore/replay
/// is needed because this state can neither authorize nor escape the solver.
#[inline(never)]
fn plan_and_apply_hlp_rebalance_pair_guidance(
    market: &mut Market,
    base_valuation: Option<HlpValuation>,
    quote_valuation: Option<HlpValuation>,
) -> Result<(HlpRebalanceReceipt, HlpRebalanceReceipt)> {
    let base_active = market.base_hlp_vault.hlp_supply > 0 || market.base_hlp_vault.residual_exposure != 0;
    let quote_active = market.quote_hlp_vault.hlp_supply > 0 || market.quote_hlp_vault.residual_exposure != 0;
    require!(
        base_valuation.is_some() == base_active && quote_valuation.is_some() == quote_active,
        ErrorCode::BrokenInvariant
    );
    let base_plan = plan_hlp_rebalance_pair_leg(market, MarketAsset::Base, base_valuation)?;
    let (base, base_post) = derive_hlp_rebalance_pair_leg_post(market, MarketAsset::Base, base_plan)?;
    if let Some(post) = base_post {
        commit_hlp_rebalance_state(market, MarketAsset::Base, post);
    }
    debug_log_heap(404);
    let quote_plan = plan_hlp_rebalance_pair_leg(market, MarketAsset::Quote, quote_valuation)?;
    let (quote, quote_post) = derive_hlp_rebalance_pair_leg_post(market, MarketAsset::Quote, quote_plan)?;
    if let Some(post) = quote_post {
        commit_hlp_rebalance_state(market, MarketAsset::Quote, post);
    }
    debug_log_heap(405);
    Ok((base, quote))
}

fn derive_hlp_rebalance_pair_leg_post(
    market: &Market,
    expected_target: MarketAsset,
    plan: HlpRebalancePairLegPlan,
) -> Result<(HlpRebalanceReceipt, Option<HlpRebalanceState>)> {
    match plan {
        HlpRebalancePairLegPlan::Inactive { target_asset } => {
            require!(target_asset == expected_target, ErrorCode::BrokenInvariant);
            Ok((empty_hlp_rebalance_receipt(target_asset), None))
        }
        HlpRebalancePairLegPlan::Active(plan) => {
            require!(
                plan.common().target_asset == expected_target,
                ErrorCode::BrokenInvariant
            );
            let receipt = plan.receipt();
            let post = derive_hlp_rebalance_post_state(market, &plan)?;
            Ok((receipt, Some(post)))
        }
    }
}

#[cfg(test)]
fn apply_hlp_rebalance_pair_to_planner_state(
    fixed: HlpPlannerStatic,
    state: &mut HlpPlannerState,
    plan: HlpRebalancePairPlan,
) -> Result<(HlpRebalanceReceipt, HlpRebalanceReceipt)> {
    let apply_leg = |state: &mut HlpPlannerState,
                     target_asset: MarketAsset,
                     leg: HlpRebalancePairLegPlan|
     -> Result<HlpRebalanceReceipt> {
        match leg {
            HlpRebalancePairLegPlan::Inactive {
                target_asset: planned_asset,
            } => {
                require!(planned_asset == target_asset, ErrorCode::BrokenInvariant);
                Ok(empty_hlp_rebalance_receipt(target_asset))
            }
            HlpRebalancePairLegPlan::Active(plan) => {
                require!(plan.common().target_asset == target_asset, ErrorCode::BrokenInvariant);
                apply_hlp_rebalance_plan_to_planner_state(fixed, state, plan)
            }
        }
    };
    let base = apply_leg(state, MarketAsset::Base, plan.base)?;
    let quote = apply_leg(state, MarketAsset::Quote, plan.quote)?;
    Ok((base, quote))
}

fn apply_hlp_rebalance_pair_plan(
    market: &mut Market,
    plan: HlpRebalancePairPlan,
) -> Result<(HlpRebalanceReceipt, HlpRebalanceReceipt)> {
    require!(
        capture_hlp_rebalance_pair_start(market) == plan.start,
        ErrorCode::BrokenInvariant
    );
    require!(
        matches!(plan.base, HlpRebalancePairLegPlan::Active(_)) == plan.start.base_active
            && matches!(plan.quote, HlpRebalancePairLegPlan::Active(_)) == plan.start.quote_active,
        ErrorCode::BrokenInvariant
    );
    #[cfg(test)]
    let planner_expected = {
        let fixed = HlpPlannerStatic::capture(market)?;
        let mut state = HlpPlannerState::capture(market);
        let receipts = apply_hlp_rebalance_pair_to_planner_state(fixed, &mut state, plan)?;
        (state, receipts)
    };
    let (base, base_post) = derive_hlp_rebalance_pair_leg_post(market, MarketAsset::Base, plan.base)?;
    if let Some(post) = base_post {
        commit_hlp_rebalance_state(market, MarketAsset::Base, post);
    }
    let quote_result = derive_hlp_rebalance_pair_leg_post(market, MarketAsset::Quote, plan.quote);
    restore_hlp_rebalance_pair_start(market, plan.start);
    let (quote, quote_post) = quote_result?;

    if let Some(post) = base_post {
        commit_hlp_rebalance_state(market, MarketAsset::Base, post);
    }
    debug_log_heap(404);
    if let Some(post) = quote_post {
        commit_hlp_rebalance_state(market, MarketAsset::Quote, post);
    }
    debug_log_heap(405);
    #[cfg(test)]
    {
        require!(
            planner_expected.0 == HlpPlannerState::capture(market),
            ErrorCode::BrokenInvariant
        );
        require!(planner_expected.1 == (base, quote), ErrorCode::BrokenInvariant);
    }
    Ok((base, quote))
}

fn plan_leverage_up_proportional_with_cash_floors(
    market: &Market,
    target_asset: MarketAsset,
    ideal_delta: i128,
    valuation: HlpValuation,
    cash_floors: SwapCashFloors,
) -> Result<HlpRebalancePlan> {
    debug_log_heap(500);
    let borrowed_asset = target_asset.opposite();
    // Controller-driven hLP funding is internal leverage, not lender cash
    // leaving the market. Clip only to cash headroom and preserve any
    // unexecuted amount as residual exposure for a later operation.
    let borrow_headroom = market
        .hlp_funding_headroom(borrowed_asset)?
        .saturating_sub(cash_floors.for_asset(borrowed_asset));
    let feasible_delta_nad = if borrow_headroom == 0 {
        0
    } else {
        ideal_delta.unsigned_abs().min(asset_value_in_target_nad_with_prices(
            market,
            valuation.prices,
            borrowed_asset,
            borrow_headroom,
            target_asset,
        )?)
    };
    let preposition_capacity_bound = feasible_delta_nad < ideal_delta.unsigned_abs();
    let feasible_delta = i128::try_from(feasible_delta_nad).map_err(|_| ErrorCode::MarketMathOverflow)?;
    let amounts = proportional_rebalance_amounts(market, target_asset, feasible_delta, valuation)?;
    debug_log_heap(501);
    if amounts.target_leg_amount == 0 || amounts.borrowed_leg_amount == 0 || amounts.debt_amount == 0 {
        return Ok(plan_hlp_noop(
            market,
            target_asset,
            ideal_delta,
            valuation,
            true,
            HlpRebalanceNoopReason::CapacityOrGranularity,
        ));
    }
    let debt_shares = require_hlp_borrow_headroom_with_cash_floor(
        market,
        borrowed_asset,
        amounts.debt_amount,
        cash_floors.for_asset(borrowed_asset),
    )?;
    let (base_leg_amount, quote_leg_amount) = match target_asset {
        MarketAsset::Base => (amounts.target_leg_amount, amounts.borrowed_leg_amount),
        MarketAsset::Quote => (amounts.borrowed_leg_amount, amounts.target_leg_amount),
    };
    let ylp_amount = ylp_for_live_reserve_deposit(market, base_leg_amount, quote_leg_amount)?;
    debug_log_heap(502);
    if ylp_amount == 0 {
        return Ok(plan_hlp_noop(
            market,
            target_asset,
            ideal_delta,
            valuation,
            true,
            HlpRebalanceNoopReason::CapacityOrGranularity,
        ));
    }
    let plan = HlpRebalancePlan::LeverageUp {
        common: hlp_rebalance_plan_common(
            market,
            target_asset,
            ideal_delta,
            valuation.nav_nad,
            preposition_capacity_bound,
        ),
        base_leg_amount,
        quote_leg_amount,
        ylp_mint_amount: ylp_amount,
        debt_shares_added: debt_shares,
        debt_principal_added: amounts.debt_amount,
    };
    Ok(plan)
}

#[cfg(test)]
fn deleverage_proportional_with_cash_floor(
    market: &mut Market,
    target_asset: MarketAsset,
    ideal_delta: i128,
    valuation: HlpValuation,
    cash_floor: Option<(MarketAsset, u64)>,
) -> Result<HlpRebalanceReceipt> {
    let cash_floors = cash_floor.map_or_else(SwapCashFloors::default, |(asset, amount)| {
        let mut floors = SwapCashFloors::default();
        floors.set(asset, amount);
        floors
    });
    let plan =
        plan_deleverage_proportional_with_cash_floors(market, target_asset, ideal_delta, valuation, cash_floors)?;
    apply_hlp_rebalance_plan(market, plan)
}

fn plan_deleverage_proportional_with_cash_floors(
    market: &Market,
    target_asset: MarketAsset,
    ideal_delta: i128,
    valuation: HlpValuation,
    cash_floors: SwapCashFloors,
) -> Result<HlpRebalancePlan> {
    debug_log_heap(600);
    let (borrow_index, vault_ylp) = match target_asset {
        MarketAsset::Base => (market.debt.quote_borrow_index_nad, market.base_hlp_vault.ylp_shares),
        MarketAsset::Quote => (market.debt.base_borrow_index_nad, market.quote_hlp_vault.ylp_shares),
    };
    let collateral_value_nad = valuation
        .values
        .target_inventory_value_nad
        .checked_add(valuation.values.opposite_inventory_value_nad)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let feasible_delta_nad = ideal_delta
        .unsigned_abs()
        .min(collateral_value_nad)
        .min(valuation.values.debt_value_nad);
    let mut preposition_capacity_bound = feasible_delta_nad < ideal_delta.unsigned_abs();
    let feasible_delta = -i128::try_from(feasible_delta_nad).map_err(|_| ErrorCode::MarketMathOverflow)?;
    let amounts = proportional_rebalance_amounts(market, target_asset, feasible_delta, valuation)?;
    debug_log_heap(601);
    if amounts.target_leg_amount == 0 || amounts.borrowed_leg_amount == 0 || amounts.debt_amount == 0 {
        return Ok(plan_hlp_noop(
            market,
            target_asset,
            ideal_delta,
            valuation,
            true,
            HlpRebalanceNoopReason::CapacityOrGranularity,
        ));
    }

    let (base_leg_amount, quote_leg_amount) = match target_asset {
        MarketAsset::Base => (amounts.target_leg_amount, amounts.borrowed_leg_amount),
        MarketAsset::Quote => (amounts.borrowed_leg_amount, amounts.target_leg_amount),
    };
    let base_ylp_burn = ylp_shares_for_live_reserve_amount(market, MarketAsset::Base, base_leg_amount)?;
    let quote_ylp_burn = ylp_shares_for_live_reserve_amount(market, MarketAsset::Quote, quote_leg_amount)?;
    let desired_ylp_burn = base_ylp_burn.min(quote_ylp_burn).min(vault_ylp);
    let burn_facts = cap_hlp_deleverage_ylp_burn(market, target_asset, desired_ylp_burn, valuation, cash_floors)?;
    let ylp_burn = burn_facts.ylp_burn_amount;
    debug_log_heap(602);
    preposition_capacity_bound |= ylp_burn < desired_ylp_burn;
    if ylp_burn == 0 {
        return Ok(plan_hlp_noop(
            market,
            target_asset,
            ideal_delta,
            valuation,
            true,
            HlpRebalanceNoopReason::CapacityOrGranularity,
        ));
    }
    // The selected facts are the exact ordinary-yLP entitlement and canonical
    // indexed-debt phase certified by the cap. Reuse them so the final plan
    // cannot drift onto a neighboring raw-share phase.
    let base_leg_amount = burn_facts.base_leg_amount;
    let quote_leg_amount = burn_facts.quote_leg_amount;
    let (target_leg_amount, borrowed_leg_amount) = match target_asset {
        MarketAsset::Base => (base_leg_amount, quote_leg_amount),
        MarketAsset::Quote => (quote_leg_amount, base_leg_amount),
    };
    let expected_interest_paid = burn_facts.interest_paid;
    let reserve_plan = plan_hlp_deleverage_reserve_legs(
        market,
        target_asset,
        target_leg_amount,
        borrowed_leg_amount,
        expected_interest_paid,
    )?;
    debug_log_heap(603);
    let vault = match target_asset {
        MarketAsset::Base => market.base_hlp_vault,
        MarketAsset::Quote => market.quote_hlp_vault,
    };
    let debt_repayment = burn_facts.debt_repayment;
    let debt_shares_to_remove = debt_repayment.shares_to_burn;
    let mut planned_vault = vault;
    let debt_clearance = planned_vault.clear_debt_repay(debt_shares_to_remove, borrow_index)?;
    require_eq!(
        debt_clearance.interest_paid,
        expected_interest_paid,
        ErrorCode::BrokenInvariant
    );
    planned_vault.debit_ylp(ylp_burn)?;
    let plan = HlpRebalancePlan::Deleverage {
        common: hlp_rebalance_plan_common(
            market,
            target_asset,
            ideal_delta,
            valuation.nav_nad,
            preposition_capacity_bound,
        ),
        ylp_burn_amount: ylp_burn,
        base_entitlement_amount: base_leg_amount,
        quote_entitlement_amount: quote_leg_amount,
        base_reserve_debit: reserve_plan.base_reserve_debit,
        quote_reserve_debit: reserve_plan.quote_reserve_debit,
        base_cash_debit: reserve_plan.base_cash_debit,
        quote_cash_debit: reserve_plan.quote_cash_debit,
        debt_repayment,
        debt_clearance,
        interest_paid: expected_interest_paid,
        exact_out_checkpoint: reserve_plan.exact_out_checkpoint,
    };
    Ok(plan)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct HlpDeleverageBurnFacts {
    ylp_burn_amount: u64,
    base_leg_amount: u64,
    quote_leg_amount: u64,
    repay_amount: u64,
    current_debt: u128,
    minimum_executable_repay: u128,
    debt_repayment: DebtRepaymentQuote,
    interest_paid: u64,
}

impl HlpDeleverageBurnFacts {
    const fn leg_amount(self, asset: MarketAsset) -> u64 {
        match asset {
            MarketAsset::Base => self.base_leg_amount,
            MarketAsset::Quote => self.quote_leg_amount,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HlpDeleverageBurnCapacity {
    target_asset: MarketAsset,
    target_live_reserve: u64,
    target_ylp_supply: u64,
    borrowed_live_reserve: u64,
    borrowed_ylp_supply: u64,
    target_leg_cap: u64,
    borrowed_leg_cap: u64,
    current_debt_raw_cap: u64,
    maximum_interest_safe_repay: u64,
}

impl HlpDeleverageBurnCapacity {
    fn capture(
        market: &Market,
        target_asset: MarketAsset,
        desired: HlpDeleverageBurnFacts,
        cash_floors: SwapCashFloors,
    ) -> Result<Option<Self>> {
        let borrowed_asset = target_asset.opposite();
        let spendable_cash = |asset: MarketAsset| {
            market
                .side(asset)
                .reserves
                .cash_reserve
                .saturating_sub(cash_floors.for_asset(asset))
        };
        let vault = match target_asset {
            MarketAsset::Base => &market.base_hlp_vault,
            MarketAsset::Quote => &market.quote_hlp_vault,
        };
        let target_hlp_available = vault.hlp_live_reserve(target_asset);
        let borrowed_hlp_available = vault.hlp_live_reserve(borrowed_asset);
        let borrowed_cash_available = spendable_cash(borrowed_asset);
        let borrow_index = market.debt.borrow_index(borrowed_asset);
        let Some(maximum_interest_safe_repay) = maximum_hlp_interest_safe_repay_input(
            vault,
            borrow_index,
            desired.current_debt,
            desired.minimum_executable_repay,
            borrowed_cash_available,
        )?
        else {
            return Ok(None);
        };
        Ok(Some(Self {
            target_asset,
            target_live_reserve: market.side(target_asset).reserves.live_reserve,
            target_ylp_supply: market.side(target_asset).shares.ylp_supply,
            borrowed_live_reserve: market.side(borrowed_asset).reserves.live_reserve,
            borrowed_ylp_supply: market.side(borrowed_asset).shares.ylp_supply,
            target_leg_cap: target_hlp_available.saturating_add(spendable_cash(target_asset)),
            borrowed_leg_cap: borrowed_hlp_available.saturating_add(borrowed_cash_available),
            current_debt_raw_cap: u64::try_from(desired.current_debt).unwrap_or(u64::MAX),
            maximum_interest_safe_repay,
        }))
    }

    fn direct_entitlement_cap(self, desired_burn: u64) -> Result<u64> {
        Ok(desired_burn
            .min(maximum_ylp_burn_for_leg_cap(
                self.target_live_reserve,
                self.target_ylp_supply,
                self.target_leg_cap,
            )?)
            .min(maximum_ylp_burn_for_leg_cap(
                self.borrowed_live_reserve,
                self.borrowed_ylp_supply,
                self.borrowed_leg_cap,
            )?))
    }

    fn repayment_input_for_burn(self, market: &Market, ylp_burn: u64, prices: HlpCurvePrices) -> Result<u64> {
        #[cfg(test)]
        HLP_DELEVERAGE_CHEAP_REPAYMENT_EVALUATIONS.with(|count| count.set(count.get().saturating_add(1)));
        let target_leg =
            ylp_live_underlying_amount_from_values(self.target_live_reserve, self.target_ylp_supply, ylp_burn)?;
        let borrowed_leg =
            ylp_live_underlying_amount_from_values(self.borrowed_live_reserve, self.borrowed_ylp_supply, ylp_burn)?;
        hlp_deleverage_repay_amount_for_legs(
            market,
            self.target_asset,
            target_leg,
            borrowed_leg,
            prices,
            self.current_debt_raw_cap,
        )
    }
}

fn maximum_ylp_burn_for_leg_cap(live_reserve: u64, ylp_supply: u64, leg_cap: u64) -> Result<u64> {
    // `floor(burn * live / supply) <= cap`. Avoid the product entirely when
    // the complete entitlement fits; this also covers a zero live reserve.
    if leg_cap >= live_reserve {
        return Ok(ylp_supply);
    }
    require!(live_reserve > 0 && ylp_supply > 0, ErrorCode::SupplyUnderflow);
    let numerator = (leg_cap as u128)
        .checked_add(1)
        .and_then(|value| value.checked_mul(ylp_supply as u128))
        .and_then(|value| value.checked_sub(1))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    u64::try_from(numerator / live_reserve as u128).map_err(|_| ErrorCode::MarketMathOverflow.into())
}

fn maximum_hlp_interest_safe_repay_input(
    vault: &HlpVault,
    borrow_index_nad: u128,
    current_debt: u128,
    minimum_executable_repay: u128,
    borrowed_cash_available: u64,
) -> Result<Option<u64>> {
    let current_debt_raw_cap = u64::try_from(current_debt).unwrap_or(u64::MAX);
    let principal = u128::from(vault.debt_principal).min(current_debt);
    let accrued_interest = current_debt.checked_sub(principal).ok_or(ErrorCode::DebtMathOverflow)?;
    if accrued_interest == 0 || borrowed_cash_available as u128 >= accrued_interest {
        return Ok(Some(current_debt_raw_cap));
    }

    // realized_interest(r) = r - floor(P*r/D) = ceil((D-P)*r/D).
    // Therefore the largest interest-safe canonical debt reduction is exact.
    let maximum_reduction = mul_div_u128(borrowed_cash_available as u128, current_debt, accrued_interest)?;
    if maximum_reduction < minimum_executable_repay {
        return Ok(None);
    }
    if maximum_reduction >= current_debt_raw_cap as u128 {
        return Ok(Some(current_debt_raw_cap));
    }

    // Convert the reduction cap to the caller-input phase used by
    // `repayment_for_max`. Inputs through `next_reduction - 1` select the same
    // final interest-safe share burn; this preserves the aggregate-debt floor
    // and its one-adjacent-share rule exactly.
    let reduction_cap = u64::try_from(maximum_reduction).map_err(|_| ErrorCode::DebtMathOverflow)?;
    let repayment = vault.repayment_for_max(reduction_cap, borrow_index_nad)?;
    if repayment.shares_to_burn == vault.debt_shares {
        return Ok(Some(current_debt_raw_cap));
    }
    let next_share_burn = repayment
        .shares_to_burn
        .checked_add(1)
        .ok_or(ErrorCode::DebtShareMathOverflow)?;
    let debt_after_next = Debt::shares_to_debt(
        vault
            .debt_shares
            .checked_sub(next_share_burn)
            .ok_or(ErrorCode::DebtShareMathOverflow)?,
        borrow_index_nad,
    )?;
    let next_reduction = current_debt
        .checked_sub(debt_after_next)
        .ok_or(ErrorCode::DebtMathOverflow)?;
    let maximum_input = next_reduction
        .checked_sub(1)
        .ok_or(ErrorCode::DebtMathOverflow)?
        .min(current_debt_raw_cap as u128);
    u64::try_from(maximum_input)
        .map(Some)
        .map_err(|_| ErrorCode::DebtMathOverflow.into())
}

fn cap_hlp_deleverage_ylp_burn(
    market: &Market,
    target_asset: MarketAsset,
    desired_burn: u64,
    valuation: HlpValuation,
    cash_floors: SwapCashFloors,
) -> Result<HlpDeleverageBurnFacts> {
    let selected = cap_hlp_deleverage_ylp_burn_from_facts(market, target_asset, desired_burn, valuation, cash_floors)?;
    #[cfg(test)]
    require_eq!(
        selected.ylp_burn_amount,
        cap_hlp_deleverage_ylp_burn_reference(market, target_asset, desired_burn, valuation, cash_floors,)?,
        ErrorCode::BrokenInvariant
    );
    Ok(selected)
}

fn cap_hlp_deleverage_ylp_burn_from_facts(
    market: &Market,
    target_asset: MarketAsset,
    desired_burn: u64,
    valuation: HlpValuation,
    cash_floors: SwapCashFloors,
) -> Result<HlpDeleverageBurnFacts> {
    debug_log_heap(700);
    if desired_burn == 0 {
        return Ok(HlpDeleverageBurnFacts::default());
    }

    // Preserve the legacy endpoint/error domain: a non-executable desired
    // burn returns zero immediately, while an exact feasible endpoint avoids
    // all cap derivation.
    let Some((desired, desired_cash_available)) =
        hlp_deleverage_cash_capacity_for_burn(market, target_asset, desired_burn, valuation, cash_floors)?
    else {
        return Ok(HlpDeleverageBurnFacts::default());
    };
    if desired_cash_available {
        return Ok(desired);
    }

    let Some(capacity) = HlpDeleverageBurnCapacity::capture(market, target_asset, desired, cash_floors)? else {
        return Ok(HlpDeleverageBurnFacts::default());
    };
    let mut upper = capacity.direct_entitlement_cap(desired_burn)?;
    if upper == 0 {
        return Ok(HlpDeleverageBurnFacts::default());
    }
    if capacity.maximum_interest_safe_repay < capacity.current_debt_raw_cap {
        let mut low = 0_u64;
        while low < upper {
            let probe = low + (upper - low + 1) / 2;
            if capacity.repayment_input_for_burn(market, probe, valuation.prices)?
                <= capacity.maximum_interest_safe_repay
            {
                low = probe;
            } else {
                upper = probe - 1;
            }
        }
        upper = low;
    }
    if upper == 0 {
        return Ok(HlpDeleverageBurnFacts::default());
    }

    let Some((selected, selected_cash_available)) =
        hlp_deleverage_cash_capacity_for_burn(market, target_asset, upper, valuation, cash_floors)?
    else {
        // All higher candidates are excluded by an exact monotone cap, so an
        // upper candidate below the first indexed-share phase means no
        // executable cash-safe burn exists.
        return Ok(HlpDeleverageBurnFacts::default());
    };
    require!(selected_cash_available, ErrorCode::BrokenInvariant);
    require!(upper < desired_burn, ErrorCode::BrokenInvariant);
    let adjacent = upper.checked_add(1).ok_or(ErrorCode::MarketMathOverflow)?;
    let Some((_, adjacent_cash_available)) =
        hlp_deleverage_cash_capacity_for_burn(market, target_asset, adjacent, valuation, cash_floors)?
    else {
        return err!(ErrorCode::BrokenInvariant);
    };
    require!(!adjacent_cash_available, ErrorCode::BrokenInvariant);
    debug_log_heap(704);
    Ok(selected)
}

fn hlp_deleverage_cash_capacity_for_burn(
    market: &Market,
    target_asset: MarketAsset,
    ylp_burn: u64,
    valuation: HlpValuation,
    cash_floors: SwapCashFloors,
) -> Result<Option<(HlpDeleverageBurnFacts, bool)>> {
    #[cfg(test)]
    HLP_DELEVERAGE_FULL_CAPACITY_EVALUATIONS.with(|count| count.set(count.get().saturating_add(1)));
    hlp_deleverage_cash_capacity_for_burn_untracked(market, target_asset, ylp_burn, valuation, cash_floors)
}

fn hlp_deleverage_cash_capacity_for_burn_untracked(
    market: &Market,
    target_asset: MarketAsset,
    ylp_burn: u64,
    valuation: HlpValuation,
    cash_floors: SwapCashFloors,
) -> Result<Option<(HlpDeleverageBurnFacts, bool)>> {
    debug_log_heap(701);
    let Some(facts) = hlp_deleverage_burn_facts_if_executable(market, target_asset, ylp_burn, valuation)? else {
        return Ok(None);
    };
    debug_log_heap(703);
    let borrowed_asset = target_asset.opposite();
    let vault = match target_asset {
        MarketAsset::Base => &market.base_hlp_vault,
        MarketAsset::Quote => &market.quote_hlp_vault,
    };
    let spendable_cash = |asset: MarketAsset| {
        market
            .side(asset)
            .reserves
            .cash_reserve
            .saturating_sub(cash_floors.for_asset(asset))
    };
    let target_cash_needed = facts
        .leg_amount(target_asset)
        .saturating_sub(vault.hlp_live_reserve(target_asset));
    let borrowed_cash_needed = facts
        .leg_amount(borrowed_asset)
        .saturating_sub(vault.hlp_live_reserve(borrowed_asset))
        .max(facts.interest_paid);
    Ok(Some((
        facts,
        target_cash_needed <= spendable_cash(target_asset) && borrowed_cash_needed <= spendable_cash(borrowed_asset),
    )))
}

#[cfg(test)]
fn cap_hlp_deleverage_ylp_burn_reference(
    market: &Market,
    target_asset: MarketAsset,
    desired_burn: u64,
    valuation: HlpValuation,
    cash_floors: SwapCashFloors,
) -> Result<u64> {
    if desired_burn == 0 {
        return Ok(0);
    }
    let borrowed_asset = target_asset.opposite();
    let spendable_cash = |asset: MarketAsset| {
        market
            .side(asset)
            .reserves
            .cash_reserve
            .saturating_sub(cash_floors.for_asset(asset))
    };
    let vault = match target_asset {
        MarketAsset::Base => &market.base_hlp_vault,
        MarketAsset::Quote => &market.quote_hlp_vault,
    };
    let target_hlp_available = vault.hlp_live_reserve(target_asset);
    let borrowed_hlp_available = vault.hlp_live_reserve(borrowed_asset);
    let target_cash_available = spendable_cash(target_asset);
    let borrowed_cash_available = spendable_cash(borrowed_asset);
    let cash_capacity = |ylp_burn: u64| -> Result<Option<bool>> {
        HLP_DELEVERAGE_LEGACY_CAPACITY_EVALUATIONS.with(|count| count.set(count.get().saturating_add(1)));
        let target_leg = ylp_live_underlying_amount(market, target_asset, ylp_burn)?;
        let borrowed_leg = ylp_live_underlying_amount(market, borrowed_asset, ylp_burn)?;
        let Some(interest) = hlp_deleverage_interest_if_executable(market, target_asset, ylp_burn, valuation)? else {
            return Ok(None);
        };
        let target_cash_needed = target_leg.saturating_sub(target_hlp_available);
        let borrowed_cash_needed = borrowed_leg.saturating_sub(borrowed_hlp_available).max(interest);
        Ok(Some(
            target_cash_needed <= target_cash_available && borrowed_cash_needed <= borrowed_cash_available,
        ))
    };
    match cash_capacity(desired_burn)? {
        Some(true) => Ok(desired_burn),
        None => Ok(0),
        Some(false) => {
            let mut low = 0_u64;
            let mut high = desired_burn;
            let mut best = 0_u64;
            while high - low > 1 {
                let probe = low + (high - low) / 2;
                match cash_capacity(probe)? {
                    None => low = probe,
                    Some(true) => {
                        best = probe;
                        low = probe;
                    }
                    Some(false) => high = probe,
                }
            }
            Ok(best)
        }
    }
}

#[cfg(test)]
fn hlp_deleverage_interest_for_burn(
    market: &Market,
    target_asset: MarketAsset,
    ylp_burn: u64,
    valuation: HlpValuation,
) -> Result<u64> {
    hlp_deleverage_burn_facts_if_executable(market, target_asset, ylp_burn, valuation)?
        .map(|facts| facts.interest_paid)
        .ok_or_else(|| error!(ErrorCode::DebtShareDivisionOverflow))
}

#[cfg(test)]
fn hlp_deleverage_interest_if_executable(
    market: &Market,
    target_asset: MarketAsset,
    ylp_burn: u64,
    valuation: HlpValuation,
) -> Result<Option<u64>> {
    hlp_deleverage_burn_facts_if_executable(market, target_asset, ylp_burn, valuation)
        .map(|facts| facts.map(|facts| facts.interest_paid))
}

fn hlp_deleverage_repay_amount_for_legs(
    market: &Market,
    target_asset: MarketAsset,
    target_leg: u64,
    borrowed_leg: u64,
    prices: HlpCurvePrices,
    current_debt_raw_cap: u64,
) -> Result<u64> {
    let borrowed_asset = target_asset.opposite();
    let removed_value_nad =
        asset_value_in_target_nad_with_prices(market, prices, target_asset, target_leg, target_asset)?
            .checked_add(asset_value_in_target_nad_with_prices(
                market,
                prices,
                borrowed_asset,
                borrowed_leg,
                target_asset,
            )?)
            .ok_or(ErrorCode::MarketMathOverflow)?;
    Ok(
        raw_amount_from_target_value_nad_with_prices(market, prices, borrowed_asset, target_asset, removed_value_nad)?
            .min(current_debt_raw_cap),
    )
}

fn hlp_deleverage_burn_facts_if_executable(
    market: &Market,
    target_asset: MarketAsset,
    ylp_burn: u64,
    valuation: HlpValuation,
) -> Result<Option<HlpDeleverageBurnFacts>> {
    let borrowed_asset = target_asset.opposite();
    let base_leg = ylp_live_underlying_amount(market, MarketAsset::Base, ylp_burn)?;
    let quote_leg = ylp_live_underlying_amount(market, MarketAsset::Quote, ylp_burn)?;
    let (target_leg, borrowed_leg, vault) = match target_asset {
        MarketAsset::Base => (base_leg, quote_leg, &market.base_hlp_vault),
        MarketAsset::Quote => (quote_leg, base_leg, &market.quote_hlp_vault),
    };
    let borrow_index = market.debt.borrow_index(borrowed_asset);
    let current_debt = Debt::shares_to_debt(vault.debt_shares, borrow_index)?;
    let repay_amount = hlp_deleverage_repay_amount_for_legs(
        market,
        target_asset,
        target_leg,
        borrowed_leg,
        valuation.prices,
        u64::try_from(current_debt).unwrap_or(u64::MAX),
    )?;
    if repay_amount == 0 {
        return Ok(None);
    }
    let minimum_repay = current_debt
        .checked_sub(Debt::shares_to_debt(vault.debt_shares.saturating_sub(1), borrow_index)?)
        .ok_or(ErrorCode::DebtMathOverflow)?;
    if (repay_amount as u128) < minimum_repay {
        return Ok(None);
    }
    let repayment = vault.repayment_for_max(repay_amount, borrow_index)?;
    let principal = u128::from(vault.debt_principal).min(current_debt);
    let (_, interest_paid) =
        crate::math::realized_interest_split(repayment.position_debt_reduced, current_debt, principal)?;
    Ok(Some(HlpDeleverageBurnFacts {
        ylp_burn_amount: ylp_burn,
        base_leg_amount: base_leg,
        quote_leg_amount: quote_leg,
        repay_amount,
        current_debt,
        minimum_executable_repay: minimum_repay,
        debt_repayment: repayment,
        interest_paid,
    }))
}

fn proportional_rebalance_amounts(
    market: &Market,
    target_asset: MarketAsset,
    total_value_delta_nad: i128,
    valuation: HlpValuation,
) -> Result<ProportionalRebalanceAmounts> {
    if total_value_delta_nad == 0 {
        return Ok(ProportionalRebalanceAmounts::default());
    }
    let collateral_value = valuation
        .values
        .target_inventory_value_nad
        .checked_add(valuation.values.opposite_inventory_value_nad)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    require!(collateral_value > 0, ErrorCode::DenominatorOverflow);
    let total_value_delta = total_value_delta_nad.unsigned_abs();
    let target_value_delta = mul_div_u128(
        total_value_delta,
        valuation.values.target_inventory_value_nad,
        collateral_value,
    )?;
    let borrowed_value_delta = total_value_delta
        .checked_sub(target_value_delta)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let target_leg_amount = raw_amount_from_target_value_nad_with_prices(
        market,
        valuation.prices,
        target_asset,
        target_asset,
        target_value_delta,
    )?;
    let borrowed_asset = target_asset.opposite();
    let borrowed_leg_amount = raw_amount_from_target_value_nad_with_prices(
        market,
        valuation.prices,
        borrowed_asset,
        target_asset,
        borrowed_value_delta,
    )?;
    let debt_amount = raw_amount_from_target_value_nad_with_prices(
        market,
        valuation.prices,
        borrowed_asset,
        target_asset,
        total_value_delta,
    )?;
    Ok(ProportionalRebalanceAmounts {
        target_leg_amount,
        borrowed_leg_amount,
        debt_amount,
    })
}

fn refresh_hlp_after_rebalance(
    market: &mut Market,
    target_asset: MarketAsset,
    receipt: HlpRebalanceReceipt,
    post_prices: HlpCurvePrices,
) -> Result<HlpRebalanceReceipt> {
    // Revalue actual post-mutation inventory and debt. Requested raw amounts
    // do not capture debt-share, yLP-share, and reserve-rounding effects.
    let post_valuation = current_hlp_valuation_with_prices(market, target_asset, post_prices)?;
    refresh_hlp_after_rebalance_from_valuation(market, target_asset, receipt, post_valuation)
}

fn refresh_hlp_after_rebalance_from_valuation(
    market: &mut Market,
    target_asset: MarketAsset,
    mut receipt: HlpRebalanceReceipt,
    post_valuation: HlpValuation,
) -> Result<HlpRebalanceReceipt> {
    let settlement_price = post_valuation.prices.for_asset(target_asset);
    let actual_residual_exposure = post_valuation.ideal_delta;
    let residual_exposure = recognized_hlp_residual_exposure(actual_residual_exposure, post_valuation.nav_nad);
    if !post_valuation.proportional_hedge_available && residual_exposure != 0 {
        // `ideal_delta` normally means total proportional liquidity value. In
        // the zero-target coordinate the persisted value is instead the
        // signed unhedgeable O-D exposure. Do not subtract unlike control
        // coordinates and report fictitious execution.
        receipt.ideal_delta = residual_exposure;
        receipt.executed_delta = 0;
    } else {
        receipt.executed_delta = receipt
            .ideal_delta
            .checked_sub(residual_exposure)
            .ok_or(ErrorCode::MarketMathOverflow)?;
    }
    let vault = match target_asset {
        MarketAsset::Base => &mut market.base_hlp_vault,
        MarketAsset::Quote => &mut market.quote_hlp_vault,
    };
    vault.last_nav_nad = post_valuation.nav_nad;
    vault.residual_exposure = residual_exposure;
    // Only a fully settled hedge earns a new settlement reference. Advancing
    // it after a partial or no-op correction would let repeated worsening flow
    // ratchet the divergence band around stale inventory.
    if residual_exposure == 0 {
        vault.cached_settlement_price_nad = settlement_price;
    }
    receipt.residual_exposure = residual_exposure;
    receipt.nav_nad = post_valuation.nav_nad;
    market.assert_virtual_reserve_invariant(MarketAsset::Base)?;
    market.assert_virtual_reserve_invariant(MarketAsset::Quote)?;
    Ok(receipt)
}

fn ylp_shares_for_live_reserve_amount(market: &Market, asset: MarketAsset, reserve_amount: u64) -> Result<u64> {
    if reserve_amount == 0 {
        return Ok(0);
    }
    let side = market.side(asset);
    require!(
        side.reserves.live_reserve > 0 && side.shares.ylp_supply > 0,
        ErrorCode::InsufficientLiquidity
    );
    side.shares
        .shares_for_deposit(side.reserves.live_reserve, reserve_amount)
}

fn require_hlp_borrow_headroom(market: &Market, borrowed_asset: MarketAsset, amount: u64) -> Result<u128> {
    require_hlp_borrow_headroom_with_cash_floor(market, borrowed_asset, amount, 0)
}

fn require_hlp_borrow_headroom_with_cash_floor(
    market: &Market,
    borrowed_asset: MarketAsset,
    amount: u64,
    cash_floor: u64,
) -> Result<u128> {
    let (current_shares, borrow_index_nad) = match borrowed_asset {
        MarketAsset::Base => (market.quote_hlp_vault.debt_shares, market.debt.base_borrow_index_nad),
        MarketAsset::Quote => (market.base_hlp_vault.debt_shares, market.debt.quote_borrow_index_nad),
    };
    let added_shares = Debt::debt_to_shares(amount, borrow_index_nad)?;
    let projected_shares = current_shares
        .checked_add(added_shares)
        .ok_or(ErrorCode::DebtShareMathOverflow)?;
    let projected_debt = Debt::shares_to_debt(projected_shares, borrow_index_nad)?;
    let spendable_cash = market
        .side(borrowed_asset)
        .reserves
        .cash_reserve
        .saturating_sub(cash_floor);
    require_gte!(
        spendable_cash as u128,
        projected_debt,
        ErrorCode::InsufficientBorrowHeadroom
    );
    Ok(added_shares)
}

pub(crate) fn checkpoint_one_hlp_with_prices(
    market: &mut Market,
    target_asset: MarketAsset,
    prices: HlpCurvePrices,
) -> Result<i128> {
    let valuation = current_hlp_valuation_with_prices(market, target_asset, prices)?;
    let nav = valuation.nav_nad;
    let vault = match target_asset {
        MarketAsset::Base => &mut market.base_hlp_vault,
        MarketAsset::Quote => &mut market.quote_hlp_vault,
    };
    let ideal_delta = recognized_hlp_residual_exposure(valuation.ideal_delta, nav);
    vault.last_nav_nad = nav;
    vault.residual_exposure = ideal_delta;
    // This reference belongs to the last actual hLP settlement/rebalance.
    // Updating it during a generic market checkpoint would make the later
    // settlement-divergence guard compare the current price with itself.
    Ok(ideal_delta)
}

pub(crate) fn checkpoint_hlp_yield_from_ylp(market: &mut Market, target_asset: MarketAsset) -> Result<()> {
    let ylp_shares = match target_asset {
        MarketAsset::Base => market.base_hlp_vault.ylp_shares,
        MarketAsset::Quote => market.quote_hlp_vault.ylp_shares,
    };
    checkpoint_hlp_yield_from_ylp_shares(market, target_asset, ylp_shares)
}

/// Checkpoints both hLP vaults from one carried fee snapshot. The legacy
/// base-then-quote sequence carried the same four side accumulators twice;
/// the first vault checkpoint cannot mutate those side indexes, so the second
/// carry/snapshot was observationally redundant.
fn checkpoint_hlp_yield_from_ylp_pair(
    market: &mut Market,
    checkpoint_base: bool,
    checkpoint_quote: bool,
) -> Result<()> {
    if !checkpoint_base && !checkpoint_quote {
        return Ok(());
    }
    market.base_side.carry_forward_swap_fees()?;
    market.base_side.carry_forward_interest()?;
    market.quote_side.carry_forward_swap_fees()?;
    market.quote_side.carry_forward_interest()?;
    let base_side = market.base_side;
    let quote_side = market.quote_side;
    let base_shares = market.base_hlp_vault.ylp_shares;
    let quote_shares = market.quote_hlp_vault.ylp_shares;
    if checkpoint_base {
        market
            .base_hlp_vault
            .checkpoint_yield_from_ylp_shares(&base_side, &quote_side, base_shares)?;
    }
    if checkpoint_quote {
        market
            .quote_hlp_vault
            .checkpoint_yield_from_ylp_shares(&base_side, &quote_side, quote_shares)?;
    }
    Ok(())
}

pub(crate) fn checkpoint_hlp_yield_from_ylp_shares(
    market: &mut Market,
    target_asset: MarketAsset,
    eligible_ylp_shares: u64,
) -> Result<()> {
    market.base_side.carry_forward_swap_fees()?;
    market.base_side.carry_forward_interest()?;
    market.quote_side.carry_forward_swap_fees()?;
    market.quote_side.carry_forward_interest()?;
    let base_side = market.base_side;
    let quote_side = market.quote_side;
    match target_asset {
        MarketAsset::Base => {
            market
                .base_hlp_vault
                .checkpoint_yield_from_ylp_shares(&base_side, &quote_side, eligible_ylp_shares)
        }
        MarketAsset::Quote => {
            market
                .quote_hlp_vault
                .checkpoint_yield_from_ylp_shares(&base_side, &quote_side, eligible_ylp_shares)
        }
    }
}

fn require_hlp_settlement_available(market: &Market, target_asset: MarketAsset) -> Result<()> {
    let prices = current_hlp_curve_prices(market)?;
    let vault = match target_asset {
        MarketAsset::Base => &market.base_hlp_vault,
        MarketAsset::Quote => &market.quote_hlp_vault,
    };
    if vault.hlp_supply == 0 || vault.cached_settlement_price_nad == 0 {
        return Ok(());
    }
    let current_price = prices.for_asset(target_asset);
    let reference_price = vault.cached_settlement_price_nad;
    let divergence = if current_price >= reference_price {
        current_price
            .checked_sub(reference_price)
            .ok_or(ErrorCode::MarketMathOverflow)?
    } else {
        reference_price
            .checked_sub(current_price)
            .ok_or(ErrorCode::MarketMathOverflow)?
    };
    let max_divergence = reference_price
        .checked_mul(market.config.settlement_divergence_bps as u128)
        .and_then(|value| value.checked_div(crate::constants::BPS_DENOMINATOR as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    require!(divergence <= max_divergence, ErrorCode::HlpSettlementUnavailable);
    Ok(())
}

/// One executable marginal-price evaluation supplies both reciprocal
/// numeraires used by a single hLP accounting snapshot. Re-evaluating the
/// identical curve for every inventory/debt leg is semantically redundant and
/// exhausts Solana's non-freeing 32 KiB program heap on composite swaps.
pub(crate) fn current_hlp_curve_prices(market: &Market) -> Result<HlpCurvePrices> {
    let slot = curve_slot(market);
    let parameters = market.current_curve_parameters(slot);
    let base_price_nad = if parameters.is_cpmm() {
        // hLP checkpointing requests the marginal price several times around
        // predictive and maximum-safe corrections. For CPMM the prepared
        // invariant is irrelevant to that price: it is exactly quote/base.
        // Bypass invariant preparation while retaining the same floor rounding
        // as `ConcentratedPreparedCurve::marginal_price_nad`.
        let reserves = market.curve_reserves_nad()?;
        concentrated_marginal_price_nad(
            reserves.base,
            reserves.quote,
            market.current_curve_center_price_nad()? as u128,
            0,
            0,
        )?
    } else {
        market.curve_marginal_price_nad(slot)? as u128
    };
    hlp_curve_prices_from_base_price_nad(base_price_nad)
}

pub(crate) fn hlp_curve_prices_from_base_price_nad(base_in_quote_nad: u128) -> Result<HlpCurvePrices> {
    require!(base_in_quote_nad > 0, ErrorCode::InvalidSettlementPrice);
    let base_in_quote_nad = u64::try_from(base_in_quote_nad).map_err(|_| ErrorCode::MarketMathOverflow)? as u128;
    let quote_in_base_nad = (NAD as u128)
        .checked_mul(NAD as u128)
        .and_then(|value| value.checked_div(base_in_quote_nad))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let quote_in_base_nad = u64::try_from(quote_in_base_nad).map_err(|_| ErrorCode::MarketMathOverflow)? as u128;
    require!(quote_in_base_nad > 0, ErrorCode::InvalidSettlementPrice);
    Ok(HlpCurvePrices {
        base_in_quote_nad,
        quote_in_base_nad,
    })
}

#[cfg(test)]
fn current_settlement_price_nad(market: &Market, target_asset: MarketAsset) -> Result<u128> {
    Ok(current_hlp_curve_prices(market)?.for_asset(target_asset))
}

fn hlp_nav_nad(market: &Market, target_asset: MarketAsset) -> Result<u128> {
    let vault = match target_asset {
        MarketAsset::Base => &market.base_hlp_vault,
        MarketAsset::Quote => &market.quote_hlp_vault,
    };
    if vault.ylp_shares == 0 && vault.debt_shares == 0 {
        return Ok(0);
    }
    hlp_nav_nad_with_prices(market, target_asset, current_hlp_curve_prices(market)?)
}

fn hlp_nav_nad_with_prices(market: &Market, target_asset: MarketAsset, prices: HlpCurvePrices) -> Result<u128> {
    let values = current_hlp_inventory_values_nad_with_prices(market, target_asset, prices)?;
    let collateral = values
        .target_inventory_value_nad
        .checked_add(values.opposite_inventory_value_nad)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let debt = values.debt_value_nad;
    collateral
        .checked_sub(debt)
        .ok_or_else(|| ErrorCode::Undercollateralized.into())
}

#[cfg(test)]
fn hlp_collateral_value_nad(market: &Market, target_asset: MarketAsset, vault: &HlpVault) -> Result<u128> {
    let base_underlying = ylp_curve_underlying_amount(market, MarketAsset::Base, vault.ylp_shares)?;
    let quote_underlying = ylp_curve_underlying_amount(market, MarketAsset::Quote, vault.ylp_shares)?;
    let base_value = asset_value_in_target_nad(market, MarketAsset::Base, base_underlying, target_asset)?;
    let quote_value = asset_value_in_target_nad(market, MarketAsset::Quote, quote_underlying, target_asset)?;
    base_value
        .checked_add(quote_value)
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

#[cfg(test)]
fn hlp_debt_value_nad(market: &Market, target_asset: MarketAsset) -> Result<u128> {
    let borrowed_asset = target_asset.opposite();
    let debt_amount = hlp_debt_amount(market, target_asset)?;
    asset_value_in_target_nad(market, borrowed_asset, debt_amount, target_asset)
}

fn hlp_debt_amount(market: &Market, target_asset: MarketAsset) -> Result<u64> {
    let debt_amount = match target_asset {
        MarketAsset::Base => {
            Debt::shares_to_debt(market.base_hlp_vault.debt_shares, market.debt.quote_borrow_index_nad)?
        }
        MarketAsset::Quote => {
            Debt::shares_to_debt(market.quote_hlp_vault.debt_shares, market.debt.base_borrow_index_nad)?
        }
    };
    u64::try_from(debt_amount).map_err(|_| ErrorCode::DebtMathOverflow.into())
}

fn ylp_curve_underlying_amount(market: &Market, asset: MarketAsset, ylp_amount: u64) -> Result<u64> {
    let side = market.side(asset);
    if ylp_amount == 0 || side.shares.ylp_supply == 0 {
        return Ok(0);
    }
    let reserve_amount = (ylp_amount as u128)
        .checked_mul(market.curve_reserve(asset)? as u128)
        .and_then(|value| value.checked_div(side.shares.ylp_supply as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    u64::try_from(reserve_amount).map_err(|_| ErrorCode::MarketMathOverflow.into())
}

fn ylp_live_underlying_amount(market: &Market, asset: MarketAsset, ylp_amount: u64) -> Result<u64> {
    let side = market.side(asset);
    ylp_live_underlying_amount_from_values(side.reserves.live_reserve, side.shares.ylp_supply, ylp_amount)
}

#[cfg(test)]
fn asset_value_in_target_nad(
    market: &Market,
    asset: MarketAsset,
    amount: u64,
    target_asset: MarketAsset,
) -> Result<u128> {
    if amount == 0 {
        return Ok(0);
    }
    if asset == target_asset {
        return normalize_to_nad(amount as u128, market.side(asset).asset_decimals);
    }
    asset_value_in_target_nad_with_prices(market, current_hlp_curve_prices(market)?, asset, amount, target_asset)
}

fn asset_value_in_target_nad_with_prices(
    market: &Market,
    prices: HlpCurvePrices,
    asset: MarketAsset,
    amount: u64,
    target_asset: MarketAsset,
) -> Result<u128> {
    if amount == 0 {
        return Ok(0);
    }
    let amount_nad = normalize_to_nad(amount as u128, market.side(asset).asset_decimals)?;
    if asset == target_asset {
        return Ok(amount_nad);
    }
    let price_nad = prices.for_asset(asset);
    mul_div_u128(amount_nad, price_nad, NAD as u128)
}

fn current_hlp_inventory_values_nad_with_prices(
    market: &Market,
    target_asset: MarketAsset,
    prices: HlpCurvePrices,
) -> Result<HlpInventoryValuesNad> {
    let vault = match target_asset {
        MarketAsset::Base => &market.base_hlp_vault,
        MarketAsset::Quote => &market.quote_hlp_vault,
    };
    let opposite_asset = target_asset.opposite();
    let target_underlying = ylp_curve_underlying_amount(market, target_asset, vault.ylp_shares)?;
    let opposite_underlying = ylp_curve_underlying_amount(market, opposite_asset, vault.ylp_shares)?;
    let debt_amount = u64::try_from(match target_asset {
        MarketAsset::Base => {
            Debt::shares_to_debt(market.base_hlp_vault.debt_shares, market.debt.quote_borrow_index_nad)?
        }
        MarketAsset::Quote => {
            Debt::shares_to_debt(market.quote_hlp_vault.debt_shares, market.debt.base_borrow_index_nad)?
        }
    })
    .map_err(|_| ErrorCode::DebtMathOverflow)?;
    Ok(HlpInventoryValuesNad {
        target_inventory_value_nad: asset_value_in_target_nad_with_prices(
            market,
            prices,
            target_asset,
            target_underlying,
            target_asset,
        )?,
        opposite_inventory_value_nad: asset_value_in_target_nad_with_prices(
            market,
            prices,
            opposite_asset,
            opposite_underlying,
            target_asset,
        )?,
        debt_value_nad: asset_value_in_target_nad_with_prices(
            market,
            prices,
            opposite_asset,
            debt_amount,
            target_asset,
        )?,
    })
}

/// Values both vaults from one immutable reserve/supply snapshot. This keeps
/// the two numeraires on the same curve state while avoiding four repeated
/// curve-reserve derivations in the joint lifecycle hot path.
#[cfg(test)]
fn hlp_planner_asset_value_in_target_nad(
    fixed: HlpPlannerStatic,
    prices: HlpCurvePrices,
    asset: MarketAsset,
    amount: u64,
    target_asset: MarketAsset,
) -> Result<u128> {
    if amount == 0 {
        return Ok(0);
    }
    let amount_nad = normalize_to_nad(amount as u128, fixed.decimals(asset))?;
    if asset == target_asset {
        return Ok(amount_nad);
    }
    mul_div_u128(amount_nad, prices.for_asset(asset), NAD as u128)
}

/// Fused compact-state inventory snapshot. Curve reserves, yLP claims, and
/// indexed hLP debts are each derived once and then valued in both numeraires.
#[cfg(test)]
fn hlp_planner_inventory_values_pair_nad_with_prices(
    fixed: HlpPlannerStatic,
    state: HlpPlannerState,
    prices: HlpCurvePrices,
    base_active: bool,
    quote_active: bool,
) -> Result<(HlpInventoryValuesNad, HlpInventoryValuesNad)> {
    let base_curve = state.curve_reserve(fixed, MarketAsset::Base)?;
    let quote_curve = state.curve_reserve(fixed, MarketAsset::Quote)?;
    hlp_planner_inventory_values_pair_for_curve_with_prices(
        fixed,
        state,
        base_curve,
        quote_curve,
        prices,
        base_active,
        quote_active,
    )
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn hlp_planner_inventory_values_pair_for_curve_with_prices(
    fixed: HlpPlannerStatic,
    state: HlpPlannerState,
    base_curve: u64,
    quote_curve: u64,
    prices: HlpCurvePrices,
    base_active: bool,
    quote_active: bool,
) -> Result<(HlpInventoryValuesNad, HlpInventoryValuesNad)> {
    let base_supply = state.base_side.ylp_supply;
    let quote_supply = state.quote_side.ylp_supply;
    require_eq!(base_supply, quote_supply, ErrorCode::BrokenInvariant);

    let claim = |reserve: u64, shares: u64, supply: u64| -> Result<u64> {
        if shares == 0 || supply == 0 {
            return Ok(0);
        }
        u64::try_from(mul_div_u128(reserve as u128, shares as u128, supply as u128)?)
            .map_err(|_| ErrorCode::MarketMathOverflow.into())
    };

    let base_values = if base_active {
        let shares = state.base_vault.ylp_shares;
        let base_claim = claim(base_curve, shares, base_supply)?;
        let quote_claim = claim(quote_curve, shares, quote_supply)?;
        let quote_debt = u64::try_from(Debt::shares_to_debt(
            state.base_vault.debt_shares,
            fixed.quote_borrow_index_nad,
        )?)
        .map_err(|_| ErrorCode::DebtMathOverflow)?;
        HlpInventoryValuesNad {
            target_inventory_value_nad: hlp_planner_asset_value_in_target_nad(
                fixed,
                prices,
                MarketAsset::Base,
                base_claim,
                MarketAsset::Base,
            )?,
            opposite_inventory_value_nad: hlp_planner_asset_value_in_target_nad(
                fixed,
                prices,
                MarketAsset::Quote,
                quote_claim,
                MarketAsset::Base,
            )?,
            debt_value_nad: hlp_planner_asset_value_in_target_nad(
                fixed,
                prices,
                MarketAsset::Quote,
                quote_debt,
                MarketAsset::Base,
            )?,
        }
    } else {
        HlpInventoryValuesNad::default()
    };

    let quote_values = if quote_active {
        let shares = state.quote_vault.ylp_shares;
        let base_claim = claim(base_curve, shares, base_supply)?;
        let quote_claim = claim(quote_curve, shares, quote_supply)?;
        let base_debt = u64::try_from(Debt::shares_to_debt(
            state.quote_vault.debt_shares,
            fixed.base_borrow_index_nad,
        )?)
        .map_err(|_| ErrorCode::DebtMathOverflow)?;
        HlpInventoryValuesNad {
            target_inventory_value_nad: hlp_planner_asset_value_in_target_nad(
                fixed,
                prices,
                MarketAsset::Quote,
                quote_claim,
                MarketAsset::Quote,
            )?,
            opposite_inventory_value_nad: hlp_planner_asset_value_in_target_nad(
                fixed,
                prices,
                MarketAsset::Base,
                base_claim,
                MarketAsset::Quote,
            )?,
            debt_value_nad: hlp_planner_asset_value_in_target_nad(
                fixed,
                prices,
                MarketAsset::Base,
                base_debt,
                MarketAsset::Quote,
            )?,
        }
    } else {
        HlpInventoryValuesNad::default()
    };
    Ok((base_values, quote_values))
}

#[cfg(test)]
fn hlp_planner_raw_amount_from_target_value_nad(
    fixed: HlpPlannerStatic,
    prices: HlpCurvePrices,
    asset: MarketAsset,
    target_asset: MarketAsset,
    value_nad: u128,
) -> Result<u64> {
    if value_nad == 0 {
        return Ok(0);
    }
    let amount_nad = if asset == target_asset {
        value_nad
    } else {
        let price_nad = prices.for_asset(asset);
        require!(price_nad > 0, ErrorCode::InvalidSettlementPrice);
        mul_div_u128(value_nad, NAD as u128, price_nad)?
    };
    denormalize_from_nad_floor(amount_nad, fixed.decimals(asset))
}

#[cfg(test)]
fn hlp_planner_proportional_rebalance_amounts(
    fixed: HlpPlannerStatic,
    target_asset: MarketAsset,
    total_value_delta_nad: i128,
    valuation: HlpValuation,
) -> Result<ProportionalRebalanceAmounts> {
    if total_value_delta_nad == 0 {
        return Ok(ProportionalRebalanceAmounts::default());
    }
    let collateral_value = valuation
        .values
        .target_inventory_value_nad
        .checked_add(valuation.values.opposite_inventory_value_nad)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    require!(collateral_value > 0, ErrorCode::DenominatorOverflow);
    let total_value_delta = total_value_delta_nad.unsigned_abs();
    let target_value_delta = mul_div_u128(
        total_value_delta,
        valuation.values.target_inventory_value_nad,
        collateral_value,
    )?;
    let borrowed_value_delta = total_value_delta
        .checked_sub(target_value_delta)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let borrowed_asset = target_asset.opposite();
    Ok(ProportionalRebalanceAmounts {
        target_leg_amount: hlp_planner_raw_amount_from_target_value_nad(
            fixed,
            valuation.prices,
            target_asset,
            target_asset,
            target_value_delta,
        )?,
        borrowed_leg_amount: hlp_planner_raw_amount_from_target_value_nad(
            fixed,
            valuation.prices,
            borrowed_asset,
            target_asset,
            borrowed_value_delta,
        )?,
        debt_amount: hlp_planner_raw_amount_from_target_value_nad(
            fixed,
            valuation.prices,
            borrowed_asset,
            target_asset,
            total_value_delta,
        )?,
    })
}

#[cfg(test)]
fn hlp_planner_plan_common(
    fixed: HlpPlannerStatic,
    state: HlpPlannerState,
    target_asset: MarketAsset,
    ideal_delta_nad: i128,
    nav_nad: u128,
    capacity_bound: bool,
) -> HlpRebalancePlanCommon {
    HlpRebalancePlanCommon {
        start: capture_hlp_rebalance_state_from_planner(fixed, state, target_asset),
        target_asset,
        ideal_delta_nad,
        nav_nad,
        capacity_bound,
        current_swap_fee_eligible_ylp_shares: 0,
    }
}

#[cfg(test)]
fn hlp_planner_noop(
    fixed: HlpPlannerStatic,
    state: HlpPlannerState,
    target_asset: MarketAsset,
    ideal_delta_nad: i128,
    valuation: HlpValuation,
    capacity_bound: bool,
    reason: HlpRebalanceNoopReason,
) -> HlpCompactRebalancePlan {
    HlpCompactRebalancePlan {
        plan: HlpRebalancePlan::Noop {
            common: hlp_planner_plan_common(
                fixed,
                state,
                target_asset,
                ideal_delta_nad,
                valuation.nav_nad,
                capacity_bound,
            ),
            reason,
        },
        guidance_settlement: None,
    }
}

#[cfg(test)]
fn hlp_planner_funding_headroom(
    fixed: HlpPlannerStatic,
    state: HlpPlannerState,
    borrowed_asset: MarketAsset,
) -> Result<u64> {
    let debt_shares = state.vault(borrowed_asset.opposite()).debt_shares;
    let borrow_index_nad = fixed.borrow_index(borrowed_asset);
    let cash = state.side(borrowed_asset).cash_reserve as u128;
    let max_total_shares = crate::math::mul_div_ceil_u128(
        cash.checked_add(1).ok_or(ErrorCode::MarketMathOverflow)?,
        NAD as u128,
        borrow_index_nad,
    )?
    .checked_sub(1)
    .ok_or(ErrorCode::MarketMathOverflow)?;
    let available_shares = max_total_shares.saturating_sub(debt_shares);
    let headroom = mul_div_u128(available_shares, borrow_index_nad, NAD as u128)?;
    u64::try_from(headroom).map_err(|_| ErrorCode::MarketMathOverflow.into())
}

#[cfg(test)]
fn hlp_planner_require_borrow_headroom(
    fixed: HlpPlannerStatic,
    state: HlpPlannerState,
    borrowed_asset: MarketAsset,
    amount: u64,
) -> Result<u128> {
    let current_shares = state.vault(borrowed_asset.opposite()).debt_shares;
    let borrow_index_nad = fixed.borrow_index(borrowed_asset);
    let added_shares = Debt::debt_to_shares(amount, borrow_index_nad)?;
    let projected_shares = current_shares
        .checked_add(added_shares)
        .ok_or(ErrorCode::DebtShareMathOverflow)?;
    let projected_debt = Debt::shares_to_debt(projected_shares, borrow_index_nad)?;
    require_gte!(
        state.side(borrowed_asset).cash_reserve as u128,
        projected_debt,
        ErrorCode::InsufficientBorrowHeadroom
    );
    Ok(added_shares)
}

#[cfg(test)]
fn plan_compact_hlp_leverage_up(
    fixed: HlpPlannerStatic,
    state: HlpPlannerState,
    target_asset: MarketAsset,
    ideal_delta: i128,
    valuation: HlpValuation,
) -> Result<HlpCompactRebalancePlan> {
    let borrowed_asset = target_asset.opposite();
    let borrow_headroom = hlp_planner_funding_headroom(fixed, state, borrowed_asset)?;
    let feasible_delta_nad = if borrow_headroom == 0 {
        0
    } else {
        ideal_delta.unsigned_abs().min(hlp_planner_asset_value_in_target_nad(
            fixed,
            valuation.prices,
            borrowed_asset,
            borrow_headroom,
            target_asset,
        )?)
    };
    let capacity_bound = feasible_delta_nad < ideal_delta.unsigned_abs();
    let feasible_delta = i128::try_from(feasible_delta_nad).map_err(|_| ErrorCode::MarketMathOverflow)?;
    let amounts = hlp_planner_proportional_rebalance_amounts(fixed, target_asset, feasible_delta, valuation)?;
    if !complete_rebalance_amounts(amounts) {
        return Ok(hlp_planner_noop(
            fixed,
            state,
            target_asset,
            ideal_delta,
            valuation,
            true,
            HlpRebalanceNoopReason::CapacityOrGranularity,
        ));
    }
    let debt_shares_added =
        hlp_planner_require_borrow_headroom(fixed, state, borrowed_asset, amounts.debt_amount)?;
    let (base_leg_amount, quote_leg_amount) = match target_asset {
        MarketAsset::Base => (amounts.target_leg_amount, amounts.borrowed_leg_amount),
        MarketAsset::Quote => (amounts.borrowed_leg_amount, amounts.target_leg_amount),
    };
    let start = capture_hlp_rebalance_state_from_planner(fixed, state, target_asset);
    let ylp_mint_amount = ylp_for_live_reserve_deposit_from_state(start, base_leg_amount, quote_leg_amount)?;
    if ylp_mint_amount == 0 {
        return Ok(hlp_planner_noop(
            fixed,
            state,
            target_asset,
            ideal_delta,
            valuation,
            true,
            HlpRebalanceNoopReason::CapacityOrGranularity,
        ));
    }
    Ok(HlpCompactRebalancePlan {
        plan: HlpRebalancePlan::LeverageUp {
            common: hlp_planner_plan_common(
                fixed,
                state,
                target_asset,
                ideal_delta,
                valuation.nav_nad,
                capacity_bound,
            ),
            base_leg_amount,
            quote_leg_amount,
            ylp_mint_amount,
            debt_shares_added,
            debt_principal_added: amounts.debt_amount,
        },
        guidance_settlement: None,
    })
}

#[cfg(test)]
fn hlp_planner_deleverage_repay_amount_for_legs(
    fixed: HlpPlannerStatic,
    target_asset: MarketAsset,
    target_leg: u64,
    borrowed_leg: u64,
    prices: HlpCurvePrices,
    current_debt_raw_cap: u64,
) -> Result<u64> {
    let borrowed_asset = target_asset.opposite();
    let removed_value_nad = hlp_planner_asset_value_in_target_nad(
        fixed,
        prices,
        target_asset,
        target_leg,
        target_asset,
    )?
    .checked_add(hlp_planner_asset_value_in_target_nad(
        fixed,
        prices,
        borrowed_asset,
        borrowed_leg,
        target_asset,
    )?)
    .ok_or(ErrorCode::MarketMathOverflow)?;
    Ok(hlp_planner_raw_amount_from_target_value_nad(
        fixed,
        prices,
        borrowed_asset,
        target_asset,
        removed_value_nad,
    )?
    .min(current_debt_raw_cap))
}

#[cfg(test)]
fn hlp_planner_deleverage_burn_facts_if_executable(
    fixed: HlpPlannerStatic,
    state: HlpPlannerState,
    target_asset: MarketAsset,
    ylp_burn: u64,
    valuation: HlpValuation,
) -> Result<Option<HlpDeleverageBurnFacts>> {
    let start = capture_hlp_rebalance_state_from_planner(fixed, state, target_asset);
    let base_leg = ylp_live_underlying_amount_from_state(start, MarketAsset::Base, ylp_burn)?;
    let quote_leg = ylp_live_underlying_amount_from_state(start, MarketAsset::Quote, ylp_burn)?;
    let (target_leg, borrowed_leg) = match target_asset {
        MarketAsset::Base => (base_leg, quote_leg),
        MarketAsset::Quote => (quote_leg, base_leg),
    };
    let vault = hlp_vault_from_rebalance_state(start);
    let borrow_index = fixed.borrow_index(target_asset.opposite());
    let current_debt = Debt::shares_to_debt(vault.debt_shares, borrow_index)?;
    let repay_amount = hlp_planner_deleverage_repay_amount_for_legs(
        fixed,
        target_asset,
        target_leg,
        borrowed_leg,
        valuation.prices,
        u64::try_from(current_debt).unwrap_or(u64::MAX),
    )?;
    if repay_amount == 0 {
        return Ok(None);
    }
    let minimum_repay = current_debt
        .checked_sub(Debt::shares_to_debt(vault.debt_shares.saturating_sub(1), borrow_index)?)
        .ok_or(ErrorCode::DebtMathOverflow)?;
    if (repay_amount as u128) < minimum_repay {
        return Ok(None);
    }
    let debt_repayment = vault.repayment_for_max(repay_amount, borrow_index)?;
    let principal = u128::from(vault.debt_principal).min(current_debt);
    let (_, interest_paid) = crate::math::realized_interest_split(
        debt_repayment.position_debt_reduced,
        current_debt,
        principal,
    )?;
    Ok(Some(HlpDeleverageBurnFacts {
        ylp_burn_amount: ylp_burn,
        base_leg_amount: base_leg,
        quote_leg_amount: quote_leg,
        repay_amount,
        current_debt,
        minimum_executable_repay: minimum_repay,
        debt_repayment,
        interest_paid,
    }))
}

#[cfg(test)]
fn hlp_planner_deleverage_cash_capacity_for_burn(
    fixed: HlpPlannerStatic,
    state: HlpPlannerState,
    target_asset: MarketAsset,
    ylp_burn: u64,
    valuation: HlpValuation,
) -> Result<Option<(HlpDeleverageBurnFacts, bool)>> {
    let Some(facts) =
        hlp_planner_deleverage_burn_facts_if_executable(fixed, state, target_asset, ylp_burn, valuation)?
    else {
        return Ok(None);
    };
    let borrowed_asset = target_asset.opposite();
    let vault = state.vault(target_asset);
    let target_cash_needed = facts
        .leg_amount(target_asset)
        .saturating_sub(match target_asset {
            MarketAsset::Base => vault.base_hlp_live_reserve,
            MarketAsset::Quote => vault.quote_hlp_live_reserve,
        });
    let borrowed_cash_needed = facts
        .leg_amount(borrowed_asset)
        .saturating_sub(match borrowed_asset {
            MarketAsset::Base => vault.base_hlp_live_reserve,
            MarketAsset::Quote => vault.quote_hlp_live_reserve,
        })
        .max(facts.interest_paid);
    Ok(Some((
        facts,
        target_cash_needed <= state.side(target_asset).cash_reserve
            && borrowed_cash_needed <= state.side(borrowed_asset).cash_reserve,
    )))
}

#[cfg(test)]
fn cap_hlp_planner_deleverage_ylp_burn(
    fixed: HlpPlannerStatic,
    state: HlpPlannerState,
    target_asset: MarketAsset,
    desired_burn: u64,
    valuation: HlpValuation,
) -> Result<HlpDeleverageBurnFacts> {
    if desired_burn == 0 {
        return Ok(HlpDeleverageBurnFacts::default());
    }
    let Some((desired, available)) =
        hlp_planner_deleverage_cash_capacity_for_burn(fixed, state, target_asset, desired_burn, valuation)?
    else {
        return Ok(HlpDeleverageBurnFacts::default());
    };
    if available {
        return Ok(desired);
    }
    let start = capture_hlp_rebalance_state_from_planner(fixed, state, target_asset);
    let vault = hlp_vault_from_rebalance_state(start);
    let borrowed_asset = target_asset.opposite();
    let borrowed_cash = state.side(borrowed_asset).cash_reserve;
    let Some(maximum_interest_safe_repay) = maximum_hlp_interest_safe_repay_input(
        &vault,
        fixed.borrow_index(borrowed_asset),
        desired.current_debt,
        desired.minimum_executable_repay,
        borrowed_cash,
    )?
    else {
        return Ok(HlpDeleverageBurnFacts::default());
    };
    let target_hlp_available = match target_asset {
        MarketAsset::Base => vault.base_hlp_live_reserve,
        MarketAsset::Quote => vault.quote_hlp_live_reserve,
    };
    let borrowed_hlp_available = match borrowed_asset {
        MarketAsset::Base => vault.base_hlp_live_reserve,
        MarketAsset::Quote => vault.quote_hlp_live_reserve,
    };
    let capacity = HlpDeleverageBurnCapacity {
        target_asset,
        target_live_reserve: state.side(target_asset).live_reserve,
        target_ylp_supply: state.side(target_asset).ylp_supply,
        borrowed_live_reserve: state.side(borrowed_asset).live_reserve,
        borrowed_ylp_supply: state.side(borrowed_asset).ylp_supply,
        target_leg_cap: target_hlp_available.saturating_add(state.side(target_asset).cash_reserve),
        borrowed_leg_cap: borrowed_hlp_available.saturating_add(borrowed_cash),
        current_debt_raw_cap: u64::try_from(desired.current_debt).unwrap_or(u64::MAX),
        maximum_interest_safe_repay,
    };
    let mut upper = capacity.direct_entitlement_cap(desired_burn)?;
    if capacity.maximum_interest_safe_repay < capacity.current_debt_raw_cap {
        let mut low = 0_u64;
        while low < upper {
            let probe = low + (upper - low + 1) / 2;
            let target_leg = ylp_live_underlying_amount_from_values(
                capacity.target_live_reserve,
                capacity.target_ylp_supply,
                probe,
            )?;
            let borrowed_leg = ylp_live_underlying_amount_from_values(
                capacity.borrowed_live_reserve,
                capacity.borrowed_ylp_supply,
                probe,
            )?;
            if hlp_planner_deleverage_repay_amount_for_legs(
                fixed,
                target_asset,
                target_leg,
                borrowed_leg,
                valuation.prices,
                capacity.current_debt_raw_cap,
            )? <= capacity.maximum_interest_safe_repay
            {
                low = probe;
            } else {
                upper = probe - 1;
            }
        }
        upper = low;
    }
    if upper == 0 {
        return Ok(HlpDeleverageBurnFacts::default());
    }
    let Some((selected, selected_available)) =
        hlp_planner_deleverage_cash_capacity_for_burn(fixed, state, target_asset, upper, valuation)?
    else {
        return Ok(HlpDeleverageBurnFacts::default());
    };
    require!(selected_available, ErrorCode::BrokenInvariant);
    require!(upper < desired_burn, ErrorCode::BrokenInvariant);
    let adjacent = upper.checked_add(1).ok_or(ErrorCode::MarketMathOverflow)?;
    let Some((_, adjacent_available)) =
        hlp_planner_deleverage_cash_capacity_for_burn(fixed, state, target_asset, adjacent, valuation)?
    else {
        return err!(ErrorCode::BrokenInvariant);
    };
    require!(!adjacent_available, ErrorCode::BrokenInvariant);
    Ok(selected)
}

#[cfg(test)]
fn plan_compact_hlp_deleverage(
    fixed: HlpPlannerStatic,
    state: HlpPlannerState,
    target_asset: MarketAsset,
    ideal_delta: i128,
    valuation: HlpValuation,
    settlement_anchor: ConcentratedGuidanceCurve,
    settlement_anchor_supply: u64,
    settlement_mode: HlpGuidanceSettlementProbeMode,
) -> Result<HlpCompactRebalancePlan> {
    let start = capture_hlp_rebalance_state_from_planner(fixed, state, target_asset);
    let collateral_value_nad = valuation
        .values
        .target_inventory_value_nad
        .checked_add(valuation.values.opposite_inventory_value_nad)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let feasible_delta_nad = ideal_delta
        .unsigned_abs()
        .min(collateral_value_nad)
        .min(valuation.values.debt_value_nad);
    let mut capacity_bound = feasible_delta_nad < ideal_delta.unsigned_abs();
    let feasible_delta = -i128::try_from(feasible_delta_nad).map_err(|_| ErrorCode::MarketMathOverflow)?;
    let amounts = hlp_planner_proportional_rebalance_amounts(fixed, target_asset, feasible_delta, valuation)?;
    if !complete_rebalance_amounts(amounts) {
        return Ok(hlp_planner_noop(
            fixed,
            state,
            target_asset,
            ideal_delta,
            valuation,
            true,
            HlpRebalanceNoopReason::CapacityOrGranularity,
        ));
    }
    let (base_leg, quote_leg) = match target_asset {
        MarketAsset::Base => (amounts.target_leg_amount, amounts.borrowed_leg_amount),
        MarketAsset::Quote => (amounts.borrowed_leg_amount, amounts.target_leg_amount),
    };
    let base_burn = mul_div_u128(
        base_leg as u128,
        start.base_ylp_supply as u128,
        start.base_live_reserve as u128,
    )?;
    let quote_burn = mul_div_u128(
        quote_leg as u128,
        start.quote_ylp_supply as u128,
        start.quote_live_reserve as u128,
    )?;
    let desired_burn = u64::try_from(base_burn.min(quote_burn))
        .map_err(|_| ErrorCode::MarketMathOverflow)?
        .min(start.target_ylp_shares);
    let burn = cap_hlp_planner_deleverage_ylp_burn(
        fixed,
        state,
        target_asset,
        desired_burn,
        valuation,
    )?;
    capacity_bound |= burn.ylp_burn_amount < desired_burn;
    if burn.ylp_burn_amount == 0 {
        return Ok(hlp_planner_noop(
            fixed,
            state,
            target_asset,
            ideal_delta,
            valuation,
            true,
            HlpRebalanceNoopReason::CapacityOrGranularity,
        ));
    }
    let borrowed_asset = target_asset.opposite();
    let (target_entitlement, borrowed_entitlement) = match target_asset {
        MarketAsset::Base => (burn.base_leg_amount, burn.quote_leg_amount),
        MarketAsset::Quote => (burn.quote_leg_amount, burn.base_leg_amount),
    };
    let post_supply = start
        .base_ylp_supply
        .checked_sub(burn.ylp_burn_amount)
        .ok_or(ErrorCode::SupplyUnderflow)?;
    let (target_reserve_debit, guidance_settlement) = if burn.interest_paid > borrowed_entitlement {
        let (target_amount, proof) = HlpGuidanceSettlementProof::plan(
            fixed,
            state,
            settlement_anchor,
            settlement_anchor_supply,
            target_asset,
            target_entitlement,
            borrowed_entitlement,
            burn.interest_paid,
            post_supply,
            settlement_mode,
        )?;
        (target_amount, Some(proof))
    } else {
        (target_entitlement, None)
    };
    let borrowed_reserve_debit = if guidance_settlement.is_some() {
        burn.interest_paid
    } else {
        borrowed_entitlement
    };
    let (base_reserve_debit, quote_reserve_debit) = match target_asset {
        MarketAsset::Base => (target_reserve_debit, borrowed_reserve_debit),
        MarketAsset::Quote => (borrowed_reserve_debit, target_reserve_debit),
    };
    let vault = state.vault(target_asset);
    let hlp_available = |asset| match asset {
        MarketAsset::Base => vault.base_hlp_live_reserve,
        MarketAsset::Quote => vault.quote_hlp_live_reserve,
    };
    let base_interest = if borrowed_asset == MarketAsset::Base {
        burn.interest_paid
    } else {
        0
    };
    let quote_interest = if borrowed_asset == MarketAsset::Quote {
        burn.interest_paid
    } else {
        0
    };
    require_gte!(base_reserve_debit, base_interest, ErrorCode::HlpSettlementUnavailable);
    require_gte!(quote_reserve_debit, quote_interest, ErrorCode::HlpSettlementUnavailable);
    let base_cash_debit = base_interest.max(base_reserve_debit.saturating_sub(hlp_available(MarketAsset::Base)));
    let quote_cash_debit = quote_interest.max(quote_reserve_debit.saturating_sub(hlp_available(MarketAsset::Quote)));
    let mut planned_vault = hlp_vault_from_rebalance_state(start);
    let debt_clearance = planned_vault.clear_debt_repay(
        burn.debt_repayment.shares_to_burn,
        fixed.borrow_index(borrowed_asset),
    )?;
    require_eq!(debt_clearance.interest_paid, burn.interest_paid, ErrorCode::BrokenInvariant);
    planned_vault.debit_ylp(burn.ylp_burn_amount)?;
    Ok(HlpCompactRebalancePlan {
        plan: HlpRebalancePlan::Deleverage {
            common: hlp_planner_plan_common(
                fixed,
                state,
                target_asset,
                ideal_delta,
                valuation.nav_nad,
                capacity_bound,
            ),
            ylp_burn_amount: burn.ylp_burn_amount,
            base_entitlement_amount: burn.base_leg_amount,
            quote_entitlement_amount: burn.quote_leg_amount,
            base_reserve_debit,
            quote_reserve_debit,
            base_cash_debit,
            quote_cash_debit,
            debt_repayment: burn.debt_repayment,
            debt_clearance,
            interest_paid: burn.interest_paid,
            exact_out_checkpoint: None,
        },
        guidance_settlement,
    })
}

#[cfg(test)]
fn plan_compact_hlp_rebalance_from_valuation(
    fixed: HlpPlannerStatic,
    state: HlpPlannerState,
    target_asset: MarketAsset,
    valuation: HlpValuation,
    settlement_anchor: ConcentratedGuidanceCurve,
    settlement_anchor_supply: u64,
    settlement_mode: HlpGuidanceSettlementProbeMode,
) -> Result<HlpCompactRebalancePlan> {
    let ideal_delta = recognized_hlp_residual_exposure(valuation.ideal_delta, valuation.nav_nad);
    if !valuation.proportional_hedge_available && ideal_delta != 0 {
        Ok(hlp_planner_noop(
            fixed,
            state,
            target_asset,
            ideal_delta,
            valuation,
            false,
            HlpRebalanceNoopReason::Unhedgeable,
        ))
    } else if ideal_delta > 0 {
        plan_compact_hlp_leverage_up(fixed, state, target_asset, ideal_delta, valuation)
    } else if ideal_delta < 0 {
        plan_compact_hlp_deleverage(
            fixed,
            state,
            target_asset,
            ideal_delta,
            valuation,
            settlement_anchor,
            settlement_anchor_supply,
            settlement_mode,
        )
    } else {
        Ok(hlp_planner_noop(
            fixed,
            state,
            target_asset,
            0,
            valuation,
            false,
            HlpRebalanceNoopReason::Settled,
        ))
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HlpCompactPostRebalance {
    state: HlpPlannerState,
    base_receipt: HlpRebalanceReceipt,
    quote_receipt: HlpRebalanceReceipt,
    final_prices: HlpCurvePrices,
    base_endpoint: HlpLifecycleEndpoint,
    quote_endpoint: HlpLifecycleEndpoint,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HlpCompactLifecycleResult {
    state: HlpPlannerState,
    tracking: HlpLifecycleTracking,
    base_post_receipt: HlpRebalanceReceipt,
    quote_post_receipt: HlpRebalanceReceipt,
    transition: crate::market::LeverageLifecycleTransition,
}

#[cfg(test)]
fn hlp_planner_signed_asset_value_in_target_nad(
    fixed: HlpPlannerStatic,
    prices: HlpCurvePrices,
    asset: MarketAsset,
    amount: i128,
    target_asset: MarketAsset,
) -> Result<i128> {
    let magnitude = u64::try_from(amount.unsigned_abs()).map_err(|_| ErrorCode::MarketMathOverflow)?;
    let value = i128::try_from(hlp_planner_asset_value_in_target_nad(
        fixed,
        prices,
        asset,
        magnitude,
        target_asset,
    )?)
    .map_err(|_| ErrorCode::MarketMathOverflow)?;
    if amount < 0 {
        value.checked_neg().ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    } else {
        Ok(value)
    }
}

#[cfg(test)]
fn hlp_planner_frozen_interest_claim_delta_value_nad(
    fixed: HlpPlannerStatic,
    state: HlpPlannerState,
    target_asset: MarketAsset,
    prices: HlpCurvePrices,
    tracking: HlpTrackingReference,
) -> Result<i128> {
    let vault = state.vault(target_asset);
    let final_ylp_supply = state.base_side.ylp_supply;
    require_eq!(
        final_ylp_supply,
        state.quote_side.ylp_supply,
        ErrorCode::BrokenInvariant
    );
    let (base_public_claim, quote_public_claim) = hlp_interest_claims_for_shares(
        tracking.base_unrealized_interest,
        tracking.quote_unrealized_interest,
        vault.ylp_shares,
        final_ylp_supply,
    )?;
    let (start_base_public_claim, start_quote_public_claim) = hlp_interest_claims_for_shares(
        tracking.base_unrealized_interest,
        tracking.quote_unrealized_interest,
        tracking.start_ylp_shares,
        tracking.start_ylp_supply,
    )?;
    let base_delta = i128::from(base_public_claim)
        .checked_sub(i128::from(start_base_public_claim))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let quote_delta = i128::from(quote_public_claim)
        .checked_sub(i128::from(start_quote_public_claim))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    hlp_planner_signed_asset_value_in_target_nad(
        fixed,
        prices,
        MarketAsset::Base,
        base_delta,
        target_asset,
    )?
    .checked_add(hlp_planner_signed_asset_value_in_target_nad(
        fixed,
        prices,
        MarketAsset::Quote,
        quote_delta,
        target_asset,
    )?)
    .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

#[cfg(test)]
fn hlp_planner_tracking_deltas_nad(
    fixed: HlpPlannerStatic,
    state: HlpPlannerState,
    target_asset: MarketAsset,
    prices: HlpCurvePrices,
    final_principal_nav_nad: i128,
    tracking: HlpTrackingReference,
) -> Result<(i128, i128, i128)> {
    let principal_delta = final_principal_nav_nad
        .checked_sub(tracking.principal_nav_nad)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let claim_delta = hlp_planner_frozen_interest_claim_delta_value_nad(
        fixed,
        state,
        target_asset,
        prices,
        tracking,
    )?;
    let combined_delta = principal_delta
        .checked_add(claim_delta)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    Ok((principal_delta, claim_delta, combined_delta))
}

#[cfg(test)]
fn hlp_planner_tracking_from_endpoint(
    fixed: HlpPlannerStatic,
    state: HlpPlannerState,
    target_asset: MarketAsset,
    prices: HlpCurvePrices,
    endpoint: HlpLifecycleEndpoint,
    start: ConcentratedHlpStart,
) -> Result<(i128, i128, i128, i128)> {
    if !start.active {
        return Ok((0, 0, 0, 0));
    }
    let (principal, interest, combined) = hlp_planner_tracking_deltas_nad(
        fixed,
        state,
        target_asset,
        prices,
        endpoint.principal_nav_nad,
        start.tracking,
    )?;
    Ok((principal, interest, combined, endpoint.opposite_exposure_nad))
}

#[cfg(test)]
fn hlp_planner_tracking_start_after_transition(
    fixed: HlpPlannerStatic,
    state: HlpPlannerState,
    mut start: ConcentratedHlpStart,
    transition: crate::market::LeverageLifecycleTransition,
    removed_interest_asset: Option<MarketAsset>,
    socialized_nav_delta_nad: i128,
) -> Result<ConcentratedHlpStart> {
    if !start.active {
        return Ok(start);
    }
    if let (Some(asset), amount) = (removed_interest_asset, transition.removed_unrealized_interest) {
        if amount > 0 {
            let tracked = match asset {
                MarketAsset::Base => &mut start.tracking.base_unrealized_interest,
                MarketAsset::Quote => &mut start.tracking.quote_unrealized_interest,
            };
            *tracked = tracked
                .checked_sub(amount)
                .ok_or(ErrorCode::MarketMathOverflow)?;
        }
    }
    start.tracking.base_unrealized_interest = start.tracking.base_unrealized_interest.min(
        u64::try_from(state.unrealized_interest(fixed, MarketAsset::Base)?)
            .map_err(|_| ErrorCode::MarketMathOverflow)?,
    );
    start.tracking.quote_unrealized_interest = start.tracking.quote_unrealized_interest.min(
        u64::try_from(state.unrealized_interest(fixed, MarketAsset::Quote)?)
            .map_err(|_| ErrorCode::MarketMathOverflow)?,
    );
    start.tracking.principal_nav_nad = start
        .tracking
        .principal_nav_nad
        .checked_add(socialized_nav_delta_nad)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    Ok(start)
}

#[cfg(test)]
fn hlp_planner_signed_navs_with_prices(
    fixed: HlpPlannerStatic,
    state: HlpPlannerState,
    prices: HlpCurvePrices,
) -> Result<(i128, i128)> {
    let base_active = state.active(fixed, MarketAsset::Base);
    let quote_active = state.active(fixed, MarketAsset::Quote);
    let (base_values, quote_values) = hlp_planner_inventory_values_pair_nad_with_prices(
        fixed,
        state,
        prices,
        base_active,
        quote_active,
    )?;
    Ok((
        hlp_lifecycle_endpoint_from_values(base_values)?.principal_nav_nad,
        hlp_lifecycle_endpoint_from_values(quote_values)?.principal_nav_nad,
    ))
}

#[cfg(test)]
fn refresh_compact_hlp_after_rebalance_from_valuation(
    state: &mut HlpPlannerState,
    target_asset: MarketAsset,
    mut receipt: HlpRebalanceReceipt,
    post_valuation: HlpValuation,
) -> Result<HlpRebalanceReceipt> {
    let actual_residual_exposure = post_valuation.ideal_delta;
    let residual_exposure = recognized_hlp_residual_exposure(actual_residual_exposure, post_valuation.nav_nad);
    if !post_valuation.proportional_hedge_available && residual_exposure != 0 {
        receipt.ideal_delta = residual_exposure;
        receipt.executed_delta = 0;
    } else {
        receipt.executed_delta = receipt
            .ideal_delta
            .checked_sub(residual_exposure)
            .ok_or(ErrorCode::MarketMathOverflow)?;
    }
    state.vault_mut(target_asset).residual_exposure = residual_exposure;
    receipt.residual_exposure = residual_exposure;
    receipt.nav_nad = post_valuation.nav_nad;
    Ok(receipt)
}

#[cfg(test)]
fn apply_compact_hlp_socialized_loss(
    market: &Market,
    fixed: HlpPlannerStatic,
    state: &mut HlpPlannerState,
    endpoints: HlpGuidanceEndpointCapability,
    debt_asset: MarketAsset,
    transition: crate::market::LeverageLifecycleTransition,
    reserve_price_nad: u64,
) -> Result<(
    crate::market::HlpSocializedLossRebase,
    ConcentratedGuidanceCurve,
    HlpCurvePrices,
    bool,
)> {
    if transition.socialized_principal_loss == 0 {
        return Ok((
            crate::market::HlpSocializedLossRebase::default(),
            endpoints.reserve_prepared,
            hlp_curve_prices_from_base_price_nad(reserve_price_nad as u128)?,
            false,
        ));
    }

    let start_reserves = state.curve_reserves_nad(fixed)?;
    let start = endpoints.fresh_guidance_for_reserves(market, start_reserves)?;
    let start_prices = hlp_curve_prices_from_base_price_nad(start.marginal_price_nad()?)?;
    let nav_before = hlp_planner_signed_navs_with_prices(fixed, *state, start_prices)?;

    let loss = transition.socialized_principal_loss;
    state.side_mut(debt_asset).live_reserve = state
        .side(debt_asset)
        .live_reserve
        .checked_sub(loss)
        .ok_or(ErrorCode::ReserveUnderflow)?;
    let successor_reserves = state.curve_reserves_nad(fixed)?;
    let loss_nad = normalize_to_nad(loss as u128, fixed.decimals(debt_asset))?;
    let mut expected_successor = start_reserves;
    match debt_asset {
        MarketAsset::Base => {
            expected_successor.base = expected_successor
                .base
                .checked_sub(loss_nad)
                .ok_or(ErrorCode::ReserveUnderflow)?;
        }
        MarketAsset::Quote => {
            expected_successor.quote = expected_successor
                .quote
                .checked_sub(loss_nad)
                .ok_or(ErrorCode::ReserveUnderflow)?;
        }
    }
    require!(successor_reserves == expected_successor, ErrorCode::BrokenInvariant);
    let successor = endpoints.fresh_guidance_for_reserves(market, successor_reserves)?;
    let successor_prices = hlp_curve_prices_from_base_price_nad(successor.marginal_price_nad()?)?;
    let nav_after = hlp_planner_signed_navs_with_prices(fixed, *state, successor_prices)?;
    Ok((
        crate::market::HlpSocializedLossRebase {
            base_nav_delta_nad: nav_after
                .0
                .checked_sub(nav_before.0)
                .ok_or(ErrorCode::MarketMathOverflow)?,
            quote_nav_delta_nad: nav_after
                .1
                .checked_sub(nav_before.1)
                .ok_or(ErrorCode::MarketMathOverflow)?,
        },
        successor,
        successor_prices,
        true,
    ))
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn rebalance_compact_hlps_after_swap_joint(
    market: &Market,
    fixed: HlpPlannerStatic,
    mut state: HlpPlannerState,
    endpoints: HlpGuidanceEndpointCapability,
    start_curve: ConcentratedGuidanceCurve,
    start_prices: HlpCurvePrices,
    fresh_canonical_final: bool,
) -> Result<HlpCompactPostRebalance> {
    let base_active = state.active(fixed, MarketAsset::Base);
    let quote_active = state.active(fixed, MarketAsset::Quote);
    let (base_values, quote_values) = hlp_planner_inventory_values_pair_nad_with_prices(
        fixed,
        state,
        start_prices,
        base_active,
        quote_active,
    )?;
    let base_valuation = base_active
        .then(|| hlp_valuation_from_values(base_values, start_prices))
        .transpose()?;
    let quote_valuation = quote_active
        .then(|| hlp_valuation_from_values(quote_values, start_prices))
        .transpose()?;
    let start_ylp_supply = state.base_side.ylp_supply;
    require_eq!(
        start_ylp_supply,
        state.quote_side.ylp_supply,
        ErrorCode::BrokenInvariant
    );

    let base_receipt = if let Some(valuation) = base_valuation {
        let plan = plan_compact_hlp_rebalance_from_valuation(
            fixed,
            state,
            MarketAsset::Base,
            valuation,
            start_curve,
            start_ylp_supply,
            HlpGuidanceSettlementProbeMode::ExactReference,
        )?;
        apply_compact_hlp_rebalance_plan_to_planner_state(fixed, &mut state, plan)?
    } else {
        empty_hlp_rebalance_receipt(MarketAsset::Base)
    };
    let quote_receipt = if let Some(valuation) = quote_valuation {
        let plan = plan_compact_hlp_rebalance_from_valuation(
            fixed,
            state,
            MarketAsset::Quote,
            valuation,
            start_curve,
            start_ylp_supply,
            HlpGuidanceSettlementProbeMode::ExactReference,
        )?;
        apply_compact_hlp_rebalance_plan_to_planner_state(fixed, &mut state, plan)?
    } else {
        empty_hlp_rebalance_receipt(MarketAsset::Quote)
    };
    let inventory_changed = base_receipt.ylp_mint_amount != 0
        || base_receipt.ylp_burn_amount != 0
        || quote_receipt.ylp_mint_amount != 0
        || quote_receipt.ylp_burn_amount != 0;
    let final_prices = if inventory_changed {
        let final_ylp_supply = state.base_side.ylp_supply;
        require_eq!(
            final_ylp_supply,
            state.quote_side.ylp_supply,
            ErrorCode::BrokenInvariant
        );
        require!(start_ylp_supply > 0 && final_ylp_supply > 0, ErrorCode::SupplyUnderflow);
        let reserves = state.curve_reserves_nad(fixed)?;
        let final_curve = if fresh_canonical_final {
            endpoints.fresh_guidance_for_reserves(market, reserves)?
        } else {
            let invariant_d = mul_div_u128(
                start_curve.invariant_d(),
                final_ylp_supply as u128,
                start_ylp_supply as u128,
            )?;
            start_curve.prepare_guidance_successor_with_invariant(
                reserves.base,
                reserves.quote,
                invariant_d,
            )?
        };
        hlp_curve_prices_from_base_price_nad(final_curve.marginal_price_nad()?)?
    } else {
        start_prices
    };
    let (base_final_values, quote_final_values) = hlp_planner_inventory_values_pair_nad_with_prices(
        fixed,
        state,
        final_prices,
        base_active,
        quote_active,
    )?;
    let base_receipt = if base_active {
        let valuation = hlp_valuation_from_values(base_final_values, final_prices)?;
        refresh_compact_hlp_after_rebalance_from_valuation(
            &mut state,
            MarketAsset::Base,
            base_receipt,
            valuation,
        )?
    } else {
        base_receipt
    };
    let quote_receipt = if quote_active {
        let valuation = hlp_valuation_from_values(quote_final_values, final_prices)?;
        refresh_compact_hlp_after_rebalance_from_valuation(
            &mut state,
            MarketAsset::Quote,
            quote_receipt,
            valuation,
        )?
    } else {
        quote_receipt
    };
    Ok(HlpCompactPostRebalance {
        state,
        base_receipt,
        quote_receipt,
        final_prices,
        base_endpoint: hlp_lifecycle_endpoint_from_values(base_final_values)?,
        quote_endpoint: hlp_lifecycle_endpoint_from_values(quote_final_values)?,
    })
}

#[cfg(test)]
fn compact_hlp_lifecycle_tracking(
    market: &Market,
    context: &ConcentratedHlpSolveContext,
    args: &HlpAuthoritativeLifecycleArgs,
) -> Result<HlpCompactLifecycleResult> {
    let HlpLifecycleEndpointMode::Guidance(endpoints) = args.endpoints else {
        return err!(ErrorCode::BrokenInvariant);
    };
    endpoints.require_identity(market)?;
    let fixed = HlpPlannerStatic::capture(market)?;
    let mut state = HlpPlannerState::capture(market);
    let transition = super::apply_leverage_lifecycle_to_planner_state(
        fixed,
        &mut state,
        context.cash_policy,
        context.asset_in,
        args.amount_in_after_fee,
        args.amount_out,
    )?;
    let debt_asset = match context.cash_policy {
        SwapCashPolicy::Decrease { debt_asset, .. }
        | SwapCashPolicy::Close { debt_asset, .. }
        | SwapCashPolicy::Liquidate { debt_asset, .. } => Some(debt_asset),
        _ => None,
    };
    let base_endpoint_start = hlp_planner_tracking_start_after_transition(
        fixed,
        state,
        context.base_start,
        transition,
        debt_asset,
        0,
    )?;
    let quote_endpoint_start = hlp_planner_tracking_start_after_transition(
        fixed,
        state,
        context.quote_start,
        transition,
        debt_asset,
        0,
    )?;

    let trade_base = state.curve_reserve(fixed, MarketAsset::Base)?;
    let trade_quote = state.curve_reserve(fixed, MarketAsset::Quote)?;
    require!(
        state.curve_reserves_nad(fixed)? == endpoints.trade_reserves(),
        ErrorCode::BrokenInvariant
    );
    let trade_prices = hlp_curve_prices_from_base_price_nad(args.expected_trade_price_nad as u128)?;
    let reserve_prices = hlp_curve_prices_from_base_price_nad(args.expected_reserve_price_nad as u128)?;
    let base_active = state.active(fixed, MarketAsset::Base);
    let quote_active = state.active(fixed, MarketAsset::Quote);
    let (base_trade_values, quote_trade_values) = hlp_planner_inventory_values_pair_for_curve_with_prices(
        fixed,
        state,
        trade_base,
        trade_quote,
        trade_prices,
        base_active,
        quote_active,
    )?;
    let base_trade_endpoint = hlp_lifecycle_endpoint_from_values(base_trade_values)?;
    let quote_trade_endpoint = hlp_lifecycle_endpoint_from_values(quote_trade_values)?;
    let base_trade = hlp_planner_tracking_from_endpoint(
        fixed,
        state,
        MarketAsset::Base,
        trade_prices,
        base_trade_endpoint,
        base_endpoint_start,
    )?;
    let quote_trade = hlp_planner_tracking_from_endpoint(
        fixed,
        state,
        MarketAsset::Quote,
        trade_prices,
        quote_trade_endpoint,
        quote_endpoint_start,
    )?;

    if args.retained_surcharge > 0 {
        let side = state.side_mut(context.asset_in);
        side.live_reserve = side
            .live_reserve
            .checked_add(args.retained_surcharge)
            .ok_or(ErrorCode::ReserveOverflow)?;
        side.cash_reserve = side
            .cash_reserve
            .checked_add(args.retained_surcharge)
            .ok_or(ErrorCode::ReserveOverflow)?;
    } else {
        require!(endpoints.trade_reserves() == endpoints.reserve_reserves(), ErrorCode::BrokenInvariant);
    }
    require!(
        state.curve_reserves_nad(fixed)? == endpoints.reserve_reserves(),
        ErrorCode::BrokenInvariant
    );
    let reserve_base = state.curve_reserve(fixed, MarketAsset::Base)?;
    let reserve_quote = state.curve_reserve(fixed, MarketAsset::Quote)?;
    let (base_reserve_values, quote_reserve_values) = hlp_planner_inventory_values_pair_for_curve_with_prices(
        fixed,
        state,
        reserve_base,
        reserve_quote,
        reserve_prices,
        base_active,
        quote_active,
    )?;
    let base_reserve_endpoint = hlp_lifecycle_endpoint_from_values(base_reserve_values)?;
    let quote_reserve_endpoint = hlp_lifecycle_endpoint_from_values(quote_reserve_values)?;
    let base_reserve = hlp_planner_tracking_from_endpoint(
        fixed,
        state,
        MarketAsset::Base,
        reserve_prices,
        base_reserve_endpoint,
        base_endpoint_start,
    )?;
    let quote_reserve = hlp_planner_tracking_from_endpoint(
        fixed,
        state,
        MarketAsset::Quote,
        reserve_prices,
        quote_reserve_endpoint,
        quote_endpoint_start,
    )?;
    let (base_trade_at_reserve_values, quote_trade_at_reserve_values) =
        hlp_planner_inventory_values_pair_for_curve_with_prices(
            fixed,
            state,
            trade_base,
            trade_quote,
            reserve_prices,
            base_active,
            quote_active,
        )?;
    let base_trade_at_reserve = hlp_planner_tracking_from_endpoint(
        fixed,
        state,
        MarketAsset::Base,
        reserve_prices,
        hlp_lifecycle_endpoint_from_values(base_trade_at_reserve_values)?,
        base_endpoint_start,
    )?;
    let quote_trade_at_reserve = hlp_planner_tracking_from_endpoint(
        fixed,
        state,
        MarketAsset::Quote,
        reserve_prices,
        hlp_lifecycle_endpoint_from_values(quote_trade_at_reserve_values)?,
        quote_endpoint_start,
    )?;
    let base_retained_contribution_nad = base_reserve
        .2
        .checked_sub(base_trade_at_reserve.2)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let quote_retained_contribution_nad = quote_reserve
        .2
        .checked_sub(quote_trade_at_reserve.2)
        .ok_or(ErrorCode::MarketMathOverflow)?;

    let (rebase, rebalance_curve, rebalance_prices, fresh_canonical_final) = if let Some(asset) = debt_asset {
        apply_compact_hlp_socialized_loss(
            market,
            fixed,
            &mut state,
            endpoints,
            asset,
            transition,
            args.expected_reserve_price_nad,
        )?
    } else {
        (
            crate::market::HlpSocializedLossRebase::default(),
            endpoints.reserve_prepared,
            reserve_prices,
            false,
        )
    };
    let base_start = hlp_planner_tracking_start_after_transition(
        fixed,
        state,
        context.base_start,
        transition,
        debt_asset,
        rebase.base_nav_delta_nad,
    )?;
    let quote_start = hlp_planner_tracking_start_after_transition(
        fixed,
        state,
        context.quote_start,
        transition,
        debt_asset,
        rebase.quote_nav_delta_nad,
    )?;
    let post = rebalance_compact_hlps_after_swap_joint(
        market,
        fixed,
        state,
        endpoints,
        rebalance_curve,
        rebalance_prices,
        fresh_canonical_final,
    )?;
    let base = hlp_planner_tracking_from_endpoint(
        fixed,
        post.state,
        MarketAsset::Base,
        post.final_prices,
        post.base_endpoint,
        base_start,
    )?;
    let quote = hlp_planner_tracking_from_endpoint(
        fixed,
        post.state,
        MarketAsset::Quote,
        post.final_prices,
        post.quote_endpoint,
        quote_start,
    )?;
    Ok(HlpCompactLifecycleResult {
        state: post.state,
        tracking: HlpLifecycleTracking {
            base_principal_error_nad: base.0,
            base_error_nad: base.2,
            base_trade_error_nad: base_trade.2,
            base_reserve_error_nad: base_reserve.2,
            base_retained_contribution_nad,
            base_exposure_nad: base.3,
            quote_principal_error_nad: quote.0,
            quote_error_nad: quote.2,
            quote_trade_error_nad: quote_trade.2,
            quote_reserve_error_nad: quote_reserve.2,
            quote_retained_contribution_nad,
            quote_exposure_nad: quote.3,
        },
        base_post_receipt: post.base_receipt,
        quote_post_receipt: post.quote_receipt,
        transition,
    })
}

fn current_hlp_inventory_values_pair_nad_with_prices(
    market: &Market,
    prices: HlpCurvePrices,
    base_active: bool,
    quote_active: bool,
) -> Result<(HlpInventoryValuesNad, HlpInventoryValuesNad)> {
    let base_curve = market.curve_reserve(MarketAsset::Base)?;
    let quote_curve = market.curve_reserve(MarketAsset::Quote)?;
    let base_supply = market.base_side.shares.ylp_supply;
    let quote_supply = market.quote_side.shares.ylp_supply;

    let claim = |reserve: u64, shares: u64, supply: u64| -> Result<u64> {
        if shares == 0 || supply == 0 {
            return Ok(0);
        }
        u64::try_from(mul_div_u128(reserve as u128, shares as u128, supply as u128)?)
            .map_err(|_| ErrorCode::MarketMathOverflow.into())
    };

    let base_values = if base_active {
        let shares = market.base_hlp_vault.ylp_shares;
        let base_claim = claim(base_curve, shares, base_supply)?;
        let quote_claim = claim(quote_curve, shares, quote_supply)?;
        let quote_debt = u64::try_from(Debt::shares_to_debt(
            market.base_hlp_vault.debt_shares,
            market.debt.quote_borrow_index_nad,
        )?)
        .map_err(|_| ErrorCode::DebtMathOverflow)?;
        HlpInventoryValuesNad {
            target_inventory_value_nad: asset_value_in_target_nad_with_prices(
                market,
                prices,
                MarketAsset::Base,
                base_claim,
                MarketAsset::Base,
            )?,
            opposite_inventory_value_nad: asset_value_in_target_nad_with_prices(
                market,
                prices,
                MarketAsset::Quote,
                quote_claim,
                MarketAsset::Base,
            )?,
            debt_value_nad: asset_value_in_target_nad_with_prices(
                market,
                prices,
                MarketAsset::Quote,
                quote_debt,
                MarketAsset::Base,
            )?,
        }
    } else {
        HlpInventoryValuesNad::default()
    };

    let quote_values = if quote_active {
        let shares = market.quote_hlp_vault.ylp_shares;
        let base_claim = claim(base_curve, shares, base_supply)?;
        let quote_claim = claim(quote_curve, shares, quote_supply)?;
        let base_debt = u64::try_from(Debt::shares_to_debt(
            market.quote_hlp_vault.debt_shares,
            market.debt.base_borrow_index_nad,
        )?)
        .map_err(|_| ErrorCode::DebtMathOverflow)?;
        HlpInventoryValuesNad {
            target_inventory_value_nad: asset_value_in_target_nad_with_prices(
                market,
                prices,
                MarketAsset::Quote,
                quote_claim,
                MarketAsset::Quote,
            )?,
            opposite_inventory_value_nad: asset_value_in_target_nad_with_prices(
                market,
                prices,
                MarketAsset::Base,
                base_claim,
                MarketAsset::Quote,
            )?,
            debt_value_nad: asset_value_in_target_nad_with_prices(
                market,
                prices,
                MarketAsset::Base,
                base_debt,
                MarketAsset::Quote,
            )?,
        }
    } else {
        HlpInventoryValuesNad::default()
    };
    let values = (base_values, quote_values);
    #[cfg(test)]
    {
        let fixed = HlpPlannerStatic::capture(market)?;
        let state = HlpPlannerState::capture(market);
        require!(
            hlp_planner_inventory_values_pair_nad_with_prices(fixed, state, prices, base_active, quote_active,)?
                == values,
            ErrorCode::BrokenInvariant
        );
    }
    Ok(values)
}

fn hlp_interest_claims_for_shares(
    base_unrealized_interest: u64,
    quote_unrealized_interest: u64,
    ylp_shares: u64,
    ylp_supply: u64,
) -> Result<(u64, u64)> {
    if ylp_shares == 0 {
        return Ok((0, 0));
    }
    require!(ylp_supply > 0, ErrorCode::BrokenInvariant);
    let base_claim = u64::try_from(mul_div_u128(
        base_unrealized_interest as u128,
        ylp_shares as u128,
        ylp_supply as u128,
    )?)
    .map_err(|_| ErrorCode::MarketMathOverflow)?;
    let quote_claim = u64::try_from(mul_div_u128(
        quote_unrealized_interest as u128,
        ylp_shares as u128,
        ylp_supply as u128,
    )?)
    .map_err(|_| ErrorCode::MarketMathOverflow)?;
    Ok((base_claim, quote_claim))
}

fn signed_asset_value_in_target_nad_with_prices(
    market: &Market,
    prices: HlpCurvePrices,
    asset: MarketAsset,
    amount: i128,
    target_asset: MarketAsset,
) -> Result<i128> {
    let magnitude = u64::try_from(amount.unsigned_abs()).map_err(|_| ErrorCode::MarketMathOverflow)?;
    let value = i128::try_from(asset_value_in_target_nad_with_prices(
        market,
        prices,
        asset,
        magnitude,
        target_asset,
    )?)
    .map_err(|_| ErrorCode::MarketMathOverflow)?;
    if amount < 0 {
        value.checked_neg().ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    } else {
        Ok(value)
    }
}

fn hlp_frozen_interest_claim_delta_value_nad(
    market: &Market,
    target_asset: MarketAsset,
    prices: HlpCurvePrices,
    tracking: HlpTrackingReference,
) -> Result<i128> {
    let vault = match target_asset {
        MarketAsset::Base => &market.base_hlp_vault,
        MarketAsset::Quote => &market.quote_hlp_vault,
    };
    let final_ylp_supply = market.base_side.shares.ylp_supply;
    require_eq!(
        final_ylp_supply,
        market.quote_side.shares.ylp_supply,
        ErrorCode::BrokenInvariant
    );
    let (base_public_claim, quote_public_claim) = hlp_interest_claims_for_shares(
        tracking.base_unrealized_interest,
        tracking.quote_unrealized_interest,
        vault.ylp_shares,
        final_ylp_supply,
    )?;
    let (start_base_public_claim, start_quote_public_claim) = hlp_interest_claims_for_shares(
        tracking.base_unrealized_interest,
        tracking.quote_unrealized_interest,
        tracking.start_ylp_shares,
        tracking.start_ylp_supply,
    )?;
    let base_delta = i128::from(base_public_claim)
        .checked_sub(i128::from(start_base_public_claim))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let quote_delta = i128::from(quote_public_claim)
        .checked_sub(i128::from(start_quote_public_claim))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    signed_asset_value_in_target_nad_with_prices(market, prices, MarketAsset::Base, base_delta, target_asset)?
        .checked_add(signed_asset_value_in_target_nad_with_prices(
            market,
            prices,
            MarketAsset::Quote,
            quote_delta,
            target_asset,
        )?)
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

fn hlp_tracking_deltas_nad(
    market: &Market,
    target_asset: MarketAsset,
    prices: HlpCurvePrices,
    final_principal_nav_nad: i128,
    tracking: HlpTrackingReference,
) -> Result<(i128, i128, i128)> {
    let principal_delta = final_principal_nav_nad
        .checked_sub(tracking.principal_nav_nad)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let claim_delta = hlp_frozen_interest_claim_delta_value_nad(market, target_asset, prices, tracking)?;
    let combined_delta = principal_delta
        .checked_add(claim_delta)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    Ok((principal_delta, claim_delta, combined_delta))
}

pub(crate) fn stamp_hlp_tracking_reference(receipt: &mut HlpRebalanceReceipt, tracking: HlpTrackingReference) {
    receipt.tracking_start_nav_nad = tracking.principal_nav_nad;
    receipt.tracking_loss_budget_nad = tracking.loss_budget_nad;
    receipt.tracking_base_unrealized_interest = tracking.base_unrealized_interest;
    receipt.tracking_quote_unrealized_interest = tracking.quote_unrealized_interest;
    receipt.tracking_start_ylp_shares = tracking.start_ylp_shares;
    receipt.tracking_start_ylp_supply = tracking.start_ylp_supply;
}

pub(crate) fn consume_hlp_tracking_unrealized_interest(
    receipt: &mut HlpRebalanceReceipt,
    asset: MarketAsset,
    amount: u64,
) -> Result<()> {
    if receipt.tracking_loss_budget_nad == 0 {
        return Ok(());
    }
    let tracked = match asset {
        MarketAsset::Base => &mut receipt.tracking_base_unrealized_interest,
        MarketAsset::Quote => &mut receipt.tracking_quote_unrealized_interest,
    };
    require_gte!(*tracked, amount, ErrorCode::BrokenInvariant);
    *tracked = tracked.checked_sub(amount).ok_or(ErrorCode::MarketMathOverflow)?;
    Ok(())
}

pub(crate) fn cap_hlp_tracking_unrealized_interest(
    receipt: &mut HlpRebalanceReceipt,
    asset: MarketAsset,
    surviving_amount: u64,
) {
    let tracked = match asset {
        MarketAsset::Base => &mut receipt.tracking_base_unrealized_interest,
        MarketAsset::Quote => &mut receipt.tracking_quote_unrealized_interest,
    };
    *tracked = (*tracked).min(surviving_amount);
}

#[cfg(test)]
fn current_hlp_inventory_values_nad(market: &Market, target_asset: MarketAsset) -> Result<HlpInventoryValuesNad> {
    current_hlp_inventory_values_nad_with_prices(market, target_asset, current_hlp_curve_prices(market)?)
}

fn raw_amount_from_target_value_nad_with_prices(
    market: &Market,
    prices: HlpCurvePrices,
    asset: MarketAsset,
    target_asset: MarketAsset,
    value_nad: u128,
) -> Result<u64> {
    if value_nad == 0 {
        return Ok(0);
    }
    let amount_nad = if asset == target_asset {
        value_nad
    } else {
        let price_nad = prices.for_asset(asset);
        require!(price_nad > 0, ErrorCode::InvalidSettlementPrice);
        mul_div_u128(value_nad, NAD as u128, price_nad)?
    };
    denormalize_from_nad_floor(amount_nad, market.side(asset).asset_decimals)
}

fn ylp_for_live_reserve_deposit(market: &Market, base_amount: u64, quote_amount: u64) -> Result<u64> {
    require!(market.base_side.shares.ylp_supply > 0, ErrorCode::SupplyUnderflow);
    let base_reserve = market.base_side.reserves.live_reserve;
    let quote_reserve = market.quote_side.reserves.live_reserve;
    require!(base_reserve > 0 && quote_reserve > 0, ErrorCode::InsufficientLiquidity);
    market.ylp_for_deposit(base_reserve, quote_reserve, base_amount, quote_amount)
}

fn curve_slot(market: &Market) -> u64 {
    // Curve parameters are explicitly admitted into `applied_curve_parameters`
    // by the instruction update path; merely observing wall-clock time never
    // advances a ramp. Avoid repeated Clock deserialization inside the bounded
    // hLP solver because Solana's bump allocator cannot reclaim that memory.
    market.amm.last_observation_slot.max(market.last_update_slot)
}

fn proportional(amount: u64, numerator: u64, denominator: u64) -> Result<u64> {
    let value = (amount as u128)
        .checked_mul(numerator as u128)
        .and_then(|value| value.checked_div(denominator as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    u64::try_from(value).map_err(|_| ErrorCode::MarketMathOverflow.into())
}

fn proportional_u128(amount: u128, numerator: u64, denominator: u64) -> Result<u128> {
    amount
        .checked_mul(numerator as u128)
        .and_then(|value| value.checked_div(denominator as u128))
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

fn hlp_shares_for_delta_nav(delta_nav_nad: u128, nav_basis_nad: u128, hlp_supply: u64) -> Result<u64> {
    require!(delta_nav_nad > 0, ErrorCode::AmountZero);
    require!(nav_basis_nad > 0, ErrorCode::MarketMathOverflow);
    let shares = delta_nav_nad
        .checked_mul(hlp_supply as u128)
        .and_then(|value| value.checked_div(nav_basis_nad))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let shares = u64::try_from(shares).map_err(|_| ErrorCode::MarketMathOverflow)?;
    require!(shares > 0, ErrorCode::AmountZero);
    Ok(shares)
}

#[cfg(test)]
mod tests {
    include!("../tests/market/hlp_rebalance.rs");
}
