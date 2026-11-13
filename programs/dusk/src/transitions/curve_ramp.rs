use crate::{errors::ErrorCode, state::Market};
use anchor_lang::prelude::*;

impl Market {
    /// Swaps lazily attempt the announced ramp. An unfunded or unhealthy
    /// candidate remains unapplied; the existing market stays usable.
    /// Withdrawals and repayments never depend on successfully advancing it.
    pub(crate) fn advance_concentration_ramp(&mut self, slot: u64) -> Result<bool> {
        let ramp = self.amm.concentration_ramp;
        if !ramp.active || !self.amm.initialized || slot <= ramp.last_attempt_slot {
            return Ok(false);
        }
        let config = self.config;
        let amm = self.amm.clone();
        let oracle = self.oracle.clone();
        let base_side = self.base_side.clone();
        let quote_side = self.quote_side.clone();
        let risk = self.risk;
        let curve_revision = self.curve_revision;
        let risk_revision = self.risk_revision;
        let observation = self.last_marginal_observation_nad;
        let last_update_slot = self.last_update_slot;
        let attempt: Result<()> = (|| {
            self.assert_parameter_execution_utilization()?;
            let funded_before = self.funded_reserve_value()?;
            self.config
                .amm
                .set_concentrated_curve_parameters(ramp.candidate(slot)?)?;
            self.config.validate()?;
            if self.config.amm.is_cpmm() {
                self.release_protected_recenter_reserves()?;
            }
            self.apply_concentrated_curve_parameter_update(slot)?;
            require!(
                self.funded_reserve_value()? >= funded_before,
                ErrorCode::OperatorPolicyExceeded
            );
            self.assert_market_health()?;
            Ok(())
        })();
        if attempt.is_err() {
            self.config = config;
            self.amm = amm;
            self.oracle = oracle;
            self.base_side = base_side;
            self.quote_side = quote_side;
            self.risk = risk;
            self.curve_revision = curve_revision;
            self.risk_revision = risk_revision;
            self.last_marginal_observation_nad = observation;
            self.last_update_slot = last_update_slot;
            // A later quote in this slot sees the same scheduled candidate.
            // Do not repeat its full guard work inside a composed operation.
            self.amm.concentration_ramp.last_attempt_slot = slot;
            return Ok(false);
        }
        self.amm.concentration_ramp.last_attempt_slot = slot;
        self.amm.concentration_ramp.active = slot < ramp.end_slot;
        Ok(true)
    }
}
