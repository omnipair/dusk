use anchor_lang::prelude::*;

use crate::{
    constants::{DEFAULT_JUNIOR_INCOME_SHARE_BPS, MAX_JUNIOR_INCOME_SHARE_BPS, MIN_JUNIOR_INCOME_SHARE_BPS},
    errors::ErrorCode,
};

/// Operator-configured junior income allocation. Exit timing and the request and
/// liquidation fees remain fixed by the protocol.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, InitSpace, PartialEq, Eq)]
pub struct TranchePolicy {
    pub junior_income_share_bps: u16,
}

impl Default for TranchePolicy {
    fn default() -> Self {
        Self {
            junior_income_share_bps: DEFAULT_JUNIOR_INCOME_SHARE_BPS,
        }
    }
}

impl TranchePolicy {
    pub fn validate(&self) -> Result<()> {
        require!(
            (MIN_JUNIOR_INCOME_SHARE_BPS..=MAX_JUNIOR_INCOME_SHARE_BPS).contains(&self.junior_income_share_bps),
            ErrorCode::InvalidParameterUpdate
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operator_bounds_admit_defaults_and_reject_economic_extremes() {
        assert!(TranchePolicy::default().validate().is_ok());
        for share in [999, 9_001] {
            assert!(TranchePolicy {
                junior_income_share_bps: share,
                ..Default::default()
            }
            .validate()
            .is_err());
        }
    }
}
