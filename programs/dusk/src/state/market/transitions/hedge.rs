use anchor_lang::prelude::*;

#[cfg(test)]
use crate::state::HlpVault;
use crate::{
    constants::{HLP_PRE_SOLVE_LOSS_THRESHOLD_NAD, HLP_PRE_SOLVE_MAX_ITERS, NAD},
    errors::ErrorCode,
    math::{
        allocate_hlp_proportional_adjustment_nad, calculate_normalized_amount_out, closed_form_pre_adjustment_nad,
        concentrated_marginal_price_nad, denormalize_from_nad_floor, hlp_opposite_exposure_nad,
        ideal_hlp_rebalance_nad, normalize_to_nad, ratio_lte_full_width, tracking_loss_nad, HlpInventoryValuesNad,
    },
    state::{Debt, Market, MarketAsset},
};

/// Post-transition exposure is protocol dust only when it is no more than
/// 0.00001 target tokens and no more than one part per million of current hLP
/// NAV. Coarse assets and small vaults therefore fail closed rather than hide
/// a meaningful constrained gap.
const HLP_REBALANCE_DUST_MAX_NAD: u128 = 10_000;
const HLP_REBALANCE_DUST_NAV_DENOMINATOR: u128 = 1_000_000;

fn recognized_hlp_pending(actual_pending_nad: i128, nav_nad: u128) -> i128 {
    let tolerance_nad = HLP_REBALANCE_DUST_MAX_NAD.min(nav_nad / HLP_REBALANCE_DUST_NAV_DENOMINATOR);
    if actual_pending_nad.unsigned_abs() <= tolerance_nad {
        0
    } else {
        actual_pending_nad
    }
}

pub struct DepositSingleSided {
    pub target_asset: MarketAsset,
    pub deposit_amount: u64,
    pub min_hlp_amount: u64,
}

pub struct WithdrawSingleSided {
    pub target_asset: MarketAsset,
    pub hlp_amount: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HedgeReceipt {
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
    pub pending_rebalance: i128,
    pub current_swap_fee_eligible_ylp_shares: u64,
    pub ylp_mint_amount: u64,
    pub ylp_burn_amount: u64,
    pub debt_delta: i128,
    pub interest_paid: u64,
    pub nav_nad: u128,
}

impl Default for HlpRebalanceReceipt {
    fn default() -> Self {
        Self {
            target_asset: MarketAsset::Base,
            ideal_delta: 0,
            executed_delta: 0,
            pending_rebalance: 0,
            current_swap_fee_eligible_ylp_shares: 0,
            ylp_mint_amount: 0,
            ylp_burn_amount: 0,
            debt_delta: 0,
            interest_paid: 0,
            nav_nad: 0,
        }
    }
}

impl DepositSingleSided {
    pub fn new(target_asset: MarketAsset, deposit_amount: u64, min_hlp_amount: u64) -> Self {
        Self {
            target_asset,
            deposit_amount,
            min_hlp_amount,
        }
    }

