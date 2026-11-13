use super::*;

#[cfg(test)]
mod tests {
    include!("../../../tests/transitions/liquidity_hlp_engine.rs");
}

#[cfg(test)]
mod exit_tests {
    include!("../../../tests/transitions/liquidity_hlp_exit.rs");
}

fn recognized_hlp_residual_exposure(actual_residual_nad: i128, nav_nad: u128, amount_decimals: u8) -> i128 {
    // Keep the absolute dust allowance at 0.00001 tokens at every market scale.
    // Saturation is exact for this min: an unrepresentable absolute cap can
    // never be tighter than the representable relative NAV cap.
    let absolute_cap =
        HLP_REBALANCE_DUST_MAX_NAD.saturating_mul(10_u128.saturating_pow(u32::from(amount_decimals - NAD_DECIMALS)));
    let tolerance_nad = absolute_cap.min(nav_nad / HLP_REBALANCE_DUST_NAV_DENOMINATOR);
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
    pub srlp_amount: u64,
    pub hlp_amount: u64,
    pub hlp_supply: u64,
    pub target_amount_out: u64,
    pub debt_repaid: u64,
    pub interest_paid: u64,
    pub rebalances: [HlpRebalanceReceipt; 2],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HlpRebalanceReceipt {
    pub target_asset: MarketAsset,
    pub ideal_delta: i128,
    pub executed_delta: i128,
    pub residual_exposure: i128,
    pub current_swap_fee_eligible_srlp_shares: u64,
    pub srlp_mint_amount: u64,
    pub srlp_burn_amount: u64,
    pub debt_delta: i128,
    pub interest_paid: u64,
    pub nav_nad: u128,
    pub(crate) tracking_start_nav_nad: i128,
    pub(crate) tracking_loss_budget_nad: u128,
    /// Frozen public-borrow interest only; hLP funding interest is ineligible.
    pub(crate) tracking_base_unrealized_interest: u64,
    /// Frozen public-borrow interest only; hLP funding interest is ineligible.
    pub(crate) tracking_quote_unrealized_interest: u64,
    pub(crate) tracking_start_srlp_shares: u64,
    pub(crate) tracking_start_srlp_supply: u64,
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
            current_swap_fee_eligible_srlp_shares: 0,
            srlp_mint_amount: 0,
            srlp_burn_amount: 0,
            debt_delta: 0,
            interest_paid: 0,
            nav_nad: 0,
            tracking_start_nav_nad: 0,
            tracking_loss_budget_nad: 0,
            tracking_base_unrealized_interest: 0,
            tracking_quote_unrealized_interest: 0,
            tracking_start_srlp_shares: 0,
            tracking_start_srlp_supply: 0,
            tracking_retained_contribution_nad: 0,
            preposition_capacity_bound: false,
        }
    }
}

/// Identity-bound O(1) hLP ownership/debt reconstruction for the concentrated
/// curve. The quote fixes the ordinary tranche; this plan only realizes
/// accrued hLP funding interest and refinances both vaults to the quoted
/// zero-opposite-exposure endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ConcentratedHlpTransition {
    frozen_hlp_mask: u8,
    hlp_capitalized: [[u64; 3]; 2],
    junior_compound: Option<(u64, u64, u8, u16)>,
    junior_seed_compound: u64,
    expected_curve_revision: u64,
    expected_srlp_supply: u64,
    expected_base_srlp_shares: u64,
    expected_quote_srlp_shares: u64,
    expected_base_debt_shares: u128,
    expected_quote_debt_shares: u128,
    expected_base_debt_principal: u64,
    expected_quote_debt_principal: u64,
    final_srlp_supply: u64,
    final_base_srlp_shares: u64,
    final_quote_srlp_shares: u64,
    final_base_debt_shares: u128,
    final_quote_debt_shares: u128,
    final_base_debt: u64,
    final_quote_debt: u64,
    final_base_hlp_live: u64,
    final_quote_hlp_live: u64,
    final_base_curve_reserve: u64,
    final_quote_curve_reserve: u64,
    base_output_rounding_carry: u64,
    quote_output_rounding_carry: u64,
    base_interest_paid: u64,
    quote_interest_paid: u64,
    base_receipt: HlpRebalanceReceipt,
    quote_receipt: HlpRebalanceReceipt,
}

fn signed_u64_delta(end: u64, start: u64) -> Result<i128> {
    if end >= start {
        Ok(i128::from(end - start))
    } else {
        Ok(-i128::from(start - end))
    }
}

fn concentrated_hlp_receipt(
    target_asset: MarketAsset,
    start_srlp_shares: u64,
    end_srlp_shares: u64,
    start_debt: u64,
    end_debt: u64,
    interest_paid: u64,
    nav_nad: u128,
    start_supply: u64,
) -> Result<HlpRebalanceReceipt> {
    Ok(HlpRebalanceReceipt {
        target_asset,
        ideal_delta: signed_u64_delta(end_debt, start_debt)?,
        executed_delta: signed_u64_delta(end_debt, start_debt)?,
        residual_exposure: 0,
        current_swap_fee_eligible_srlp_shares: start_srlp_shares,
        srlp_mint_amount: end_srlp_shares.saturating_sub(start_srlp_shares),
        srlp_burn_amount: start_srlp_shares.saturating_sub(end_srlp_shares),
        debt_delta: signed_u64_delta(end_debt, start_debt)?,
        interest_paid,
        nav_nad,
        tracking_start_nav_nad: i128::try_from(nav_nad).map_err(|_| ErrorCode::MarketMathOverflow)?,
        tracking_loss_budget_nad: 0,
        tracking_base_unrealized_interest: 0,
        tracking_quote_unrealized_interest: 0,
        tracking_start_srlp_shares: start_srlp_shares,
        tracking_start_srlp_supply: start_supply,
        tracking_retained_contribution_nad: 0,
        preposition_capacity_bound: false,
    })
}

fn canonical_debt_for_proportional_claim_with_carry(
    non_debt_reserve: u128,
    hlp_srlp_shares: u64,
    total_srlp_supply: u64,
    borrow_index_nad: u128,
    interest_carried: u64,
    opposite_receivable: u64,
    quoted_debt: Option<u64>,
) -> Result<(u128, u64)> {
    if hlp_srlp_shares == 0 && opposite_receivable == 0 {
        return Ok((0, 0));
    }
    let ordinary_and_other_shares = total_srlp_supply
        .checked_sub(hlp_srlp_shares)
        .ok_or(ErrorCode::SupplyUnderflow)?;
    require!(ordinary_and_other_shares > 0, ErrorCode::SupplyUnderflow);
    require!(borrow_index_nad >= NAD as u128, ErrorCode::DebtShareDivisionOverflow);

    let max_live = u128::from(u64::MAX).saturating_sub(non_debt_reserve);
    let denominator = ordinary_and_other_shares as u128;
    // Ordinary reserves fit one exact native division. Use full-width products
    // only when the widened accounting reserve needs them.
    let numerator = non_debt_reserve
        .checked_mul(hlp_srlp_shares as u128)
        .and_then(|v| v.checked_add(opposite_receivable as u128 * total_srlp_supply as u128));
    let continuous_live = if let Some(numerator) = numerator {
        crate::math::div_rem_native(numerator, denominator).0
    } else {
        let (live, live_fraction) = mul_div_rem_u128(non_debt_reserve, hlp_srlp_shares as u128, denominator)?;
        let (receivable, receivable_fraction) =
            mul_div_rem_u128(opposite_receivable as u128, total_srlp_supply as u128, denominator)?;
        live.checked_add(receivable)
            .and_then(|v| v.checked_add((live_fraction + receivable_fraction) / denominator))
            .ok_or(ErrorCode::DebtMathOverflow)?
    }
    .min(max_live);
    let continuous = continuous_live
        .checked_add(u128::from(interest_carried))
        .ok_or(ErrorCode::DebtMathOverflow)?
        .min(u128::from(u64::MAX));
    // Retain a valid quoted endpoint. Otherwise test the indexed debts
    // bracketing the fixed point, rather than raw amounts that can all round
    // to the same share count after interest accrues. The signed error
    // d - floor((R + d) * h / S) is monotone in d and is zero at the floored
    // continuous solution, so no farther share count can improve either side.
    // Clipping to reserve capacity also covers a solution near u64::MAX.
    let shares_for = |debt: u128| {
        let (whole, fraction) = crate::math::div_rem_native(debt * NAD as u128, borrow_index_nad);
        whole + u128::from(fraction != 0)
    };
    let upper_shares = shares_for(continuous);
    let quoted_shares = quoted_debt.map(|debt| shares_for(debt as u128));
    for debt_shares in [quoted_shares, Some(upper_shares), upper_shares.checked_sub(1)]
        .into_iter()
        .flatten()
    {
        let debt = Debt::shares_to_debt(debt_shares, borrow_index_nad)?;
        // An upper candidate may exceed capacity while its predecessor fits.
        if debt > u128::from(u64::MAX) || debt < u128::from(interest_carried) {
            continue;
        }
        let live = debt - u128::from(interest_carried);
        if live > max_live {
            continue;
        }
        let claim = mul_div_u128(
            non_debt_reserve + live,
            hlp_srlp_shares as u128,
            total_srlp_supply as u128,
        )?;
        let claim = claim
            .checked_add(opposite_receivable as u128)
            .ok_or(ErrorCode::DebtMathOverflow)?;
        if live.abs_diff(claim) <= 1 {
            return Ok((debt_shares, debt as u64));
        }
    }
    err!(ErrorCode::BrokenInvariant)
}

pub(crate) fn prepare_concentrated_hlp_transition(
    market: &Market,
    start: &IntegratedCurveState,
    quote: &ConcentratedIntegratedAmmQuote,
    asset_in: MarketAsset,
) -> Result<ConcentratedHlpTransition> {
    let asset_out = asset_in.opposite();
    let decimals = market.side(asset_out).asset_decimals;
    let atom_scale = 10_u128
        .checked_pow(u32::from(market.amount_decimals()? - decimals))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let (start_ordinary, end_ordinary) = match asset_out {
        MarketAsset::Base => (start.ordinary_base, quote.integrated.executable.curve.end.base_reserve),
        MarketAsset::Quote => (
            start.ordinary_quote,
            quote.integrated.executable.curve.end.quote_reserve,
        ),
    };
    // Cash debits the floor of the fractional curve output, while the quote
    // floors the remaining ordinary reserve. The two floors can leave one
    // extra raw atom in the live-reserve identity.
    require_eq!(
        start_ordinary
            .checked_sub(end_ordinary)
            .ok_or(ErrorCode::BrokenInvariant)?,
        quote.integrated.executable.curve.amount_out,
        ErrorCode::BrokenInvariant
    );
    let ordinary_gross_out = quote
        .gross_amount_out
        .checked_sub(quote.recovery.bonus_output)
        .ok_or(ErrorCode::BrokenInvariant)?;
    let raw_output_nad = u128::from(ordinary_gross_out)
        .checked_mul(atom_scale)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let fractional_output = quote
        .integrated
        .executable
        .curve
        .amount_out
        .checked_sub(raw_output_nad)
        .ok_or(ErrorCode::BrokenInvariant)?;
    require!(fractional_output < atom_scale, ErrorCode::BrokenInvariant);
    // With start - end = exact output, the only rounding term is the borrow
    // when the output's fractional atom exceeds the start's fractional atom.
    let rounding_carry = u64::from(start_ordinary % atom_scale < fractional_output);
    prepare_concentrated_hlp_transition_from_end(
        market,
        quote.integrated.executable.end,
        false,
        quote.recovery.bonus_output > 0,
        if asset_out == MarketAsset::Base {
            rounding_carry
        } else {
            0
        },
        if asset_out == MarketAsset::Quote {
            rounding_carry
        } else {
            0
        },
    )
}

