//! Oracle-adjusted execution over Dusk's existing ownership ledger.
//!
//! Quotes certify the rounded shares/debts that settlement will expose. The
//! raw cash endpoint alone is insufficient when funding interest is carried.

use super::*;
use crate::math::op_gamm::BoundedOracleCurve;
use crate::transitions::liquidity::{
    materialized_hlp_endpoint, prepare_concentrated_hlp_transition_from_end, reconstruct_hlp_endpoint,
    IntegratedExactInQuote,
};

impl Market {
    /// One marginal mark for swaps, credit and hLP valuation. Zero mode
    /// executes the native calculation without consulting a feed.
    #[inline(always)]
    pub(crate) fn execution_spot_at(
        &self,
        geometry: &ConcentratedCurveGeometry,
        point: ConcentratedCurvePoint,
    ) -> Result<u128> {
        match self.bounded_curve()? {
            Some(curve) => curve.spot_price_nad(point).map(u128::from),
            None => geometry.spot_price_nad_prevalidated(point),
        }
    }

    /// Carry cover is backing, not net NAV. Solve it at the materialized
    /// endpoint mark so a later quote does not charge the same interest twice.
    #[inline(never)]
    fn oracle_ownership_endpoint(
        &self,
        curve: &BoundedOracleCurve,
        state: IntegratedCurveState,
        input: u128,
        output: u128,
        asset_in: MarketAsset,
        initial_mark: u64,
    ) -> Result<(IntegratedCurveState, ConcentratedCurvePoint)> {
        let mut cash = state;
        match asset_in {
            MarketAsset::Base => {
                cash.ordinary_base = cash
                    .ordinary_base
                    .checked_add(input)
                    .ok_or(ErrorCode::ReserveOverflow)?;
                cash.ordinary_quote = cash
                    .ordinary_quote
                    .checked_sub(output)
                    .ok_or(ErrorCode::InsufficientLiquidity)?;
            }
            MarketAsset::Quote => {
                cash.ordinary_quote = cash
                    .ordinary_quote
                    .checked_add(input)
                    .ok_or(ErrorCode::ReserveOverflow)?;
                cash.ordinary_base = cash
                    .ordinary_base
                    .checked_sub(output)
                    .ok_or(ErrorCode::InsufficientLiquidity)?;
            }
        }
        self.reprice_oracle_endpoint(curve, cash, initial_mark)
    }

    #[inline(never)]
    pub(super) fn reprice_oracle_endpoint(
        &self,
        curve: &BoundedOracleCurve,
        cash: IntegratedCurveState,
        initial_mark: u64,
    ) -> Result<(IntegratedCurveState, ConcentratedCurvePoint)> {
        let mut mark = initial_mark;
        for _ in 0..32 {
            let mut end = cash.with_op_carry_cover(mark)?;
            let endpoint = reconstruct_hlp_endpoint(end)?;
            end.base_hlp_quote_debt = endpoint.base_hlp_quote_debt;
            end.quote_hlp_base_debt = endpoint.quote_hlp_base_debt;
            let plan = prepare_concentrated_hlp_transition_from_end(self, end, false, false, 0, 0)?;
            let point = plan.final_ordinary_point_nad(self)?;
            if cash.base_hlp_quote_carry == 0 && cash.quote_hlp_base_carry == 0 {
                return Ok((end, point));
            }
            let next_mark = curve.spot_price_nad(point)?;
            if cash.with_op_carry_cover(next_mark)?.op_carry_cover == end.op_carry_cover {
                return Ok((end, point));
            }
            mark = next_mark;
        }
        err!(ErrorCode::HlpSettlementUnavailable)
    }

    #[inline(never)]
    pub(super) fn quote_oracle_integrated(
        &self,
        curve: &BoundedOracleCurve,
        state: IntegratedCurveState,
        geometry: &ConcentratedCurveGeometry,
        gross_input: u128,
        frozen_fee: u128,
        asset_in: MarketAsset,
    ) -> Result<IntegratedFrozenFeeQuote> {
        require!(frozen_fee < gross_input, ErrorCode::InvalidSwapFeeBps);
        let input = gross_input - frozen_fee;
        let start = self.ordinary_curve_point_nad()?;
        let mark = curve.spot_price_nad(start)?;
        // Preparation gains belong to LPs. Neither a continuous ownership
        // reset nor its integer rounding may subsidize the next trade.
        let (baseline, rounded_baseline) = self.oracle_ownership_endpoint(curve, state, 0, 0, asset_in, mark)?;
        let continuous_baseline = baseline
            .reconstructed_curve_point()?
            .ok_or(ErrorCode::InsufficientLiquidity)?;
        let required = curve
            .invariant_delta_bounds(start, rounded_baseline)?
            .1
            .max(0)
            .max(curve.invariant_delta_bounds(start, continuous_baseline)?.1);
        let (book_base, book_quote) = state.book_ordinary()?;
        let (limit, quantum) = match asset_in {
            MarketAsset::Base => (state.ordinary_quote.min(book_quote), state.quote_atom_nad),
            MarketAsset::Quote => (state.ordinary_base.min(book_base), state.base_atom_nad),
        };
        let mut last_endpoint = None;
        let (output, pricing_end, oracle_slack_q56) = curve.solve_output(
            start,
            input,
            quantum,
            asset_in == MarketAsset::Base,
            limit,
            required,
            |output| {
                let endpoint = self.oracle_ownership_endpoint(curve, state, input, output, asset_in, mark)?;
                last_endpoint = Some((output, endpoint));
                Ok(endpoint.1)
            },
        )?;
        let (end, verified_point) = match last_endpoint {
            Some((last, endpoint)) if last == output => endpoint,
            _ => self.oracle_ownership_endpoint(curve, state, input, output, asset_in, mark)?,
        };
        require!(pricing_end == verified_point, ErrorCode::BrokenInvariant);
        let hlp = materialized_hlp_endpoint(end)?;
        let difference = |a: u128, b: u128| -> Result<i128> {
            let a = i128::try_from(a).map_err(|_| ErrorCode::MarketMathOverflow)?;
            let b = i128::try_from(b).map_err(|_| ErrorCode::MarketMathOverflow)?;
            a.checked_sub(b).ok_or_else(|| ErrorCode::MarketMathOverflow.into())
        };
        Ok(IntegratedFrozenFeeQuote {
            total_fee: frozen_fee,
            amount_in_after_fee: input,
            executable: IntegratedExactInQuote {
                oracle_slack_q56,
                amount_out: output,
                end,
                hlp,
                base_hlp_quote_debt_delta: difference(hlp.base_hlp_quote_debt, state.base_hlp_quote_debt)?,
                quote_hlp_base_debt_delta: difference(hlp.quote_hlp_base_debt, state.quote_hlp_base_debt)?,
                pricing_end,
                curve: ConcentratedCurveQuote {
                    amount_in: input,
                    amount_out: output,
                    end: ConcentratedCurvePoint {
                        base_reserve: end.ordinary_base,
                        quote_reserve: end.ordinary_quote,
                    },
                    end_branch: geometry.branch(pricing_end),
                    // Native branch crossings are diagnostic only; the OP
                    // integrator independently traverses every shape boundary.
                    boundary_crossings: 0,
                },
            },
        })
    }
}
