use crate::{
    constants::NAD,
    errors::ErrorCode,
    math::mul_div_u128,
    state::*,
    transitions::{
        amm::{ConcentratedCurveDirection, ConcentratedCurvePoint},
        flash_liquidation::LiquidationPositionRef,
        LeverageCollateralFee,
    },
};
use anchor_lang::prelude::*;

impl Market {
    #[inline(never)]
    pub(crate) fn liquidation_scratch(&self) -> Box<Market> {
        Box::new(self.clone())
    }
    /// Keep Market's clone frame separate from the curve proof's interval state
    /// so neither SBF frame exceeds 4 KiB.
    #[inline(never)]
    fn executable_liquidation_market(&self, slot: u64) -> Result<Box<Market>> {
        let mut executable = self.liquidation_scratch();
        executable.prepare_amm_for_swap(slot)?;
        executable.advance_one_amm_controller_target(slot)?;
        Ok(executable)
    }

    /// A conservative proof over every integer collateral slice, not a failed
    /// keeper-selected quote. Each interval gets more repayment, more remaining
    /// collateral value, and a lower maintenance rate than any real fill in it.
    /// If even that optimistic state cannot improve health, discard the whole
    /// interval. Otherwise bisect. A singleton that might improve, or exhaustion
    /// of the computation budget, refuses full-close permission.
    ///
    /// The repayment bound includes both the fixed-price flash purchase and a
    /// fee-free AMM sale. Fees, caller rewards, cash limits and dust constraints
    /// can only remove real partials; ignoring them cannot authorize a full sale.
    pub(crate) fn proves_no_useful_partial(
        &self,
        position: &LiquidationPositionRef<'_>,
        asset: MarketAsset,
        collateral_fee: LeverageCollateralFee,
        eligibility_fee: LeverageCollateralFee,
        debt_fee: LeverageCollateralFee,
        now: i64,
        slot: u64,
    ) -> Result<bool> {
        self.proves_partial_bound(
            position,
            asset,
            collateral_fee,
            eligibility_fee,
            debt_fee,
            now,
            slot,
            None,
        )
    }

