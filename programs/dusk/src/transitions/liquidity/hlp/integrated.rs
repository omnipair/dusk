//! Integrated curve and opposite-asset-neutral hLP settlement.
//!
//! Cash flow and full-value ownership have separate coordinates when public
//! interest is unpaid. With no such hLP receivable, quoting stays analytic.
//! Otherwise, a bounded, certified search prices the actual reconstructed
//! endpoint so updating ownership cannot reset the curve between two trades.

use anchor_lang::prelude::*;

use crate::errors::ErrorCode;

use crate::{
    math::mul_div_u128,
    transitions::amm::{
        ConcentratedCurveDirection, ConcentratedCurveGeometry, ConcentratedCurvePoint, ConcentratedCurveQuote,
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IntegratedSwapDirection {
    BaseToQuote,
    QuoteToBase,
}

/// Ordinary-yLP cash curve plus fixed hLP target-asset equities.
///
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct IntegratedCurveState {
    pub ordinary_base: u128,
    pub ordinary_quote: u128,
    pub base_hlp_equity: u128,
    pub quote_hlp_equity: u128,
    pub base_hlp_quote_debt: u128,
    pub quote_hlp_base_debt: u128,
    /// Indexed hLP funding interest kept as debt when this transition cannot
    /// safely pay it from reserve cash. It is not executable AMM principal.
    pub base_hlp_quote_carry: u64,
    pub quote_hlp_base_carry: u64,
    // Book ownership includes accrued public interest; the trading coordinates
    // above do not. Offsets are frozen for one quote, so trader cash moves both
    // ledgers equally without inserting an unpaid receivable into the curve.
    pub ordinary_base_book_offset: i128,
    pub ordinary_quote_book_offset: i128,
    pub base_unpaid_interest: u128,
    pub quote_unpaid_interest: u128,
    pub base_atom_nad: u128,
    pub quote_atom_nad: u128,
    /// Frozen vaults retain their actual LP shares and indexed debt. Their
    /// negative NAV is not a tradable reserve or a hedge-rebalance input.
    pub frozen_hlp_mask: u8,
    pub frozen_base_curve_offset_nad: i128,
    pub frozen_quote_curve_offset_nad: i128,
}

impl IntegratedCurveState {
    pub(crate) fn uses_book_hedge(self) -> bool {
        self.frozen_hlp_mask != 0
            || ((self.base_hlp_equity > 0 || self.quote_hlp_equity > 0)
                && (self.ordinary_base_book_offset != 0 || self.ordinary_quote_book_offset != 0))
    }

    // Reconstruct the curve coordinates that the completed full-value hedge
    // will actually expose. This is separate from the cash-flow coordinates:
    // changing ownership also changes the fraction of excluded receivables.
    #[inline(never)]
    pub(crate) fn reconstructed_curve_point(self) -> Result<Option<ConcentratedCurvePoint>> {
        let endpoint = reconstruct_hlp_endpoint(self)?;
        let base_live = endpoint
            .total_base
            .checked_add(self.base_unpaid_interest)
            .ok_or(ErrorCode::ReserveOverflow)?;
        let quote_live = endpoint
            .total_quote
            .checked_add(self.quote_unpaid_interest)
            .ok_or(ErrorCode::ReserveOverflow)?;
        if endpoint.total_base == 0 || endpoint.total_quote == 0 {
            return Ok(None);
        }
        let base_claim = mul_div_u128(self.base_hlp_equity, endpoint.total_base, base_live)?;
        let base_opposite = mul_div_u128(self.base_hlp_equity, endpoint.total_quote, base_live)?;
        let quote_claim = mul_div_u128(self.quote_hlp_equity, endpoint.total_quote, quote_live)?;
        let quote_opposite = mul_div_u128(self.quote_hlp_equity, endpoint.total_base, quote_live)?;
        let signed = |v: u128| i128::try_from(v).map_err(|_| ErrorCode::MarketMathOverflow);
        let value = |claim: u128, debt: u128, numerator: u128, denominator: u128| -> Result<i128> {
            let v = signed(mul_div_u128(claim.abs_diff(debt), numerator, denominator)?)?;
            Ok(if claim < debt { -v } else { v })
        };
        let base_equity = signed(base_claim)?
            .checked_add(value(
                base_opposite,
                endpoint.base_hlp_quote_debt,
                endpoint.total_base,
                endpoint.total_quote,
            )?)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let quote_equity = signed(quote_claim)?
            .checked_add(value(
                quote_opposite,
                endpoint.quote_hlp_base_debt,
                endpoint.total_quote,
                endpoint.total_base,
            )?)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let base = signed(endpoint.total_base)?
            .checked_sub(base_equity)
            .and_then(|v| v.checked_sub(signed(endpoint.quote_hlp_base_debt).ok()?))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let quote = signed(endpoint.total_quote)?
            .checked_sub(quote_equity)
            .and_then(|v| v.checked_sub(signed(endpoint.base_hlp_quote_debt).ok()?))
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let base = base
            .checked_add(self.frozen_base_curve_offset_nad)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let quote = quote
            .checked_add(self.frozen_quote_curve_offset_nad)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        Ok((base > 0 && quote > 0).then_some(ConcentratedCurvePoint {
            base_reserve: base.max(0) as u128,
            quote_reserve: quote.max(0) as u128,
        }))
    }

    pub(crate) fn book_ordinary(self) -> Result<(u128, u128)> {
        let add_offset = |amount: u128, offset: i128| -> Result<u128> {
            if offset >= 0 {
                amount.checked_add(offset as u128)
            } else {
                amount.checked_sub(offset.unsigned_abs())
            }
            .ok_or_else(|| ErrorCode::InsufficientLiquidity.into())
        };
        Ok((
            add_offset(self.ordinary_base, self.ordinary_base_book_offset)?,
            add_offset(self.ordinary_quote, self.ordinary_quote_book_offset)?,
        ))
    }
}

#[cfg(test)]
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
            ..Self::default()
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
    pub curve: ConcentratedCurveQuote,
    pub pricing_end: ConcentratedCurvePoint,
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

pub(crate) fn reconstruct_hlp_endpoint(mut state: IntegratedCurveState) -> Result<IntegratedHlpEndpoint> {
    let (base, quote) = state.book_ordinary()?;
    require!(base > 0 && quote > 0, ErrorCode::InsufficientLiquidity);
    if state.frozen_hlp_mask & 2 == 0 {
        state.quote_hlp_base_debt = mul_div_u128(state.quote_hlp_equity, base, quote)?;
        if state.quote_hlp_base_carry > 0 {
            state.quote_hlp_base_debt = state
                .quote_hlp_base_debt
                .checked_add(
                    u128::from(state.quote_hlp_base_carry)
                        .checked_mul(state.base_atom_nad)
                        .ok_or(ErrorCode::MarketMathOverflow)?,
                )
                .ok_or(ErrorCode::MarketMathOverflow)?;
        }
    }
    if state.frozen_hlp_mask & 1 == 0 {
        state.base_hlp_quote_debt = mul_div_u128(state.base_hlp_equity, quote, base)?;
        if state.base_hlp_quote_carry > 0 {
            state.base_hlp_quote_debt = state
                .base_hlp_quote_debt
                .checked_add(
                    u128::from(state.base_hlp_quote_carry)
                        .checked_mul(state.quote_atom_nad)
                        .ok_or(ErrorCode::MarketMathOverflow)?,
                )
                .ok_or(ErrorCode::MarketMathOverflow)?;
        }
    }
    materialized_hlp_endpoint(state)
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
/// equity at the frozen concentrated curve endpoint price, then the one-sided
/// claims and debts are reconstructed algebraically.
pub(crate) fn apply_compounded_ylp_fee(
    start: IntegratedCurveState,
    quote: &mut IntegratedFrozenFeeQuote,
    fee_is_base: bool,
    compounded_fee_nad: u128,
    endpoint_price_nad: u128,
    eligible_ylp_supply: u64,
    base_hlp_ylp_shares: u64,
    quote_hlp_ylp_shares: u64,
) -> Result<()> {
    if compounded_fee_nad == 0 {
        return Ok(());
    }
    require!(eligible_ylp_supply > 0, ErrorCode::SupplyUnderflow);
    require!(endpoint_price_nad > 0, ErrorCode::InvalidSettlementPrice);
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
    // A frozen cohort still earns its proportional fee through fixed yLP
    // ownership. Keep that fee in the static tranche instead of turning it
    // into a new positive hLP equity and silently unfreezing its hedge.
    let base_hlp_fee = if start.frozen_hlp_mask & 1 != 0 {
        0
    } else {
        base_hlp_fee
    };
    let quote_hlp_fee = if start.frozen_hlp_mask & 2 != 0 {
        0
    } else {
        quote_hlp_fee
    };
    if fee_is_base {
        let quote_equity_gain = mul_div_u128(quote_hlp_fee, endpoint_price_nad, crate::constants::NAD as u128)?;
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
        let base_equity_gain = mul_div_u128(base_hlp_fee, crate::constants::NAD as u128, endpoint_price_nad)?;
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
    let (base, quote) = state.book_ordinary()?;
    let total_base = base
        .checked_add(state.base_hlp_equity)
        .and_then(|value| value.checked_add(state.quote_hlp_base_debt))
        .and_then(|value| value.checked_sub(state.base_unpaid_interest))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let total_quote = quote
        .checked_add(state.quote_hlp_equity)
        .and_then(|value| value.checked_add(state.base_hlp_quote_debt))
        .and_then(|value| value.checked_sub(state.quote_unpaid_interest))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    Ok(IntegratedHlpEndpoint {
        total_base,
        total_quote,
        base_hlp_quote_debt: state.base_hlp_quote_debt,
        quote_hlp_base_debt: state.quote_hlp_base_debt,
    })
}

/// Quotes exact trader cash flow and the matching hLP ownership endpoint.
/// Public receivables stay outside pricing principal; an active full-value
/// hedge must nevertheless use its reconstructed endpoint in the invariant.
/// Retained fees do not feed back into the trade that generated them.
#[inline(never)]
pub(crate) fn quote_integrated_exact_in(
    state: IntegratedCurveState,
    geometry: ConcentratedCurveGeometry,
    amount_in: u128,
    direction: IntegratedSwapDirection,
) -> Result<IntegratedExactInQuote> {
    require!(amount_in > 0, ErrorCode::AmountZero);
    require!(
        state.ordinary_base > 0 && state.ordinary_quote > 0,
        ErrorCode::InsufficientLiquidity
    );

    let curve_direction = match direction {
        IntegratedSwapDirection::BaseToQuote => ConcentratedCurveDirection::BaseToQuote,
        IntegratedSwapDirection::QuoteToBase => ConcentratedCurveDirection::QuoteToBase,
    };
    if state.uses_book_hedge()
        && (state.base_unpaid_interest > 0
            || state.quote_unpaid_interest > 0
            || state.base_hlp_quote_carry > 0
            || state.quote_hlp_base_carry > 0
            || state.frozen_hlp_mask != 0)
    {
        let start = state
            .reconstructed_curve_point()?
            .ok_or(ErrorCode::InsufficientLiquidity)?;
        let (book_base, book_quote) = state.book_ordinary()?;
        let output_limit = match direction {
            IntegratedSwapDirection::BaseToQuote => state.ordinary_quote.min(book_quote),
            IntegratedSwapDirection::QuoteToBase => state.ordinary_base.min(book_base),
        };
        let quantum = match direction {
            IntegratedSwapDirection::BaseToQuote => state.quote_atom_nad.max(1),
            IntegratedSwapDirection::QuoteToBase => state.base_atom_nad.max(1),
        };
        let mut low = 0_u128;
        let mut high = output_limit.checked_sub(1).ok_or(ErrorCode::InsufficientLiquidity)?;
        let mut accepted = None;
        let mut candidate = geometry
            .quote_exact_in_prevalidated(start, amount_in, curve_direction)?
            .amount_out;
        // First use the observed opposite-coordinate gap to approach the root.
        // Every candidate is independently certified and bounded. If this does
        // not converge, 64 bisections retain the original one-atom guarantee.
        for iteration in 0..72 {
            let output = if iteration < 8 && candidate > low && candidate <= high {
                candidate
            } else {
                low + (high - low).div_ceil(2)
            };
            let mut end = state;
            match direction {
                IntegratedSwapDirection::BaseToQuote => {
                    end.ordinary_base = end
                        .ordinary_base
                        .checked_add(amount_in)
                        .ok_or(ErrorCode::ReserveOverflow)?;
                    end.ordinary_quote -= output;
                }
                IntegratedSwapDirection::QuoteToBase => {
                    end.ordinary_quote = end
                        .ordinary_quote
                        .checked_add(amount_in)
                        .ok_or(ErrorCode::ReserveOverflow)?;
                    end.ordinary_base -= output;
                }
            }
            let hlp = reconstruct_hlp_endpoint(end)?;
            end.base_hlp_quote_debt = hlp.base_hlp_quote_debt;
            end.quote_hlp_base_debt = hlp.quote_hlp_base_debt;
            let point = end
                .reconstructed_curve_point()?
                .ok_or(ErrorCode::InsufficientLiquidity)?;
            let (input_start, input_end, opposite_end) = match direction {
                IntegratedSwapDirection::BaseToQuote => (start.base_reserve, point.base_reserve, point.quote_reserve),
                IntegratedSwapDirection::QuoteToBase => (start.quote_reserve, point.quote_reserve, point.base_reserve),
            };
            let reference = if input_end > input_start {
                geometry.quote_exact_in_prevalidated(start, input_end - input_start, curve_direction)?
            } else if input_end < input_start {
                geometry.quote_exact_out(
                    start,
                    input_start - input_end,
                    match direction {
                        IntegratedSwapDirection::BaseToQuote => ConcentratedCurveDirection::QuoteToBase,
                        IntegratedSwapDirection::QuoteToBase => ConcentratedCurveDirection::BaseToQuote,
                    },
                )?
            } else {
                ConcentratedCurveQuote {
                    amount_in: 0,
                    amount_out: 0,
                    end: start,
                    end_branch: geometry.branch(start),
                    boundary_crossings: 0,
                }
            };
            let required_opposite = match direction {
                IntegratedSwapDirection::BaseToQuote => reference.end.quote_reserve,
                IntegratedSwapDirection::QuoteToBase => reference.end.base_reserve,
            };
            let correction = opposite_end.abs_diff(required_opposite).max(quantum);
            if opposite_end >= required_opposite {
                candidate = output.saturating_add(correction);
                low = output;
                accepted = Some(IntegratedExactInQuote {
                    amount_out: output,
                    end,
                    hlp,
                    base_hlp_quote_debt_delta: signed_delta(hlp.base_hlp_quote_debt, state.base_hlp_quote_debt)?,
                    quote_hlp_base_debt_delta: signed_delta(hlp.quote_hlp_base_debt, state.quote_hlp_base_debt)?,
                    pricing_end: point,
                    curve: ConcentratedCurveQuote {
                        amount_in,
                        amount_out: output,
                        end: ConcentratedCurvePoint {
                            base_reserve: end.ordinary_base,
                            quote_reserve: end.ordinary_quote,
                        },
                        end_branch: geometry.branch(point),
                        boundary_crossings: reference.boundary_crossings,
                    },
                });
            } else {
                candidate = output.saturating_sub(correction);
                high = output.checked_sub(1).ok_or(ErrorCode::InsufficientLiquidity)?;
            }
            if high.saturating_sub(low) < quantum {
                break;
            }
        }
        return accepted.ok_or_else(|| ErrorCode::InsufficientLiquidity.into());
    }

    let curve_start = if state.uses_book_hedge() {
        state
            .reconstructed_curve_point()?
            .ok_or(ErrorCode::InsufficientLiquidity)?
    } else {
        ConcentratedCurvePoint {
            base_reserve: state.ordinary_base,
            quote_reserve: state.ordinary_quote,
        }
    };
    let mut curve = geometry.quote_exact_in_prevalidated(curve_start, amount_in, curve_direction)?;
    let pricing_end = curve.end;
    let mut end = state;
    match direction {
        IntegratedSwapDirection::BaseToQuote => {
            end.ordinary_base = end
                .ordinary_base
                .checked_add(amount_in)
                .ok_or(ErrorCode::ReserveOverflow)?;
            end.ordinary_quote = end
                .ordinary_quote
                .checked_sub(curve.amount_out)
                .ok_or(ErrorCode::InsufficientLiquidity)?;
        }
        IntegratedSwapDirection::QuoteToBase => {
            end.ordinary_quote = end
                .ordinary_quote
                .checked_add(amount_in)
                .ok_or(ErrorCode::ReserveOverflow)?;
            end.ordinary_base = end
                .ordinary_base
                .checked_sub(curve.amount_out)
                .ok_or(ErrorCode::InsufficientLiquidity)?;
        }
    }
    curve.end = ConcentratedCurvePoint {
        base_reserve: end.ordinary_base,
        quote_reserve: end.ordinary_quote,
    };
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
        pricing_end,
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
    geometry: ConcentratedCurveGeometry,
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
    include!("../../../tests/transitions/liquidity_hlp_integrated.rs");
}
