use crate::{
    errors::ErrorCode,
    math::op_gamm::BoundedOracleCurve,
    state::{Market, RuntimePricing},
};
use anchor_lang::prelude::*;

impl Market {
    /// Reconstruct the prior mark only to measure an observation change.
    /// This does not grant execution authority to the persisted report.
    pub(crate) fn remembered_oracle_mark(&self) -> Result<Option<u64>> {
        if !self.amm.initialized {
            return Ok(None);
        }
        let point = self.ordinary_curve_point_nad()?;
        if self.oracle.policy.lambda_bps == 0 || self.oracle.last_observation.price_nad == 0 {
            return u64::try_from(
                self.amm
                    .concentrated_curve_cache
                    .geometry()?
                    .spot_price_nad_prevalidated(point)?,
            )
            .map(Some)
            .map_err(|_| ErrorCode::MarketMathOverflow.into());
        }
        let runtime = RuntimePricing::bind(self.oracle.policy, self.oracle.last_observation);
        runtime
            .curve(
                self.config.amm.concentrated_curve_parameters()?,
                self.current_curve_center_price_nad()?,
                self.oracle.policy,
            )?
            .spot_price_nad(point)
            .map(Some)
    }

    pub(crate) fn checkpoint_oracle_risk_depth(&mut self) -> Result<()> {
        if self.oracle.policy.lambda_bps == 0 || !self.amm.initialized {
            return Ok(());
        }
        let ordinary = self
            .base_side
            .shares
            .ylp_supply
            .checked_sub(self.base_hlp_vault.ylp_shares)
            .and_then(|s| s.checked_sub(self.quote_hlp_vault.ylp_shares))
            .ok_or(ErrorCode::SupplyUnderflow)?;
        require!(ordinary > 0, ErrorCode::SupplyUnderflow);
        let current = self.amm.concentrated_curve_cache.tail_liquidity;
        let recognized = if self.oracle.recognized_ordinary_supply == 0 {
            current
        } else {
            crate::math::mul_div_u128(
                self.oracle.recognized_tail_liquidity,
                ordinary as u128,
                self.oracle.recognized_ordinary_supply as u128,
            )?
            .min(current)
        };
        self.oracle.recognized_tail_liquidity = recognized;
        self.oracle.recognized_ordinary_supply = ordinary;
        Ok(())
    }

    pub(crate) fn recognized_oracle_depth(&self, native_total: u128) -> Result<u128> {
        if self.oracle.policy.lambda_bps == 0 || self.oracle.recognized_ordinary_supply == 0 {
            return Ok(native_total);
        }
        let tail = self.amm.concentrated_curve_cache.tail_liquidity;
        require!(tail > 0, ErrorCode::InsufficientLiquidity);
        crate::math::mul_div_u128(native_total, self.oracle.recognized_tail_liquidity.min(tail), tail)
    }

    /// Only debt-token repayment and collateral additions use this branch.
    /// It never authorizes a swap, collateral seizure or additional debt.
    pub(crate) fn oracle_price_unavailable(&self) -> bool {
        self.oracle.policy.lambda_bps > 0 && self.oracle.runtime.observation().is_err()
    }

    pub(crate) fn bind_optional_repayment_oracle(&mut self, accounts: &[AccountInfo], clock: &Clock) {
        if self.bind_oracle(accounts, clock).is_err() {
            self.oracle.runtime = RuntimePricing::default();
        }
    }

    pub(crate) fn update_for_debt_reduction(&mut self) -> Result<()> {
        if !self.oracle_price_unavailable() {
            return self.update();
        }
        self.assert_current_version()?;
        self.accrue_interest_to_slot(Clock::get()?.slot)
    }

    pub(crate) fn bind_current_oracle(&mut self, accounts: &[AccountInfo]) -> Result<()> {
        if self.oracle.policy.lambda_bps == 0 {
            self.oracle.runtime = RuntimePricing::default();
            return Ok(());
        }
        self.bind_oracle(accounts, &Clock::get()?)
    }

    /// Authenticate a source observation for this invocation. A persisted
    /// observation is replay protection and event data, never authorization
    /// to execute a price-sensitive action without reading the feed again.
    pub fn bind_oracle(&mut self, accounts: &[AccountInfo], clock: &Clock) -> Result<()> {
        let policy = self.oracle.policy;
        if policy.lambda_bps == 0 {
            self.oracle.runtime = RuntimePricing::default();
            return Ok(());
        }
        policy.validate(self.config.oracle_bounds)?;
        let observation = policy.read(accounts, clock, self.base_side.asset_mint, self.quote_side.asset_mint)?;
        let previous = self.oracle.last_observation;
        observation.assert_not_before(previous, policy.format)?;
        let revision = if observation.price_nad != previous.price_nad {
            self.curve_revision
                .checked_add(1)
                .ok_or(ErrorCode::MarketMathOverflow)?
        } else {
            self.curve_revision
        };
        let runtime = RuntimePricing::bind(policy, observation);
        let mut amm = self.amm;
        if revision != self.curve_revision && self.amm.initialized {
            let curve = runtime.curve(
                self.config.amm.concentrated_curve_parameters()?,
                self.current_curve_center_price_nad()?,
                policy,
            )?;
            let point = self.ordinary_curve_point_nad()?;
            let before = if previous.price_nad > 0 {
                curve.historical_mark_nad(point, previous.price_nad)?
            } else {
                u64::try_from(
                    self.amm
                        .concentrated_curve_cache
                        .geometry()?
                        .spot_price_nad_prevalidated(point)?,
                )
                .map_err(|_| ErrorCode::MarketMathOverflow)?
            };
            let after = curve.spot_price_nad(point)?;
            // Attribute only the source move. Inventory, shares and debt do
            // not move; the same shape/coordinate serves both marks.
            amm.checkpoint_trade(&self.config.amm, before, after, clock.slot)?;
        }
        self.checkpoint_oracle_risk_depth()?;
        self.oracle.runtime = runtime;
        self.oracle.last_observation = observation;
        self.amm = amm;
        if revision != self.curve_revision {
            self.amm.invalidate_deferred_controller_target();
            self.curve_revision = revision;
        }
        Ok(())
    }

    #[inline(always)]
    pub(crate) fn bounded_curve(&self) -> Result<Option<std::rc::Rc<BoundedOracleCurve>>> {
        if self.oracle.policy.lambda_bps == 0 {
            return Ok(None);
        }
        self.oracle
            .runtime
            .curve(
                self.config.amm.concentrated_curve_parameters()?,
                self.current_curve_center_price_nad()?,
                self.oracle.policy,
            )
            .map(Some)
    }
}