    /// Conservative recovery cap for emergency sales. A candidate crossing
    /// MM + buffer is refused if any smaller slice could have already done so.
    /// Keepers can always request a useful below-target partial instead.
    pub(crate) fn proves_partial_bound(
        &self,
        position: &LiquidationPositionRef<'_>,
        asset: MarketAsset,
        collateral_fee: LeverageCollateralFee,
        eligibility_fee: LeverageCollateralFee,
        debt_fee: LeverageCollateralFee,
        now: i64,
        slot: u64,
        recovery_before: Option<u64>,
    ) -> Result<bool> {
        let before = self.flash_liquidation_health(position, asset, eligibility_fee)?;
        require!(before.emergency_allowed()?, ErrorCode::PositionNotLiquidatable);
        let collateral = position.collateral(asset);
        let shares = position.shares(asset);
        if collateral <= 1 || shares <= 1 {
            return Ok(true);
        }
        let rates = before.rates(position.age(asset, now)?)?;
        // Risk is fixed for this proof. Recomputing current_risk for every
        // interval needlessly re-evaluates the concentrated curve and EMA.
        let risk = self.current_risk()?;
        let collateral_scale = self.normalize_amount(1, self.side(asset.opposite()).asset_decimals)?;
        let debt_scale = self.normalize_amount(1, self.side(asset).asset_decimals)?;
        let price = u128::from(self.pessimistic_collateral_price_nad(asset.opposite(), &risk, false));
        // Cancelling common decimal factors preserves the nested floor exactly.
        let (numerator, denominator) = if collateral_scale >= debt_scale {
            (
                price
                    .checked_mul(collateral_scale / debt_scale)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
                u128::from(NAD),
            )
        } else {
            (
                price,
                u128::from(NAD)
                    .checked_mul(debt_scale / collateral_scale)
                    .ok_or(ErrorCode::MarketMathOverflow)?,
            )
        };
        let value = |credit| -> Result<u64> {
            u64::try_from(mul_div_u128(u128::from(credit), numerator, denominator)?)
                .map_err(|_| ErrorCode::MarketMathOverflow.into())
        };
        let smallest_maintenance = position.maintenance(asset, 1)?;
        let flat_maintenance = (smallest_maintenance == before.maintenance_bps).then_some(smallest_maintenance);
        // A next Dusk swap may execute a previously scheduled recenter. Prove
        // against that executable curve, including its ordinary yLP tranche.
        let executable = self.executable_liquidation_market(slot)?;
        let geometry = executable
            .current_concentrated_curve_geometry()?
            .ok_or(ErrorCode::BrokenInvariant)?;
        let state = executable.integrated_curve_state_nad()?;
        let point = ConcentratedCurvePoint {
            base_reserve: state.ordinary_base,
            quote_reserve: state.ordinary_quote,
        };
        let direction = if asset == MarketAsset::Quote {
            ConcentratedCurveDirection::BaseToQuote
        } else {
            ConcentratedCurveDirection::QuoteToBase
        };
        let index = self.debt.borrow_index(asset);
        let aggregate = match (position, asset) {
            (LiquidationPositionRef::Borrow(_), MarketAsset::Base) => self.debt.fixed_base_shares,
            (LiquidationPositionRef::Borrow(_), MarketAsset::Quote) => self.debt.fixed_quote_shares,
            (LiquidationPositionRef::Leverage(_), MarketAsset::Base) => self.debt.isolated_base_shares,
            (LiquidationPositionRef::Leverage(_), MarketAsset::Quote) => self.debt.isolated_quote_shares,
        };
        // Depth-first bisection needs at most 64 outstanding intervals for u64
        // collateral. The step limit is a proof limit, never a loss permission.
        let mut pending = Vec::with_capacity(64);
        pending.push((1_u64, recovery_before.unwrap_or(collateral).saturating_sub(1), None));
        for _ in 0..4096 {
            let Some((lo, hi, cached_budget)) = pending.pop() else {
                return Ok(true);
            };
            let budget = if let Some(budget) = cached_budget {
                budget
            } else {
                let credit = collateral_fee.unwind_credit(hi)?;
                let flash_bound = u64::try_from(mul_div_u128(
                    u128::from(value(credit)?),
                    u128::from(10_000 - rates.buyer_discount_bps),
                    10_000,
                )?)
                .map_err(|_| ErrorCode::MarketMathOverflow)?;
                let input =
                    executable.normalize_amount(u128::from(credit), self.side(asset.opposite()).asset_decimals)?;
                let amm_bound = if input == 0 {
                    0
                } else {
                    executable.denormalize_amount_floor(
                        geometry
                            .quote_exact_in_prevalidated(point, input, direction)?
                            .amount_out,
                        self.side(asset).asset_decimals,
                    )?
                };
                // For untaxed debt tokens, y - floor(y * reward_rate) is monotone.
                // Capped token transfer fees can break that monotonicity; retain
                // the looser pre-reward bound for those mints.
                let amm_bound = if debt_fee.haircut_bps() == 0 {
                    amm_bound
                        - u64::try_from(mul_div_u128(
                            u128::from(amm_bound),
                            u128::from(rates.emergency_reward_bps),
                            10_000,
                        )?)
                        .map_err(|_| ErrorCode::MarketMathOverflow)?
                } else {
                    amm_bound
                };
                // Every partial must fund R + floor(R / 100). This is the exact
                // largest R satisfying that inequality; transfer costs only lower it.
                ((u128::from(if recovery_before.is_some() {
                    amm_bound
                } else {
                    flash_bound.max(amm_bound)
                }) * 100
                    + 99)
                    / 101) as u64
            };
            if budget == 0 {
                continue;
            }
            let mut burn = mul_div_u128(u128::from(budget), u128::from(NAD), index)?.min(shares - 1);
            if burn < shares - 1 && Debt::aggregate_debt_reduction_for_shares(aggregate, burn + 1, index)? <= budget {
                burn += 1;
            }
            if burn == 0 {
                continue;
            }
            let optimistic = LiquidationHealth {
                collateral_value: value(eligibility_fee.unwind_credit(collateral - lo)?)?,
                debt: u64::try_from(Debt::shares_to_debt(shares - burn, index)?)
                    .map_err(|_| ErrorCode::DebtMathOverflow)?,
                maintenance_bps: match flat_maintenance {
                    Some(rate) => rate,
                    None => position.maintenance(asset, collateral - hi)?,
                },
            };
            let possible = if recovery_before.is_some() {
                optimistic.recovered()?
            } else {
                before.improves(optimistic)?
            };
            if !possible {
                continue;
            }
            if lo == hi {
                return Ok(false);
            }
            let mid = lo + (hi - lo) / 2;
            pending.push((mid + 1, hi, Some(budget)));
            pending.push((lo, mid, None));
        }
        Ok(false)
    }
}