/// Adds the hLP funding-recovery tranche to a Spot quote. The complete
/// input remains on the ordinary curve; only the incremental price improvement
/// is paid by the hLP whose borrowed asset matches `asset_in`.
pub(crate) fn apply_concentrated_hlp_recovery(
    market: &Market,
    asset_in: MarketAsset,
    start: IntegratedCurveState,
    quote: &mut ConcentratedIntegratedAmmQuote,
) -> Result<()> {
    let target_asset = asset_in.opposite();
    let vault = match target_asset {
        MarketAsset::Base => &market.base_hlp_vault,
        MarketAsset::Quote => &market.quote_hlp_vault,
    };
    if vault.hlp_supply == 0 || vault.srlp_shares == 0 || vault.debt_shares == 0 || vault.flash_collateral_debit > 0 {
        return Ok(());
    }
    let supply = market.base_side.shares.srlp_supply;
    require_eq!(supply, market.quote_side.shares.srlp_supply, ErrorCode::BrokenInvariant);
    require!(supply > 0, ErrorCode::SupplyUnderflow);

    let opposite_reserve = market.side(asset_in).reserves.live_reserve;
    let canonical_opposite_claim = u64::try_from(
        opposite_reserve
            .checked_mul(vault.srlp_shares as u128)
            .ok_or(ErrorCode::MarketMathOverflow)?
            / supply as u128,
    )
    .map_err(|_| ErrorCode::MarketMathOverflow)?;
    let actual_debt = u64::try_from(Debt::shares_to_debt(
        vault.debt_shares,
        match target_asset {
            MarketAsset::Base => market.debt.quote_borrow_index_nad,
            MarketAsset::Quote => market.debt.base_borrow_index_nad,
        },
    )?)
    .map_err(|_| ErrorCode::DebtMathOverflow)?;
    let (target_equity_nad, target_reserve_nad, opposite_reserve_nad) = match target_asset {
        MarketAsset::Base => (start.base_hlp_equity, start.ordinary_base, start.ordinary_quote),
        MarketAsset::Quote => (start.quote_hlp_equity, start.ordinary_quote, start.ordinary_base),
    };
    let target_equity = market.denormalize_amount_floor(target_equity_nad, market.side(target_asset).asset_decimals)?;
    let target_reserve =
        market.denormalize_amount_floor(target_reserve_nad, market.side(target_asset).asset_decimals)?;
    let ordinary_opposite_reserve =
        market.denormalize_amount_floor(opposite_reserve_nad, market.side(asset_in).asset_decimals)?;

    // Gross hLP yield is already a holder liability in the current ledger.
    // Until the net-yield checkpoint is introduced, none of it may be spent
    // here; the recovery bonus is therefore funded solely by hLP equity.
    let recovery = quote_hlp_recovery(
        actual_debt,
        canonical_opposite_claim,
        0,
        quote.fee.amount_in_for_quote,
        target_equity,
        target_reserve,
        ordinary_opposite_reserve,
    )?;
    let bonus_output = recovery.bonus_output;
    quote.recovery = HlpRecoveryBreakdown {
        target_asset: target_asset.code(),
        funding_gap: recovery.funding_gap,
        matched_input: recovery.matched_input,
        bonus_output,
        discount_bps: recovery.discount_bps,
        critical: recovery.critical,
    };
    if bonus_output == 0 {
        return Ok(());
    }
    let effective_bonus_nad =
        market.normalize_amount(bonus_output as u128, market.side(target_asset).asset_decimals)?;
    apply_hlp_recovery_bonus(
        start,
        &mut quote.integrated,
        target_asset == MarketAsset::Base,
        effective_bonus_nad,
    )?;
    quote.amount_out = quote
        .amount_out
        .checked_add(bonus_output)
        .ok_or(ErrorCode::ReserveOverflow)?;
    quote.gross_amount_out = quote
        .gross_amount_out
        .checked_add(bonus_output)
        .ok_or(ErrorCode::ReserveOverflow)?;
    quote.fee.gross_amount_out = quote.gross_amount_out;
    Ok(())
}

/// Identity-bound terminal settlement for an hLP whose indexed funding debt
/// has consumed all of its marked collateral. The vault's srLP ownership is
/// retired first; junior absorbs principal loss before senior and unpaid funding
/// interest is canceled. Available cash limits realized funding interest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HlpTerminalWaterfallPlan {
    target_asset: MarketAsset,
    expected_curve_revision: u64,
    expected_srlp_supply: u64,
    expected_srlp_shares: u64,
    expected_debt_shares: u128,
    expected_debt_principal: u64,
    expected_target_hlp_live: u64,
    expected_borrowed_hlp_live: u64,
    debt: u64,
    reference_recovery_in_borrowed: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HlpTerminalWaterfallReceipt {
    pub target_asset: MarketAsset,
    pub srlp_burn_amount: u64,
    pub debt_closed: u64,
    pub interest_paid: u64,
    pub socialized_loss: u64,
    pub interest_canceled: u64,
    pub remaining_hlp_supply: u64,
}

impl HlpTerminalWaterfallPlan {
    pub(crate) fn target_asset(self) -> MarketAsset {
        self.target_asset
    }

    pub(crate) fn consume(self, market: &mut Market, max_socialized_loss: u64) -> Result<HlpTerminalWaterfallReceipt> {
        require_eq!(
            market.curve_revision,
            self.expected_curve_revision,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            market.base_side.shares.srlp_supply,
            self.expected_srlp_supply,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            market.quote_side.shares.srlp_supply,
            self.expected_srlp_supply,
            ErrorCode::BrokenInvariant
        );
        let vault = match self.target_asset {
            MarketAsset::Base => &market.base_hlp_vault,
            MarketAsset::Quote => &market.quote_hlp_vault,
        };
        require_eq!(vault.srlp_shares, self.expected_srlp_shares, ErrorCode::BrokenInvariant);
        require_eq!(vault.debt_shares, self.expected_debt_shares, ErrorCode::BrokenInvariant);
        require_eq!(
            vault.debt_principal,
            self.expected_debt_principal,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            vault.hlp_live_reserve(self.target_asset),
            self.expected_target_hlp_live,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            vault.hlp_live_reserve(self.target_asset.opposite()),
            self.expected_borrowed_hlp_live,
            ErrorCode::BrokenInvariant
        );
        let remaining_hlp_supply = vault.hlp_supply;

        let mut loss = crate::state::LiquidationLossAllocation::quote(
            self.expected_debt_principal.min(self.debt),
            self.debt,
            self.reference_recovery_in_borrowed,
        )?;
        let socialized_loss = loss.principal_written_off;
        require_gte!(
            max_socialized_loss,
            socialized_loss,
            ErrorCode::LiquidationSocializationExceeded
        );
        let borrowed_asset = self.target_asset.opposite();
        let cash = market.side(borrowed_asset).reserves.cash_reserve;
        let interest_paid = loss.interest_paid.min(cash);
        loss.interest_canceled = loss
            .interest_canceled
            .checked_add(loss.interest_paid - interest_paid)
            .ok_or(ErrorCode::MarketMathOverflow)?;

        for asset in [MarketAsset::Base, MarketAsset::Quote] {
            market.capitalize_hlp_receivable(self.target_asset, asset, 1, 1)?;
        }
        market.base_side.shares.burn(self.expected_srlp_shares)?;
        market.quote_side.shares.burn(self.expected_srlp_shares)?;
        if self.expected_target_hlp_live > 0 {
            market.side_mut(self.target_asset).reserves.live_reserve = market
                .side(self.target_asset)
                .reserves
                .live_reserve
                .checked_sub(u128::from(self.expected_target_hlp_live))
                .ok_or(ErrorCode::ReserveUnderflow)?;
        }
        if self.expected_borrowed_hlp_live > 0 {
            market.side_mut(borrowed_asset).reserves.live_reserve = market
                .side(borrowed_asset)
                .reserves
                .live_reserve
                .checked_sub(u128::from(self.expected_borrowed_hlp_live))
                .ok_or(ErrorCode::ReserveUnderflow)?;
        }
        debit_cash_for_hlp_interest(market, borrowed_asset, interest_paid)?;

        let vault = match self.target_asset {
            MarketAsset::Base => &mut market.base_hlp_vault,
            MarketAsset::Quote => &mut market.quote_hlp_vault,
        };
        vault.srlp_shares = 0;
        vault.base_hlp_live_reserve = 0;
        vault.quote_hlp_live_reserve = 0;
        vault.debt_shares = 0;
        vault.debt_principal = 0;
        vault.residual_exposure = 0;
        vault.last_nav_nad = 0;
        vault.cached_settlement_price_nad = 0;
        vault.funding_apr_ema_nad = 0;
        vault.funding_apr_ema_last_slot = 0;

        market.retire_hlp_receivables(self.target_asset)?;
        market.sync_income_receivable(borrowed_asset, true, 0)?;
        market.clear_frozen_hlp(self.target_asset);
        market.absorb_funded_credit_loss(borrowed_asset, socialized_loss)?;
        market.rebase_concentrated_curve_after_terminal_hlp_loss()?;
        market.assert_virtual_reserve_invariant(MarketAsset::Base)?;
        market.assert_virtual_reserve_invariant(MarketAsset::Quote)?;
        Ok(HlpTerminalWaterfallReceipt {
            target_asset: self.target_asset,
            srlp_burn_amount: self.expected_srlp_shares,
            debt_closed: self.debt,
            interest_paid,
            socialized_loss,
            interest_canceled: loss.interest_canceled,
            remaining_hlp_supply,
        })
    }
}

impl Market {
    /// Canonical terminal hLP health mark in raw borrowed-asset atoms.
    /// This linear symmetric-EMA value controls eligibility only. Native
    /// execution separately applies the EMA curve, smoothed depth and fees.
    pub(crate) fn hlp_ema_collateral_value_in_borrowed(
        &self,
        target_asset: MarketAsset,
        target_claim: u64,
        borrowed_claim: u64,
        current_slot: u64,
    ) -> Result<u64> {
        let borrowed_asset = target_asset.opposite();
        // Refresh the stored observation to the current slot without persisting
        // it. Terminal eligibility and the EMA execution quote must see the
        // same time-adjusted symmetric EMA; otherwise an old snapshot can keep
        // a recovered vault liquidatable (or delay a newly insolvent close).
        let risk = self.current_risk_at_slot(current_slot)?;
        let ema = match target_asset {
            MarketAsset::Base => risk.base_price_ema_nad,
            MarketAsset::Quote => risk.quote_price_ema_nad,
        };
        require!(ema > 0, ErrorCode::InvalidSettlementPrice);
        let normalized_target =
            self.normalize_amount(u128::from(target_claim), self.side(target_asset).asset_decimals)?;
        let target_value = mul_div_u128(normalized_target, u128::from(ema), u128::from(NAD))?;
        let target_value_in_borrowed =
            self.denormalize_amount_floor(target_value, self.side(borrowed_asset).asset_decimals)?;
        borrowed_claim
            .checked_add(target_value_in_borrowed)
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
    }

    pub(crate) fn hlp_terminally_closed(&self, target_asset: MarketAsset) -> bool {
        let vault = match target_asset {
            MarketAsset::Base => self.base_hlp_vault,
            MarketAsset::Quote => self.quote_hlp_vault,
        };
        vault.srlp_shares == 0
            && vault.last_nav_nad == 0
            && vault.debt_shares == 0
            && vault.debt_principal == 0
            && vault.base_hlp_live_reserve == 0
            && vault.quote_hlp_live_reserve == 0
    }

