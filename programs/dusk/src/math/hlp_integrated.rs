//! O(1) integrated curve + opposite-asset-neutral hLP transition.
//!
//! Trader cash moves on the ordinary yLP tranche `(U,V)`. hLP synthetic liquidity is
//! reconstructed afterward from fixed target-asset equities, which keeps both
//! hLPs exactly one-sided without a finite-difference or Broyden solve.

use anchor_lang::prelude::*;

use crate::errors::ErrorCode;

use super::{mul_div_u128, ExplicitCurveDirection, ExplicitCurveGeometry, ExplicitCurvePoint, ExplicitCurveQuote};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IntegratedSwapDirection {
    BaseToQuote,
    QuoteToBase,
}

/// Ordinary-yLP cash curve plus fixed hLP target-asset equities.
///
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct IntegratedCurveState {
    pub ordinary_base: u128,
    pub ordinary_quote: u128,
    pub base_hlp_equity: u128,
    pub quote_hlp_equity: u128,
    pub base_hlp_quote_debt: u128,
    pub quote_hlp_base_debt: u128,
}

impl IntegratedCurveState {
    /// Decomposes total live reserves into the ordinary trader-facing tranche
    /// and two perfectly hedged hLP positions at the current reserve ratio.
    ///
    /// The Base hLP owns `base_hlp_equity` Base plus an equal Quote
    /// claim/debt; the Quote hLP is symmetric. Integer opposite claims are
    /// floored once and the same atoms are used for debt, so the residual
    /// opposite exposure is exactly zero rather than approximately zero.
    pub(crate) fn from_total_reserves(
        total_base: u128,
        total_quote: u128,
        base_hlp_equity: u128,
        quote_hlp_equity: u128,
    ) -> Result<Self> {
        require!(total_base > 0 && total_quote > 0, ErrorCode::InsufficientLiquidity);
        let quote_hlp_base_claim = mul_div_u128(quote_hlp_equity, total_base, total_quote)?;
        let base_hlp_quote_claim = mul_div_u128(base_hlp_equity, total_quote, total_base)?;
        let ordinary_base = total_base
            .checked_sub(base_hlp_equity)
            .and_then(|value| value.checked_sub(quote_hlp_base_claim))
            .ok_or(ErrorCode::InsufficientLiquidity)?;
        let ordinary_quote = total_quote
            .checked_sub(quote_hlp_equity)
            .and_then(|value| value.checked_sub(base_hlp_quote_claim))
            .ok_or(ErrorCode::InsufficientLiquidity)?;
        require!(
            ordinary_base > 0 && ordinary_quote > 0,
            ErrorCode::InsufficientLiquidity
        );
        Ok(Self {
            ordinary_base,
            ordinary_quote,
            base_hlp_equity,
            quote_hlp_equity,
            base_hlp_quote_debt: base_hlp_quote_claim,
            quote_hlp_base_debt: quote_hlp_base_claim,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct IntegratedHlpEndpoint {
    pub total_base: u128,
    pub total_quote: u128,
    /// Quote units owed by the Base-target hLP.
    pub base_hlp_quote_debt: u128,
    /// Base units owed by the Quote-target hLP.
    pub quote_hlp_base_debt: u128,
}

/// Canonical yLP ownership implied by an ordinary tranche and two one-sided
/// hLP equities. Ordinary yLP shares never change during a swap; only the two
/// vault-owned balances are reconstructed around them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct IntegratedOwnershipEndpoint {
    pub total_ylp_supply: u64,
    pub base_hlp_ylp_shares: u64,
    pub quote_hlp_ylp_shares: u64,
}

pub(crate) fn reconstruct_hlp_ownership(
    ordinary_ylp_supply: u64,
    ordinary_base: u128,
    ordinary_quote: u128,
    base_hlp_equity: u128,
    quote_hlp_equity: u128,
) -> Result<IntegratedOwnershipEndpoint> {
    require!(ordinary_ylp_supply > 0, ErrorCode::SupplyUnderflow);
    require!(
        ordinary_base > 0 && ordinary_quote > 0,
        ErrorCode::InsufficientLiquidity
    );
    let base_hlp_ylp_shares = u64::try_from(mul_div_u128(
        ordinary_ylp_supply as u128,
        base_hlp_equity,
        ordinary_base,
    )?)
    .map_err(|_| ErrorCode::SupplyOverflow)?;
    let quote_hlp_ylp_shares = u64::try_from(mul_div_u128(
        ordinary_ylp_supply as u128,
        quote_hlp_equity,
        ordinary_quote,
    )?)
    .map_err(|_| ErrorCode::SupplyOverflow)?;
    let total_ylp_supply = ordinary_ylp_supply
        .checked_add(base_hlp_ylp_shares)
        .and_then(|value| value.checked_add(quote_hlp_ylp_shares))
        .ok_or(ErrorCode::SupplyOverflow)?;
    Ok(IntegratedOwnershipEndpoint {
        total_ylp_supply,
        base_hlp_ylp_shares,
        quote_hlp_ylp_shares,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct IntegratedExactInQuote {
    pub amount_out: u128,
    pub end: IntegratedCurveState,
    pub hlp: IntegratedHlpEndpoint,
    pub base_hlp_quote_debt_delta: i128,
    pub quote_hlp_base_debt_delta: i128,
    pub curve: ExplicitCurveQuote,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct IntegratedFrozenFeeQuote {
    pub total_fee: u128,
    pub amount_in_after_fee: u128,
    /// Sole executable curve and hedge transition.
    pub executable: IntegratedExactInQuote,
}

fn signed_delta(end: u128, start: u128) -> Result<i128> {
    if end >= start {
        i128::try_from(end - start).map_err(|_| ErrorCode::MarketMathOverflow.into())
    } else {
        i128::try_from(start - end)
            .map(|value| -value)
            .map_err(|_| ErrorCode::MarketMathOverflow.into())
    }
}

pub(crate) fn reconstruct_hlp_endpoint(state: IntegratedCurveState) -> Result<IntegratedHlpEndpoint> {
    require!(
        state.ordinary_base > 0 && state.ordinary_quote > 0,
        ErrorCode::InsufficientLiquidity
    );

    let base_from_quote_hlp = mul_div_u128(state.quote_hlp_equity, state.ordinary_base, state.ordinary_quote)?;
    let quote_from_base_hlp = mul_div_u128(state.base_hlp_equity, state.ordinary_quote, state.ordinary_base)?;

    let total_base = state
        .ordinary_base
        .checked_add(state.base_hlp_equity)
        .and_then(|value| value.checked_add(base_from_quote_hlp))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let total_quote = state
        .ordinary_quote
        .checked_add(state.quote_hlp_equity)
        .and_then(|value| value.checked_add(quote_from_base_hlp))
        .ok_or(ErrorCode::MarketMathOverflow)?;

    Ok(IntegratedHlpEndpoint {
        total_base,
        total_quote,
        base_hlp_quote_debt: quote_from_base_hlp,
        quote_hlp_base_debt: base_from_quote_hlp,
    })
}

/// Applies an hLP-funded target-asset output bonus after the ordinary curve
/// has priced the complete input. The bonus changes no curve coordinate; it
/// burns only the selected hLP's equity and reconstructs that vault's matching
/// opposite claim/debt at the already-quoted endpoint.
pub(crate) fn apply_hlp_recovery_bonus(
    start: IntegratedCurveState,
    quote: &mut IntegratedFrozenFeeQuote,
    target_is_base: bool,
    bonus_output_nad: u128,
) -> Result<()> {
    if bonus_output_nad == 0 {
        return Ok(());
    }
    let end = &mut quote.executable.end;
    if target_is_base {
        end.base_hlp_equity = end
            .base_hlp_equity
            .checked_sub(bonus_output_nad)
            .ok_or(ErrorCode::InsufficientLiquidity)?;
    } else {
        end.quote_hlp_equity = end
            .quote_hlp_equity
            .checked_sub(bonus_output_nad)
            .ok_or(ErrorCode::InsufficientLiquidity)?;
    }
    let hlp = reconstruct_hlp_endpoint(*end)?;
    end.base_hlp_quote_debt = hlp.base_hlp_quote_debt;
    end.quote_hlp_base_debt = hlp.quote_hlp_base_debt;
    quote.executable.hlp = hlp;
    quote.executable.base_hlp_quote_debt_delta = signed_delta(hlp.base_hlp_quote_debt, start.base_hlp_quote_debt)?;
    quote.executable.quote_hlp_base_debt_delta = signed_delta(hlp.quote_hlp_base_debt, start.quote_hlp_base_debt)?;
    quote.executable.amount_out = quote
        .executable
        .amount_out
        .checked_add(bonus_output_nad)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    Ok(())
}

/// Adds one already-priced LP fee to total reserves and attributes its value
/// to the yLP owners which existed before the swap. Ordinary yLP supply stays
/// fixed; each hLP's pro-rata fee gain is converted into its target-asset
/// equity at the final total-reserve ratio, then the canonical one-sided
/// claims and debts are reconstructed algebraically.
pub(crate) fn apply_compounded_ylp_fee(
    start: IntegratedCurveState,
    quote: &mut IntegratedFrozenFeeQuote,
    fee_is_base: bool,
    compounded_fee_nad: u128,
    eligible_ylp_supply: u64,
    base_hlp_ylp_shares: u64,
    quote_hlp_ylp_shares: u64,
) -> Result<()> {
    if compounded_fee_nad == 0 {
        return Ok(());
    }
    require!(eligible_ylp_supply > 0, ErrorCode::SupplyUnderflow);
    require_gte!(
        eligible_ylp_supply,
        base_hlp_ylp_shares
            .checked_add(quote_hlp_ylp_shares)
            .ok_or(ErrorCode::SupplyOverflow)?,
        ErrorCode::SupplyUnderflow
    );

    let base_hlp_fee = mul_div_u128(
        compounded_fee_nad,
        base_hlp_ylp_shares as u128,
        eligible_ylp_supply as u128,
    )?;
    let quote_hlp_fee = mul_div_u128(
        compounded_fee_nad,
        quote_hlp_ylp_shares as u128,
        eligible_ylp_supply as u128,
    )?;
    let end = &mut quote.executable.end;
    // Compounding is an internal, endpoint-priced ownership transition. A
    // fee already denominated in an hLP's target asset grows that equity
    // directly. The opposite-target hLP exchanges its pro-rata fee against
    // the ordinary tranche at the frozen trader endpoint: its fee asset stays
    // in ordinary reserves and the equal target value moves from ordinary
    // reserves into hLP equity. Thus the only external reserve addition is
    // exactly `compounded_fee_nad`; the matching hedge debt remains the sole
    // additional funded reserve movement.
    if fee_is_base {
        let quote_equity_gain = mul_div_u128(quote_hlp_fee, end.ordinary_quote, end.ordinary_base)?;
        end.ordinary_base = end
            .ordinary_base
            .checked_add(
                compounded_fee_nad
                    .checked_sub(base_hlp_fee)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
            )
            .ok_or(ErrorCode::MarketMathOverflow)?;
        end.ordinary_quote = end
            .ordinary_quote
            .checked_sub(quote_equity_gain)
            .ok_or(ErrorCode::InsufficientLiquidity)?;
        end.base_hlp_equity = end
            .base_hlp_equity
            .checked_add(base_hlp_fee)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        end.quote_hlp_equity = end
            .quote_hlp_equity
            .checked_add(quote_equity_gain)
            .ok_or(ErrorCode::MarketMathOverflow)?;
    } else {
        let base_equity_gain = mul_div_u128(base_hlp_fee, end.ordinary_base, end.ordinary_quote)?;
        end.ordinary_quote = end
            .ordinary_quote
            .checked_add(
                compounded_fee_nad
                    .checked_sub(quote_hlp_fee)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
            )
            .ok_or(ErrorCode::MarketMathOverflow)?;
        end.ordinary_base = end
            .ordinary_base
            .checked_sub(base_equity_gain)
            .ok_or(ErrorCode::InsufficientLiquidity)?;
        end.quote_hlp_equity = end
            .quote_hlp_equity
            .checked_add(quote_hlp_fee)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        end.base_hlp_equity = end
            .base_hlp_equity
            .checked_add(base_equity_gain)
            .ok_or(ErrorCode::MarketMathOverflow)?;
    }
    let hlp = reconstruct_hlp_endpoint(*end)?;
    end.base_hlp_quote_debt = hlp.base_hlp_quote_debt;
    end.quote_hlp_base_debt = hlp.quote_hlp_base_debt;
    quote.executable.hlp = hlp;
    quote.executable.base_hlp_quote_debt_delta = signed_delta(hlp.base_hlp_quote_debt, start.base_hlp_quote_debt)?;
    quote.executable.quote_hlp_base_debt_delta = signed_delta(hlp.quote_hlp_base_debt, start.quote_hlp_base_debt)?;
    Ok(())
}

pub(crate) fn materialized_hlp_endpoint(state: IntegratedCurveState) -> Result<IntegratedHlpEndpoint> {
    let total_base = state
        .ordinary_base
        .checked_add(state.base_hlp_equity)
        .and_then(|value| value.checked_add(state.quote_hlp_base_debt))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let total_quote = state
        .ordinary_quote
        .checked_add(state.quote_hlp_equity)
        .and_then(|value| value.checked_add(state.base_hlp_quote_debt))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    Ok(IntegratedHlpEndpoint {
        total_base,
        total_quote,
        base_hlp_quote_debt: state.base_hlp_quote_debt,
        quote_hlp_base_debt: state.quote_hlp_base_debt,
    })
}

/// One exact-input virtual-reserve CPMM quote followed by an exact algebraic
/// hLP reconstruction. The caller may invoke this once on gross input to price
/// toxicity and once on final net input; neither call feeds a newly retained
/// fee back into its own curve state.
pub(crate) fn quote_integrated_exact_in(
    state: IntegratedCurveState,
    geometry: ExplicitCurveGeometry,
    amount_in: u128,
    direction: IntegratedSwapDirection,
) -> Result<IntegratedExactInQuote> {
    require!(amount_in > 0, ErrorCode::AmountZero);
    require!(
        state.ordinary_base > 0 && state.ordinary_quote > 0,
        ErrorCode::InsufficientLiquidity
    );

    let curve_direction = match direction {
        IntegratedSwapDirection::BaseToQuote => ExplicitCurveDirection::BaseToQuote,
        IntegratedSwapDirection::QuoteToBase => ExplicitCurveDirection::QuoteToBase,
    };
    let curve = geometry.quote_exact_in_prevalidated(
        ExplicitCurvePoint {
            base_reserve: state.ordinary_base,
            quote_reserve: state.ordinary_quote,
        },
        amount_in,
        curve_direction,
    )?;

    let mut end = state;
    end.ordinary_base = curve.end.base_reserve;
    end.ordinary_quote = curve.end.quote_reserve;
    let start_hlp = materialized_hlp_endpoint(state)?;
    let hlp = reconstruct_hlp_endpoint(end)?;
    end.base_hlp_quote_debt = hlp.base_hlp_quote_debt;
    end.quote_hlp_base_debt = hlp.quote_hlp_base_debt;

    Ok(IntegratedExactInQuote {
        amount_out: curve.amount_out,
        end,
        hlp,
        base_hlp_quote_debt_delta: signed_delta(hlp.base_hlp_quote_debt, start_hlp.base_hlp_quote_debt)?,
        quote_hlp_base_debt_delta: signed_delta(hlp.quote_hlp_base_debt, start_hlp.quote_hlp_base_debt)?,
        curve,
    })
}

#[cfg(test)]
pub(crate) fn quote_integrated_exact_out(
    state: IntegratedCurveState,
    geometry: ExplicitCurveGeometry,
    amount_out: u128,
    direction: IntegratedSwapDirection,
) -> Result<IntegratedExactInQuote> {
    require!(amount_out > 0, ErrorCode::AmountZero);
    require!(
        state.ordinary_base > 0 && state.ordinary_quote > 0,
        ErrorCode::InsufficientLiquidity
    );
    let curve_direction = match direction {
        IntegratedSwapDirection::BaseToQuote => ExplicitCurveDirection::BaseToQuote,
        IntegratedSwapDirection::QuoteToBase => ExplicitCurveDirection::QuoteToBase,
    };
    let curve = geometry.quote_exact_out(
        ExplicitCurvePoint {
            base_reserve: state.ordinary_base,
            quote_reserve: state.ordinary_quote,
        },
        amount_out,
        curve_direction,
    )?;
    let mut end = state;
    end.ordinary_base = curve.end.base_reserve;
    end.ordinary_quote = curve.end.quote_reserve;
    let start_hlp = materialized_hlp_endpoint(state)?;
    let hlp = reconstruct_hlp_endpoint(end)?;
    end.base_hlp_quote_debt = hlp.base_hlp_quote_debt;
    end.quote_hlp_base_debt = hlp.quote_hlp_base_debt;
    Ok(IntegratedExactInQuote {
        amount_out: curve.amount_out,
        end,
        hlp,
        base_hlp_quote_debt_delta: signed_delta(hlp.base_hlp_quote_debt, start_hlp.base_hlp_quote_debt)?,
        quote_hlp_base_debt_delta: signed_delta(hlp.quote_hlp_base_debt, start_hlp.quote_hlp_base_debt)?,
        curve,
    })
}

/// Executes once using the fee frozen from the gross input-reserve path.
/// The fee potential depends on the monotone input coordinate
/// `start_input + gross_input`, so materializing a counterfactual curve quote
/// would duplicate the expensive branch traversal without adding evidence.
/// Retained principal is deliberately not an input here: it is credited only
/// after the executable endpoint.
pub(crate) fn quote_integrated_exact_in_with_frozen_fee(
    state: IntegratedCurveState,
    geometry: ExplicitCurveGeometry,
    gross_amount_in: u128,
    frozen_total_fee: u128,
    direction: IntegratedSwapDirection,
) -> Result<IntegratedFrozenFeeQuote> {
    require!(frozen_total_fee < gross_amount_in, ErrorCode::InvalidSwapFeeBps);
    let amount_in_after_fee = gross_amount_in
        .checked_sub(frozen_total_fee)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let executable = quote_integrated_exact_in(state, geometry, amount_in_after_fee, direction)?;
    Ok(IntegratedFrozenFeeQuote {
        total_fee: frozen_total_fee,
        amount_in_after_fee,
        executable,
    })
}

#[cfg(test)]
mod tests {
    include!("../tests/math/hlp_integrated.rs");
}
