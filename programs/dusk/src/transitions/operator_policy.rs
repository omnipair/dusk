use crate::state::*;
use anchor_lang::prelude::*;

impl Market {
    /// The same operator policy applies to every pool. There are no LP-owned
    /// approval baselines, movement budgets, or voting-based exceptions.
    pub fn validate_operator_updates(&self, updates: &[MarketParameterUpdate]) -> Result<()> {
        if updates.is_empty() {
            return Ok(());
        }
        self.validate_parameter_updates(updates)?;
        Ok(())
    }
}