    pub(crate) fn prepare_terminal_hlp_waterfall(
        &mut self,
        target_asset: MarketAsset,
        current_slot: u64,
        current_unix_timestamp: i64,
        protocol_fee_bps: u16,
    ) -> Result<HlpTerminalWaterfallPlan> {
        self.require_hlp_flash_idle(target_asset)?;
        checkpoint_hlp_yield_from_srlp(self, target_asset)?;
        let vault = match target_asset {
            MarketAsset::Base => self.base_hlp_vault,
            MarketAsset::Quote => self.quote_hlp_vault,
        };
        require!(
            vault.hlp_supply > 0 && vault.srlp_shares > 0 && vault.debt_shares > 0,
            ErrorCode::HlpNotLiquidatable
        );
        let supply = self.base_side.shares.srlp_supply;
        require_eq!(supply, self.quote_side.shares.srlp_supply, ErrorCode::BrokenInvariant);
        require!(supply > 0, ErrorCode::SupplyUnderflow);
        let target_claim = hlp_exit_underlying_amount(self, target_asset, target_asset, vault.hlp_supply)?;
        let borrowed_asset = target_asset.opposite();
        let borrowed_claim = hlp_exit_underlying_amount(self, target_asset, borrowed_asset, vault.hlp_supply)?;
        let eligibility_value_in_borrowed =
            self.hlp_ema_collateral_value_in_borrowed(target_asset, target_claim, borrowed_claim, current_slot)?;
        let debt = u64::try_from(Debt::shares_to_debt(
            vault.debt_shares,
            self.debt.borrow_index(borrowed_asset),
        )?)
        .map_err(|_| ErrorCode::DebtMathOverflow)?;
        require!(debt >= eligibility_value_in_borrowed, ErrorCode::HlpNotLiquidatable);
        // The native close uses the same EMA-price, smoothed-depth and full-fee
        // curve as other internal emergency liquidations. Live spot determines
        // neither this recovery amount nor the resulting loss allocation.
        let reference_swap_output = if target_claim == 0 {
            0
        } else {
            crate::transitions::amm::SwapRequest {
                current_slot,
                current_unix_timestamp,
                asset_in: target_asset,
                reserve_credit: target_claim,
                protocol_fee_bps,
            }
            .quote_ema_liquidation(self, &mut None, &mut Default::default())?
            .amount_out
        };
        let reference_recovery_in_borrowed = borrowed_claim
            .checked_add(reference_swap_output)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        Ok(HlpTerminalWaterfallPlan {
            target_asset,
            expected_curve_revision: self.curve_revision,
            expected_srlp_supply: supply,
            expected_srlp_shares: vault.srlp_shares,
            expected_debt_shares: vault.debt_shares,
            expected_debt_principal: vault.debt_principal,
            expected_target_hlp_live: vault.hlp_live_reserve(target_asset),
            expected_borrowed_hlp_live: vault.hlp_live_reserve(borrowed_asset),
            debt,
            reference_recovery_in_borrowed,
        })
    }
}

/// Rebuilds the exact one-sided hLP endpoint around an already materialized
/// ordinary reserve point. Liquidation uses this after a socialized reserve
/// haircut so the loss cannot be overwritten by the pre-loss swap plan.
pub(crate) fn prepare_concentrated_hlp_transition_at_current_state(
    market: &Market,
) -> Result<ConcentratedHlpTransition> {
    prepare_concentrated_hlp_transition_from_end(market, market.integrated_curve_state_nad()?, true, false, 0, 0)
}

