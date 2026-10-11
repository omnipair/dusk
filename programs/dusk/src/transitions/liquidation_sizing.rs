use crate::{
    constants::NAD,
    errors::ErrorCode,
    math::mul_div_u128,
    state::*,
    transitions::{
        amm::{cpmm_amount_out_nad, ConcentratedCurveDirection, ConcentratedCurvePoint},
        flash_liquidation::LiquidationPositionRef,
        liquidation_policy::LIQUIDATION_MINIMUM_DISCOUNT_BPS,
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
    /// If that optimistic state fails the requested improvement/recovery test,
    /// discard the whole interval. Otherwise bisect. Remaining AMM singleton candidates use the
    /// execution path, including its actual fees. Uncertainty or exhaustion of
    /// the computation budget refuses full-close permission.
    ///
    /// Ordinary flash full-close bounds include the smallest allowed discount,
    /// so an affordable purchase prevents that permission too. AMM bounds
    /// include the configured base fee; additional surcharges can only lower
    /// output. Each path uses its own share rounding. Every partial retains
    /// at least one debt share, without an economic residual-debt threshold.
    /// With a recovery bound, certify that no smaller AMM slice can reach
    /// MM + buffer. This caps a partial at its first recovery crossing; a bound
    /// equal to all collateral authorizes the emergency AMM full-sale fallback.
    /// Without a recovery bound, retain the stricter ordinary flash full-close
    /// proof over both flash and AMM health-improving partials. Uncertainty never
    /// authorizes full closure. Useful below-target partials remain permitted.
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
        // A diagonal bound avoids enumerating raw-token atoms when the cheapest
        // flash price cannot improve a flat-MM position. For value removed x:
        // R <= x * (10_000-min_discount)/10_100 + 99/101. Position debt reduction
        // is <= R; remaining value rounding loses at least x-1. Therefore no
        // improvement is possible once x*(D*10_100-V*(10_000-min_discount))
        // >= D*10_100+V*9_900. Division keeps all products within u128.
        // Eligibility transfer-fee caps and changing MM need the interval proof.
        let flash_no_improvement_from = if flat_maintenance.is_some() && eligibility_fee.haircut_bps() == 0 {
            let debt = u128::from(before.debt);
            let value = u128::from(before.collateral_value);
            let spread = (debt * 10_100).saturating_sub(value * u128::from(10_000 - LIQUIDATION_MINIMUM_DISCOUNT_BPS));
            (spread > 0).then(|| (debt * 10_100 + value * 9_900).div_ceil(spread))
        } else {
            None
        };
        // A next Dusk swap may execute a previously scheduled recenter. Prove
        // against that executable curve, including its ordinary yLP tranche.
        let executable = self.executable_liquidation_market(slot)?;
        let geometry = executable
            .current_concentrated_curve_geometry()?
            .ok_or(ErrorCode::BrokenInvariant)?;
        let state = executable.integrated_curve_state_nad()?;
        let cpmm = geometry.is_cpmm();
        let point = ConcentratedCurvePoint {
            base_reserve: state.ordinary_base,
            quote_reserve: state.ordinary_quote,
        };
        require!(
            point.base_reserve > 0 && point.quote_reserve > 0,
            ErrorCode::InsufficientLiquidity
        );
        let direction = if asset == MarketAsset::Quote {
            ConcentratedCurveDirection::BaseToQuote
        } else {
            ConcentratedCurveDirection::QuoteToBase
        };
        let index = self.debt.borrow_index(asset);
        // A partial must retain debt; no configurable minimum excludes an
        // otherwise useful slice or manufactures full-close permission.
        let maximum_burn = shares - 1;
        let fee_on_input = executable.config.swap_fee_asset(asset.opposite())? == asset.opposite();
        let base_fee =
            |amount: u64| -> u64 { (u128::from(amount) * u128::from(executable.config.swap_fee_bps) / 10_000) as u64 };
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
                    u128::from(10_000 - LIQUIDATION_MINIMUM_DISCOUNT_BPS),
                    10_000,
                )?)
                .map_err(|_| ErrorCode::MarketMathOverflow)?;
                // x - floor(x * base_fee) is monotone. Launch and dynamic
                // fees never lower this configured base fee.
                let curve_credit = if fee_on_input {
                    credit - base_fee(credit)
                } else {
                    credit
                };
                // Mint decimal scales are immutable and were validated above.
                // Reuse them instead of calculating powers of ten per interval.
                let input = u128::from(curve_credit)
                    .checked_mul(collateral_scale)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                let amm_bound = if input == 0 {
                    0
                } else {
                    // Exact CPMM has no segment crossings. Reuse its canonical
                    // integer quote directly instead of copying the full
                    // concentrated geometry through every proof interval.
                    let output = if cpmm {
                        match direction {
                            ConcentratedCurveDirection::BaseToQuote => {
                                cpmm_amount_out_nad(point.base_reserve, point.quote_reserve, input)?
                            }
                            ConcentratedCurveDirection::QuoteToBase => {
                                cpmm_amount_out_nad(point.quote_reserve, point.base_reserve, input)?
                            }
                        }
                    } else {
                        geometry
                            .quote_exact_in_prevalidated(point, input, direction)?
                            .amount_out
                    };
                    require!(output > 0, ErrorCode::InsufficientOutputAmount);
                    u64::try_from(output / debt_scale).map_err(|_| ErrorCode::MarketMathOverflow)?
                };
                let amm_bound = if fee_on_input {
                    amm_bound
                } else {
                    amm_bound - base_fee(amm_bound)
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
                let repay_bound = |budget: u64| ((u128::from(budget) * 100 + 99) / 101) as u64;
                (repay_bound(flash_bound), repay_bound(amm_bound))
            };
            let (flash_budget, amm_budget) = budget;
            // Flash cash rounds UP per position; the AMM offsets the aggregate
            // floor reduction and may burn one extra share. Do not mix them.
            let flash_impossible = match flash_no_improvement_from {
                Some(threshold) => u128::from(value(lo)?) >= threshold,
                None => false,
            };
            let flash_burn = if flash_impossible {
                0
            } else {
                mul_div_u128(u128::from(flash_budget), u128::from(NAD), index)?.min(maximum_burn)
            };
            let mut amm_burn = mul_div_u128(u128::from(amm_budget), u128::from(NAD), index)?.min(maximum_burn);
            if amm_burn < maximum_burn
                && Debt::aggregate_debt_reduction_for_shares(aggregate, amm_burn + 1, index)? <= amm_budget
            {
                amm_burn += 1;
            }
            let burn = if recovery_before.is_some() {
                amm_burn
            } else {
                flash_burn.max(amm_burn)
            };
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
                // The flash bound can represent a smaller collateral debit
                // after rounding. Keep its permission conservative.
                if recovery_before.is_none() && flash_burn > 0 {
                    let flash_health = LiquidationHealth {
                        debt: u64::try_from(Debt::shares_to_debt(shares - flash_burn, index)?)
                            .map_err(|_| ErrorCode::DebtMathOverflow)?,
                        ..optimistic
                    };
                    if before.improves(flash_health)? {
                        return Ok(false);
                    }
                }
                // Evaluate the one remaining input with actual AMM fee math.
                // Do not materialize PreparedSwap/account clones in this loop:
                // Solana's bump allocator cannot reclaim each dropped scratch.
                let exact = (|| -> Result<LiquidationHealth> {
                    let credit = collateral_fee.unwind_credit(lo)?;
                    let preliminary = executable.preliminary_swap_inputs_for_state_at_time(
                        asset.opposite(),
                        credit,
                        slot,
                        now,
                        executable.dynamic_fee_pre_state(slot)?,
                    )?;
                    let (output, _) = executable.evaluate_concentrated_swap(
                        asset.opposite(),
                        credit,
                        preliminary,
                        state,
                        0,
                        false,
                    )?;
                    let reward = u64::try_from(mul_div_u128(
                        u128::from(debt_fee.unwind_credit(output)?),
                        u128::from(rates.emergency_reward_bps),
                        10_000,
                    )?)
                    .map_err(|_| ErrorCode::MarketMathOverflow)?;
                    let budget = output
                        .checked_sub(debt_fee.effective_gross_for_credit(reward)?)
                        .ok_or(ErrorCode::InsufficientAmount)?;
                    let (allocation, _) =
                        executable.quote_emergency_partial_repayment(position, asset, budget, debt_fee)?;
                    Ok(LiquidationHealth {
                        collateral_value: executable
                            .liquidation_value(asset, eligibility_fee.unwind_credit(collateral - lo)?)?,
                        debt: allocation.remaining_debt,
                        maintenance_bps: position.maintenance(asset, collateral - lo)?,
                    })
                })();
                match exact {
                    Ok(health) => {
                        if before.improves(health)? && (recovery_before.is_none() || health.recovered()?) {
                            return Ok(false);
                        }
                    }
                    Err(anchor_lang::error::Error::AnchorError(error))
                        if [
                            ErrorCode::InsufficientAmount,
                            ErrorCode::InsufficientOutputAmount,
                            ErrorCode::AmountZero,
                        ]
                        .iter()
                        .any(|code| error.error_code_number == u32::from(*code)) => {}
                    // A priceable healthy candidate still refuses permission
                    // even if an unmodeled custody/settlement check might fail.
                    // Unrelated errors likewise cannot authorize a full sale.
                    Err(_) => return Ok(false),
                }
                continue;
            }
            let mid = lo + (hi - lo) / 2;
            pending.push((mid + 1, hi, Some(budget)));
            pending.push((lo, mid, None));
        }
        Ok(false)
    }
}
