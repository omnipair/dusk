use anchor_lang::prelude::*;

use crate::{
    constants::{HLP_PRE_SOLVE_LOSS_THRESHOLD_NAD, HLP_PRE_SOLVE_MAX_ITERS, NAD},
    errors::ErrorCode,
    math::{
        bisect, calculate_normalized_amount_in, calculate_raw_amount_out, closed_form_pre_adjustment_nad,
        denormalize_from_nad_floor, market_spot_price_nad, normalize_to_nad, tracking_loss_nad,
    },
    shared::math::ceil_div,
    state::{Debt, HlpVault, Market, MarketAsset},
};

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
        let borrowed_amount = market.spot_value_in_opposite(self.target_asset, self.deposit_amount)?;
        require!(borrowed_amount > 0, ErrorCode::InsufficientLiquidity);
        checkpoint_hlp_yield_from_ylp(market, self.target_asset)?;

        let (ylp_amount, hlp_amount, hlp_supply) = match self.target_asset {
            MarketAsset::Base => deposit_base_hlp(market, self.deposit_amount, borrowed_amount)?,
            MarketAsset::Quote => deposit_quote_hlp(market, self.deposit_amount, borrowed_amount)?,
        };
        require_gte!(hlp_amount, self.min_hlp_amount, ErrorCode::SlippageExceeded);
        let health = market.refresh_market_health()?;
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
        require_hlp_settlement_available(market, self.target_asset)?;
        checkpoint_hlp_yield_from_ylp(market, self.target_asset)?;
        let receipt = match self.target_asset {
            MarketAsset::Base => withdraw_base_hlp(market, self.hlp_amount)?,
            MarketAsset::Quote => withdraw_quote_hlp(market, self.hlp_amount)?,
        };
        market.refresh_risk()?;
        market.assert_virtual_reserve_invariant(MarketAsset::Base)?;
        market.assert_virtual_reserve_invariant(MarketAsset::Quote)?;
        Ok(receipt)
    }
}

pub(in crate::state::market) fn checkpoint_hlp_vaults(market: &mut Market, current_slot: u64) -> Result<(i128, i128)> {
    let base_delta = checkpoint_one_hlp(market, MarketAsset::Base, current_slot)?;
    let quote_delta = checkpoint_one_hlp(market, MarketAsset::Quote, current_slot)?;
    Ok((base_delta, quote_delta))
}

pub(in crate::state::market) fn rebalance_hlp_vaults(
    market: &mut Market,
    current_slot: u64,
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
        rebalance_one_hlp(market, MarketAsset::Base, current_slot)?
    } else {
        empty_hlp_rebalance_receipt(MarketAsset::Base)
    };
    let quote_receipt = if market.quote_hlp_vault.hlp_supply > 0 || market.quote_hlp_vault.pending_rebalance != 0 {
        rebalance_one_hlp(market, MarketAsset::Quote, current_slot)?
    } else {
        empty_hlp_rebalance_receipt(MarketAsset::Quote)
    };
    Ok((base_receipt, quote_receipt))
}

pub(in crate::state::market) fn rebalance_hlp_vault_for_swap(
    market: &mut Market,
    preferred_asset: MarketAsset,
    current_slot: u64,
) -> Result<(HlpRebalanceReceipt, HlpRebalanceReceipt)> {
    // Keep swap-triggered hLP rebalancing bounded for SBF heap: one vault per swap.
    let base_needed = hlp_rebalance_needed(market, MarketAsset::Base);
    let quote_needed = hlp_rebalance_needed(market, MarketAsset::Quote);
    if !base_needed && !quote_needed {
        return Ok((
            empty_hlp_rebalance_receipt(MarketAsset::Base),
            empty_hlp_rebalance_receipt(MarketAsset::Quote),
        ));
    }
    let target_asset = if hlp_rebalance_needed(market, preferred_asset) {
        preferred_asset
    } else {
        preferred_asset.opposite()
    };
    let receipt = rebalance_one_hlp(market, target_asset, current_slot)?;
    match target_asset {
        MarketAsset::Base => Ok((receipt, empty_hlp_rebalance_receipt(MarketAsset::Quote))),
        MarketAsset::Quote => Ok((empty_hlp_rebalance_receipt(MarketAsset::Base), receipt)),
    }
}

pub(in crate::state::market) fn pre_solve_hlp_vaults_for_swap(
    market: &mut Market,
    asset_in: MarketAsset,
    amount_in_after_fee: u64,
    current_slot: u64,
) -> Result<(HlpRebalanceReceipt, HlpRebalanceReceipt)> {
    if amount_in_after_fee == 0 {
        return Ok((
            empty_hlp_rebalance_receipt(MarketAsset::Base),
            empty_hlp_rebalance_receipt(MarketAsset::Quote),
        ));
    }

    let base_receipt =
        pre_solve_one_hlp_for_swap(market, MarketAsset::Base, asset_in, amount_in_after_fee, current_slot)?;
    let quote_receipt =
        pre_solve_one_hlp_for_swap(market, MarketAsset::Quote, asset_in, amount_in_after_fee, current_slot)?;
    Ok((base_receipt, quote_receipt))
}

