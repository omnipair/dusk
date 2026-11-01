//! Versioned replay interfaces and counterfactual intent rules.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::canonical::{CanonicalEventV1, ChainOrder, SignedTokenAmount, TokenAmount};

pub const REPLAY_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalOperationV1 {
    pub schema_version: u16,
    pub operation_id: String,
    pub signature: String,
    pub pair: String,
    pub actor: Option<String>,
    pub order: ChainOrder,
    pub block_time_unix_seconds: i64,
    pub decoder_version_id: String,
    pub payload: OperationPayloadV1,
    /// Emitted update/position events are post-transition assertions.
    pub assertions: Vec<CanonicalEventV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
pub enum OperationPayloadV1 {
    InitializeMarket {
        token0: String,
        token1: String,
        token0_decimals: u8,
        token1_decimals: u8,
        gross_amount0: TokenAmount,
        gross_amount1: TokenAmount,
        vault_credit0: TokenAmount,
        vault_credit1: TokenAmount,
        minted_lp: TokenAmount,
        min_liquidity_out: TokenAmount,
        fields: BTreeMap<String, String>,
    },
    AddLiquidity {
        gross_amount0: TokenAmount,
        gross_amount1: TokenAmount,
        vault_credit0: TokenAmount,
        vault_credit1: TokenAmount,
        minted_lp: TokenAmount,
        min_liquidity_out: TokenAmount,
    },
    RemoveLiquidity {
        requested_lp: TokenAmount,
        burned_lp: TokenAmount,
        vault_debit0: TokenAmount,
        vault_debit1: TokenAmount,
        recipient_credit0: TokenAmount,
        recipient_credit1: TokenAmount,
        min_amount0_out: TokenAmount,
        min_amount1_out: TokenAmount,
    },
    SwapExactIn {
        token0_in: bool,
        gross_amount_in: TokenAmount,
        vault_credit_in: TokenAmount,
        min_amount_out: TokenAmount,
        vault_debit_out: TokenAmount,
        recipient_credit_out: TokenAmount,
    },
    CollateralDelta {
        position: String,
        amount0: SignedTokenAmount,
        amount1: SignedTokenAmount,
        requested_amount: TokenAmount,
        gross_transfer_amount: TokenAmount,
        vault_amount: TokenAmount,
        counterparty_amount: TokenAmount,
    },
    Borrow {
        position: String,
        debt_token0: bool,
        requested_amount: TokenAmount,
        gross_amount: TokenAmount,
        recipient_credit: TokenAmount,
    },
    Repay {
        position: String,
        debt_token0: bool,
        requested_amount: TokenAmount,
        gross_amount: TokenAmount,
        vault_credit: TokenAmount,
    },
    Liquidate {
        position: String,
        debt_token0: bool,
        requested_repay: TokenAmount,
        collateral_vault_debit: TokenAmount,
        reserve_vault_credit: TokenAmount,
        liquidator_recipient_credit: TokenAmount,
    },
    Flashloan {
        amount0: TokenAmount,
        amount1: TokenAmount,
        fee0: TokenAmount,
        fee1: TokenAmount,
        net_vault_credit0: TokenAmount,
        net_vault_credit1: TokenAmount,
    },
    ClaimProtocolRevenue {
        amount0: TokenAmount,
        amount1: TokenAmount,
    },
    UpdateConfig {
        fields: BTreeMap<String, String>,
    },
    /// An instruction that advances native clocks/interest but has no second
    /// independent transition beyond its asserted update event.
    ClockOnly,
}

impl CanonicalOperationV1 {
    pub fn validate(&self) -> Result<(), ReplayError> {
        if self.schema_version != REPLAY_SCHEMA_VERSION {
            return Err(ReplayError::SchemaVersion(self.schema_version));
        }
        if self.operation_id.is_empty() || self.signature.is_empty() || self.pair.is_empty() {
            return Err(ReplayError::MissingIdentity);
        }
        if self.assertions.iter().any(|event| {
            event.signature != self.signature || event.pair.as_deref() != Some(&self.pair)
        }) {
            return Err(ReplayError::AssertionScope);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SideCheckpointV1 {
    pub virtual_reserve: TokenAmount,
    pub cash_reserve: TokenAmount,
    pub total_debt: TokenAmount,
    pub total_debt_shares: TokenAmount,
    pub total_collateral: TokenAmount,
    pub borrow_index_nad: TokenAmount,
    pub rate_apr_nad: TokenAmount,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevenueCheckpointV1 {
    pub lp_swap_fee0: TokenAmount,
    pub lp_swap_fee1: TokenAmount,
    pub protocol_swap_fee0: TokenAmount,
    pub protocol_swap_fee1: TokenAmount,
    pub lp_interest0: TokenAmount,
    pub lp_interest1: TokenAmount,
    pub protocol_interest0: TokenAmount,
    pub protocol_interest1: TokenAmount,
    pub flashloan_fee0: TokenAmount,
    pub flashloan_fee1: TokenAmount,
    pub retained_recenter0: TokenAmount,
    pub retained_recenter1: TokenAmount,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayCheckpointV1 {
    pub schema_version: u16,
    pub operation_id: String,
    pub order: ChainOrder,
    pub pair: String,
    pub side0: SideCheckpointV1,
    pub side1: SideCheckpointV1,
    pub lp_supply: TokenAmount,
    pub open_borrow_positions: u64,
    pub open_leverage_positions: u64,
    pub revenue: RevenueCheckpointV1,
    pub state_sha256: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayArm {
    FixedFlow,
    Intent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HlpScenario {
    NoHlp,
    MaximumFeasible,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintReason {
    Cash,
    DailyBorrowBucket,
    GlobalHealth,
    PositionHealth,
    MinimumLiquidity,
    Funding,
    Settlement,
    MinOut,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FillOutcomeV1 {
    pub requested: TokenAmount,
    pub filled: TokenAmount,
    pub unmet: TokenAmount,
    pub limiting_constraints: Vec<ConstraintReason>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ReplayError {
    #[error("unsupported replay schema version {0}")]
    SchemaVersion(u16),
    #[error("operation identity fields must be non-empty")]
    MissingIdentity,
    #[error("assertion signature or market does not match its operation")]
    AssertionScope,
    #[error("canonical operations are not in strict chain order")]
    NonCanonicalOrder,
    #[error("integer amount exceeds the u128 replay domain")]
    AmountOverflow,
    #[error("arithmetic overflow")]
    ArithmeticOverflow,
    #[error("V1 admissible debt is zero while actual debt is non-zero")]
    InvalidCapacityFraction,
    #[error("price-band solver was called with an invalid range")]
    InvalidSolverRange,
}

pub trait ReplayEngineV1 {
    fn engine_id(&self) -> &'static str;
    fn apply(&mut self, operation: &CanonicalOperationV1) -> Result<FillOutcomeV1, ReplayError>;
    fn checkpoint(
        &self,
        operation: &CanonicalOperationV1,
    ) -> Result<ReplayCheckpointV1, ReplayError>;
}

pub fn assert_strict_canonical_order(
    operations: &[CanonicalOperationV1],
) -> Result<(), ReplayError> {
    if operations
        .windows(2)
        .any(|window| window[0].order >= window[1].order)
    {
        return Err(ReplayError::NonCanonicalOrder);
    }
    Ok(())
}

fn parse_amount(value: &TokenAmount) -> Result<u128, ReplayError> {
    value
        .as_str()
        .parse::<u128>()
        .map_err(|_| ReplayError::AmountOverflow)
}

fn token(value: u128) -> TokenAmount {
    TokenAmount::new(value.to_string()).expect("u128 formatting is canonical")
}

/// Dusk target debt for an observed V1 debt action. Flooring is intentional:
/// it is the greatest raw-unit debt that cannot exceed the observed fraction
/// of admissible capacity.
pub fn capacity_preserving_target_debt(
    v1_actual_debt: &TokenAmount,
    v1_admissible_max: &TokenAmount,
    dusk_admissible_max: &TokenAmount,
) -> Result<TokenAmount, ReplayError> {
    let actual = parse_amount(v1_actual_debt)?;
    let v1_max = parse_amount(v1_admissible_max)?;
    let dusk_max = parse_amount(dusk_admissible_max)?;
    if v1_max == 0 {
        return if actual == 0 {
            Ok(TokenAmount::zero())
        } else {
            Err(ReplayError::InvalidCapacityFraction)
        };
    }
    let target = dusk_max
        .checked_mul(actual)
        .ok_or(ReplayError::ArithmeticOverflow)?
        / v1_max;
    Ok(token(target.min(dusk_max)))
}

/// Repay enough Dusk debt to preserve the observed V1 fractional debt
/// reduction. The remaining debt is floored, so rounding never understates the
/// repayment fraction.
pub fn fractional_repayment_amount(
    dusk_debt_before: &TokenAmount,
    v1_debt_before: &TokenAmount,
    v1_debt_after: &TokenAmount,
) -> Result<TokenAmount, ReplayError> {
    let dusk_before = parse_amount(dusk_debt_before)?;
    let v1_before = parse_amount(v1_debt_before)?;
    let v1_after = parse_amount(v1_debt_after)?;
    if v1_before == 0 || v1_after >= v1_before {
        return Ok(TokenAmount::zero());
    }
    let remaining = dusk_before
        .checked_mul(v1_after)
        .ok_or(ReplayError::ArithmeticOverflow)?
        / v1_before;
    Ok(token(dusk_before - remaining))
}

/// Monotone minimum-input search used for block-boundary synthetic arbitrage.
/// `in_band(amount)` must remain false and then true as input grows. Returning
/// `None` records that even the allowed cash/inventory bound cannot restore the
/// historical price band.
pub fn minimum_arbitrage_input<F>(
    maximum_input: u64,
    mut in_band: F,
) -> Result<Option<u64>, ReplayError>
where
    F: FnMut(u64) -> Result<bool, ReplayError>,
{
    if in_band(0)? {
        return Ok(Some(0));
    }
    if maximum_input == 0 || !in_band(maximum_input)? {
        return Ok(None);
    }
    let mut low = 0_u64;
    let mut high = maximum_input;
    while low + 1 < high {
        let middle = low + (high - low) / 2;
        if in_band(middle)? {
            high = middle;
        } else {
            low = middle;
        }
    }
    Ok(Some(high))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn amount(value: u128) -> TokenAmount {
        token(value)
    }

    #[test]
    fn capacity_use_is_preserved_without_exceeding_limit() {
        assert_eq!(
            capacity_preserving_target_debt(&amount(75), &amount(100), &amount(140)).unwrap(),
            amount(105)
        );
        assert_eq!(
            capacity_preserving_target_debt(&amount(2), &amount(3), &amount(10)).unwrap(),
            amount(6)
        );
    }

    #[test]
    fn fractional_repayment_rounds_toward_more_repayment() {
        assert_eq!(
            fractional_repayment_amount(&amount(101), &amount(100), &amount(75)).unwrap(),
            amount(26)
        );
    }

    #[test]
    fn binary_search_finds_minimum_arbitrage() {
        let answer = minimum_arbitrage_input(100, |input| Ok(input >= 37)).unwrap();
        assert_eq!(answer, Some(37));
        let impossible = minimum_arbitrage_input(36, |input| Ok(input >= 37)).unwrap();
        assert_eq!(impossible, None);
    }
}