pub(crate) fn prepare_concentrated_hlp_transition_from_end(
    market: &Market,
    end: IntegratedCurveState,
    preserve_current_ordinary_reserves: bool,
    certify_proportional_claim: bool,
    base_output_rounding_carry: u64,
    quote_output_rounding_carry: u64,
) -> Result<ConcentratedHlpTransition> {
    require_eq!(
        market.base_side.shares.srlp_supply,
        market.quote_side.shares.srlp_supply,
        ErrorCode::BrokenInvariant
    );
    require_eq!(
        market.base_side.reserves.total_hlp_backing_inventory()?,
        0,
        ErrorCode::BrokenInvariant
    );
    require_eq!(
        market.quote_side.reserves.total_hlp_backing_inventory()?,
        0,
        ErrorCode::BrokenInvariant
    );
    let start_supply = market.base_side.shares.srlp_supply;
    let unrealized = [
        market
            .unrealized_interest(MarketAsset::Base)?
            .checked_sub(end.hlp_capitalized[0][2] as u128)
            .ok_or(ErrorCode::BrokenInvariant)?,
        market
            .unrealized_interest(MarketAsset::Quote)?
            .checked_sub(end.hlp_capitalized[1][2] as u128)
            .ok_or(ErrorCode::BrokenInvariant)?,
    ];
    // No hedge exists in an ordinary market. Keep the same checked settlement
    // receipt and reserve-identity validation without solving zero ownership
    // ratios and zero debts for both hLP vaults.
    if !market.has_active_hlp()
        && end.junior_compound.is_none()
        && market.base_hlp_vault.srlp_shares == 0
        && market.quote_hlp_vault.srlp_shares == 0
        && market.base_hlp_vault.debt_shares == 0
        && market.quote_hlp_vault.debt_shares == 0
        && market.base_hlp_vault.debt_principal == 0
        && market.quote_hlp_vault.debt_principal == 0
        && market.amm.frozen_hlp_mask == 0
        && end.frozen_hlp_mask == 0
        && end.base_hlp_equity == 0
        && end.quote_hlp_equity == 0
        && end.base_hlp_quote_debt == 0
        && end.quote_hlp_base_debt == 0
        && end.base_hlp_quote_carry == 0
        && end.quote_hlp_base_carry == 0
    {
        require!(start_supply > 0, ErrorCode::SupplyUnderflow);
        let endpoint = integrated::materialized_hlp_endpoint(end)?;
        require!(
            endpoint.total_base > 0 && endpoint.total_quote > 0,
            ErrorCode::InsufficientLiquidity
        );
        let base = if preserve_current_ordinary_reserves {
            market.curve_reserve(MarketAsset::Base)?
        } else {
            market.denormalize_amount_floor(endpoint.total_base, market.base_side.asset_decimals)?
        };
        let quote = if preserve_current_ordinary_reserves {
            market.curve_reserve(MarketAsset::Quote)?
        } else {
            market.denormalize_amount_floor(endpoint.total_quote, market.quote_side.asset_decimals)?
        };
        return Ok(ConcentratedHlpTransition {
            frozen_hlp_mask: 0,
            hlp_capitalized: [[0; 3]; 2],
            junior_compound: None,
            junior_seed_compound: 0,
            expected_curve_revision: market.curve_revision,
            expected_srlp_supply: start_supply,
            expected_base_srlp_shares: 0,
            expected_quote_srlp_shares: 0,
            expected_base_debt_shares: 0,
            expected_quote_debt_shares: 0,
            expected_base_debt_principal: 0,
            expected_quote_debt_principal: 0,
            final_srlp_supply: start_supply,
            final_base_srlp_shares: 0,
            final_quote_srlp_shares: 0,
            final_base_debt_shares: 0,
            final_quote_debt_shares: 0,
            final_base_debt: 0,
            final_quote_debt: 0,
            final_base_hlp_live: 0,
            final_quote_hlp_live: 0,
            final_base_curve_reserve: base,
            final_quote_curve_reserve: quote,
            base_output_rounding_carry,
            quote_output_rounding_carry,
            base_interest_paid: 0,
            quote_interest_paid: 0,
            base_receipt: concentrated_hlp_receipt(MarketAsset::Base, 0, 0, 0, 0, 0, 0, start_supply)?,
            quote_receipt: concentrated_hlp_receipt(MarketAsset::Quote, 0, 0, 0, 0, 0, 0, start_supply)?,
        });
    }
    let ordinary_supply = start_supply
        .checked_sub(market.base_hlp_vault.srlp_shares)
        .and_then(|value| value.checked_sub(market.quote_hlp_vault.srlp_shares))
        .ok_or(ErrorCode::BrokenInvariant)?;
    let frozen_mask = end.frozen_hlp_mask;
    require_eq!(frozen_mask, market.amm.frozen_hlp_mask, ErrorCode::BrokenInvariant);
    let static_supply = ordinary_supply
        .checked_add(if frozen_mask & 1 != 0 {
            market.base_hlp_vault.srlp_shares
        } else {
            0
        })
        .and_then(|value| {
            value.checked_add(if frozen_mask & 2 != 0 {
                market.quote_hlp_vault.srlp_shares
            } else {
                0
            })
        })
        .ok_or(ErrorCode::SupplyOverflow)?;
    // The integrated quote carries its canonical, atom-rounded debts. This is
    // especially important after fee compounding: recomputing them from the
    // derived ordinary tranche can lose a few reserve atoms.
    let endpoint = integrated::materialized_hlp_endpoint(end)?;
    let (book_base, book_quote) = end.book_ordinary()?;
    let static_supply = if let Some((old, new, _, _)) = end.junior_compound {
        require_eq!(market.tranches.junior_reserve_shares, old, ErrorCode::BrokenInvariant);
        static_supply
            .checked_sub(old)
            .and_then(|v| v.checked_add(new))
            .ok_or(ErrorCode::SupplyOverflow)?
    } else {
        static_supply
    };
    let (ownership_base, ownership_quote) = end.ownership_basis()?;
    let (base_backing, quote_backing) = end.backing_equities()?;
    let mut ownership = reconstruct_hlp_ownership(
        static_supply,
        ownership_base,
        ownership_quote,
        base_backing,
        quote_backing,
    )?;
    if frozen_mask & 1 != 0 {
        ownership.base_hlp_srlp_shares = market.base_hlp_vault.srlp_shares;
    }
    if frozen_mask & 2 != 0 {
        ownership.quote_hlp_srlp_shares = market.quote_hlp_vault.srlp_shares;
    }

    let current_base_debt = u64::try_from(Debt::shares_to_debt(
        market.base_hlp_vault.debt_shares,
        market.debt.quote_borrow_index_nad,
    )?)
    .map_err(|_| ErrorCode::DebtMathOverflow)?;
    let current_quote_debt = u64::try_from(Debt::shares_to_debt(
        market.quote_hlp_vault.debt_shares,
        market.debt.base_borrow_index_nad,
    )?)
    .map_err(|_| ErrorCode::DebtMathOverflow)?;
    let base_interest_due = if frozen_mask & 1 != 0 {
        0
    } else {
        current_base_debt.saturating_sub(market.base_hlp_vault.debt_principal)
    };
    let quote_interest_due = if frozen_mask & 2 != 0 {
        0
    } else {
        current_quote_debt.saturating_sub(market.quote_hlp_vault.debt_principal)
    };
    let base_interest_carried = end.base_hlp_quote_carry;
    let quote_interest_carried = end.quote_hlp_base_carry;
    let base_interest_paid = base_interest_due
        .checked_sub(base_interest_carried)
        .ok_or(ErrorCode::BrokenInvariant)?;
    let quote_interest_paid = quote_interest_due
        .checked_sub(quote_interest_carried)
        .ok_or(ErrorCode::BrokenInvariant)?;

    let (base_non_debt_reserve, quote_non_debt_reserve) = if preserve_current_ordinary_reserves {
        // Changing hLP ownership/debt around an already materialized reserve
        // point must not create or destroy ordinary backing. Use full live
        // claims for hedge ownership and debt. Public interest is subtracted
        // only when converting the completed book endpoint into curve reserves.
        let old_base_hlp_live =
            u64::try_from(market.hlp_live_reserve(MarketAsset::Base)?).map_err(|_| ErrorCode::ReserveOverflow)?;
        let old_quote_hlp_live =
            u64::try_from(market.hlp_live_reserve(MarketAsset::Quote)?).map_err(|_| ErrorCode::ReserveOverflow)?;
        (
            market
                .side(MarketAsset::Base)
                .reserves
                .live_reserve
                .checked_sub(u128::from(old_base_hlp_live))
                .and_then(|value| value.checked_sub(u128::from(quote_interest_paid)))
                .and_then(|value| value.checked_add(u128::from(quote_interest_carried)))
                .ok_or(ErrorCode::ReserveUnderflow)?,
            market
                .side(MarketAsset::Quote)
                .reserves
                .live_reserve
                .checked_sub(u128::from(old_quote_hlp_live))
                .and_then(|value| value.checked_sub(u128::from(base_interest_paid)))
                .and_then(|value| value.checked_add(u128::from(base_interest_carried)))
                .ok_or(ErrorCode::ReserveUnderflow)?,
        )
    } else {
        // A quoted swap has not materialized its endpoint yet, so reconstruct
        // the exact post-swap live reserves from the quoted ordinary point.
        let ordinary_base = market.denormalize_amount_floor(book_base, market.base_side.asset_decimals)?;
        let ordinary_quote = market.denormalize_amount_floor(book_quote, market.quote_side.asset_decimals)?;
        let base_equity = market.denormalize_amount_floor(base_backing, market.base_side.asset_decimals)?;
        let quote_equity = market.denormalize_amount_floor(quote_backing, market.quote_side.asset_decimals)?;
        let mut base = ordinary_base
            .checked_add(base_equity)
            .ok_or(ErrorCode::ReserveUnderflow)?;
        let mut quote = ordinary_quote
            .checked_add(quote_equity)
            .ok_or(ErrorCode::ReserveUnderflow)?;
        if end.receivable_hedge {
            base = base
                .checked_sub(
                    market.denormalize_amount_floor(end.base_unpaid_interest, market.base_side.asset_decimals)?,
                )
                .and_then(|v| v.checked_add(u64::try_from(unrealized[0]).ok()?))
                .ok_or(ErrorCode::ReserveUnderflow)?;
            quote = quote
                .checked_sub(
                    market.denormalize_amount_floor(end.quote_unpaid_interest, market.quote_side.asset_decimals)?,
                )
                .and_then(|v| v.checked_add(u64::try_from(unrealized[1]).ok()?))
                .ok_or(ErrorCode::ReserveUnderflow)?;
        }
        if quote_interest_carried > 0 {
            base = base
                .checked_add(quote_interest_carried)
                .ok_or(ErrorCode::ReserveUnderflow)?;
        }
        if base_interest_carried > 0 {
            quote = quote
                .checked_add(base_interest_carried)
                .ok_or(ErrorCode::ReserveUnderflow)?;
        }
        (u128::from(base), u128::from(quote))
    };
    // Integer srLP ownership can move the opposite-asset claim by more than
    // one token atom, especially when the mints have different decimals.
    // Keep the quoted debt only when it still matches the materialized claim;
    // otherwise certify the debt against the actual rounded ownership.
    let (final_base_debt_shares, final_base_debt) = if frozen_mask & 1 != 0 {
        (market.base_hlp_vault.debt_shares, current_base_debt)
    } else {
        canonical_debt_for_proportional_claim_with_carry(
            if end.receivable_hedge {
                quote_non_debt_reserve
                    .checked_sub(unrealized[1])
                    .ok_or(ErrorCode::ReserveUnderflow)?
            } else {
                quote_non_debt_reserve
            },
            ownership.base_hlp_srlp_shares,
            ownership.total_srlp_supply,
            market.debt.quote_borrow_index_nad,
            base_interest_carried,
            market.denormalize_amount_floor(end.hlp_unpaid[0][1], market.quote_side.asset_decimals)?,
            if certify_proportional_claim {
                None
            } else {
                Some(market.denormalize_amount_floor(endpoint.base_hlp_quote_debt, market.quote_side.asset_decimals)?)
            },
        )?
    };
    let (final_quote_debt_shares, final_quote_debt) = if frozen_mask & 2 != 0 {
        (market.quote_hlp_vault.debt_shares, current_quote_debt)
    } else {
        canonical_debt_for_proportional_claim_with_carry(
            if end.receivable_hedge {
                base_non_debt_reserve
                    .checked_sub(unrealized[0])
                    .ok_or(ErrorCode::ReserveUnderflow)?
            } else {
                base_non_debt_reserve
            },
            ownership.quote_hlp_srlp_shares,
            ownership.total_srlp_supply,
            market.debt.base_borrow_index_nad,
            quote_interest_carried,
            market.denormalize_amount_floor(end.hlp_unpaid[1][0], market.base_side.asset_decimals)?,
            if certify_proportional_claim {
                None
            } else {
                Some(market.denormalize_amount_floor(endpoint.quote_hlp_base_debt, market.base_side.asset_decimals)?)
            },
        )?
    };
    let final_base_hlp_live = if frozen_mask & 1 != 0 {
        market.base_hlp_vault.hlp_live_reserve(MarketAsset::Quote)
    } else if base_interest_carried == 0 {
        final_base_debt
    } else {
        final_base_debt
            .checked_sub(base_interest_carried)
            .ok_or(ErrorCode::BrokenInvariant)?
    };
    let final_quote_hlp_live = if frozen_mask & 2 != 0 {
        market.quote_hlp_vault.hlp_live_reserve(MarketAsset::Base)
    } else if quote_interest_carried == 0 {
        final_quote_debt
    } else {
        final_quote_debt
            .checked_sub(quote_interest_carried)
            .ok_or(ErrorCode::BrokenInvariant)?
    };
    // Under the debug flag, print the reconstruction's own inputs. The
    // identity's two sides disagree in the ordinary (non-debt) portion, and
    // separating the quoted ordinary reserve from the quoted hLP equity says
    // which of them carries the gap — and whether it tracks trade size, which
    // would make it solver error, or hLP debt, which would make it accounting.
    #[cfg(feature = "debug-hlp-drift")]
    msg!(
        "hlp-recon base_non_debt={} quote_non_debt={} preserve={} certify={}",
        base_non_debt_reserve,
        quote_non_debt_reserve,
        preserve_current_ordinary_reserves,
        certify_proportional_claim,
    );

    let final_base_curve_reserve = u64::try_from(
        base_non_debt_reserve
            .checked_add(u128::from(final_quote_hlp_live))
            .and_then(|value| value.checked_sub(unrealized[0]))
            .ok_or(ErrorCode::ReserveOverflow)?,
    )
    .map_err(|_| ErrorCode::ReserveOverflow)?;
    let final_quote_curve_reserve = u64::try_from(
        quote_non_debt_reserve
            .checked_add(u128::from(final_base_hlp_live))
            .and_then(|value| value.checked_sub(unrealized[1]))
            .ok_or(ErrorCode::ReserveOverflow)?,
    )
    .map_err(|_| ErrorCode::ReserveOverflow)?;

    let base_receipt = concentrated_hlp_receipt(
        MarketAsset::Base,
        market.base_hlp_vault.srlp_shares,
        ownership.base_hlp_srlp_shares,
        current_base_debt,
        final_base_debt,
        base_interest_paid,
        end.base_hlp_equity
            .checked_add(end.hlp_unpaid[0][0])
            .ok_or(ErrorCode::MarketMathOverflow)?,
        start_supply,
    )?;
    let quote_receipt = concentrated_hlp_receipt(
        MarketAsset::Quote,
        market.quote_hlp_vault.srlp_shares,
        ownership.quote_hlp_srlp_shares,
        current_quote_debt,
        final_quote_debt,
        quote_interest_paid,
        end.quote_hlp_equity
            .checked_add(end.hlp_unpaid[1][1])
            .ok_or(ErrorCode::MarketMathOverflow)?,
        start_supply,
    )?;

    Ok(ConcentratedHlpTransition {
        frozen_hlp_mask: frozen_mask,
        hlp_capitalized: end.hlp_capitalized,
        junior_compound: end.junior_compound,
        junior_seed_compound: end.junior_seed_compound,
        expected_curve_revision: market.curve_revision,
        expected_srlp_supply: start_supply,
        expected_base_srlp_shares: market.base_hlp_vault.srlp_shares,
        expected_quote_srlp_shares: market.quote_hlp_vault.srlp_shares,
        expected_base_debt_shares: market.base_hlp_vault.debt_shares,
        expected_quote_debt_shares: market.quote_hlp_vault.debt_shares,
        expected_base_debt_principal: market.base_hlp_vault.debt_principal,
        expected_quote_debt_principal: market.quote_hlp_vault.debt_principal,
        final_srlp_supply: ownership.total_srlp_supply,
        final_base_srlp_shares: ownership.base_hlp_srlp_shares,
        final_quote_srlp_shares: ownership.quote_hlp_srlp_shares,
        final_base_debt_shares,
        final_quote_debt_shares,
        final_base_debt,
        final_quote_debt,
        final_base_hlp_live,
        final_quote_hlp_live,
        final_base_curve_reserve,
        final_quote_curve_reserve,
        base_output_rounding_carry,
        quote_output_rounding_carry,
        base_interest_paid,
        quote_interest_paid,
        base_receipt,
        quote_receipt,
    })
}

impl ConcentratedHlpTransition {
    pub(crate) fn final_ordinary_point_nad(
        &self,
        market: &Market,
    ) -> Result<crate::transitions::amm::ConcentratedCurvePoint> {
        use crate::transitions::amm::{CurveReservesNad, HlpCurveBasis};
        let basis = HlpCurveBasis::from_inventory(
            CurveReservesNad {
                base: market.normalize_amount(
                    u128::from(self.final_base_curve_reserve) + u128::from(self.base_output_rounding_carry),
                    market.base_side.asset_decimals,
                )?,
                quote: market.normalize_amount(
                    u128::from(self.final_quote_curve_reserve) + u128::from(self.quote_output_rounding_carry),
                    market.quote_side.asset_decimals,
                )?,
            },
            self.final_srlp_supply,
            self.final_base_srlp_shares,
            self.final_quote_srlp_shares,
            market.normalize_amount(u128::from(self.final_base_debt), market.quote_side.asset_decimals)?,
            market.normalize_amount(u128::from(self.final_quote_debt), market.base_side.asset_decimals)?,
        )?;
        market.frozen_adjusted_ordinary(&basis)
    }

    pub(crate) fn debt_deltas(&self) -> (i128, i128) {
        (self.base_receipt.debt_delta, self.quote_receipt.debt_delta)
    }

    pub(crate) fn interest_cash_floors(&self, asset_in: MarketAsset, amount_out: u64) -> SwapCashFloors {
        let mut floors = SwapCashFloors::default();
        floors.set(
            MarketAsset::Base,
            self.quote_interest_paid
                .saturating_add(if asset_in == MarketAsset::Quote { amount_out } else { 0 }),
        );
        floors.set(
            MarketAsset::Quote,
            self.base_interest_paid
                .saturating_add(if asset_in == MarketAsset::Base { amount_out } else { 0 }),
        );
        floors
    }