fn pre_solve_one_hlp_for_swap(
    market: &mut Market,
    target_asset: MarketAsset,
    asset_in: MarketAsset,
    amount_in_after_fee: u64,
    current_slot: u64,
) -> Result<HlpRebalanceReceipt> {
    if !hlp_rebalance_needed(market, target_asset) {
        return Ok(empty_hlp_rebalance_receipt(target_asset));
    }

    let equity_nad = hlp_nav_nad(market, target_asset)?;
    if equity_nad == 0 {
        return Ok(empty_hlp_rebalance_receipt(target_asset));
    }

    let provisional_ratio =
        simulated_swap_price_ratio_nad(market, target_asset, asset_in, amount_in_after_fee, 0, true)?;
    let estimated_loss = tracking_loss_nad(equity_nad, provisional_ratio)?;
    if estimated_loss <= HLP_PRE_SOLVE_LOSS_THRESHOLD_NAD {
        return Ok(empty_hlp_rebalance_receipt(target_asset));
    }

    let (_, lever_up) = closed_form_pre_adjustment_nad(equity_nad, provisional_ratio)?;
    let pre_adjustment_nad = solve_pre_adjustment_nad(
        market,
        target_asset,
        asset_in,
        amount_in_after_fee,
        equity_nad,
        lever_up,
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
    let receipt = if ideal_delta > 0 {
        leverage_up_balanced(market, target_asset, ideal_delta)?
    } else {
        deleverage_balanced(market, target_asset, ideal_delta)?
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
    refresh_hlp_after_rebalance(market, target_asset, current_slot, receipt)
}

fn solve_pre_adjustment_nad(
    market: &Market,
    target_asset: MarketAsset,
    asset_in: MarketAsset,
    amount_in_after_fee: u64,
    equity_nad: u128,
    lever_up: bool,
) -> Result<u128> {
    let provisional_ratio =
        simulated_swap_price_ratio_nad(market, target_asset, asset_in, amount_in_after_fee, 0, lever_up)?;
    let (guess, guess_lever_up) = closed_form_pre_adjustment_nad(equity_nad, provisional_ratio)?;
    if guess == 0 || guess_lever_up != lever_up {
        return Ok(0);
    }

    let cap = pre_adjustment_cap_nad(market, target_asset, lever_up)?;
    if cap == 0 {
        return Ok(0);
    }

    let mut hi = guess
        .checked_mul(2)
        .and_then(|value| value.checked_add(NAD as u128))
        .unwrap_or(u128::MAX)
        .min(cap);
    if hi == 0 {
        return Ok(0);
    }

    for _ in 0..8 {
        let needed = needed_pre_adjustment_nad(
            market,
            target_asset,
            asset_in,
            amount_in_after_fee,
            equity_nad,
            hi,
            lever_up,
        )?;
        if needed <= hi || hi == cap {
            break;
        }
        hi = hi.saturating_mul(2).min(cap);
    }

    bisect(0, hi, HLP_PRE_SOLVE_MAX_ITERS, |candidate| {
        let needed = needed_pre_adjustment_nad(
            market,
            target_asset,
            asset_in,
            amount_in_after_fee,
            equity_nad,
            candidate,
            lever_up,
        )?;
        let candidate = i128::try_from(candidate).map_err(|_| ErrorCode::MarketMathOverflow)?;
        let needed = i128::try_from(needed).map_err(|_| ErrorCode::MarketMathOverflow)?;
        candidate
            .checked_sub(needed)
            .ok_or(ErrorCode::MarketMathOverflow.into())
    })
    .map(|amount| amount.min(cap))
}

fn needed_pre_adjustment_nad(
    market: &Market,
    target_asset: MarketAsset,
    asset_in: MarketAsset,
    amount_in_after_fee: u64,
    equity_nad: u128,
    candidate_nad: u128,
    lever_up: bool,
) -> Result<u128> {
    let ratio = simulated_swap_price_ratio_nad(
        market,
        target_asset,
        asset_in,
        amount_in_after_fee,
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

fn pre_adjustment_cap_nad(market: &Market, target_asset: MarketAsset, lever_up: bool) -> Result<u128> {
    if lever_up {
        let borrowed_asset = target_asset.opposite();
        let borrow_headroom = market.side(borrowed_asset)?.reserves.cash_reserve;
        return asset_value_in_target_nad(market, borrowed_asset, borrow_headroom, target_asset);
    }

    let debt = hlp_debt_value_nad(market, target_asset)?;
    let collateral = match target_asset {
        MarketAsset::Base => hlp_collateral_value_nad(market, MarketAsset::Base, &market.base_hlp_vault)?,
        MarketAsset::Quote => hlp_collateral_value_nad(market, MarketAsset::Quote, &market.quote_hlp_vault)?,
    };
    Ok(debt.min(collateral))
}

fn simulated_swap_price_ratio_nad(
    market: &Market,
    target_asset: MarketAsset,
    asset_in: MarketAsset,
    amount_in_after_fee: u64,
    pre_adjustment_nad: u128,
    lever_up: bool,
) -> Result<u128> {
    let mut base_side = market.base_side;
    let mut quote_side = market.quote_side;
    apply_simulated_pre_adjustment(
        market,
        target_asset,
        pre_adjustment_nad,
        lever_up,
        &mut base_side,
        &mut quote_side,
    )?;

    let price_before = spot_price_for_target_nad(&base_side, &quote_side, target_asset)?;
    require!(price_before > 0, ErrorCode::InsufficientLiquidity);

    if amount_in_after_fee > 0 {
        let (side_in, side_out) = match asset_in {
            MarketAsset::Base => (&mut base_side, &mut quote_side),
            MarketAsset::Quote => (&mut quote_side, &mut base_side),
        };
        let amount_out = calculate_raw_amount_out(
            side_in.reserves.live_reserve,
            side_out.reserves.live_reserve,
            amount_in_after_fee,
        )?;
        side_in.reserves.live_reserve = side_in
            .reserves
            .live_reserve
            .checked_add(amount_in_after_fee)
            .ok_or(ErrorCode::ReserveOverflow)?;
        side_out.reserves.live_reserve = side_out
            .reserves
            .live_reserve
            .checked_sub(amount_out)
            .ok_or(ErrorCode::ReserveUnderflow)?;
    }

    let price_after = spot_price_for_target_nad(&base_side, &quote_side, target_asset)?;
    price_after
        .checked_mul(NAD as u128)
        .and_then(|value| value.checked_div(price_before))
        .ok_or(ErrorCode::MarketMathOverflow.into())
}

fn apply_simulated_pre_adjustment(
    market: &Market,
    target_asset: MarketAsset,
    delta_nad: u128,
    lever_up: bool,
    base_side: &mut crate::state::MarketSide,
    quote_side: &mut crate::state::MarketSide,
) -> Result<()> {
    if delta_nad == 0 {
        return Ok(());
    }
    let target_total_amount = target_raw_amount_from_delta(market, target_asset, delta_nad)?;
    let target_leg_amount = target_total_amount / 2;
    if target_leg_amount == 0 {
        return Ok(());
    }
    let borrowed_leg_amount = simulated_spot_value_in_opposite(base_side, quote_side, target_asset, target_leg_amount)?;
    let (base_leg_amount, quote_leg_amount) = match target_asset {
        MarketAsset::Base => (target_leg_amount, borrowed_leg_amount),
        MarketAsset::Quote => (borrowed_leg_amount, target_leg_amount),
    };

    if lever_up {
        base_side.reserves.live_reserve = base_side
            .reserves
            .live_reserve
            .checked_add(base_leg_amount)
            .ok_or(ErrorCode::ReserveOverflow)?;
        quote_side.reserves.live_reserve = quote_side
            .reserves
            .live_reserve
            .checked_add(quote_leg_amount)
            .ok_or(ErrorCode::ReserveOverflow)?;
    } else {
        base_side.reserves.live_reserve = base_side
            .reserves
            .live_reserve
            .checked_sub(base_leg_amount)
            .ok_or(ErrorCode::ReserveUnderflow)?;
        quote_side.reserves.live_reserve = quote_side
            .reserves
            .live_reserve
            .checked_sub(quote_leg_amount)
            .ok_or(ErrorCode::ReserveUnderflow)?;
    }
    Ok(())
}

fn simulated_spot_value_in_opposite(
    base_side: &crate::state::MarketSide,
    quote_side: &crate::state::MarketSide,
    asset: MarketAsset,
    amount: u64,
) -> Result<u64> {
    let (from_reserve, to_reserve) = match asset {
        MarketAsset::Base => (base_side.reserves.live_reserve, quote_side.reserves.live_reserve),
        MarketAsset::Quote => (quote_side.reserves.live_reserve, base_side.reserves.live_reserve),
    };
    require!(from_reserve > 0 && to_reserve > 0, ErrorCode::InsufficientLiquidity);
    let value = (amount as u128)
        .checked_mul(to_reserve as u128)
        .and_then(|value| value.checked_div(from_reserve as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    u64::try_from(value).map_err(|_| ErrorCode::MarketMathOverflow.into())
}

fn spot_price_for_target_nad(
    base_side: &crate::state::MarketSide,
    quote_side: &crate::state::MarketSide,
    target_asset: MarketAsset,
) -> Result<u128> {
    match target_asset {
        MarketAsset::Base => market_spot_price_nad(base_side, quote_side).map(u128::from),
        MarketAsset::Quote => market_spot_price_nad(quote_side, base_side).map(u128::from),
    }
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

fn deposit_base_hlp(market: &mut Market, base_deposit: u64, quote_borrow: u64) -> Result<(u64, u64, u64)> {
    require_hlp_borrow_headroom(&market.quote_side, quote_borrow)?;
    let hlp_supply_before = market.base_hlp_vault.hlp_supply;
    let nav_before_nad = if hlp_supply_before == 0 {
        0
    } else if market.base_hlp_vault.last_nav_nad > 0 {
        market.base_hlp_vault.last_nav_nad
    } else {
        hlp_nav_nad(market, MarketAsset::Base)?
    };
    let base_reserve_before = market.base_side.reserves.live_reserve;
    let quote_reserve_before = market.quote_side.reserves.live_reserve;
    let ylp_amount = market.ylp_for_deposit(base_reserve_before, quote_reserve_before, base_deposit, quote_borrow)?;
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
    let current_nav_nad = hlp_nav_nad(market, MarketAsset::Base)?;
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
    market.base_hlp_vault.cached_settlement_price_nad = current_settlement_price_nad(market, MarketAsset::Base)?;
    Ok((ylp_amount, hlp_amount, market.base_hlp_vault.hlp_supply))
}

fn deposit_quote_hlp(market: &mut Market, quote_deposit: u64, base_borrow: u64) -> Result<(u64, u64, u64)> {
    require_hlp_borrow_headroom(&market.base_side, base_borrow)?;
    let hlp_supply_before = market.quote_hlp_vault.hlp_supply;
    let nav_before_nad = if hlp_supply_before == 0 {
        0
    } else if market.quote_hlp_vault.last_nav_nad > 0 {
        market.quote_hlp_vault.last_nav_nad
    } else {
        hlp_nav_nad(market, MarketAsset::Quote)?
    };
    let base_reserve_before = market.base_side.reserves.live_reserve;
    let quote_reserve_before = market.quote_side.reserves.live_reserve;
    let ylp_amount = market.ylp_for_deposit(base_reserve_before, quote_reserve_before, base_borrow, quote_deposit)?;
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
    let current_nav_nad = hlp_nav_nad(market, MarketAsset::Quote)?;
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
    market.quote_hlp_vault.cached_settlement_price_nad = current_settlement_price_nad(market, MarketAsset::Quote)?;
    Ok((ylp_amount, hlp_amount, market.quote_hlp_vault.hlp_supply))
}

fn withdraw_base_hlp(market: &mut Market, hlp_amount: u64) -> Result<HedgeReceipt> {
    let supply = market.base_hlp_vault.hlp_supply;
    require_gte!(supply, hlp_amount, ErrorCode::InsufficientBalance);
    let ylp_amount = proportional(market.base_hlp_vault.ylp_shares, hlp_amount, supply)?;
    let quote_debt_shares = proportional_u128(market.base_hlp_vault.debt_shares, hlp_amount, supply)?;
    let base_out = market
        .base_side
        .shares
        .reserve_for_burn(market.base_side.reserves.live_reserve, ylp_amount)?;
    let quote_redeemed = market
        .quote_side
        .shares
        .reserve_for_burn(market.quote_side.reserves.live_reserve, ylp_amount)?;
    let debt_repaid = Debt::shares_to_debt(quote_debt_shares, market.debt.quote_borrow_index_nad)?;
    let debt_repaid = u64::try_from(debt_repaid).map_err(|_| ErrorCode::DebtMathOverflow)?;
    let base_hlp_live_debit = proportional(market.base_hlp_vault.base_hlp_live_reserve, hlp_amount, supply)?;
    let quote_hlp_live_debit = proportional(market.base_hlp_vault.quote_hlp_live_reserve, hlp_amount, supply)?;
    let base_out = settled_close_target_amount(
        &market.base_side,
        &market.quote_side,
        base_out,
        quote_redeemed,
        debt_repaid,
    )?;
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
    market.base_hlp_vault.last_nav_nad = hlp_nav_nad(market, MarketAsset::Base)?;
    market.base_hlp_vault.cached_settlement_price_nad = current_settlement_price_nad(market, MarketAsset::Base)?;
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
    let quote_out = market
        .quote_side
        .shares
        .reserve_for_burn(market.quote_side.reserves.live_reserve, ylp_amount)?;
    let base_redeemed = market
        .base_side
        .shares
        .reserve_for_burn(market.base_side.reserves.live_reserve, ylp_amount)?;
    let debt_repaid = Debt::shares_to_debt(base_debt_shares, market.debt.base_borrow_index_nad)?;
    let debt_repaid = u64::try_from(debt_repaid).map_err(|_| ErrorCode::DebtMathOverflow)?;
    let base_hlp_live_debit = proportional(market.quote_hlp_vault.base_hlp_live_reserve, hlp_amount, supply)?;
    let quote_hlp_live_debit = proportional(market.quote_hlp_vault.quote_hlp_live_reserve, hlp_amount, supply)?;
    let quote_out = settled_close_target_amount(
        &market.quote_side,
        &market.base_side,
        quote_out,
        base_redeemed,
        debt_repaid,
    )?;
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
    market.quote_hlp_vault.last_nav_nad = hlp_nav_nad(market, MarketAsset::Quote)?;
    market.quote_hlp_vault.cached_settlement_price_nad = current_settlement_price_nad(market, MarketAsset::Quote)?;
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
    market.side_mut(reserve_asset)?.credit_reserve(amount, false)?;
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
    market.side_mut(reserve_asset)?.debit_reserve(amount, false)?;
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
        market.side_mut(reserve_asset)?.debit_reserve(cash_debit, true)?;
    }
    Ok(())
}

fn settled_close_target_amount(
    target_side: &crate::state::MarketSide,
    borrowed_side: &crate::state::MarketSide,
    target_redeemed: u64,
    borrowed_redeemed: u64,
    debt_repaid: u64,
) -> Result<u64> {
    let target_reserve_after_burn = target_side
        .reserves
        .live_reserve
        .checked_sub(target_redeemed)
        .ok_or(ErrorCode::ReserveUnderflow)?;
    let borrowed_reserve_after_burn = borrowed_side
        .reserves
        .live_reserve
        .checked_sub(borrowed_redeemed)
        .ok_or(ErrorCode::ReserveUnderflow)?;

    if borrowed_redeemed == debt_repaid {
        return Ok(target_redeemed);
    }

    if borrowed_redeemed > debt_repaid {
        let surplus_borrowed = borrowed_redeemed
            .checked_sub(debt_repaid)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let target_from_surplus =
            calculate_raw_amount_out(borrowed_reserve_after_burn, target_reserve_after_burn, surplus_borrowed)?;
        return target_redeemed
            .checked_add(target_from_surplus)
            .ok_or(ErrorCode::MarketMathOverflow.into());
    }

    let borrowed_shortfall = debt_repaid
        .checked_sub(borrowed_redeemed)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let target_needed = calculate_normalized_amount_in(
        target_reserve_after_burn as u128,
        borrowed_reserve_after_burn as u128,
        borrowed_shortfall as u128,
    )?;
    let target_needed = u64::try_from(target_needed).map_err(|_| ErrorCode::MarketMathOverflow)?;
    require_gte!(target_redeemed, target_needed, ErrorCode::HlpSettlementUnavailable);
    target_redeemed
        .checked_sub(target_needed)
        .ok_or(ErrorCode::MarketMathOverflow.into())
}

fn rebalance_one_hlp(market: &mut Market, target_asset: MarketAsset, current_slot: u64) -> Result<HlpRebalanceReceipt> {
    checkpoint_hlp_yield_from_ylp(market, target_asset)?;
    let valuation = current_hlp_valuation(market, target_asset)?;
    let ideal_delta = valuation.ideal_delta;
    let receipt = if ideal_delta > 0 {
        leverage_up_balanced(market, target_asset, ideal_delta)?
    } else if ideal_delta < 0 {
        deleverage_balanced(market, target_asset, ideal_delta)?
    } else {
        HlpRebalanceReceipt {
            target_asset,
            ..HlpRebalanceReceipt::default()
        }
    };
    let receipt = HlpRebalanceReceipt {
        nav_nad: valuation.nav_nad,
        ..receipt
    };
    refresh_hlp_after_rebalance(market, target_asset, current_slot, receipt)
}

#[cfg(test)]
fn current_hlp_ideal_delta(market: &Market, target_asset: MarketAsset) -> Result<i128> {
    current_hlp_valuation(market, target_asset).map(|valuation| valuation.ideal_delta)
}

fn current_hlp_valuation(market: &Market, target_asset: MarketAsset) -> Result<HlpValuation> {
    let (collateral, debt) = match target_asset {
        MarketAsset::Base => (
            hlp_collateral_value_nad(market, MarketAsset::Base, &market.base_hlp_vault)?,
            hlp_debt_value_nad(market, MarketAsset::Base)?,
        ),
        MarketAsset::Quote => (
            hlp_collateral_value_nad(market, MarketAsset::Quote, &market.quote_hlp_vault)?,
            hlp_debt_value_nad(market, MarketAsset::Quote)?,
        ),
    };
    let nav_nad = collateral.checked_sub(debt).ok_or(ErrorCode::Undercollateralized)?;
    let ideal_delta = (collateral as i128)
        .checked_sub(debt.checked_mul(2).ok_or(ErrorCode::DebtMathOverflow)? as i128)
        .ok_or(ErrorCode::DebtMathOverflow)?;
    Ok(HlpValuation { ideal_delta, nav_nad })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct BalancedRebalanceAmounts {
    target_leg_amount: u64,
    borrowed_leg_amount: u64,
    debt_amount: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct HlpValuation {
    ideal_delta: i128,
    nav_nad: u128,
}

fn leverage_up_balanced(
    market: &mut Market,
    target_asset: MarketAsset,
    ideal_delta: i128,
) -> Result<HlpRebalanceReceipt> {
    let target_total_amount = feasible_leverage_up_target_amount(market, target_asset, ideal_delta as u128)?;
    let amounts = balanced_rebalance_amounts_from_target_amount(market, target_asset, target_total_amount)?;
    if amounts.target_leg_amount == 0 || amounts.borrowed_leg_amount == 0 || amounts.debt_amount == 0 {
        return Ok(HlpRebalanceReceipt {
            target_asset,
            ideal_delta,
            ..HlpRebalanceReceipt::default()
        });
    }
    let borrowed_asset = target_asset.opposite();
    require_hlp_borrow_headroom(market.side(borrowed_asset)?, amounts.debt_amount)?;
    let (base_leg_amount, quote_leg_amount) = match target_asset {
        MarketAsset::Base => (amounts.target_leg_amount, amounts.borrowed_leg_amount),
        MarketAsset::Quote => (amounts.borrowed_leg_amount, amounts.target_leg_amount),
    };
    let base_reserve_before = market.base_side.reserves.live_reserve;
    let quote_reserve_before = market.quote_side.reserves.live_reserve;
    let ylp_amount = market.ylp_for_deposit(
        base_reserve_before,
        quote_reserve_before,
        base_leg_amount,
        quote_leg_amount,
    )?;
    if ylp_amount == 0 {
        return Ok(HlpRebalanceReceipt {
            target_asset,
            ideal_delta,
            ..HlpRebalanceReceipt::default()
        });
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
    let executed_delta = executed_delta_for_borrowed_amount(market, target_asset, amounts.debt_amount)?;
    Ok(HlpRebalanceReceipt {
        target_asset,
        ideal_delta,
        executed_delta,
        ylp_mint_amount: ylp_amount,
        debt_delta: amounts.debt_amount as i128,
        ..HlpRebalanceReceipt::default()
    })
}

fn feasible_leverage_up_target_amount(
    market: &Market,
    target_asset: MarketAsset,
    requested_delta_nad: u128,
) -> Result<u64> {
    let requested_target_amount = target_raw_amount_from_delta(market, target_asset, requested_delta_nad)?;
    let borrow_headroom = market.side(target_asset.opposite())?.reserves.cash_reserve;
    if borrow_headroom == 0 {
        return Ok(0);
    }
    let headroom_value_nad = asset_value_in_target_nad(market, target_asset.opposite(), borrow_headroom, target_asset)?;
    let headroom_target_amount = target_raw_amount_from_delta(market, target_asset, headroom_value_nad)?;
    Ok(requested_target_amount.min(headroom_target_amount))
}

fn deleverage_balanced(
    market: &mut Market,
    target_asset: MarketAsset,
    ideal_delta: i128,
) -> Result<HlpRebalanceReceipt> {
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
    let target_side = market.side(target_asset)?;
    let borrowed_side = market.side(borrowed_asset)?;
    let target_underlying = ylp_underlying_amount(target_side, vault_ylp)?;
    let borrowed_underlying = ylp_underlying_amount(borrowed_side, vault_ylp)?;
    let target_total_amount = feasible_deleverage_target_amount(
        market,
        target_asset,
        ideal_delta.unsigned_abs(),
        target_underlying,
        borrowed_underlying,
        current_debt,
    )?;
    let amounts = balanced_rebalance_amounts_from_target_amount(market, target_asset, target_total_amount)?;
    if amounts.target_leg_amount == 0 || amounts.borrowed_leg_amount == 0 || amounts.debt_amount == 0 {
        return Ok(HlpRebalanceReceipt {
            target_asset,
            ideal_delta,
            ..HlpRebalanceReceipt::default()
        });
    }

    let (base_leg_amount, quote_leg_amount) = match target_asset {
        MarketAsset::Base => (amounts.target_leg_amount, amounts.borrowed_leg_amount),
        MarketAsset::Quote => (amounts.borrowed_leg_amount, amounts.target_leg_amount),
    };
    let base_ylp_burn = ylp_shares_for_reserve_amount(&market.base_side, base_leg_amount)?;
    let quote_ylp_burn = ylp_shares_for_reserve_amount(&market.quote_side, quote_leg_amount)?;
    let ylp_burn = base_ylp_burn.max(quote_ylp_burn).min(vault_ylp);
    require!(ylp_burn > 0, ErrorCode::AmountZero);
    debit_hlp_rebalance_reserve(market, target_asset, MarketAsset::Base, base_leg_amount)?;
    debit_hlp_rebalance_reserve(market, target_asset, MarketAsset::Quote, quote_leg_amount)?;
    market.base_side.shares.burn(ylp_burn)?;
    market.quote_side.shares.burn(ylp_burn)?;
    market.base_side.assert_share_backing()?;
    market.quote_side.assert_share_backing()?;

    let repay_amount = amounts.debt_amount.min(current_debt);
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
    let executed_abs = executed_delta_for_borrowed_amount(market, target_asset, debt_clearance.debt_reduced)?;
    Ok(HlpRebalanceReceipt {
        target_asset,
        ideal_delta,
        executed_delta: -executed_abs,
        ylp_burn_amount: ylp_burn,
        debt_delta: -(debt_clearance.debt_reduced as i128),
        interest_paid: debt_clearance.interest_paid,
        ..HlpRebalanceReceipt::default()
    })
}

fn balanced_rebalance_amounts_from_target_amount(
    market: &Market,
    target_asset: MarketAsset,
    target_total_amount: u64,
) -> Result<BalancedRebalanceAmounts> {
    let target_leg_amount = target_total_amount / 2;
    if target_leg_amount == 0 {
        return Ok(BalancedRebalanceAmounts::default());
    }
    let borrowed_leg_amount = market.spot_value_in_opposite(target_asset, target_leg_amount)?;
    let debt_amount = market.spot_value_in_opposite(target_asset, target_total_amount)?;
    Ok(BalancedRebalanceAmounts {
        target_leg_amount,
        borrowed_leg_amount,
        debt_amount,
    })
}

fn feasible_deleverage_target_amount(
    market: &Market,
    target_asset: MarketAsset,
    requested_delta_nad: u128,
    target_underlying: u64,
    borrowed_underlying: u64,
    current_debt: u64,
) -> Result<u64> {
    let requested_target_amount = target_raw_amount_from_delta(market, target_asset, requested_delta_nad)?;
    let target_cap = target_underlying.checked_mul(2).ok_or(ErrorCode::MarketMathOverflow)?;
    let borrowed_value_nad =
        asset_value_in_target_nad(market, target_asset.opposite(), borrowed_underlying, target_asset)?;
    let borrowed_cap = target_raw_amount_from_delta(market, target_asset, borrowed_value_nad)?
        .checked_mul(2)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let debt_value_nad = asset_value_in_target_nad(market, target_asset.opposite(), current_debt, target_asset)?;
    let debt_cap = target_raw_amount_from_delta(market, target_asset, debt_value_nad)?;
    Ok(requested_target_amount.min(target_cap).min(borrowed_cap).min(debt_cap))
}

fn refresh_hlp_after_rebalance(
    market: &mut Market,
    target_asset: MarketAsset,
    current_slot: u64,
    mut receipt: HlpRebalanceReceipt,
) -> Result<HlpRebalanceReceipt> {
    let nav = if receipt.nav_nad > 0 {
        receipt.nav_nad
    } else {
        hlp_nav_nad(market, target_asset)?
    };
    let settlement_price = current_settlement_price_nad(market, target_asset)?;
    let pending_rebalance = receipt
        .ideal_delta
        .checked_sub(receipt.executed_delta)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let vault = match target_asset {
        MarketAsset::Base => &mut market.base_hlp_vault,
        MarketAsset::Quote => &mut market.quote_hlp_vault,
    };
    vault.last_nav_nad = nav;
    vault.pending_rebalance = pending_rebalance;
    vault.cached_settlement_price_nad = settlement_price;
    vault.last_rebalance_slot = current_slot;
    receipt.pending_rebalance = pending_rebalance;
    receipt.nav_nad = nav;
    market.assert_virtual_reserve_invariant(MarketAsset::Base)?;
    market.assert_virtual_reserve_invariant(MarketAsset::Quote)?;
    Ok(receipt)
}

fn target_raw_amount_from_delta(market: &Market, target_asset: MarketAsset, delta_nad: u128) -> Result<u64> {
    let decimals = market.side(target_asset)?.asset_decimals;
    denormalize_from_nad_floor(delta_nad, decimals)
}

fn executed_delta_for_borrowed_amount(
    market: &Market,
    target_asset: MarketAsset,
    borrowed_amount: u64,
) -> Result<i128> {
    let value = asset_value_in_target_nad(market, target_asset.opposite(), borrowed_amount, target_asset)?;
    i128::try_from(value).map_err(|_| ErrorCode::MarketMathOverflow.into())
}

fn ylp_shares_for_reserve_amount(side: &crate::state::MarketSide, reserve_amount: u64) -> Result<u64> {
    if reserve_amount == 0 {
        return Ok(0);
    }
    require!(
        side.reserves.live_reserve > 0 && side.shares.ylp_supply > 0,
        ErrorCode::InsufficientLiquidity
    );
    let shares = ceil_div(
        (reserve_amount as u128)
            .checked_mul(side.shares.ylp_supply as u128)
            .ok_or(ErrorCode::MarketMathOverflow)?,
        side.reserves.live_reserve as u128,
    )
    .ok_or(ErrorCode::MarketMathOverflow)?;
    u64::try_from(shares).map_err(|_| ErrorCode::MarketMathOverflow.into())
}

fn require_hlp_borrow_headroom(side: &crate::state::MarketSide, amount: u64) -> Result<()> {
    require_gte!(
        side.reserves.cash_reserve,
        amount,
        ErrorCode::InsufficientBorrowHeadroom
    );
    Ok(())
}

fn checkpoint_one_hlp(market: &mut Market, target_asset: MarketAsset, current_slot: u64) -> Result<i128> {
    checkpoint_hlp_yield_from_ylp(market, target_asset)?;
    let nav = hlp_nav_nad(market, target_asset)?;
    let settlement_price = current_settlement_price_nad(market, target_asset)?;
    let (collateral, debt, vault) = match target_asset {
        MarketAsset::Base => {
            let collateral = hlp_collateral_value_nad(market, MarketAsset::Base, &market.base_hlp_vault)?;
            let debt = hlp_debt_value_nad(market, MarketAsset::Base)?;
            (collateral, debt, &mut market.base_hlp_vault)
        }
        MarketAsset::Quote => {
            let collateral = hlp_collateral_value_nad(market, MarketAsset::Quote, &market.quote_hlp_vault)?;
            let debt = hlp_debt_value_nad(market, MarketAsset::Quote)?;
            (collateral, debt, &mut market.quote_hlp_vault)
        }
    };
    let ideal_delta = (collateral as i128)
        .checked_sub(debt.checked_mul(2).ok_or(ErrorCode::DebtMathOverflow)? as i128)
        .ok_or(ErrorCode::DebtMathOverflow)?;
    vault.last_nav_nad = nav;
    vault.pending_rebalance = ideal_delta;
    vault.cached_settlement_price_nad = settlement_price;
    vault.last_rebalance_slot = current_slot;
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
    let vault = match target_asset {
        MarketAsset::Base => &market.base_hlp_vault,
        MarketAsset::Quote => &market.quote_hlp_vault,
    };
    if vault.hlp_supply == 0 || vault.cached_settlement_price_nad == 0 {
        return Ok(());
    }
    let current_price = current_settlement_price_nad(market, target_asset)?;
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

fn current_settlement_price_nad(market: &Market, target_asset: MarketAsset) -> Result<u128> {
    match target_asset {
        MarketAsset::Base => market_spot_price_nad(&market.base_side, &market.quote_side).map(u128::from),
        MarketAsset::Quote => market_spot_price_nad(&market.quote_side, &market.base_side).map(u128::from),
    }
}

fn hlp_nav_nad(market: &Market, target_asset: MarketAsset) -> Result<u128> {
    let (collateral, debt) = match target_asset {
        MarketAsset::Base => (
            hlp_collateral_value_nad(market, MarketAsset::Base, &market.base_hlp_vault)?,
            hlp_debt_value_nad(market, MarketAsset::Base)?,
        ),
        MarketAsset::Quote => (
            hlp_collateral_value_nad(market, MarketAsset::Quote, &market.quote_hlp_vault)?,
            hlp_debt_value_nad(market, MarketAsset::Quote)?,
        ),
    };
    collateral
        .checked_sub(debt)
        .ok_or(ErrorCode::Undercollateralized.into())
}

fn hlp_collateral_value_nad(market: &Market, target_asset: MarketAsset, vault: &HlpVault) -> Result<u128> {
    let base_underlying = ylp_underlying_amount(&market.base_side, vault.ylp_shares)?;
    let quote_underlying = ylp_underlying_amount(&market.quote_side, vault.ylp_shares)?;
    let base_value = asset_value_in_target_nad(market, MarketAsset::Base, base_underlying, target_asset)?;
    let quote_value = asset_value_in_target_nad(market, MarketAsset::Quote, quote_underlying, target_asset)?;
    base_value
        .checked_add(quote_value)
        .ok_or(ErrorCode::MarketMathOverflow.into())
}

fn hlp_debt_value_nad(market: &Market, target_asset: MarketAsset) -> Result<u128> {
    let (borrowed_asset, debt_amount) = match target_asset {
        MarketAsset::Base => (
            MarketAsset::Quote,
            Debt::shares_to_debt(market.base_hlp_vault.debt_shares, market.debt.quote_borrow_index_nad)?,
        ),
        MarketAsset::Quote => (
            MarketAsset::Base,
            Debt::shares_to_debt(market.quote_hlp_vault.debt_shares, market.debt.base_borrow_index_nad)?,
        ),
    };
    let debt_amount = u64::try_from(debt_amount).map_err(|_| ErrorCode::DebtMathOverflow)?;
    asset_value_in_target_nad(market, borrowed_asset, debt_amount, target_asset)
}

fn ylp_underlying_amount(side: &crate::state::MarketSide, ylp_amount: u64) -> Result<u64> {
    if ylp_amount == 0 || side.shares.ylp_supply == 0 {
        return Ok(0);
    }
    let reserve_amount = (ylp_amount as u128)
        .checked_mul(side.reserves.live_reserve as u128)
        .and_then(|value| value.checked_div(side.shares.ylp_supply as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    u64::try_from(reserve_amount).map_err(|_| ErrorCode::MarketMathOverflow.into())
}

fn asset_value_in_target_nad(
    market: &Market,
    asset: MarketAsset,
    amount: u64,
    target_asset: MarketAsset,
) -> Result<u128> {
    let asset_side = market.side(asset)?;
    let amount_nad = normalize_to_nad(amount as u128, asset_side.asset_decimals)?;
    if asset == target_asset {
        return Ok(amount_nad);
    }
    let target_side = market.side(target_asset)?;
    let price_nad = market_spot_price_nad(asset_side, target_side)? as u128;
    amount_nad
        .checked_mul(price_nad)
        .and_then(|value| value.checked_div(NAD as u128))
        .ok_or(ErrorCode::MarketMathOverflow.into())
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
        .ok_or(ErrorCode::MarketMathOverflow.into())
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