    pub fn apply(self, market: &mut Market) -> Result<HedgeReceipt> {
        require!(self.deposit_amount > 0, ErrorCode::AmountZero);
        require_hlp_settlement_available(market, self.target_asset)?;
        let pre_prices = current_hlp_curve_prices(market)?;
        let pre_entry = current_hlp_entry_state_with_prices(market, self.target_asset, pre_prices)?;
        let (hlp_supply_before, settlement_reference_before) = match self.target_asset {
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
        let borrowed_amount = proportional_opposite_leg_amount(market, self.target_asset, self.deposit_amount)?;
        require!(borrowed_amount > 0, ErrorCode::InsufficientLiquidity);
        checkpoint_hlp_yield_from_ylp(market, self.target_asset)?;

        let (ylp_amount, hlp_amount, hlp_supply, post_prices) = match self.target_asset {
            MarketAsset::Base => deposit_base_hlp(market, self.deposit_amount, borrowed_amount)?,
            MarketAsset::Quote => deposit_quote_hlp(market, self.deposit_amount, borrowed_amount)?,
        };
        require_gte!(hlp_amount, self.min_hlp_amount, ErrorCode::SlippageExceeded);
        let post_entry = current_hlp_entry_state_with_prices(market, self.target_asset, post_prices)?;
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
                    let pre_residual = pre_entry.pending_rebalance.unsigned_abs();
                    let post_residual = post_entry.pending_rebalance.unsigned_abs();
                    require!(
                        post_entry.nav_nad > pre_entry.nav_nad && hlp_supply > hlp_supply_before,
                        ErrorCode::HlpSettlementUnavailable
                    );
                    require!(post_residual <= pre_residual, ErrorCode::HlpSettlementUnavailable);
                    require!(
                        post_entry.pending_rebalance == 0
                            || post_entry.pending_rebalance.is_negative() == pre_entry.pending_rebalance.is_negative(),
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
        let vault = match self.target_asset {
            MarketAsset::Base => &mut market.base_hlp_vault,
            MarketAsset::Quote => &mut market.quote_hlp_vault,
        };
        vault.last_nav_nad = post_entry.nav_nad;
        vault.pending_rebalance = post_entry.pending_rebalance;
        vault.cached_settlement_price_nad = if hlp_supply_before == 0 || post_entry.pending_rebalance == 0 {
            post_prices.for_asset(self.target_asset)
        } else {
            settlement_reference_before
        };
        let health = market.market_health()?;
        market.assert_market_health_snapshot(&health)?;
        market.assert_virtual_reserve_invariant(MarketAsset::Base)?;
        market.assert_virtual_reserve_invariant(MarketAsset::Quote)?;
        Ok(HedgeReceipt {
            deposit_amount: self.deposit_amount,
            borrowed_amount,
            ylp_amount,
            hlp_amount,
            hlp_supply,
            target_amount_out: 0,
            debt_repaid: 0,
            interest_paid: 0,
        })
    }
}

impl WithdrawSingleSided {
    pub fn new(target_asset: MarketAsset, hlp_amount: u64) -> Self {
        Self {
            target_asset,
            hlp_amount,
        }
    }

    pub fn apply(self, market: &mut Market) -> Result<HedgeReceipt> {
        require!(self.hlp_amount > 0, ErrorCode::AmountZero);
        let pending_rebalance = match self.target_asset {
            MarketAsset::Base => market.base_hlp_vault.pending_rebalance,
            MarketAsset::Quote => market.quote_hlp_vault.pending_rebalance,
        };
        // Preserve the ordinary stale-price guard. An explicitly recorded
        // partial controller residual is different: an exit reduces or fully
        // retires that hedge, so trapping it behind the old reference would
        // create a liveness failure.
        if pending_rebalance == 0 {
            require_hlp_settlement_available(market, self.target_asset)?;
        }
        checkpoint_hlp_yield_from_ylp(market, self.target_asset)?;
        let receipt = match self.target_asset {
            MarketAsset::Base => withdraw_base_hlp(market, self.hlp_amount)?,
            MarketAsset::Quote => withdraw_quote_hlp(market, self.hlp_amount)?,
        };
        market.refresh_risk()?;
        let health = market.market_health()?;
        market.assert_market_health_snapshot(&health)?;
        market.assert_virtual_reserve_invariant(MarketAsset::Base)?;
        market.assert_virtual_reserve_invariant(MarketAsset::Quote)?;
        Ok(receipt)
    }
}

pub(in crate::state::market) fn checkpoint_hlp_vaults(market: &mut Market) -> Result<(i128, i128)> {
    let prices = current_hlp_curve_prices(market)?;
    checkpoint_hlp_vaults_with_prices(market, prices)
}

/// Checkpoints both hLP numeraires from one already-certified executable
/// marginal price. Concentrated swaps use their identity-bound endpoint
/// certificate here instead of solving the unchanged final curve again.
fn checkpoint_hlp_vaults_with_prices(market: &mut Market, prices: HlpCurvePrices) -> Result<(i128, i128)> {
    checkpoint_hlp_yield_from_ylp(market, MarketAsset::Base)?;
    checkpoint_hlp_yield_from_ylp(market, MarketAsset::Quote)?;
    let base_active = market.base_hlp_vault.hlp_supply > 0 || market.base_hlp_vault.pending_rebalance != 0;
    let quote_active = market.quote_hlp_vault.hlp_supply > 0 || market.quote_hlp_vault.pending_rebalance != 0;
    if !base_active && !quote_active {
        return Ok((0, 0));
    }

    let base_delta = if base_active {
        checkpoint_one_hlp_with_prices(market, MarketAsset::Base, prices)?
    } else {
        0
    };
    let quote_delta = if quote_active {
        checkpoint_one_hlp_with_prices(market, MarketAsset::Quote, prices)?
    } else {
        0
    };
    Ok((base_delta, quote_delta))
}

pub(in crate::state::market) fn rebalance_hlp_vaults(
    market: &mut Market,
) -> Result<(HlpRebalanceReceipt, HlpRebalanceReceipt)> {
    if market.base_hlp_vault.hlp_supply == 0
        && market.base_hlp_vault.pending_rebalance == 0
        && market.quote_hlp_vault.hlp_supply == 0
        && market.quote_hlp_vault.pending_rebalance == 0
    {
        return Ok((
            empty_hlp_rebalance_receipt(MarketAsset::Base),
            empty_hlp_rebalance_receipt(MarketAsset::Quote),
        ));
    }
    let base_receipt = if market.base_hlp_vault.hlp_supply > 0 || market.base_hlp_vault.pending_rebalance != 0 {
        rebalance_one_hlp(market, MarketAsset::Base)?
    } else {
        empty_hlp_rebalance_receipt(MarketAsset::Base)
    };
    let quote_receipt = if market.quote_hlp_vault.hlp_supply > 0 || market.quote_hlp_vault.pending_rebalance != 0 {
        rebalance_one_hlp(market, MarketAsset::Quote)?
    } else {
        empty_hlp_rebalance_receipt(MarketAsset::Quote)
    };
    Ok((base_receipt, quote_receipt))
}

pub(in crate::state::market) fn finalize_hlp_vaults_for_swap(
    market: &mut Market,
    base_pre_rebalance: HlpRebalanceReceipt,
    quote_pre_rebalance: HlpRebalanceReceipt,
) -> Result<(HlpRebalanceReceipt, HlpRebalanceReceipt)> {
    checkpoint_pre_solve_fee_eligibility(market, &base_pre_rebalance)?;
    checkpoint_pre_solve_fee_eligibility(market, &quote_pre_rebalance)?;
    // A swap moves the relative price for both target numeraires. Finish both
    // active vaults so neither side carries avoidable delta into the next swap.
    let (base_post_rebalance, quote_post_rebalance) = rebalance_hlp_vaults(market)?;
    Ok((
        combine_hlp_rebalance_receipts(base_pre_rebalance, base_post_rebalance)?,
        combine_hlp_rebalance_receipts(quote_pre_rebalance, quote_post_rebalance)?,
    ))
}

/// Concentrated swaps cannot fit an exact CONCENTRATED trade, funded recenter, and
/// hLP inventory mutation in one Solana transaction. Record the exact
/// post-swap exposure instead; a permissionless one-vault crank executes it.
pub(in crate::state::market) fn defer_hlp_vaults_after_concentrated_swap(
    market: &mut Market,
    trade_start_base_price_nad: u64,
    trade_end_base_price_nad: u64,
) -> Result<(HlpRebalanceReceipt, HlpRebalanceReceipt)> {
    require_hlp_vaults_after_concentrated_swap_safe(market, trade_start_base_price_nad, trade_end_base_price_nad)?;
    let end_prices = hlp_curve_prices_from_base_price_nad(trade_end_base_price_nad as u128)?;
    let (base_delta, quote_delta) = checkpoint_hlp_vaults_with_prices(market, end_prices)?;
    Ok((
        deferred_hlp_rebalance_receipt(market, MarketAsset::Base, base_delta),
        deferred_hlp_rebalance_receipt(market, MarketAsset::Quote, quote_delta),
    ))
}

/// Applies the same settlement-divergence admission rule without mutating hLP
/// accounting. Quote previews use this before returning data; execution then
/// calls `defer_hlp_vaults_after_concentrated_swap` to run this identical guard
/// and persist the resulting pending exposure.
pub(in crate::state::market) fn require_hlp_vaults_after_concentrated_swap_safe(
    market: &Market,
    trade_start_base_price_nad: u64,
    trade_end_base_price_nad: u64,
) -> Result<()> {
    let start_prices = hlp_curve_prices_from_base_price_nad(trade_start_base_price_nad as u128)?;
    let end_prices = hlp_curve_prices_from_base_price_nad(trade_end_base_price_nad as u128)?;
    require_deferred_hlp_swap_safe(market, MarketAsset::Base, start_prices, end_prices)?;
    require_deferred_hlp_swap_safe(market, MarketAsset::Quote, start_prices, end_prices)?;
    Ok(())
}

fn deferred_hlp_rebalance_receipt(
    market: &Market,
    target_asset: MarketAsset,
    ideal_delta: i128,
) -> HlpRebalanceReceipt {
    let vault = match target_asset {
        MarketAsset::Base => &market.base_hlp_vault,
        MarketAsset::Quote => &market.quote_hlp_vault,
    };
    HlpRebalanceReceipt {
        target_asset,
        ideal_delta,
        pending_rebalance: vault.pending_rebalance,
        nav_nad: vault.last_nav_nad,
        ..HlpRebalanceReceipt::default()
    }
}

fn require_deferred_hlp_swap_safe(
    market: &Market,
    target_asset: MarketAsset,
    start_prices: HlpCurvePrices,
    end_prices: HlpCurvePrices,
) -> Result<()> {
    let vault = match target_asset {
        MarketAsset::Base => &market.base_hlp_vault,
        MarketAsset::Quote => &market.quote_hlp_vault,
    };
    if vault.hlp_supply == 0 || vault.cached_settlement_price_nad == 0 {
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

fn checkpoint_pre_solve_fee_eligibility(market: &mut Market, receipt: &HlpRebalanceReceipt) -> Result<()> {
    if receipt.ylp_mint_amount == 0 && receipt.ylp_burn_amount == 0 {
        return Ok(());
    }
    checkpoint_hlp_yield_from_ylp_shares(
        market,
        receipt.target_asset,
        receipt.current_swap_fee_eligible_ylp_shares,
    )
}

fn combine_hlp_rebalance_receipts(pre: HlpRebalanceReceipt, post: HlpRebalanceReceipt) -> Result<HlpRebalanceReceipt> {
    require!(pre.target_asset == post.target_asset, ErrorCode::BrokenInvariant);
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
        pending_rebalance: post.pending_rebalance,
        current_swap_fee_eligible_ylp_shares: 0,
        ylp_mint_amount: pre
            .ylp_mint_amount
            .checked_add(post.ylp_mint_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?,
        ylp_burn_amount: pre
            .ylp_burn_amount
            .checked_add(post.ylp_burn_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?,
        debt_delta: pre
            .debt_delta
            .checked_add(post.debt_delta)
            .ok_or(ErrorCode::MarketMathOverflow)?,
        interest_paid: pre
            .interest_paid
            .checked_add(post.interest_paid)
            .ok_or(ErrorCode::MarketMathOverflow)?,
        nav_nad: post.nav_nad.max(pre.nav_nad),
    })
}

pub(in crate::state::market) fn pre_solve_hlp_vaults_for_swap(
    market: &mut Market,
    asset_in: MarketAsset,
    amount_in_after_fee: u64,
) -> Result<(HlpRebalanceReceipt, HlpRebalanceReceipt)> {
    pre_solve_hlp_vaults_for_swap_with_reserve_input(market, asset_in, amount_in_after_fee, amount_in_after_fee)
}

/// Pre-positions active hLPs against a conservative preliminary swap path.
///
/// `amount_in_for_quote` determines trader output. `reserve_input_credit`
/// determines the final input reserve because a retained dynamic surcharge is
/// principal, not quote input. Keeping the coordinates separate prevents the
/// solver from understating the post-swap marginal move when surcharge
/// retention is active.
pub(in crate::state::market) fn pre_solve_hlp_vaults_for_swap_with_reserve_input(
    market: &mut Market,
    asset_in: MarketAsset,
    amount_in_for_quote: u64,
    reserve_input_credit: u64,
) -> Result<(HlpRebalanceReceipt, HlpRebalanceReceipt)> {
    require_gte!(reserve_input_credit, amount_in_for_quote, ErrorCode::BrokenInvariant);
    if amount_in_for_quote == 0 {
        require_eq!(reserve_input_credit, 0, ErrorCode::BrokenInvariant);
        return Ok((
            empty_hlp_rebalance_receipt(MarketAsset::Base),
            empty_hlp_rebalance_receipt(MarketAsset::Quote),
        ));
    }

    let base_receipt = pre_solve_one_hlp_for_swap(
        market,
        MarketAsset::Base,
        asset_in,
        amount_in_for_quote,
        reserve_input_credit,
    )?;
    let quote_receipt = pre_solve_one_hlp_for_swap(
        market,
        MarketAsset::Quote,
        asset_in,
        amount_in_for_quote,
        reserve_input_credit,
    )?;
    Ok((base_receipt, quote_receipt))
}

fn pre_solve_one_hlp_for_swap(
    market: &mut Market,
    target_asset: MarketAsset,
    asset_in: MarketAsset,
    amount_in_for_quote: u64,
    reserve_input_credit: u64,
) -> Result<HlpRebalanceReceipt> {
    if !hlp_rebalance_needed(market, target_asset) {
        return Ok(empty_hlp_rebalance_receipt(target_asset));
    }

    // The closed-form sqrt(r) theorem below is specific to a 50/50 CPMM.
    // Applying it to the Dusk Concentrated AMM would silently create the wrong
    // pre-position. Until a bounded concentrated exposure-error solver fits the swap CU budget, concentrated
    // pools deliberately use only the exact-curve post-swap rebalance.
    if !market.current_curve_parameters(curve_slot(market)).is_cpmm() {
        return Ok(empty_hlp_rebalance_receipt(target_asset));
    }

    let solver = CpmmPreSolveSnapshot::new(market, target_asset)?;
    if !solver.valuation.proportional_hedge_available {
        return Ok(empty_hlp_rebalance_receipt(target_asset));
    }
    let equity_nad = solver.valuation.nav_nad;
    if equity_nad == 0 {
        return Ok(empty_hlp_rebalance_receipt(target_asset));
    }

    let provisional_ratio =
        solver.simulated_swap_price_ratio_nad(asset_in, amount_in_for_quote, reserve_input_credit, 0, true)?;
    let estimated_loss = tracking_loss_nad(equity_nad, provisional_ratio)?;
    if estimated_loss <= HLP_PRE_SOLVE_LOSS_THRESHOLD_NAD {
        return Ok(empty_hlp_rebalance_receipt(target_asset));
    }

    let (_, lever_up) = closed_form_pre_adjustment_nad(equity_nad, provisional_ratio)?;
    let pre_adjustment_nad = solve_pre_adjustment_with_snapshot(
        &solver,
        asset_in,
        amount_in_for_quote,
        reserve_input_credit,
        equity_nad,
        lever_up,
        provisional_ratio,
    )?;
    if pre_adjustment_nad == 0 {
        return Ok(empty_hlp_rebalance_receipt(target_asset));
    }

    checkpoint_hlp_yield_from_ylp(market, target_asset)?;
    let ylp_shares_before = match target_asset {
        MarketAsset::Base => market.base_hlp_vault.ylp_shares,
        MarketAsset::Quote => market.quote_hlp_vault.ylp_shares,
    };
    let valuation = current_hlp_valuation(market, target_asset)?;
    let ideal_delta = if lever_up {
        i128::try_from(pre_adjustment_nad).map_err(|_| ErrorCode::MarketMathOverflow)?
    } else {
        -i128::try_from(pre_adjustment_nad).map_err(|_| ErrorCode::MarketMathOverflow)?
    };
    let (receipt, post_prices) = if ideal_delta > 0 {
        leverage_up_proportional(market, target_asset, ideal_delta, valuation)?
    } else {
        deleverage_proportional(market, target_asset, ideal_delta, valuation)?
    };
    let ylp_shares_after = match target_asset {
        MarketAsset::Base => market.base_hlp_vault.ylp_shares,
        MarketAsset::Quote => market.quote_hlp_vault.ylp_shares,
    };
    let current_swap_fee_eligible_ylp_shares = if receipt.ylp_mint_amount > 0 {
        ylp_shares_before
    } else {
        ylp_shares_after
    };
    let receipt = HlpRebalanceReceipt {
        current_swap_fee_eligible_ylp_shares,
        nav_nad: valuation.nav_nad,
        ..receipt
    };
    refresh_hlp_after_rebalance(market, target_asset, receipt, post_prices)
}

#[cfg(test)]
fn solve_pre_adjustment_nad(
    market: &Market,
    target_asset: MarketAsset,
    asset_in: MarketAsset,
    amount_in_after_fee: u64,
    equity_nad: u128,
    lever_up: bool,
) -> Result<u128> {
    let solver = CpmmPreSolveSnapshot::new(market, target_asset)?;
    let provisional_ratio =
        solver.simulated_swap_price_ratio_nad(asset_in, amount_in_after_fee, amount_in_after_fee, 0, lever_up)?;
    solve_pre_adjustment_with_snapshot(
        &solver,
        asset_in,
        amount_in_after_fee,
        amount_in_after_fee,
        equity_nad,
        lever_up,
        provisional_ratio,
    )
}

fn solve_pre_adjustment_with_snapshot(
    solver: &CpmmPreSolveSnapshot<'_>,
    asset_in: MarketAsset,
    amount_in_for_quote: u64,
    reserve_input_credit: u64,
    equity_nad: u128,
    lever_up: bool,
    provisional_ratio: u128,
) -> Result<u128> {
    let (guess, guess_lever_up) = closed_form_pre_adjustment_nad(equity_nad, provisional_ratio)?;
    if guess == 0 || guess_lever_up != lever_up {
        return Ok(0);
    }

    let cap = solver.pre_adjustment_cap_nad(lever_up)?;
    if cap == 0 {
        return Ok(0);
    }

    // The no-adjustment closed form is already `needed(0)`. Synthetic
    // deepening normally makes the fixed-point root smaller than that guess,
    // so test the tight bracket first instead of unconditionally doubling it.
    let mut hi = guess.min(cap);
    if hi == 0 {
        return Ok(0);
    }

    for _ in 0..8 {
        let needed = needed_pre_adjustment_with_snapshot(
            solver,
            asset_in,
            amount_in_for_quote,
            reserve_input_credit,
            equity_nad,
            hi,
            lever_up,
        )?;
        if needed <= hi || hi == cap {
            break;
        }
        hi = hi
            .checked_mul(2)
            .and_then(|value| value.checked_add(NAD as u128))
            .unwrap_or(u128::MAX)
            .min(cap);
    }

    // Candidate values are target-value NAD. Once the bracket is at most one
    // whole target unit, further iterations cannot improve the protocol's
    // one-NAD tracking-loss threshold and only burn swap CU.
    let tolerance_nad = NAD as u128;
    let mut lo = 0_u128;
    for _ in 0..HLP_PRE_SOLVE_MAX_ITERS {
        if hi <= lo.saturating_add(tolerance_nad) {
            break;
        }
        let candidate = lo + (hi - lo) / 2;
        let needed = needed_pre_adjustment_with_snapshot(
            solver,
            asset_in,
            amount_in_for_quote,
            reserve_input_credit,
            equity_nad,
            candidate,
            lever_up,
        )?;
        if candidate.abs_diff(needed) <= tolerance_nad {
            return Ok(candidate.min(cap));
        }
        if candidate >= needed {
            hi = candidate;
        } else {
            lo = candidate;
        }
    }
    Ok(hi.min(cap))
}

#[cfg(test)]
fn needed_pre_adjustment_nad(
    market: &Market,
    target_asset: MarketAsset,
    asset_in: MarketAsset,
    amount_in_after_fee: u64,
    equity_nad: u128,
    candidate_nad: u128,
    lever_up: bool,
) -> Result<u128> {
    let solver = CpmmPreSolveSnapshot::new(market, target_asset)?;
    needed_pre_adjustment_with_snapshot(
        &solver,
        asset_in,
        amount_in_after_fee,
        amount_in_after_fee,
        equity_nad,
        candidate_nad,
        lever_up,
    )
}

fn needed_pre_adjustment_with_snapshot(
    solver: &CpmmPreSolveSnapshot<'_>,
    asset_in: MarketAsset,
    amount_in_for_quote: u64,
    reserve_input_credit: u64,
    equity_nad: u128,
    candidate_nad: u128,
    lever_up: bool,
) -> Result<u128> {
    let ratio = solver.simulated_swap_price_ratio_nad(
        asset_in,
        amount_in_for_quote,
        reserve_input_credit,
        candidate_nad,
        lever_up,
    )?;
    let (needed, needed_lever_up) = closed_form_pre_adjustment_nad(equity_nad, ratio)?;
    if needed_lever_up == lever_up {
        Ok(needed)
    } else {
        Ok(0)
    }
}

#[cfg(test)]
fn simulated_swap_price_ratio_nad(
    market: &Market,
    target_asset: MarketAsset,
    asset_in: MarketAsset,
    amount_in_after_fee: u64,
    pre_adjustment_nad: u128,
    lever_up: bool,
) -> Result<u128> {
    simulated_swap_price_ratio_with_reserve_input_nad(
        market,
        target_asset,
        asset_in,
        amount_in_after_fee,
        amount_in_after_fee,
        pre_adjustment_nad,
        lever_up,
    )
}

#[cfg(test)]
fn simulated_swap_price_ratio_with_reserve_input_nad(
    market: &Market,
    target_asset: MarketAsset,
    asset_in: MarketAsset,
    amount_in_for_quote: u64,
    reserve_input_credit: u64,
    pre_adjustment_nad: u128,
    lever_up: bool,
) -> Result<u128> {
    CpmmPreSolveSnapshot::new(market, target_asset)?.simulated_swap_price_ratio_nad(
        asset_in,
        amount_in_for_quote,
        reserve_input_credit,
        pre_adjustment_nad,
        lever_up,
    )
}

/// Heap-minimal immutable context for the bounded CPMM hLP pre-solver.
///
/// Candidate adjustments all start from the same market state, so valuation,
/// interest exclusion, decimals, and center validation are invariant across
/// bisection. The old simulator cloned `Market` and rediscovered those values
/// for every candidate.
struct CpmmPreSolveSnapshot<'a> {
    market: &'a Market,
    target_asset: MarketAsset,
    valuation: HlpValuation,
    base_live_reserve: u64,
    quote_live_reserve: u64,
    base_unrealized_interest: u128,
    quote_unrealized_interest: u128,
    center_price_nad: u64,
}

impl<'a> CpmmPreSolveSnapshot<'a> {
    fn new(market: &'a Market, target_asset: MarketAsset) -> Result<Self> {
        require!(
            market.current_curve_parameters(curve_slot(market)).is_cpmm(),
            ErrorCode::InvalidArgument
        );
        Ok(Self {
            market,
            target_asset,
            valuation: current_hlp_valuation(market, target_asset)?,
            base_live_reserve: market.base_side.reserves.live_reserve,
            quote_live_reserve: market.quote_side.reserves.live_reserve,
            base_unrealized_interest: market.unrealized_interest(MarketAsset::Base)?,
            quote_unrealized_interest: market.unrealized_interest(MarketAsset::Quote)?,
            center_price_nad: market.current_curve_center_price_nad()?,
        })
    }

    fn pre_adjustment_cap_nad(&self, lever_up: bool) -> Result<u128> {
        if lever_up {
            let borrowed_asset = self.target_asset.opposite();
            let borrow_headroom = self.market.side(borrowed_asset).reserves.cash_reserve;
            return asset_value_in_target_nad_with_prices(
                self.market,
                self.valuation.prices,
                borrowed_asset,
                borrow_headroom,
                self.target_asset,
            );
        }

        let collateral = self
            .valuation
            .values
            .target_inventory_value_nad
            .checked_add(self.valuation.values.opposite_inventory_value_nad)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        Ok(self.valuation.values.debt_value_nad.min(collateral))
    }

    fn simulated_swap_price_ratio_nad(
        &self,
        asset_in: MarketAsset,
        amount_in_for_quote: u64,
        reserve_input_credit: u64,
        pre_adjustment_nad: u128,
        lever_up: bool,
    ) -> Result<u128> {
        require_gte!(reserve_input_credit, amount_in_for_quote, ErrorCode::BrokenInvariant);
        let (base_live, quote_live) = self.adjusted_live_reserves(pre_adjustment_nad, lever_up)?;
        // A CPMM hLP pre-adjustment adds or removes proportional reserve
        // claims and is price-neutral by construction. Reuse the frozen
        // pre-solve price; candidate raw-leg rounding is below this solver's
        // one-target-unit stopping tolerance.
        let price_before = self.valuation.prices.for_asset(self.target_asset);
        require!(price_before > 0, ErrorCode::InsufficientLiquidity);

        let (base_after, quote_after) = if amount_in_for_quote == 0 {
            require_eq!(reserve_input_credit, 0, ErrorCode::BrokenInvariant);
            (base_live, quote_live)
        } else {
            let (base_nad, quote_nad) = self.curve_reserves_nad(base_live, quote_live)?;
            let input_nad = normalize_to_nad(amount_in_for_quote as u128, self.market.side(asset_in).asset_decimals)?;
            require!(input_nad > 0, ErrorCode::AmountZero);
            let output_nad = match asset_in {
                MarketAsset::Base => calculate_normalized_amount_out(base_nad, quote_nad, input_nad)?,
                MarketAsset::Quote => calculate_normalized_amount_out(quote_nad, base_nad, input_nad)?,
            };
            let amount_out =
                denormalize_from_nad_floor(output_nad, self.market.side(asset_in.opposite()).asset_decimals)?;
            require!(amount_out > 0, ErrorCode::InsufficientOutputAmount);

            match asset_in {
                MarketAsset::Base => (
                    base_live
                        .checked_add(reserve_input_credit)
                        .ok_or(ErrorCode::ReserveOverflow)?,
                    quote_live.checked_sub(amount_out).ok_or(ErrorCode::ReserveUnderflow)?,
                ),
                MarketAsset::Quote => (
                    base_live.checked_sub(amount_out).ok_or(ErrorCode::ReserveUnderflow)?,
                    quote_live
                        .checked_add(reserve_input_credit)
                        .ok_or(ErrorCode::ReserveOverflow)?,
                ),
            }
        };

        let price_after = self
            .prices_for_live_reserves(base_after, quote_after)?
            .for_asset(self.target_asset);
        price_after
            .checked_mul(NAD as u128)
            .and_then(|value| value.checked_div(price_before))
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    }

    fn adjusted_live_reserves(&self, delta_nad: u128, lever_up: bool) -> Result<(u64, u64)> {
        if delta_nad == 0 {
            return Ok((self.base_live_reserve, self.quote_live_reserve));
        }
        let signed_delta = if lever_up {
            i128::try_from(delta_nad).map_err(|_| ErrorCode::MarketMathOverflow)?
        } else {
            -i128::try_from(delta_nad).map_err(|_| ErrorCode::MarketMathOverflow)?
        };
        let amounts = proportional_rebalance_amounts(self.market, self.target_asset, signed_delta, self.valuation)?;
        if amounts.target_leg_amount == 0 || amounts.borrowed_leg_amount == 0 {
            return Ok((self.base_live_reserve, self.quote_live_reserve));
        }
        let (base_leg_amount, quote_leg_amount) = match self.target_asset {
            MarketAsset::Base => (amounts.target_leg_amount, amounts.borrowed_leg_amount),
            MarketAsset::Quote => (amounts.borrowed_leg_amount, amounts.target_leg_amount),
        };
        if lever_up {
            Ok((
                self.base_live_reserve
                    .checked_add(base_leg_amount)
                    .ok_or(ErrorCode::ReserveOverflow)?,
                self.quote_live_reserve
                    .checked_add(quote_leg_amount)
                    .ok_or(ErrorCode::ReserveOverflow)?,
            ))
        } else {
            Ok((
                self.base_live_reserve
                    .checked_sub(base_leg_amount)
                    .ok_or(ErrorCode::ReserveUnderflow)?,
                self.quote_live_reserve
                    .checked_sub(quote_leg_amount)
                    .ok_or(ErrorCode::ReserveUnderflow)?,
            ))
        }
    }

    fn prices_for_live_reserves(&self, base_live: u64, quote_live: u64) -> Result<HlpCurvePrices> {
        let (base_nad, quote_nad) = self.curve_reserves_nad(base_live, quote_live)?;
        let base_price_nad = concentrated_marginal_price_nad(base_nad, quote_nad, self.center_price_nad as u128, 0, 0)?;
        hlp_curve_prices_from_base_price_nad(base_price_nad)
    }

    fn curve_reserves_nad(&self, base_live: u64, quote_live: u64) -> Result<(u128, u128)> {
        let base_raw = (base_live as u128)
            .checked_sub(self.base_unrealized_interest)
            .ok_or(ErrorCode::BrokenInvariant)?;
        let quote_raw = (quote_live as u128)
            .checked_sub(self.quote_unrealized_interest)
            .ok_or(ErrorCode::BrokenInvariant)?;
        let base_raw = u64::try_from(base_raw).map_err(|_| ErrorCode::MarketMathOverflow)?;
        let quote_raw = u64::try_from(quote_raw).map_err(|_| ErrorCode::MarketMathOverflow)?;
        Ok((
            normalize_to_nad(base_raw as u128, self.market.base_side.asset_decimals)?,
            normalize_to_nad(quote_raw as u128, self.market.quote_side.asset_decimals)?,
        ))
    }
}

#[cfg(test)]
fn stateful_simulated_swap_price_ratio_nad(
    market: &Market,
    target_asset: MarketAsset,
    asset_in: MarketAsset,
    amount_in_for_quote: u64,
    reserve_input_credit: u64,
    pre_adjustment_nad: u128,
    lever_up: bool,
) -> Result<u128> {
    let mut simulated = market.clone();
    apply_stateful_simulated_pre_adjustment(&mut simulated, target_asset, pre_adjustment_nad, lever_up)?;
    let price_before = current_settlement_price_nad(&simulated, target_asset)?;
    require!(price_before > 0, ErrorCode::InsufficientLiquidity);

    if amount_in_for_quote > 0 {
        let quote = simulated.quote_curve_exact_in(asset_in, amount_in_for_quote, curve_slot(&simulated))?;
        simulated.side_mut(asset_in).reserves.live_reserve = simulated
            .side(asset_in)
            .reserves
            .live_reserve
            .checked_add(reserve_input_credit)
            .ok_or(ErrorCode::ReserveOverflow)?;
        let asset_out = asset_in.opposite();
        simulated.side_mut(asset_out).reserves.live_reserve = simulated
            .side(asset_out)
            .reserves
            .live_reserve
            .checked_sub(quote.amount_out)
            .ok_or(ErrorCode::ReserveUnderflow)?;
    }

    let price_after = current_settlement_price_nad(&simulated, target_asset)?;
    price_after
        .checked_mul(NAD as u128)
        .and_then(|value| value.checked_div(price_before))
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

#[cfg(test)]
fn apply_stateful_simulated_pre_adjustment(
    market: &mut Market,
    target_asset: MarketAsset,
    delta_nad: u128,
    lever_up: bool,
) -> Result<()> {
    if delta_nad == 0 {
        return Ok(());
    }
    let signed_delta = if lever_up {
        i128::try_from(delta_nad).map_err(|_| ErrorCode::MarketMathOverflow)?
    } else {
        -i128::try_from(delta_nad).map_err(|_| ErrorCode::MarketMathOverflow)?
    };
    let valuation = current_hlp_valuation(market, target_asset)?;
    let amounts = proportional_rebalance_amounts(market, target_asset, signed_delta, valuation)?;
    if amounts.target_leg_amount == 0 || amounts.borrowed_leg_amount == 0 {
        return Ok(());
    }
    let (base_leg_amount, quote_leg_amount) = match target_asset {
        MarketAsset::Base => (amounts.target_leg_amount, amounts.borrowed_leg_amount),
        MarketAsset::Quote => (amounts.borrowed_leg_amount, amounts.target_leg_amount),
    };
    if lever_up {
        market.base_side.reserves.live_reserve = market
            .base_side
            .reserves
            .live_reserve
            .checked_add(base_leg_amount)
            .ok_or(ErrorCode::ReserveOverflow)?;
        market.quote_side.reserves.live_reserve = market
            .quote_side
            .reserves
            .live_reserve
            .checked_add(quote_leg_amount)
            .ok_or(ErrorCode::ReserveOverflow)?;
    } else {
        market.base_side.reserves.live_reserve = market
            .base_side
            .reserves
            .live_reserve
            .checked_sub(base_leg_amount)
            .ok_or(ErrorCode::ReserveUnderflow)?;
        market.quote_side.reserves.live_reserve = market
            .quote_side
            .reserves
            .live_reserve
            .checked_sub(quote_leg_amount)
            .ok_or(ErrorCode::ReserveUnderflow)?;
    }
    Ok(())
}

fn hlp_rebalance_needed(market: &Market, target_asset: MarketAsset) -> bool {
    match target_asset {
        MarketAsset::Base => market.base_hlp_vault.hlp_supply > 0 || market.base_hlp_vault.pending_rebalance != 0,
        MarketAsset::Quote => market.quote_hlp_vault.hlp_supply > 0 || market.quote_hlp_vault.pending_rebalance != 0,
    }
}

fn empty_hlp_rebalance_receipt(target_asset: MarketAsset) -> HlpRebalanceReceipt {
    HlpRebalanceReceipt {
        target_asset,
        ..HlpRebalanceReceipt::default()
    }
}

fn deposit_base_hlp(
    market: &mut Market,
    base_deposit: u64,
    quote_borrow: u64,
) -> Result<(u64, u64, u64, HlpCurvePrices)> {
    require_hlp_borrow_headroom(&market.quote_side, quote_borrow)?;
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
    let debt_shares = Debt::debt_to_shares(quote_borrow, market.debt.quote_borrow_index_nad)?;
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

fn deposit_quote_hlp(
    market: &mut Market,
    quote_deposit: u64,
    base_borrow: u64,
) -> Result<(u64, u64, u64, HlpCurvePrices)> {
    require_hlp_borrow_headroom(&market.base_side, base_borrow)?;
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
    let debt_shares = Debt::debt_to_shares(base_borrow, market.debt.base_borrow_index_nad)?;
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

fn withdraw_base_hlp(market: &mut Market, hlp_amount: u64) -> Result<HedgeReceipt> {
    let supply = market.base_hlp_vault.hlp_supply;
    require_gte!(supply, hlp_amount, ErrorCode::InsufficientBalance);
    let ylp_amount = proportional(market.base_hlp_vault.ylp_shares, hlp_amount, supply)?;
    let quote_debt_shares = proportional_u128(market.base_hlp_vault.debt_shares, hlp_amount, supply)?;
    let base_out = ylp_live_underlying_amount(market, MarketAsset::Base, ylp_amount)?;
    let quote_redeemed = ylp_live_underlying_amount(market, MarketAsset::Quote, ylp_amount)?;
    let debt_repaid = Debt::shares_to_debt(quote_debt_shares, market.debt.quote_borrow_index_nad)?;
    let debt_repaid = u64::try_from(debt_repaid).map_err(|_| ErrorCode::DebtMathOverflow)?;
    let base_hlp_live_debit = proportional(market.base_hlp_vault.base_hlp_live_reserve, hlp_amount, supply)?;
    let quote_hlp_live_debit = proportional(market.base_hlp_vault.quote_hlp_live_reserve, hlp_amount, supply)?;
    let base_out = settled_close_target_amount(market, MarketAsset::Base, base_out, quote_redeemed, debt_repaid)?;
    let debt_clearance =
        market
            .base_hlp_vault
            .clear_debt_repay(debt_repaid, quote_debt_shares, market.debt.quote_borrow_index_nad)?;
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
        market.base_hlp_vault.last_nav_nad = 0;
        market.base_hlp_vault.cached_settlement_price_nad = 0;
    } else {
        let current_prices = current_hlp_curve_prices(market)?;
        market.base_hlp_vault.last_nav_nad = hlp_nav_nad_with_prices(market, MarketAsset::Base, current_prices)?;
        market.base_hlp_vault.cached_settlement_price_nad = current_prices.for_asset(MarketAsset::Base);
    }
    Ok(HedgeReceipt {
        hlp_amount,
        ylp_amount,
        hlp_supply: market.base_hlp_vault.hlp_supply,
        target_amount_out: base_out,
        debt_repaid: debt_clearance.debt_reduced,
        interest_paid,
        ..HedgeReceipt::default()
    })
}

fn withdraw_quote_hlp(market: &mut Market, hlp_amount: u64) -> Result<HedgeReceipt> {
    let supply = market.quote_hlp_vault.hlp_supply;
    require_gte!(supply, hlp_amount, ErrorCode::InsufficientBalance);
    let ylp_amount = proportional(market.quote_hlp_vault.ylp_shares, hlp_amount, supply)?;
    let base_debt_shares = proportional_u128(market.quote_hlp_vault.debt_shares, hlp_amount, supply)?;
    let quote_out = ylp_live_underlying_amount(market, MarketAsset::Quote, ylp_amount)?;
    let base_redeemed = ylp_live_underlying_amount(market, MarketAsset::Base, ylp_amount)?;
    let debt_repaid = Debt::shares_to_debt(base_debt_shares, market.debt.base_borrow_index_nad)?;
    let debt_repaid = u64::try_from(debt_repaid).map_err(|_| ErrorCode::DebtMathOverflow)?;
    let base_hlp_live_debit = proportional(market.quote_hlp_vault.base_hlp_live_reserve, hlp_amount, supply)?;
    let quote_hlp_live_debit = proportional(market.quote_hlp_vault.quote_hlp_live_reserve, hlp_amount, supply)?;
    let quote_out = settled_close_target_amount(market, MarketAsset::Quote, quote_out, base_redeemed, debt_repaid)?;
    let debt_clearance =
        market
            .quote_hlp_vault
            .clear_debt_repay(debt_repaid, base_debt_shares, market.debt.base_borrow_index_nad)?;
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
        market.quote_hlp_vault.last_nav_nad = 0;
        market.quote_hlp_vault.cached_settlement_price_nad = 0;
    } else {
        let current_prices = current_hlp_curve_prices(market)?;
        market.quote_hlp_vault.last_nav_nad = hlp_nav_nad_with_prices(market, MarketAsset::Quote, current_prices)?;
        market.quote_hlp_vault.cached_settlement_price_nad = current_prices.for_asset(MarketAsset::Quote);
    }
    Ok(HedgeReceipt {
        hlp_amount,
        ylp_amount,
        hlp_supply: market.quote_hlp_vault.hlp_supply,
        target_amount_out: quote_out,
        debt_repaid: debt_clearance.debt_reduced,
        interest_paid,
        ..HedgeReceipt::default()
    })
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

fn debit_hlp_rebalance_reserve(
    market: &mut Market,
    target_asset: MarketAsset,
    reserve_asset: MarketAsset,
    amount: u64,
) -> Result<()> {
    if amount == 0 {
        return Ok(());
    }
    let hlp_live_available = match target_asset {
        MarketAsset::Base => market.base_hlp_vault.hlp_live_reserve(reserve_asset),
        MarketAsset::Quote => market.quote_hlp_vault.hlp_live_reserve(reserve_asset),
    };
    let hlp_live_debit = amount.min(hlp_live_available);
    debit_hlp_live_reserve(market, target_asset, reserve_asset, hlp_live_debit)?;
    let cash_debit = amount
        .checked_sub(hlp_live_debit)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    if cash_debit > 0 {
        market.side_mut(reserve_asset).debit_reserve(cash_debit, true)?;
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
    if borrowed_redeemed == debt_repaid {
        return Ok(target_redeemed);
    }

    // Price the settlement conversion against the executable curve after
    // redeeming the ordinary yLP claim. Global yLP entitlement is always based
    // on live reserves, including accrued-but-unpaid lending interest. Curve
    // reserves remain the quote coordinates after that live claim is removed.
    let borrowed_asset = target_asset.opposite();
    let mut simulated = market.clone();
    simulated.side_mut(target_asset).reserves.live_reserve = simulated
        .side(target_asset)
        .reserves
        .live_reserve
        .checked_sub(target_redeemed)
        .ok_or(ErrorCode::ReserveUnderflow)?;
    simulated.side_mut(borrowed_asset).reserves.live_reserve = simulated
        .side(borrowed_asset)
        .reserves
        .live_reserve
        .checked_sub(borrowed_redeemed)
        .ok_or(ErrorCode::ReserveUnderflow)?;

    if borrowed_redeemed > debt_repaid {
        let surplus_borrowed = borrowed_redeemed
            .checked_sub(debt_repaid)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let target_from_surplus = simulated
            .quote_curve_exact_in(borrowed_asset, surplus_borrowed, curve_slot(&simulated))?
            .amount_out;
        return target_redeemed
            .checked_add(target_from_surplus)
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into());
    }

    let borrowed_shortfall = debt_repaid
        .checked_sub(borrowed_redeemed)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let target_needed = simulated.quote_curve_exact_out(borrowed_asset, borrowed_shortfall, curve_slot(&simulated))?;
    require_gte!(target_redeemed, target_needed, ErrorCode::HlpSettlementUnavailable);
    target_redeemed
        .checked_sub(target_needed)
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

pub(in crate::state::market) fn rebalance_one_hlp(
    market: &mut Market,
    target_asset: MarketAsset,
) -> Result<HlpRebalanceReceipt> {
    checkpoint_hlp_yield_from_ylp(market, target_asset)?;
    let valuation = current_hlp_valuation(market, target_asset)?;
    let ideal_delta = recognized_hlp_pending(valuation.ideal_delta, valuation.nav_nad);
    let (receipt, post_prices) = if !valuation.proportional_hedge_available && ideal_delta != 0 {
        // No finite proportional liquidity change can neutralize opposite
        // exposure when the target-side yLP claim has rounded to zero. Keep a
        // fail-closed pending signal without mutating reserves; importantly,
        // this vault-local condition must not make generic market updates fail.
        (
            HlpRebalanceReceipt {
                target_asset,
                ideal_delta,
                ..HlpRebalanceReceipt::default()
            },
            valuation.prices,
        )
    } else if ideal_delta > 0 {
        leverage_up_proportional(market, target_asset, ideal_delta, valuation)?
    } else if ideal_delta < 0 {
        deleverage_proportional(market, target_asset, ideal_delta, valuation)?
    } else {
        (
            HlpRebalanceReceipt {
                target_asset,
                ..HlpRebalanceReceipt::default()
            },
            valuation.prices,
        )
    };
    let receipt = HlpRebalanceReceipt {
        nav_nad: valuation.nav_nad,
        ..receipt
    };
    refresh_hlp_after_rebalance(market, target_asset, receipt, post_prices)
}

#[cfg(test)]
fn current_hlp_ideal_delta(market: &Market, target_asset: MarketAsset) -> Result<i128> {
    current_hlp_valuation(market, target_asset).map(|valuation| valuation.ideal_delta)
}

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
    let collateral = values
        .target_inventory_value_nad
        .checked_add(values.opposite_inventory_value_nad)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let debt = values.debt_value_nad;
    // An aggregate hLP can become underwater or have its target-side yLP claim
    // round to zero. Those are vault-local fail-closed states, not reasons to
    // brick swaps, withdrawals from the other vault, or global AMM
    // maintenance. A zero NAV keeps new deposits gated by nonzero pending.
    let nav_nad = collateral.saturating_sub(debt);
    let ideal_delta = if values.target_inventory_value_nad == 0 {
        // There is no finite proportional-liquidity solution in this
        // degenerate coordinate. Persist the signed opposite exposure as an
        // explicit nonzero pending signal instead of throwing a denominator
        // error from a generic checkpoint.
        hlp_opposite_exposure_nad(values)?
    } else {
        ideal_hlp_rebalance_nad(values)?.total_liquidity_value_nad
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct HlpCurvePrices {
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
enum HlpEntryDisposition {
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
    const fn admits_entry(self) -> bool {
        matches!(self, Self::Settled | Self::ControllerGranularityLimited)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HlpEntryState {
    disposition: HlpEntryDisposition,
    pending_rebalance: i128,
    nav_nad: u128,
}

fn current_hlp_entry_state_with_prices(
    market: &Market,
    target_asset: MarketAsset,
    prices: HlpCurvePrices,
) -> Result<HlpEntryState> {
    let valuation = current_hlp_valuation_with_prices(market, target_asset, prices)?;
    let pending_rebalance = recognized_hlp_pending(valuation.ideal_delta, valuation.nav_nad);
    let disposition = if pending_rebalance == 0 {
        HlpEntryDisposition::Settled
    } else if !valuation.proportional_hedge_available || valuation.nav_nad == 0 {
        HlpEntryDisposition::Unhedgeable
    } else if pending_rebalance > 0 {
        leverage_up_entry_disposition(market, target_asset, pending_rebalance, valuation)?
    } else {
        deleverage_entry_disposition(market, target_asset, pending_rebalance, valuation)?
    };
    Ok(HlpEntryState {
        disposition,
        pending_rebalance,
        nav_nad: valuation.nav_nad,
    })
}

const fn complete_rebalance_amounts(amounts: ProportionalRebalanceAmounts) -> bool {
    amounts.target_leg_amount > 0 && amounts.borrowed_leg_amount > 0 && amounts.debt_amount > 0
}

fn leverage_up_entry_disposition(
    market: &Market,
    target_asset: MarketAsset,
    ideal_delta: i128,
    valuation: HlpValuation,
) -> Result<HlpEntryDisposition> {
    // Inspect the uncapped ideal plan before looking at cash. Otherwise zero
    // cash and a sub-raw-unit ideal adjustment collapse into the same no-op.
    let amounts = proportional_rebalance_amounts(market, target_asset, ideal_delta, valuation)?;
    if !complete_rebalance_amounts(amounts) {
        return Ok(HlpEntryDisposition::ControllerGranularityLimited);
    }
    let (base_leg, quote_leg) = match target_asset {
        MarketAsset::Base => (amounts.target_leg_amount, amounts.borrowed_leg_amount),
        MarketAsset::Quote => (amounts.borrowed_leg_amount, amounts.target_leg_amount),
    };
    if ylp_for_live_reserve_deposit(market, base_leg, quote_leg)? == 0 {
        return Ok(HlpEntryDisposition::ControllerGranularityLimited);
    }
    if market.side(target_asset.opposite()).reserves.cash_reserve < amounts.debt_amount {
        return Ok(HlpEntryDisposition::CashConstrained);
    }
    Ok(HlpEntryDisposition::Actionable)
}

fn deleverage_entry_disposition(
    market: &Market,
    target_asset: MarketAsset,
    ideal_delta: i128,
    valuation: HlpValuation,
) -> Result<HlpEntryDisposition> {
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
        return Ok(HlpEntryDisposition::Unhedgeable);
    }
    let collateral_value_nad = valuation
        .values
        .target_inventory_value_nad
        .checked_add(valuation.values.opposite_inventory_value_nad)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let requested_delta_nad = ideal_delta.unsigned_abs();
    let capacity_nad = collateral_value_nad.min(valuation.values.debt_value_nad);
    if capacity_nad < requested_delta_nad {
        return Ok(HlpEntryDisposition::Unhedgeable);
    }
    let feasible_delta_nad = requested_delta_nad;
    if feasible_delta_nad == 0 {
        return Ok(HlpEntryDisposition::Unhedgeable);
    }
    let feasible_delta = -i128::try_from(feasible_delta_nad).map_err(|_| ErrorCode::MarketMathOverflow)?;
    let amounts = proportional_rebalance_amounts(market, target_asset, feasible_delta, valuation)?;
    if !complete_rebalance_amounts(amounts) {
        return Ok(HlpEntryDisposition::ControllerGranularityLimited);
    }
    let (base_leg, quote_leg) = match target_asset {
        MarketAsset::Base => (amounts.target_leg_amount, amounts.borrowed_leg_amount),
        MarketAsset::Quote => (amounts.borrowed_leg_amount, amounts.target_leg_amount),
    };
    let base_burn = ylp_shares_for_live_reserve_amount(market, MarketAsset::Base, base_leg)?;
    let quote_burn = ylp_shares_for_live_reserve_amount(market, MarketAsset::Quote, quote_leg)?;
    let ylp_burn = base_burn.min(quote_burn).min(vault_ylp);
    if ylp_burn == 0 {
        return Ok(HlpEntryDisposition::ControllerGranularityLimited);
    }
    let base_out = ylp_live_underlying_amount(market, MarketAsset::Base, ylp_burn)?;
    let quote_out = ylp_live_underlying_amount(market, MarketAsset::Quote, ylp_burn)?;
    if base_out == 0 || quote_out == 0 {
        return Ok(HlpEntryDisposition::ControllerGranularityLimited);
    }
    let (target_out, borrowed_out) = match target_asset {
        MarketAsset::Base => (base_out, quote_out),
        MarketAsset::Quote => (quote_out, base_out),
    };
    let borrowed_asset = target_asset.opposite();
    let removed_value_nad =
        asset_value_in_target_nad_with_prices(market, valuation.prices, target_asset, target_out, target_asset)?
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
        return Ok(HlpEntryDisposition::ControllerGranularityLimited);
    }
    let (_, interest_paid) = crate::math::realized_interest_split(repay, current_debt_nadless, debt_principal)?;
    if market.side(borrowed_asset).reserves.cash_reserve < interest_paid {
        return Ok(HlpEntryDisposition::CashConstrained);
    }
    Ok(HlpEntryDisposition::Actionable)
}

/// Called after `checkpoint_hlp_vaults` refreshes the target vault. Nonzero
/// controller-resolution residuals may admit a strictly non-worsening top-up;
/// actionable, cash-constrained, and unhedgeable states remain fail closed.
pub(in crate::state::market) fn require_hlp_entry_exposure_current(
    market: &Market,
    target_asset: MarketAsset,
) -> Result<()> {
    let state = current_hlp_entry_state_with_prices(market, target_asset, current_hlp_curve_prices(market)?)?;
    require!(state.disposition.admits_entry(), ErrorCode::HlpSettlementUnavailable);
    Ok(())
}

fn leverage_up_proportional(
    market: &mut Market,
    target_asset: MarketAsset,
    ideal_delta: i128,
    valuation: HlpValuation,
) -> Result<(HlpRebalanceReceipt, HlpCurvePrices)> {
    let feasible_delta_nad =
        feasible_leverage_up_value_nad(market, target_asset, ideal_delta.unsigned_abs(), valuation.prices)?;
    let feasible_delta = i128::try_from(feasible_delta_nad).map_err(|_| ErrorCode::MarketMathOverflow)?;
    let amounts = proportional_rebalance_amounts(market, target_asset, feasible_delta, valuation)?;
    if amounts.target_leg_amount == 0 || amounts.borrowed_leg_amount == 0 || amounts.debt_amount == 0 {
        return Ok((
            HlpRebalanceReceipt {
                target_asset,
                ideal_delta,
                ..HlpRebalanceReceipt::default()
            },
            valuation.prices,
        ));
    }
    let borrowed_asset = target_asset.opposite();
    require_hlp_borrow_headroom(market.side(borrowed_asset), amounts.debt_amount)?;
    let (base_leg_amount, quote_leg_amount) = match target_asset {
        MarketAsset::Base => (amounts.target_leg_amount, amounts.borrowed_leg_amount),
        MarketAsset::Quote => (amounts.borrowed_leg_amount, amounts.target_leg_amount),
    };
    let ylp_amount = ylp_for_live_reserve_deposit(market, base_leg_amount, quote_leg_amount)?;
    if ylp_amount == 0 {
        return Ok((
            HlpRebalanceReceipt {
                target_asset,
                ideal_delta,
                ..HlpRebalanceReceipt::default()
            },
            valuation.prices,
        ));
    }
    credit_hlp_live_reserve(market, target_asset, MarketAsset::Base, base_leg_amount)?;
    credit_hlp_live_reserve(market, target_asset, MarketAsset::Quote, quote_leg_amount)?;
    market.base_side.shares.mint(ylp_amount)?;
    market.quote_side.shares.mint(ylp_amount)?;
    market.base_side.assert_share_backing()?;
    market.quote_side.assert_share_backing()?;

    let debt_shares = match target_asset {
        MarketAsset::Base => Debt::debt_to_shares(amounts.debt_amount, market.debt.quote_borrow_index_nad)?,
        MarketAsset::Quote => Debt::debt_to_shares(amounts.debt_amount, market.debt.base_borrow_index_nad)?,
    };
    match target_asset {
        MarketAsset::Base => {
            market.base_hlp_vault.add_debt_shares(debt_shares)?;
            market.base_hlp_vault.add_debt_principal(amounts.debt_amount)?;
            market.base_hlp_vault.credit_ylp(ylp_amount)?;
        }
        MarketAsset::Quote => {
            market.quote_hlp_vault.add_debt_shares(debt_shares)?;
            market.quote_hlp_vault.add_debt_principal(amounts.debt_amount)?;
            market.quote_hlp_vault.credit_ylp(ylp_amount)?;
        }
    }
    let post_prices = current_hlp_curve_prices(market)?;
    Ok((
        HlpRebalanceReceipt {
            target_asset,
            ideal_delta,
            ylp_mint_amount: ylp_amount,
            debt_delta: amounts.debt_amount as i128,
            ..HlpRebalanceReceipt::default()
        },
        post_prices,
    ))
}

fn feasible_leverage_up_value_nad(
    market: &Market,
    target_asset: MarketAsset,
    requested_delta_nad: u128,
    prices: HlpCurvePrices,
) -> Result<u128> {
    let borrow_headroom = market.side(target_asset.opposite()).reserves.cash_reserve;
    if borrow_headroom == 0 {
        return Ok(0);
    }
    let headroom_value_nad =
        asset_value_in_target_nad_with_prices(market, prices, target_asset.opposite(), borrow_headroom, target_asset)?;
    Ok(requested_delta_nad.min(headroom_value_nad))
}

fn deleverage_proportional(
    market: &mut Market,
    target_asset: MarketAsset,
    ideal_delta: i128,
    valuation: HlpValuation,
) -> Result<(HlpRebalanceReceipt, HlpCurvePrices)> {
    let borrowed_asset = target_asset.opposite();

    let (borrow_index, debt_shares, vault_ylp) = match target_asset {
        MarketAsset::Base => (
            market.debt.quote_borrow_index_nad,
            market.base_hlp_vault.debt_shares,
            market.base_hlp_vault.ylp_shares,
        ),
        MarketAsset::Quote => (
            market.debt.base_borrow_index_nad,
            market.quote_hlp_vault.debt_shares,
            market.quote_hlp_vault.ylp_shares,
        ),
    };
    let current_debt = Debt::shares_to_debt(debt_shares, borrow_index)?;
    let current_debt = u64::try_from(current_debt).unwrap_or(u64::MAX);
    let collateral_value_nad = valuation
        .values
        .target_inventory_value_nad
        .checked_add(valuation.values.opposite_inventory_value_nad)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let feasible_delta_nad = ideal_delta
        .unsigned_abs()
        .min(collateral_value_nad)
        .min(valuation.values.debt_value_nad);
    let feasible_delta = -i128::try_from(feasible_delta_nad).map_err(|_| ErrorCode::MarketMathOverflow)?;
    let amounts = proportional_rebalance_amounts(market, target_asset, feasible_delta, valuation)?;
    if amounts.target_leg_amount == 0 || amounts.borrowed_leg_amount == 0 || amounts.debt_amount == 0 {
        return Ok((
            HlpRebalanceReceipt {
                target_asset,
                ideal_delta,
                ..HlpRebalanceReceipt::default()
            },
            valuation.prices,
        ));
    }

    let (base_leg_amount, quote_leg_amount) = match target_asset {
        MarketAsset::Base => (amounts.target_leg_amount, amounts.borrowed_leg_amount),
        MarketAsset::Quote => (amounts.borrowed_leg_amount, amounts.target_leg_amount),
    };
    let base_ylp_burn = ylp_shares_for_live_reserve_amount(market, MarketAsset::Base, base_leg_amount)?;
    let quote_ylp_burn = ylp_shares_for_live_reserve_amount(market, MarketAsset::Quote, quote_leg_amount)?;
    let ylp_burn = base_ylp_burn.min(quote_ylp_burn).min(vault_ylp);
    if ylp_burn == 0 {
        return Ok((
            HlpRebalanceReceipt {
                target_asset,
                ideal_delta,
                ..HlpRebalanceReceipt::default()
            },
            valuation.prices,
        ));
    }
    // Burn an exact ordinary-yLP entitlement. Curve inventory determined the
    // desired adjustment above; the global share burn itself must use the same
    // live-reserve basis as every other yLP mint, burn, and redemption.
    let base_leg_amount = ylp_live_underlying_amount(market, MarketAsset::Base, ylp_burn)?;
    let quote_leg_amount = ylp_live_underlying_amount(market, MarketAsset::Quote, ylp_burn)?;
    let (target_leg_amount, borrowed_leg_amount) = match target_asset {
        MarketAsset::Base => (base_leg_amount, quote_leg_amount),
        MarketAsset::Quote => (quote_leg_amount, base_leg_amount),
    };
    let removed_value_nad =
        asset_value_in_target_nad_with_prices(market, valuation.prices, target_asset, target_leg_amount, target_asset)?
            .checked_add(asset_value_in_target_nad_with_prices(
                market,
                valuation.prices,
                borrowed_asset,
                borrowed_leg_amount,
                target_asset,
            )?)
            .ok_or(ErrorCode::MarketMathOverflow)?;
    let repay_amount = raw_amount_from_target_value_nad_with_prices(
        market,
        valuation.prices,
        borrowed_asset,
        target_asset,
        removed_value_nad,
    )?
    .min(current_debt);
    if repay_amount == 0 {
        return Ok((
            HlpRebalanceReceipt {
                target_asset,
                ideal_delta,
                ..HlpRebalanceReceipt::default()
            },
            valuation.prices,
        ));
    }
    debit_hlp_rebalance_reserve(market, target_asset, MarketAsset::Base, base_leg_amount)?;
    debit_hlp_rebalance_reserve(market, target_asset, MarketAsset::Quote, quote_leg_amount)?;
    market.base_side.shares.burn(ylp_burn)?;
    market.quote_side.shares.burn(ylp_burn)?;
    market.base_side.assert_share_backing()?;
    market.quote_side.assert_share_backing()?;

    let debt_shares_to_remove = Debt::debt_to_shares(repay_amount, borrow_index)?.min(debt_shares);
    let debt_clearance = match target_asset {
        MarketAsset::Base => {
            let clearance =
                market
                    .base_hlp_vault
                    .clear_debt_repay(repay_amount, debt_shares_to_remove, borrow_index)?;
            debit_cash_for_hlp_interest(&mut market.quote_side, clearance.interest_paid)?;
            market.base_hlp_vault.debit_ylp(ylp_burn)?;
            clearance
        }
        MarketAsset::Quote => {
            let clearance =
                market
                    .quote_hlp_vault
                    .clear_debt_repay(repay_amount, debt_shares_to_remove, borrow_index)?;
            debit_cash_for_hlp_interest(&mut market.base_side, clearance.interest_paid)?;
            market.quote_hlp_vault.debit_ylp(ylp_burn)?;
            clearance
        }
    };
    let post_prices = current_hlp_curve_prices(market)?;
    Ok((
        HlpRebalanceReceipt {
            target_asset,
            ideal_delta,
            ylp_burn_amount: ylp_burn,
            debt_delta: -(debt_clearance.debt_reduced as i128),
            interest_paid: debt_clearance.interest_paid,
            ..HlpRebalanceReceipt::default()
        },
        post_prices,
    ))
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
    let allocation = allocate_hlp_proportional_adjustment_nad(valuation.values, total_value_delta_nad)?;
    let target_value_delta = allocation.target_inventory_value_delta_nad.unsigned_abs();
    let borrowed_value_delta = allocation.opposite_inventory_value_delta_nad.unsigned_abs();
    let total_value_delta = allocation.debt_value_delta_nad.unsigned_abs();
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
    mut receipt: HlpRebalanceReceipt,
    post_prices: HlpCurvePrices,
) -> Result<HlpRebalanceReceipt> {
    let settlement_price = post_prices.for_asset(target_asset);
    // Revalue actual post-mutation inventory and debt. Requested raw amounts
    // do not capture debt-share, yLP-share, and reserve-rounding effects.
    let post_valuation = current_hlp_valuation_with_prices(market, target_asset, post_prices)?;
    let actual_pending_rebalance = post_valuation.ideal_delta;
    let pending_rebalance = recognized_hlp_pending(actual_pending_rebalance, post_valuation.nav_nad);
    if !post_valuation.proportional_hedge_available && pending_rebalance != 0 {
        // `ideal_delta` normally means total proportional liquidity value. In
        // the zero-target coordinate the persisted value is instead the
        // signed unhedgeable O-D exposure. Do not subtract unlike control
        // coordinates and report fictitious execution.
        receipt.ideal_delta = pending_rebalance;
        receipt.executed_delta = 0;
    } else {
        receipt.executed_delta = receipt
            .ideal_delta
            .checked_sub(pending_rebalance)
            .ok_or(ErrorCode::MarketMathOverflow)?;
    }
    let vault = match target_asset {
        MarketAsset::Base => &mut market.base_hlp_vault,
        MarketAsset::Quote => &mut market.quote_hlp_vault,
    };
    vault.last_nav_nad = post_valuation.nav_nad;
    vault.pending_rebalance = pending_rebalance;
    // Only a fully settled hedge earns a new settlement reference. Advancing
    // it after a partial or no-op crank would let repeated worsening flow
    // ratchet the divergence band around stale inventory.
    if pending_rebalance == 0 {
        vault.cached_settlement_price_nad = settlement_price;
    }
    receipt.pending_rebalance = pending_rebalance;
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

fn require_hlp_borrow_headroom(side: &crate::state::MarketSide, amount: u64) -> Result<()> {
    require_gte!(
        side.reserves.cash_reserve,
        amount,
        ErrorCode::InsufficientBorrowHeadroom
    );
    Ok(())
}

fn checkpoint_one_hlp_with_prices(
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
    let ideal_delta = recognized_hlp_pending(valuation.ideal_delta, nav);
    vault.last_nav_nad = nav;
    vault.pending_rebalance = ideal_delta;
    // This reference belongs to the last actual hLP settlement/rebalance.
    // Updating it during a generic market checkpoint would make the later
    // settlement-divergence guard compare the current price with itself.
    Ok(ideal_delta)
}

pub(in crate::state::market) fn checkpoint_hlp_yield_from_ylp(
    market: &mut Market,
    target_asset: MarketAsset,
) -> Result<()> {
    let ylp_shares = match target_asset {
        MarketAsset::Base => market.base_hlp_vault.ylp_shares,
        MarketAsset::Quote => market.quote_hlp_vault.ylp_shares,
    };
    checkpoint_hlp_yield_from_ylp_shares(market, target_asset, ylp_shares)
}

pub(in crate::state::market) fn checkpoint_hlp_yield_from_ylp_shares(
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
fn current_hlp_curve_prices(market: &Market) -> Result<HlpCurvePrices> {
    hlp_curve_prices_from_base_price_nad(market.curve_marginal_price_nad(curve_slot(market))? as u128)
}

fn hlp_curve_prices_from_base_price_nad(base_in_quote_nad: u128) -> Result<HlpCurvePrices> {
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
    side.shares.reserve_for_burn(side.reserves.live_reserve, ylp_amount)
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
    amount_nad
        .checked_mul(price_nad)
        .and_then(|value| value.checked_div(NAD as u128))
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
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
    let debt_amount = hlp_debt_amount(market, target_asset)?;
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
        value_nad
            .checked_mul(NAD as u128)
            .ok_or(ErrorCode::MarketMathOverflow)?
            / price_nad
    };
    denormalize_from_nad_floor(amount_nad, market.side(asset).asset_decimals)
}

fn proportional_opposite_leg_amount(market: &Market, target_asset: MarketAsset, target_amount: u64) -> Result<u64> {
    let target_reserve = market.curve_reserve(target_asset)?;
    let opposite_reserve = market.curve_reserve(target_asset.opposite())?;
    require!(
        target_reserve > 0 && opposite_reserve > 0,
        ErrorCode::InsufficientLiquidity
    );
    let amount = (target_amount as u128)
        .checked_mul(opposite_reserve as u128)
        .and_then(|value| value.checked_div(target_reserve as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    u64::try_from(amount).map_err(|_| ErrorCode::MarketMathOverflow.into())
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
    include!("../../../tests/transitions/hedge.rs");
}