    pub(crate) fn consume(&self, market: &mut Market) -> Result<(HlpRebalanceReceipt, HlpRebalanceReceipt)> {
        require_eq!(
            market.amm.frozen_hlp_mask,
            self.frozen_hlp_mask,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            market.curve_revision,
            self.expected_curve_revision,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            market.base_side.shares.srlp_supply,
            self.expected_srlp_supply,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            market.quote_side.shares.srlp_supply,
            self.expected_srlp_supply,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            market.base_hlp_vault.srlp_shares,
            self.expected_base_srlp_shares,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            market.quote_hlp_vault.srlp_shares,
            self.expected_quote_srlp_shares,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            market.base_hlp_vault.debt_shares,
            self.expected_base_debt_shares,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            market.quote_hlp_vault.debt_shares,
            self.expected_quote_debt_shares,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            market.base_hlp_vault.debt_principal,
            self.expected_base_debt_principal,
            ErrorCode::BrokenInvariant
        );
        require_eq!(
            market.quote_hlp_vault.debt_principal,
            self.expected_quote_debt_principal,
            ErrorCode::BrokenInvariant
        );

        if let Some((old, new, asset, carry)) = self.junior_compound {
            require_eq!(market.tranches.junior_reserve_shares, old, ErrorCode::BrokenInvariant);
            market.tranches.junior_reserve_shares = new;
            let side = market.side_mut(MarketAsset::try_from_code(asset)?);
            side.fees.junior.carry.swap_remainder_bps = carry;
            let seed = self.junior_seed_compound;
            if seed > 0 {
                side.debit_reserve(seed, true)?;
                side.fees.junior.seed_swap_amount = side
                    .fees
                    .junior
                    .seed_swap_amount
                    .checked_add(seed)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                side.fees.swap_fee_liability = side
                    .fees
                    .swap_fee_liability
                    .checked_add(seed)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                side.fees.swap_fee_custody_balance = side
                    .fees
                    .swap_fee_custody_balance
                    .checked_add(seed)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
            }
            market.synchronize_tranche_income();
        }

        for target in [MarketAsset::Base, MarketAsset::Quote] {
            let [units, owned, expected] = self.hlp_capitalized[target.code() as usize];
            if units > 0 {
                let actual = market.capitalize_hlp_receivable(target, target, units, owned)?;
                require_eq!(actual, expected, ErrorCode::BrokenInvariant);
            }
        }

        if self.quote_interest_paid > 0 {
            debit_cash_for_hlp_interest(market, MarketAsset::Base, self.quote_interest_paid)?;
        }
        if self.base_interest_paid > 0 {
            debit_cash_for_hlp_interest(market, MarketAsset::Quote, self.base_interest_paid)?;
        }
        let old_base_hlp_live = market.hlp_live_reserve(MarketAsset::Base)?;
        let old_quote_hlp_live = market.hlp_live_reserve(MarketAsset::Quote)?;
        let identity_base_live = market
            .base_side
            .reserves
            .live_reserve
            .checked_sub(old_base_hlp_live)
            .and_then(|value| value.checked_add(self.final_quote_hlp_live as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let identity_quote_live = market
            .quote_side
            .reserves
            .live_reserve
            .checked_sub(old_quote_hlp_live)
            .and_then(|value| value.checked_add(self.final_base_hlp_live as u128))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        // `IntegratedCurveState` deliberately excludes unrealized fixed and
        // isolated lending interest: it is claimable yield, not executable AMM
        // principal. The raw live-reserve identity includes that interest, so
        // remove the current amount before comparing it with the quoted curve
        // endpoint. Leverage may realize interest between quote and consume,
        // so this reads the same post-lifecycle debt state as the raw identity.
        let base_unrealized_interest = market.unrealized_interest(MarketAsset::Base)?;
        let quote_unrealized_interest = market.unrealized_interest(MarketAsset::Quote)?;
        let identity_base_curve_reserve = identity_base_live
            .checked_sub(base_unrealized_interest)
            .ok_or(ErrorCode::BrokenInvariant)?;
        let identity_quote_curve_reserve = identity_quote_live
            .checked_sub(quote_unrealized_interest)
            .ok_or(ErrorCode::BrokenInvariant)?;
        // The quoted ordinary output, each target equity, and the reconstructed
        // opposite debt are independently floored to raw atoms. Remove only
        // the output floor carry certified from the frozen curve quote; the
        // remaining reserve reconciliation still has a three-atom limit.
        const MAX_CONCENTRATED_HLP_LIVE_DUST_ATOMS: u128 = 3;
        // Under `debug-hlp-drift`, report the curve-domain distance before
        // deciding on it, alongside the public-interest terms removed from the
        // raw live reserves.
        #[cfg(feature = "debug-hlp-drift")]
        {
            msg!(
                "hlp-drift base={} quote={} base_interest={} quote_interest={} final_base_debt={} final_quote_debt={}",
                identity_base_curve_reserve as i128 - self.final_base_curve_reserve as i128,
                identity_quote_curve_reserve as i128 - self.final_quote_curve_reserve as i128,
                self.base_interest_paid,
                self.quote_interest_paid,
                self.final_base_debt,
                self.final_quote_debt,
            );
            // The individual terms, so the drift can be attributed rather than
            // only observed. The identity is built from the stored reserves
            // and the transition's reconstruction from the quoted endpoint;
            // printing both sides says which one moved.
            msg!(
                "hlp-terms base_live={} quote_live={} old_base_hlp={} old_quote_hlp={} base_public_interest={} quote_public_interest={} final_base_curve={} final_quote_curve={}",
                market.base_side.reserves.live_reserve,
                market.quote_side.reserves.live_reserve,
                old_base_hlp_live,
                old_quote_hlp_live,
                base_unrealized_interest,
                quote_unrealized_interest,
                self.final_base_curve_reserve,
                self.final_quote_curve_reserve,
            );
        }
        require!(
            identity_base_curve_reserve
                .abs_diff((self.final_base_curve_reserve as u128) + u128::from(self.base_output_rounding_carry))
                <= MAX_CONCENTRATED_HLP_LIVE_DUST_ATOMS
                && identity_quote_curve_reserve
                    .abs_diff((self.final_quote_curve_reserve as u128) + u128::from(self.quote_output_rounding_carry))
                    <= MAX_CONCENTRATED_HLP_LIVE_DUST_ATOMS,
            ErrorCode::BrokenInvariant
        );
        market.base_side.shares.srlp_supply = self.final_srlp_supply;
        market.quote_side.shares.srlp_supply = self.final_srlp_supply;
        market.base_hlp_vault.srlp_shares = self.final_base_srlp_shares;
        market.quote_hlp_vault.srlp_shares = self.final_quote_srlp_shares;
        market.base_hlp_vault.debt_shares = self.final_base_debt_shares;
        market.quote_hlp_vault.debt_shares = self.final_quote_debt_shares;
        if self.frozen_hlp_mask & 1 == 0 {
            market.base_hlp_vault.debt_principal = self.final_base_hlp_live;
        }
        if self.frozen_hlp_mask & 2 == 0 {
            market.quote_hlp_vault.debt_principal = self.final_quote_hlp_live;
        }
        market.base_hlp_vault.base_hlp_live_reserve = 0;
        market.base_hlp_vault.quote_hlp_live_reserve = self.final_base_hlp_live;
        market.quote_hlp_vault.base_hlp_live_reserve = self.final_quote_hlp_live;
        market.quote_hlp_vault.quote_hlp_live_reserve = 0;
        market.base_side.reserves.live_reserve = identity_base_live;
        market.quote_side.reserves.live_reserve = identity_quote_live;
        if self.frozen_hlp_mask & 1 == 0 {
            market.base_hlp_vault.last_nav_nad = self.base_receipt.nav_nad;
            market.base_hlp_vault.residual_exposure = 0;
        }
        if self.frozen_hlp_mask & 2 == 0 {
            market.quote_hlp_vault.last_nav_nad = self.quote_receipt.nav_nad;
            market.quote_hlp_vault.residual_exposure = 0;
        }
        market.assert_virtual_reserve_invariant(MarketAsset::Base)?;
        market.assert_virtual_reserve_invariant(MarketAsset::Quote)?;
        Ok((self.base_receipt, self.quote_receipt))
    }
}

impl Market {
    pub fn deposit_single_sided(
        &mut self,
        target_asset: MarketAsset,
        deposit_amount: u64,
        min_hlp_amount: u64,
    ) -> Result<SingleSidedLiquidityReceipt> {
        let market = self;
        market.require_hlp_flash_idle(target_asset)?;
        market.require_hlp_flash_idle(target_asset.opposite())?;
        require!(!market.hlp_frozen(target_asset), ErrorCode::HlpSettlementUnavailable);
        require!(deposit_amount > 0, ErrorCode::AmountZero);
        // Freeze all existing claims before changing either vault's ownership.
        // The same canonical endpoint used by swaps settles accrued funding
        // and matches indexed debt to the actual rounded srLP inventory.
        let pre_prices = current_hlp_curve_prices(market)?;
        let mut before = [None, None];
        for asset in [MarketAsset::Base, MarketAsset::Quote] {
            if market.hlp_frozen(asset) {
                continue;
            }
            let vault = if asset == MarketAsset::Base {
                &market.base_hlp_vault
            } else {
                &market.quote_hlp_vault
            };
            if asset != target_asset && vault.hlp_supply == 0 && vault.srlp_shares == 0 && vault.debt_shares == 0 {
                continue;
            }
            before[asset.code() as usize] = Some((
                vault.hlp_supply,
                vault.cached_settlement_price_nad,
                current_hlp_entry_state_with_prices(market, asset, pre_prices)?,
            ));
        }
        let supply_before = match target_asset {
            MarketAsset::Base => market.base_hlp_vault.hlp_supply,
            MarketAsset::Quote => market.quote_hlp_vault.hlp_supply,
        };
        let nav_before = hlp_share_nav_nad_with_prices(market, target_asset, pre_prices)?;
        checkpoint_hlp_yield_from_srlp(market, MarketAsset::Base)?;
        checkpoint_hlp_yield_from_srlp(market, MarketAsset::Quote)?;
        let mut end = market.integrated_curve_state_nad()?;
        let value = market.normalize_amount(deposit_amount as u128, market.side(target_asset).asset_decimals)?;
        let equity = match target_asset {
            MarketAsset::Base => &mut end.base_hlp_equity,
            MarketAsset::Quote => &mut end.quote_hlp_equity,
        };
        *equity = equity.checked_add(value).ok_or(ErrorCode::MarketMathOverflow)?;
        market.side_mut(target_asset).credit_reserve(deposit_amount, true)?;
        let transition = prepare_concentrated_hlp_transition_from_end(market, end, false, true, 0, 0)?;
        let principal_before = [
            market.quote_hlp_vault.debt_principal,
            market.base_hlp_vault.debt_principal,
        ];
        let (base_rebalance, quote_rebalance) = transition.consume(market)?;
        for asset in [MarketAsset::Base, MarketAsset::Quote] {
            let principal_after = match asset {
                MarketAsset::Base => market.quote_hlp_vault.debt_principal,
                MarketAsset::Quote => market.base_hlp_vault.debt_principal,
            };
            let funding = u64::try_from(market.hlp_funding_debt(asset)?.saturating_sub(principal_after as u128))
                .map_err(|_| ErrorCode::DebtMathOverflow)?;
            let public =
                u64::try_from(market.public_unpaid_interest(asset)?).map_err(|_| ErrorCode::DebtMathOverflow)?;
            require!(
                market.income_accrual_fits(asset, public, funding)?,
                ErrorCode::IncomeEpochNeedsRollover
            );
            if principal_after > principal_before[asset.code() as usize] {
                require_gte!(
                    market.side(asset).reserves.cash_reserve as u128,
                    market.hlp_funding_debt(asset)?,
                    ErrorCode::InsufficientBorrowHeadroom
                );
            }
        }
        // The first mint establishes token units. Later mints use actual
        // economic NAV at the frozen pre-entry mark, including old coupons.
        if supply_before == 0 {
            match target_asset {
                MarketAsset::Base => market.base_hlp_vault.mint_hlp(deposit_amount)?,
                MarketAsset::Quote => market.quote_hlp_vault.mint_hlp(deposit_amount)?,
            }
        }
        let nav_after = hlp_share_nav_nad_with_prices(market, target_asset, pre_prices)?;
        let hlp_amount = if supply_before == 0 {
            // Receipts backed by zero value would price every later entry
            // against zero NAV and reject it. A dust deposit whose ownership
            // rounds away must not become the vault's first cohort.
            require!(nav_after > 0, ErrorCode::AmountZero);
            deposit_amount
        } else {
            let delta = nav_after
                .checked_sub(nav_before)
                .ok_or(ErrorCode::MarketMathOverflow)?
                .min(value);
            require!(delta > 0, ErrorCode::AmountZero);
            require!(nav_before > 0, ErrorCode::MarketMathOverflow);
            // Price the new shares against the post-entry NAV net of the
            // credited value. When the entry's own rebalance rounds extra
            // value into the vault, that surplus stays with existing holders
            // instead of being redeemable by the entrant; otherwise this is
            // the pre-entry NAV.
            let amount = u64::try_from(mul_div_u128(delta, supply_before as u128, nav_after - delta)?)
                .map_err(|_| ErrorCode::MarketMathOverflow)?;
            require!(amount > 0, ErrorCode::AmountZero);
            match target_asset {
                MarketAsset::Base => market.base_hlp_vault.mint_hlp(amount)?,
                MarketAsset::Quote => market.quote_hlp_vault.mint_hlp(amount)?,
            }
            amount
        };
        require_gte!(hlp_amount, min_hlp_amount, ErrorCode::SlippageExceeded);
        let post_prices = current_hlp_curve_prices(market)?;
        for asset in [MarketAsset::Base, MarketAsset::Quote] {
            let Some((old_supply, reference, pre_entry)) = before[asset.code() as usize] else {
                continue;
            };
            let post_entry = current_hlp_entry_state_with_prices(market, asset, post_prices)?;
            require!(
                matches!(
                    post_entry.disposition,
                    HlpEntryDisposition::Settled | HlpEntryDisposition::ControllerGranularityLimited
                ),
                ErrorCode::HlpSettlementUnavailable
            );
            let vault = if asset == MarketAsset::Base {
                &market.base_hlp_vault
            } else {
                &market.quote_hlp_vault
            };
            let new_supply = vault.hlp_supply;
            if old_supply > 0 {
                if pre_entry.disposition == HlpEntryDisposition::ControllerGranularityLimited {
                    let pre_residual = pre_entry.residual_exposure.unsigned_abs();
                    let post_residual = post_entry.residual_exposure.unsigned_abs();
                    require!(
                        post_entry.nav_nad >= pre_entry.nav_nad
                            && post_residual <= pre_residual
                            && (post_entry.residual_exposure == 0
                                || post_entry.residual_exposure.is_negative()
                                    == pre_entry.residual_exposure.is_negative())
                            && ratio_lte_full_width(
                                post_residual,
                                post_entry.nav_nad,
                                pre_residual,
                                pre_entry.nav_nad,
                            )?
                            && ratio_lte_full_width(
                                post_residual,
                                new_supply as u128,
                                pre_residual,
                                old_supply as u128,
                            )?
                            && ratio_lte_full_width(
                                pre_entry.nav_nad,
                                old_supply as u128,
                                post_entry.nav_nad,
                                new_supply as u128,
                            )?,
                        ErrorCode::HlpSettlementUnavailable
                    );
                } else {
                    require!(
                        post_entry.disposition == HlpEntryDisposition::Settled,
                        ErrorCode::HlpSettlementUnavailable
                    );
                }
            }
            let vault = if asset == MarketAsset::Base {
                &mut market.base_hlp_vault
            } else {
                &mut market.quote_hlp_vault
            };
            vault.last_nav_nad = post_entry.nav_nad;
            vault.residual_exposure = post_entry.residual_exposure;
            vault.cached_settlement_price_nad = if old_supply == 0 || post_entry.residual_exposure == 0 {
                post_prices.base_in_quote_nad()
            } else {
                reference
            };
        }
        let hlp_supply = if target_asset == MarketAsset::Base {
            market.base_hlp_vault.hlp_supply
        } else {
            market.quote_hlp_vault.hlp_supply
        };
        let rebalances = [base_rebalance, quote_rebalance];
        let own = rebalances[target_asset.code() as usize];
        let health = market.market_health()?;
        market.assert_market_health_snapshot(&health)?;
        market.assert_virtual_reserve_invariant(MarketAsset::Base)?;
        market.assert_virtual_reserve_invariant(MarketAsset::Quote)?;
        Ok(SingleSidedLiquidityReceipt {
            deposit_amount,
            borrowed_amount: (own.debt_delta + i128::from(own.interest_paid)).max(0) as u64,
            srlp_amount: own.srlp_mint_amount,
            hlp_amount,
            hlp_supply,
            rebalances,
            ..SingleSidedLiquidityReceipt::default()
        })
    }

    /// Exact principal-only close quote for an hLP tranche. Separately
    /// claimable swap-fee and lending-interest yield is intentionally excluded
    /// from Stop Loss semantics and remains owned by the token holder.
    pub fn quote_hlp_withdrawal(&self, target_asset: MarketAsset, hlp_amount: u64) -> Result<u64> {
        require!(hlp_amount > 0, ErrorCode::AmountZero);
        if self.hlp_terminally_closed(target_asset) {
            return Ok(0);
        }
        let vault = match target_asset {
            MarketAsset::Base => &self.base_hlp_vault,
            MarketAsset::Quote => &self.quote_hlp_vault,
        };
        require_gte!(vault.hlp_supply, hlp_amount, ErrorCode::InsufficientBalance);
        let target_redeemed = hlp_exit_underlying_amount(self, target_asset, target_asset, hlp_amount)?;
        let borrowed_asset = target_asset.opposite();
        let borrowed_redeemed = hlp_exit_underlying_amount(self, target_asset, borrowed_asset, hlp_amount)?;
        let debt_shares = hlp_exit_debt_shares(
            vault.debt_shares,
            hlp_amount,
            vault.hlp_supply,
            self.debt.borrow_index(borrowed_asset),
        )?;
        let debt_repaid = Debt::aggregate_debt_reduction_for_shares(
            vault.debt_shares,
            debt_shares,
            self.debt.borrow_index(borrowed_asset),
        )?;
        settled_close_target_amount(self, target_asset, target_redeemed, borrowed_redeemed, debt_repaid)
    }

    /// Withdrawable principal NAV per hLP token in NAD. hLP token decimals
    /// equal their target asset decimals, so the raw output/share ratio is
    /// dimensionless before applying the NAD scale.
    pub fn hlp_principal_nav_per_token_nad(&self, target_asset: MarketAsset, hlp_amount: u64) -> Result<u64> {
        let output = self.quote_hlp_withdrawal(target_asset, hlp_amount)?;
        let value = mul_div_u128(output as u128, NAD as u128, hlp_amount as u128)?;
        u64::try_from(value).map_err(|_| ErrorCode::MarketMathOverflow.into())
    }

    pub fn withdraw_single_sided(
        &mut self,
        target_asset: MarketAsset,
        hlp_amount: u64,
    ) -> Result<SingleSidedLiquidityReceipt> {
        let market = self;
        market.require_hlp_flash_idle(target_asset)?;
        require!(hlp_amount > 0, ErrorCode::AmountZero);
        if market.hlp_terminally_closed(target_asset) {
            // Terminal hLP tokens represent no remaining principal. Burning
            // them must not depend on a later AMM/controller checkpoint;
            // already-accrued fee claims live in the separate yield account.
            let vault = match target_asset {
                MarketAsset::Base => &mut market.base_hlp_vault,
                MarketAsset::Quote => &mut market.quote_hlp_vault,
            };
            vault.burn_hlp(hlp_amount)?;
            if vault.hlp_supply == 0 {
                vault.last_nav_nad = 0;
                vault.cached_settlement_price_nad = 0;
            }
            return Ok(SingleSidedLiquidityReceipt {
                hlp_amount,
                hlp_supply: vault.hlp_supply,
                ..SingleSidedLiquidityReceipt::default()
            });
        }
        // A negative-value cohort is a shared liability. An ordinary holder
        // cannot redeem its backing and leave the deficit with other holders.
        require!(!market.hlp_frozen(target_asset), ErrorCode::HlpSettlementUnavailable);
        let residual_exposure = match target_asset {
            MarketAsset::Base => market.base_hlp_vault.residual_exposure,
            MarketAsset::Quote => market.quote_hlp_vault.residual_exposure,
        };
        let settlement_reference_before = match target_asset {
            MarketAsset::Base => market.base_hlp_vault.cached_settlement_price_nad,
            MarketAsset::Quote => market.quote_hlp_vault.cached_settlement_price_nad,
        };
        // An exit burns the proportional hLP/srLP ownership and debt at the
        // current curve quote. A cached price from the last settlement must
        // not trap the owner after an ordinary trade moves the market; the
        // owner's token minimum and post-state cash/health checks still bind.
        checkpoint_hlp_yield_from_srlp(market, target_asset)?;
        let mut final_price_nad = None;
        let receipt = match target_asset {
            MarketAsset::Base => {
                let supply = market.base_hlp_vault.hlp_supply;
                require_gte!(supply, hlp_amount, ErrorCode::InsufficientBalance);
                let srlp_amount = proportional(market.base_hlp_vault.srlp_shares, hlp_amount, supply)?;
                let quote_debt_shares = hlp_exit_debt_shares(
                    market.base_hlp_vault.debt_shares,
                    hlp_amount,
                    supply,
                    market.debt.quote_borrow_index_nad,
                )?;
                let base_out = hlp_exit_underlying_amount(market, MarketAsset::Base, MarketAsset::Base, hlp_amount)?;
                let quote_redeemed =
                    hlp_exit_underlying_amount(market, MarketAsset::Base, MarketAsset::Quote, hlp_amount)?;
                let debt_repaid = Debt::aggregate_debt_reduction_for_shares(
                    market.base_hlp_vault.debt_shares,
                    quote_debt_shares,
                    market.debt.quote_borrow_index_nad,
                )?;
                let base_hlp_live_debit =
                    proportional(market.base_hlp_vault.base_hlp_live_reserve, hlp_amount, supply)?;
                let base_out =
                    settled_close_target_amount(market, MarketAsset::Base, base_out, quote_redeemed, debt_repaid)?;
                for asset in [MarketAsset::Base, MarketAsset::Quote] {
                    market.capitalize_hlp_receivable(MarketAsset::Base, asset, hlp_amount, supply)?;
                }
                release_hlp_backing_inventory(market, MarketAsset::Base, hlp_amount, supply)?;
                let principal_before = market.base_hlp_vault.debt_principal;
                let debt_clearance = market
                    .base_hlp_vault
                    .clear_debt_repay(quote_debt_shares, market.debt.quote_borrow_index_nad)?;
                let quote_hlp_live_debit = borrowed_hlp_live_debit(
                    market.base_hlp_vault.quote_hlp_live_reserve,
                    principal_before,
                    market.base_hlp_vault.debt_principal,
                    hlp_amount == supply,
                );
                let interest_paid = debt_clearance.interest_paid;
                market.base_side.debit_reserve(base_out, true)?;
                debit_hlp_live_reserve(market, MarketAsset::Base, MarketAsset::Base, base_hlp_live_debit)?;
                debit_hlp_live_reserve(market, MarketAsset::Base, MarketAsset::Quote, quote_hlp_live_debit)?;
                market.base_side.shares.burn(srlp_amount)?;
                market.quote_side.shares.burn(srlp_amount)?;
                market.base_side.assert_share_backing()?;
                market.quote_side.assert_share_backing()?;
                market.base_hlp_vault.debit_srlp(srlp_amount)?;
                debit_cash_for_hlp_interest(market, MarketAsset::Quote, interest_paid)?;
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
                    final_price_nad = Some(current_prices.base_in_quote_nad() as u64);
                    market.base_hlp_vault.last_nav_nad =
                        hlp_nav_nad_with_prices(market, MarketAsset::Base, current_prices)?;
                    if residual_exposure == 0 {
                        market.base_hlp_vault.cached_settlement_price_nad = current_prices.base_in_quote_nad();
                    } else {
                        market.base_hlp_vault.cached_settlement_price_nad = settlement_reference_before;
                    }
                }
                SingleSidedLiquidityReceipt {
                    hlp_amount,
                    srlp_amount,
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
                let srlp_amount = proportional(market.quote_hlp_vault.srlp_shares, hlp_amount, supply)?;
                let base_debt_shares = hlp_exit_debt_shares(
                    market.quote_hlp_vault.debt_shares,
                    hlp_amount,
                    supply,
                    market.debt.base_borrow_index_nad,
                )?;
                let quote_out = hlp_exit_underlying_amount(market, MarketAsset::Quote, MarketAsset::Quote, hlp_amount)?;
                let base_redeemed =
                    hlp_exit_underlying_amount(market, MarketAsset::Quote, MarketAsset::Base, hlp_amount)?;
                let debt_repaid = Debt::aggregate_debt_reduction_for_shares(
                    market.quote_hlp_vault.debt_shares,
                    base_debt_shares,
                    market.debt.base_borrow_index_nad,
                )?;
                let quote_hlp_live_debit =
                    proportional(market.quote_hlp_vault.quote_hlp_live_reserve, hlp_amount, supply)?;
                let quote_out =
                    settled_close_target_amount(market, MarketAsset::Quote, quote_out, base_redeemed, debt_repaid)?;
                for asset in [MarketAsset::Base, MarketAsset::Quote] {
                    market.capitalize_hlp_receivable(MarketAsset::Quote, asset, hlp_amount, supply)?;
                }
                release_hlp_backing_inventory(market, MarketAsset::Quote, hlp_amount, supply)?;
                let principal_before = market.quote_hlp_vault.debt_principal;
                let debt_clearance = market
                    .quote_hlp_vault
                    .clear_debt_repay(base_debt_shares, market.debt.base_borrow_index_nad)?;
                let base_hlp_live_debit = borrowed_hlp_live_debit(
                    market.quote_hlp_vault.base_hlp_live_reserve,
                    principal_before,
                    market.quote_hlp_vault.debt_principal,
                    hlp_amount == supply,
                );
                let interest_paid = debt_clearance.interest_paid;
                market.quote_side.debit_reserve(quote_out, true)?;
                debit_hlp_live_reserve(market, MarketAsset::Quote, MarketAsset::Quote, quote_hlp_live_debit)?;
                debit_hlp_live_reserve(market, MarketAsset::Quote, MarketAsset::Base, base_hlp_live_debit)?;
                market.base_side.shares.burn(srlp_amount)?;
                market.quote_side.shares.burn(srlp_amount)?;
                market.base_side.assert_share_backing()?;
                market.quote_side.assert_share_backing()?;
                market.quote_hlp_vault.debit_srlp(srlp_amount)?;
                debit_cash_for_hlp_interest(market, MarketAsset::Base, interest_paid)?;
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
                    final_price_nad = Some(current_prices.base_in_quote_nad() as u64);
                    market.quote_hlp_vault.last_nav_nad =
                        hlp_nav_nad_with_prices(market, MarketAsset::Quote, current_prices)?;
                    if residual_exposure == 0 {
                        market.quote_hlp_vault.cached_settlement_price_nad = current_prices.base_in_quote_nad();
                    } else {
                        market.quote_hlp_vault.cached_settlement_price_nad = settlement_reference_before;
                    }
                }
                SingleSidedLiquidityReceipt {
                    hlp_amount,
                    srlp_amount,
                    hlp_supply: market.quote_hlp_vault.hlp_supply,
                    target_amount_out: quote_out,
                    debt_repaid: debt_clearance.debt_reduced,
                    interest_paid,
                    ..SingleSidedLiquidityReceipt::default()
                }
            }
        };
        if receipt.hlp_supply == 0 {
            market.retire_hlp_receivables(target_asset)?;
        }
        if let Some(price_nad) = final_price_nad {
            // NAV checkpointing changes no curve inputs. Share its exact price
            // with risk rather than evaluating the identical geometry twice.
            let slot = Clock::get().map(|clock| clock.slot).unwrap_or(market.last_update_slot);
            let curve_depth_nad = market
                .amm
                .concentrated_curve_cache
                .tail_liquidity
                .checked_add(market.amm.concentrated_curve_cache.concentrated_liquidity)
                .ok_or(ErrorCode::InvariantOverflow)?;
            market.observe_risk_from_concentrated_curve(price_nad, curve_depth_nad, slot)?;
            market.risk_revision = market.curve_revision;
        } else {
            market.refresh_risk()?;
        }
        let health = market.market_health()?;
        market.assert_market_health_snapshot(&health)?;
        market.assert_virtual_reserve_invariant(MarketAsset::Base)?;
        market.assert_virtual_reserve_invariant(MarketAsset::Quote)?;
        Ok(receipt)
    }
}

fn debit_cash_for_hlp_interest(market: &mut Market, asset: MarketAsset, interest_paid: u64) -> Result<()> {
    if interest_paid == 0 {
        return Ok(());
    }
    if market.side(asset).fees.junior.initialized {
        // The plan settles this amount before installing its final vault debt.
        // Remove the matching frozen fees alongside the LP coupon, rather than
        // treating the borrower's entire gross payment as LP-owned income.
        let i = asset.code() as usize + 2;
        let gross = market.income.get()?.revenue[i].gross_unpaid;
        let remaining = gross.checked_sub(interest_paid).ok_or(ErrorCode::BrokenInvariant)?;
        let markup = market.protocol_interest_markup_bps;
        let share = market.operator.interest_share_bps;
        let fee = market.income.get_mut()?.revenue[i].synchronize(remaining, interest_paid, markup, share)?;
        market.clear_income_receivable(asset, true, interest_paid - fee, interest_paid - fee)?;
    }
    let borrowed_side = market.side_mut(asset);
    borrowed_side.reserves.live_reserve = borrowed_side
        .reserves
        .live_reserve
        .checked_sub(u128::from(interest_paid))
        .ok_or(ErrorCode::ReserveUnderflow)?;
    borrowed_side.reserves.cash_reserve = borrowed_side
        .reserves
        .cash_reserve
        .checked_sub(interest_paid)
        .ok_or(ErrorCode::CashReserveUnderflow)?;
    Ok(())
}

/// The borrowed-side hLP live reserve is the vault's funded debt principal:
/// every hLP transition stores one value into both. An exit debits it by the
/// principal it actually repaid (all of it on a full exit). Independently
/// floored proportional shares drift apart by an atom or two per exit, and a
/// few consecutive exits pushed the next swap past the live-reserve identity.
fn borrowed_hlp_live_debit(live: u64, principal_before: u64, principal_after: u64, full_exit: bool) -> u64 {
    if full_exit {
        live
    } else {
        principal_before.saturating_sub(principal_after).min(live)
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
    if borrowed_redeemed == debt_repaid {
        return Ok(target_redeemed);
    }

    let start = market.ordinary_curve_point_nad()?;
    let geometry = market
        .current_concentrated_curve_geometry()?
        .ok_or(ErrorCode::BrokenInvariant)?;
    let borrowed_asset = target_asset.opposite();
    if borrowed_redeemed > debt_repaid {
        let surplus_borrowed = borrowed_redeemed
            .checked_sub(debt_repaid)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let surplus_nad =
            market.normalize_amount(surplus_borrowed as u128, market.side(borrowed_asset).asset_decimals)?;
        let direction = match borrowed_asset {
            MarketAsset::Base => ConcentratedCurveDirection::BaseToQuote,
            MarketAsset::Quote => ConcentratedCurveDirection::QuoteToBase,
        };
        let quote = geometry.quote_exact_in(start, surplus_nad, direction)?;
        let target_from_surplus =
            market.denormalize_amount_floor(quote.amount_out, market.side(target_asset).asset_decimals)?;
        return target_redeemed
            .checked_add(target_from_surplus)
            .ok_or_else(|| ErrorCode::MarketMathOverflow.into());
    }

    let borrowed_shortfall = debt_repaid
        .checked_sub(borrowed_redeemed)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let shortfall_nad =
        market.normalize_amount(borrowed_shortfall as u128, market.side(borrowed_asset).asset_decimals)?;
    let direction = match target_asset {
        MarketAsset::Base => ConcentratedCurveDirection::BaseToQuote,
        MarketAsset::Quote => ConcentratedCurveDirection::QuoteToBase,
    };
    let quote = geometry.quote_exact_out(start, shortfall_nad, direction)?;
    let target_retained = market.denormalize_amount_ceil(quote.amount_in, market.side(target_asset).asset_decimals)?;
    require_gte!(target_redeemed, target_retained, ErrorCode::HlpSettlementUnavailable);
    target_redeemed
        .checked_sub(target_retained)
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HlpCurvePrices {
    base_in_quote_nad: u128,
}

impl HlpCurvePrices {
    pub(super) const fn base_in_quote_nad(self) -> u128 {
        self.base_in_quote_nad
    }
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
    proportional_hedge_available: bool,
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
    let nav_nad = collateral.saturating_sub(debt);
    let ideal_delta = if values.target_inventory_value_nad == 0 {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HlpEntryDisposition {
    Settled,
    ControllerGranularityLimited,
    Actionable,
    CashConstrained,
    Unhedgeable,
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
    let residual_exposure =
        recognized_hlp_residual_exposure(valuation.ideal_delta, valuation.nav_nad, market.amount_decimals()?);
    let disposition = if residual_exposure == 0 {
        HlpEntryDisposition::Settled
    } else if !valuation.proportional_hedge_available || valuation.nav_nad == 0 {
        HlpEntryDisposition::Unhedgeable
    } else if residual_exposure > 0 {
        'entry: {
            let amounts = proportional_rebalance_amounts(market, target_asset, residual_exposure, valuation)?;
            if !complete_rebalance_amounts(amounts) {
                break 'entry HlpEntryDisposition::ControllerGranularityLimited;
            }
            let (base_leg, quote_leg) = match target_asset {
                MarketAsset::Base => (amounts.target_leg_amount, amounts.borrowed_leg_amount),
                MarketAsset::Quote => (amounts.borrowed_leg_amount, amounts.target_leg_amount),
            };
            let shares = market.srlp_for_deposit(
                market.curve_reserve(MarketAsset::Base)?,
                market.curve_reserve(MarketAsset::Quote)?,
                base_leg,
                quote_leg,
            )?;
            if shares == 0 {
                break 'entry HlpEntryDisposition::ControllerGranularityLimited;
            }
            if market.hlp_funding_headroom(target_asset.opposite())? < amounts.debt_amount {
                break 'entry HlpEntryDisposition::CashConstrained;
            }
            HlpEntryDisposition::Actionable
        }
    } else {
        'entry: {
            let (borrow_index, debt_shares, debt_principal, vault_srlp) = match target_asset {
                MarketAsset::Base => (
                    market.debt.quote_borrow_index_nad,
                    market.base_hlp_vault.debt_shares,
                    market.base_hlp_vault.debt_principal,
                    market.base_hlp_vault.srlp_shares,
                ),
                MarketAsset::Quote => (
                    market.debt.base_borrow_index_nad,
                    market.quote_hlp_vault.debt_shares,
                    market.quote_hlp_vault.debt_principal,
                    market.quote_hlp_vault.srlp_shares,
                ),
            };
            if debt_shares == 0 || vault_srlp == 0 || valuation.values.debt_value_nad == 0 {
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
            let base_burn = srlp_shares_for_live_reserve_amount(market, MarketAsset::Base, base_leg)?;
            let quote_burn = srlp_shares_for_live_reserve_amount(market, MarketAsset::Quote, quote_leg)?;
            let srlp_burn = base_burn.min(quote_burn).min(vault_srlp);
            if srlp_burn == 0 {
                break 'entry HlpEntryDisposition::ControllerGranularityLimited;
            }
            let base_out = srlp_live_underlying_amount(market, MarketAsset::Base, srlp_burn)?;
            let quote_out = srlp_live_underlying_amount(market, MarketAsset::Quote, srlp_burn)?;
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

/// Values both active hLP principals from a caller-proved executable price,
/// avoiding a duplicate curve solve when an identity-bound endpoint already
/// carries the marginal price.
pub(crate) fn current_hlp_signed_navs_with_prices(market: &Market, prices: HlpCurvePrices) -> Result<(i128, i128)> {
    let base = current_hlp_inventory_values_nad_with_prices(market, MarketAsset::Base, prices)?;
    let quote = current_hlp_inventory_values_nad_with_prices(market, MarketAsset::Quote, prices)?;
    let principal_nav = |values: HlpInventoryValuesNad| -> Result<i128> {
        let collateral = values
            .target_inventory_value_nad
            .checked_add(values.opposite_inventory_value_nad)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        signed_value_difference(collateral, values.debt_value_nad)
    };
    Ok((principal_nav(base)?, principal_nav(quote)?))
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

pub(crate) fn checkpoint_one_hlp_with_prices(
    market: &mut Market,
    target_asset: MarketAsset,
    prices: HlpCurvePrices,
) -> Result<i128> {
    let valuation = current_hlp_valuation_with_prices(market, target_asset, prices)?;
    let nav = valuation.nav_nad;
    let ideal_delta = recognized_hlp_residual_exposure(valuation.ideal_delta, nav, market.amount_decimals()?);
    let vault = match target_asset {
        MarketAsset::Base => &mut market.base_hlp_vault,
        MarketAsset::Quote => &mut market.quote_hlp_vault,
    };
    vault.last_nav_nad = nav;
    vault.residual_exposure = ideal_delta;
    // This reference belongs to the last actual hLP settlement/rebalance.
    // Updating it during a generic market checkpoint would make the later
    // settlement-divergence guard compare the current price with itself.
    Ok(ideal_delta)
}

pub(crate) fn checkpoint_hlp_yield_from_srlp(market: &mut Market, target_asset: MarketAsset) -> Result<()> {
    let srlp_shares = match target_asset {
        MarketAsset::Base => market.base_hlp_vault.srlp_shares,
        MarketAsset::Quote => market.quote_hlp_vault.srlp_shares,
    };
    checkpoint_hlp_yield_from_srlp_shares(market, target_asset, srlp_shares)
}

pub(crate) fn checkpoint_hlp_yield_from_srlp_shares(
    market: &mut Market,
    target_asset: MarketAsset,
    eligible_srlp_shares: u64,
) -> Result<()> {
    market.base_side.carry_forward_swap_fees()?;
    market.base_side.carry_forward_interest()?;
    market.quote_side.carry_forward_swap_fees()?;
    market.quote_side.carry_forward_interest()?;
    let base_side = *market.base_side;
    let quote_side = *market.quote_side;
    match target_asset {
        MarketAsset::Base => {
            market
                .base_hlp_vault
                .checkpoint_yield_from_srlp_shares(&base_side, &quote_side, eligible_srlp_shares)
        }
        MarketAsset::Quote => {
            market
                .quote_hlp_vault
                .checkpoint_yield_from_srlp_shares(&base_side, &quote_side, eligible_srlp_shares)
        }
    }
}

/// One executable forward price supplies both hLP valuation directions through
/// multiplication or division. Re-evaluating the curve for every inventory and
/// debt leg would exhaust Solana's non-freeing 32 KiB heap on composite swaps.
pub(crate) fn current_hlp_curve_prices(market: &Market) -> Result<HlpCurvePrices> {
    let price_nad = market
        .current_concentrated_spot_price_nad()?
        .ok_or(ErrorCode::BrokenInvariant)?;
    hlp_curve_prices_from_base_price_nad(price_nad as u128)
}

pub(crate) fn hlp_curve_prices_from_base_price_nad(base_in_quote_nad: u128) -> Result<HlpCurvePrices> {
    require!(base_in_quote_nad > 0, ErrorCode::InvalidSettlementPrice);
    let base_in_quote_nad = u64::try_from(base_in_quote_nad).map_err(|_| ErrorCode::MarketMathOverflow)? as u128;
    Ok(HlpCurvePrices { base_in_quote_nad })
}

/// Economic value of an hLP share, including the srLP claim on public interest
/// already accrued into live reserves. The controller uses these same claims;
/// swap quotes independently exclude unpaid interest from trading reserves.
pub(super) fn hlp_share_nav_nad_with_prices(
    market: &Market,
    target_asset: MarketAsset,
    prices: HlpCurvePrices,
) -> Result<u128> {
    let vault = match target_asset {
        MarketAsset::Base => &market.base_hlp_vault,
        MarketAsset::Quote => &market.quote_hlp_vault,
    };
    if vault.hlp_supply == 0 {
        return Ok(0);
    }
    let base_claim = hlp_full_underlying_amount(market, target_asset, MarketAsset::Base)?;
    let quote_claim = hlp_full_underlying_amount(market, target_asset, MarketAsset::Quote)?;
    let base_value =
        asset_value_in_target_nad_with_prices(market, prices, MarketAsset::Base, base_claim, target_asset)?;
    let quote_value =
        asset_value_in_target_nad_with_prices(market, prices, MarketAsset::Quote, quote_claim, target_asset)?;
    let debt = current_hlp_inventory_values_nad_with_prices(market, target_asset, prices)?.debt_value_nad;
    base_value
        .checked_add(quote_value)
        .and_then(|assets| assets.checked_sub(debt))
        .ok_or_else(|| ErrorCode::Undercollateralized.into())
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

pub(super) fn srlp_live_underlying_amount(market: &Market, asset: MarketAsset, srlp_amount: u64) -> Result<u64> {
    let side = market.side(asset);
    srlp_live_underlying_amount_from_values(side.reserves.live_reserve, side.shares.srlp_supply, srlp_amount)
}

fn hlp_exit_underlying_amount(
    market: &Market,
    target: MarketAsset,
    asset: MarketAsset,
    hlp_amount: u64,
) -> Result<u64> {
    let vault = if target == MarketAsset::Base {
        &market.base_hlp_vault
    } else {
        &market.quote_hlp_vault
    };
    let srlp_amount = proportional(vault.srlp_shares, hlp_amount, vault.hlp_supply)?;
    if !market.side(asset).fees.junior.initialized {
        return srlp_live_underlying_amount(market, asset, srlp_amount);
    }
    let funded = srlp_live_underlying_amount_from_values(
        u128::from(market.curve_reserve(asset)?),
        market.base_side.shares.srlp_supply,
        srlp_amount,
    )?;
    funded
        .checked_add(market.quote_hlp_receivable_conversion(target, asset, hlp_amount, vault.hlp_supply)?)
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

pub(super) fn hlp_full_underlying_amount(market: &Market, target: MarketAsset, asset: MarketAsset) -> Result<u64> {
    let vault = if target == MarketAsset::Base {
        &market.base_hlp_vault
    } else {
        &market.quote_hlp_vault
    };
    let side = market.side(asset);
    if !side.fees.junior.initialized {
        return srlp_live_underlying_amount(market, asset, vault.srlp_shares);
    }
    let funded = srlp_live_underlying_amount_from_values(
        u128::from(market.curve_reserve(asset)?),
        side.shares.srlp_supply,
        vault.srlp_shares,
    )?;
    funded
        .checked_add(market.hlp_unpaid_income(target, asset)?)
        .ok_or_else(|| ErrorCode::MarketMathOverflow.into())
}

fn srlp_live_underlying_amount_from_values(live_reserve: u128, supply: u64, srlp_amount: u64) -> Result<u64> {
    if srlp_amount == 0 {
        return Ok(0);
    }
    require_gte!(supply, srlp_amount, ErrorCode::InsufficientBalance);
    let amount = mul_div_u128(srlp_amount as u128, live_reserve, supply as u128)?;
    u64::try_from(amount).map_err(|_| ErrorCode::MarketMathOverflow.into())
}

pub(super) fn asset_value_in_target_nad_with_prices(
    market: &Market,
    prices: HlpCurvePrices,
    asset: MarketAsset,
    amount: u64,
    target_asset: MarketAsset,
) -> Result<u128> {
    if amount == 0 {
        return Ok(0);
    }
    let amount_nad = market.normalize_amount(amount as u128, market.side(asset).asset_decimals)?;
    if asset == target_asset {
        return Ok(amount_nad);
    }
    match asset {
        MarketAsset::Base => mul_div_u128(amount_nad, prices.base_in_quote_nad(), NAD as u128),
        MarketAsset::Quote => mul_div_u128(amount_nad, NAD as u128, prices.base_in_quote_nad()),
    }
}

fn current_hlp_inventory_values_nad_with_prices(
    market: &Market,
    target_asset: MarketAsset,
    prices: HlpCurvePrices,
) -> Result<HlpInventoryValuesNad> {
    let opposite_asset = target_asset.opposite();
    let target_underlying = hlp_full_underlying_amount(market, target_asset, target_asset)?;
    let opposite_underlying = hlp_full_underlying_amount(market, target_asset, opposite_asset)?;
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
        match asset {
            MarketAsset::Base => mul_div_u128(value_nad, NAD as u128, prices.base_in_quote_nad())?,
            MarketAsset::Quote => mul_div_u128(value_nad, prices.base_in_quote_nad(), NAD as u128)?,
        }
    };
    market.denormalize_amount_floor(amount_nad, market.side(asset).asset_decimals)
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

fn srlp_shares_for_live_reserve_amount(market: &Market, asset: MarketAsset, reserve_amount: u64) -> Result<u64> {
    if reserve_amount == 0 {
        return Ok(0);
    }
    let side = market.side(asset);
    require!(
        side.reserves.live_reserve > 0 && side.shares.srlp_supply > 0,
        ErrorCode::InsufficientLiquidity
    );
    side.shares
        .shares_for_deposit(side.reserves.live_reserve, reserve_amount)
}

fn proportional(amount: u64, numerator: u64, denominator: u64) -> Result<u64> {
    let value = (amount as u128)
        .checked_mul(numerator as u128)
        .and_then(|value| value.checked_div(denominator as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    u64::try_from(value).map_err(|_| ErrorCode::MarketMathOverflow.into())
}

/// Debt shares an hLP exit burns. The exiting holder takes floored asset
/// claims, so it also takes at least its fraction of the vault's indexed
/// debt, rounded up and capped at the floored total. From the floored
/// proportional share count, this burns the fewest shares whose aggregate
/// debt reduction reaches that amount. A plain floor could leave up to an
/// atom of the borrowed asset's debt with the remaining holders, and burned
/// no shares for an exit below one share.
fn hlp_exit_debt_shares(debt_shares: u128, hlp_amount: u64, hlp_supply: u64, borrow_index_nad: u128) -> Result<u128> {
    let floored = debt_shares
        .checked_mul(hlp_amount as u128)
        .and_then(|value| value.checked_div(hlp_supply as u128))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    if debt_shares == 0 {
        return Ok(floored);
    }
    require!(borrow_index_nad >= NAD as u128, ErrorCode::DebtShareDivisionOverflow);
    let debt = Debt::shares_to_debt(debt_shares, borrow_index_nad)?;
    let indexed = debt_shares
        .checked_mul(borrow_index_nad)
        .ok_or(ErrorCode::DebtShareMathOverflow)?;
    let owed = mul_div_ceil_u128(indexed, hlp_amount as u128, hlp_supply as u128 * NAD as u128)?.min(debt);
    // Remaining shares r leave floor(r * index / NAD) <= debt - owed exactly
    // when r * index < (debt - owed + 1) * NAD.
    let limit = debt
        .checked_sub(owed)
        .and_then(|left| left.checked_add(1))
        .and_then(|left| left.checked_mul(NAD as u128))
        .ok_or(ErrorCode::DebtShareMathOverflow)?;
    let max_remaining = (limit - 1) / borrow_index_nad;
    Ok(floored.max(debt_shares.saturating_sub(max_remaining)))
}

#[cfg(test)]
include!("../../../tests/support/hlp_engine.rs");
