use anchor_lang::prelude::*;

use crate::{
    constants::{
        DEFAULT_JUNIOR_INCOME_SHARE_BPS, DEFAULT_LIQUIDATION_FEE_BPS, MAX_JUNIOR_INCOME_SHARE_BPS,
        MAX_LIQUIDATION_FEE_BPS, MIN_JUNIOR_INCOME_SHARE_BPS, MIN_LIQUIDATION_FEE_BPS,
    },
    errors::ErrorCode,
};

/// Governed tranche economics. Exit timing remains protocol-defined; only its
/// request/renewal fee is changed through the tranche parameter family.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, InitSpace, PartialEq, Eq)]
pub struct TranchePolicy {
    pub junior_income_share_bps: u16,
    pub liquidation_fee_bps: u16,
}

impl Default for TranchePolicy {
    fn default() -> Self {
        Self {
            junior_income_share_bps: DEFAULT_JUNIOR_INCOME_SHARE_BPS,
            liquidation_fee_bps: DEFAULT_LIQUIDATION_FEE_BPS,
        }
    }
}

impl TranchePolicy {
    pub fn validate(&self) -> Result<()> {
        require!(
            (MIN_JUNIOR_INCOME_SHARE_BPS..=MAX_JUNIOR_INCOME_SHARE_BPS).contains(&self.junior_income_share_bps),
            ErrorCode::InvalidParameterUpdate
        );
        require!(
            (MIN_LIQUIDATION_FEE_BPS..=MAX_LIQUIDATION_FEE_BPS).contains(&self.liquidation_fee_bps),
            ErrorCode::InvalidParameterUpdate
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn governance_bounds_admit_defaults_and_reject_economic_extremes() {
        assert!(TranchePolicy::default().validate().is_ok());
        for share in [999, 9_001] {
            assert!(TranchePolicy {
                junior_income_share_bps: share,
                ..Default::default()
            }
            .validate()
            .is_err());
        }
        for fee in [24, 301] {
            assert!(TranchePolicy {
                liquidation_fee_bps: fee,
                ..Default::default()
            }
            .validate()
            .is_err());
        }
    }
}
