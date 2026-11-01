//! Exact historical Token / Token-2022 transfer-fee schedules.
//!
//! Counterfactual amounts can differ from their factual V1 transfers, so an
//! observed historical `(gross, credit)` pair is not a valid fee oracle for a
//! different amount. The evaluator must use a versioned mint schedule at the
//! operation's exact epoch and validates factual observations against it.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::canonical::TokenAmount;

pub const TRANSFER_FEE_SCHEMA_VERSION: u16 = 1;
const BASIS_POINTS_DENOMINATOR: u128 = 10_000;
const MINIMUM_SLOTS_PER_EPOCH: u64 = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenProgramKindV1 {
    LegacyToken,
    Token2022,
}

/// Mint-level Token-2022 TLV extensions understood by the pinned SPL Token
/// v6 history decoder. Account-only extensions never appear in this set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Token2022MintExtensionV1 {
    TransferFeeConfig,
    MintCloseAuthority,
    ConfidentialTransferMint,
    DefaultAccountState,
    NonTransferable,
    InterestBearingConfig,
    PermanentDelegate,
    TransferHook,
    ConfidentialTransferFeeConfig,
    MetadataPointer,
    TokenMetadata,
    GroupPointer,
    GroupMemberPointer,
    ConfidentialMintBurn,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpochScheduleV1 {
    pub slots_per_epoch: u64,
    pub leader_schedule_slot_offset: u64,
    pub warmup: bool,
    pub first_normal_epoch: u64,
    pub first_normal_slot: u64,
    pub source_response_sha256: String,
}

impl EpochScheduleV1 {
    pub fn epoch(&self, slot: u64) -> Result<u64, TransferFeeError> {
        self.validate()?;
        if slot < self.first_normal_slot {
            let epoch = slot
                .saturating_add(MINIMUM_SLOTS_PER_EPOCH)
                .saturating_add(1)
                .next_power_of_two()
                .trailing_zeros()
                .saturating_sub(MINIMUM_SLOTS_PER_EPOCH.trailing_zeros())
                .saturating_sub(1);
            Ok(u64::from(epoch))
        } else {
            Ok(self
                .first_normal_epoch
                .saturating_add(slot.saturating_sub(self.first_normal_slot) / self.slots_per_epoch))
        }
    }

    pub fn validate(&self) -> Result<(), TransferFeeError> {
        if self.slots_per_epoch < MINIMUM_SLOTS_PER_EPOCH
            || !valid_sha256(&self.source_response_sha256)
        {
            return Err(TransferFeeError::InvalidEpochSchedule);
        }
        if self.warmup {
            let next_power_of_two = self
                .slots_per_epoch
                .checked_next_power_of_two()
                .ok_or(TransferFeeError::InvalidEpochSchedule)?;
            let expected_epoch = u64::from(
                next_power_of_two
                    .trailing_zeros()
                    .saturating_sub(MINIMUM_SLOTS_PER_EPOCH.trailing_zeros()),
            );
            let expected_slot = next_power_of_two.saturating_sub(MINIMUM_SLOTS_PER_EPOCH);
            if self.first_normal_epoch != expected_epoch || self.first_normal_slot != expected_slot
            {
                return Err(TransferFeeError::InvalidEpochSchedule);
            }
        } else if self.first_normal_epoch != 0 || self.first_normal_slot != 0 {
            return Err(TransferFeeError::InvalidEpochSchedule);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferFeeEpochV1 {
    /// First epoch in which this fee is effective.
    pub effective_epoch: u64,
    pub transfer_fee_basis_points: u16,
    pub maximum_fee: TokenAmount,
    /// Historical mutation signature, or a domain-labeled DecisionGrade
    /// immutable-legacy account-proof identifier.
    pub source_signature: String,
    pub source_slot: u64,
    pub decoded_instruction_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MintTransferFeeScheduleV1 {
    pub schema_version: u16,
    pub mint: String,
    pub token_program: TokenProgramKindV1,
    /// Legacy Token has one implicit zero-fee epoch. Token-2022 epochs must
    /// begin at epoch zero so a lookup can never silently backfill.
    pub epochs: Vec<TransferFeeEpochV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferFeeRegistryV1 {
    pub schema_version: u16,
    pub epoch_schedule: EpochScheduleV1,
    pub mints: Vec<MintTransferFeeScheduleV1>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransferOutcomeV1 {
    pub gross_amount: u64,
    pub fee_amount: u64,
    pub net_amount: u64,
    pub epoch: u64,
}

#[derive(Clone, Debug)]
pub struct ValidatedTransferFeeRegistryV1 {
    epoch_schedule: EpochScheduleV1,
    mints: BTreeMap<String, MintTransferFeeScheduleV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum TransferFeeError {
    #[error("unsupported transfer-fee schema version {0}")]
    SchemaVersion(u16),
    #[error("invalid epoch schedule")]
    InvalidEpochSchedule,
    #[error("invalid or duplicate mint transfer-fee schedule {0}")]
    InvalidMint(String),
    #[error("missing exact transfer-fee schedule for mint {0}")]
    MissingMint(String),
    #[error("no transfer-fee epoch applies to mint {mint} at epoch {epoch}")]
    MissingEpoch { mint: String, epoch: u64 },
    #[error("transfer-fee arithmetic overflow")]
    ArithmeticOverflow,
    #[error("historical transfer observation differs from the decoded mint schedule")]
    ObservationMismatch,
}

impl TransferFeeRegistryV1 {
    pub fn validate(self) -> Result<ValidatedTransferFeeRegistryV1, TransferFeeError> {
        if self.schema_version != TRANSFER_FEE_SCHEMA_VERSION {
            return Err(TransferFeeError::SchemaVersion(self.schema_version));
        }
        self.epoch_schedule.validate()?;
        let mut mints = BTreeMap::new();
        for schedule in self.mints {
            schedule.validate()?;
            let mint = schedule.mint.clone();
            if mints.insert(mint.clone(), schedule).is_some() {
                return Err(TransferFeeError::InvalidMint(mint));
            }
        }
        Ok(ValidatedTransferFeeRegistryV1 {
            epoch_schedule: self.epoch_schedule,
            mints,
        })
    }
}

impl MintTransferFeeScheduleV1 {
    pub(crate) fn validate(&self) -> Result<(), TransferFeeError> {
        if self.schema_version != TRANSFER_FEE_SCHEMA_VERSION
            || self.mint.trim().is_empty()
            || self.epochs.is_empty()
        {
            return Err(TransferFeeError::InvalidMint(self.mint.clone()));
        }
        let mut epochs = BTreeSet::new();
        for (index, epoch) in self.epochs.iter().enumerate() {
            if epoch.transfer_fee_basis_points > 10_000
                || epoch.maximum_fee.as_str().parse::<u64>().is_err()
                || epoch.source_signature.trim().is_empty()
                || !valid_sha256(&epoch.decoded_instruction_sha256)
                || !epochs.insert(epoch.effective_epoch)
                || (index > 0 && self.epochs[index - 1].effective_epoch >= epoch.effective_epoch)
            {
                return Err(TransferFeeError::InvalidMint(self.mint.clone()));
            }
        }
        if self.epochs[0].effective_epoch != 0 {
            return Err(TransferFeeError::InvalidMint(self.mint.clone()));
        }
        if self.token_program == TokenProgramKindV1::LegacyToken
            && (self.epochs.len() != 1
                || self.epochs[0].transfer_fee_basis_points != 0
                || self.epochs[0].maximum_fee.as_str() != "0")
        {
            return Err(TransferFeeError::InvalidMint(self.mint.clone()));
        }
        Ok(())
    }
}

impl ValidatedTransferFeeRegistryV1 {
    pub fn transfer(
        &self,
        mint: &str,
        slot: u64,
        gross_amount: u64,
    ) -> Result<TransferOutcomeV1, TransferFeeError> {
        let epoch = self.epoch_schedule.epoch(slot)?;
        let fee_epoch = self.fee_epoch(mint, epoch)?;
        let bps = u128::from(fee_epoch.transfer_fee_basis_points);
        let maximum_fee = fee_epoch
            .maximum_fee
            .as_str()
            .parse::<u64>()
            .map_err(|_| TransferFeeError::ArithmeticOverflow)?;
        let raw_fee = if bps == 0 || gross_amount == 0 {
            0
        } else {
            let numerator = u128::from(gross_amount)
                .checked_mul(bps)
                .ok_or(TransferFeeError::ArithmeticOverflow)?;
            let rounded = numerator
                .checked_add(BASIS_POINTS_DENOMINATOR - 1)
                .ok_or(TransferFeeError::ArithmeticOverflow)?
                / BASIS_POINTS_DENOMINATOR;
            u64::try_from(rounded).map_err(|_| TransferFeeError::ArithmeticOverflow)?
        };
        let fee_amount = raw_fee.min(maximum_fee);
        let net_amount = gross_amount
            .checked_sub(fee_amount)
            .ok_or(TransferFeeError::ArithmeticOverflow)?;
        Ok(TransferOutcomeV1 {
            gross_amount,
            fee_amount,
            net_amount,
            epoch,
        })
    }

    /// Returns the smallest gross transfer whose post-fee credit is exactly
    /// `net_amount`, matching SPL Token-2022's `calculate_pre_fee_amount`.
    pub fn gross_for_exact_net(
        &self,
        mint: &str,
        slot: u64,
        net_amount: u64,
    ) -> Result<TransferOutcomeV1, TransferFeeError> {
        let epoch = self.epoch_schedule.epoch(slot)?;
        let fee_epoch = self.fee_epoch(mint, epoch)?;
        let bps = u128::from(fee_epoch.transfer_fee_basis_points);
        let maximum_fee = fee_epoch
            .maximum_fee
            .as_str()
            .parse::<u64>()
            .map_err(|_| TransferFeeError::ArithmeticOverflow)?;
        let gross = match (bps, net_amount) {
            (0, _) => net_amount,
            (_, 0) => 0,
            (BASIS_POINTS_DENOMINATOR, _) => maximum_fee
                .checked_add(net_amount)
                .ok_or(TransferFeeError::ArithmeticOverflow)?,
            _ => {
                let numerator = u128::from(net_amount)
                    .checked_mul(BASIS_POINTS_DENOMINATOR)
                    .ok_or(TransferFeeError::ArithmeticOverflow)?;
                let denominator = BASIS_POINTS_DENOMINATOR
                    .checked_sub(bps)
                    .ok_or(TransferFeeError::ArithmeticOverflow)?;
                let uncapped = numerator
                    .checked_add(denominator - 1)
                    .ok_or(TransferFeeError::ArithmeticOverflow)?
                    / denominator;
                if uncapped
                    .checked_sub(u128::from(net_amount))
                    .ok_or(TransferFeeError::ArithmeticOverflow)?
                    >= u128::from(maximum_fee)
                {
                    net_amount
                        .checked_add(maximum_fee)
                        .ok_or(TransferFeeError::ArithmeticOverflow)?
                } else {
                    u64::try_from(uncapped).map_err(|_| TransferFeeError::ArithmeticOverflow)?
                }
            }
        };
        let outcome = self.transfer(mint, slot, gross)?;
        if outcome.net_amount != net_amount {
            return Err(TransferFeeError::ArithmeticOverflow);
        }
        Ok(outcome)
    }

    pub fn assert_observed_credit(
        &self,
        mint: &str,
        slot: u64,
        gross_amount: u64,
        observed_credit: u64,
    ) -> Result<TransferOutcomeV1, TransferFeeError> {
        let outcome = self.transfer(mint, slot, gross_amount)?;
        if outcome.net_amount != observed_credit {
            return Err(TransferFeeError::ObservationMismatch);
        }
        Ok(outcome)
    }

    /// Validates a CPI-observed credit when the instruction records only the
    /// maximum gross amount the caller authorized, not the exact transfer.
    pub fn assert_observed_credit_with_maximum_gross(
        &self,
        mint: &str,
        slot: u64,
        maximum_gross_amount: u64,
        observed_credit: u64,
    ) -> Result<TransferOutcomeV1, TransferFeeError> {
        let outcome = self.gross_for_exact_net(mint, slot, observed_credit)?;
        if outcome.gross_amount > maximum_gross_amount {
            return Err(TransferFeeError::ObservationMismatch);
        }
        Ok(outcome)
    }

    fn fee_epoch(&self, mint: &str, epoch: u64) -> Result<&TransferFeeEpochV1, TransferFeeError> {
        self.mints
            .get(mint)
            .ok_or_else(|| TransferFeeError::MissingMint(mint.to_owned()))?
            .epochs
            .iter()
            .rev()
            .find(|candidate| candidate.effective_epoch <= epoch)
            .ok_or_else(|| TransferFeeError::MissingEpoch {
                mint: mint.to_owned(),
                epoch,
            })
    }
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn amount(value: u64) -> TokenAmount {
        TokenAmount::new(value.to_string()).unwrap()
    }

    fn registry() -> ValidatedTransferFeeRegistryV1 {
        TransferFeeRegistryV1 {
            schema_version: 1,
            epoch_schedule: EpochScheduleV1 {
                slots_per_epoch: 432_000,
                leader_schedule_slot_offset: 432_000,
                warmup: false,
                first_normal_epoch: 0,
                first_normal_slot: 0,
                source_response_sha256: "a".repeat(64),
            },
            mints: vec![MintTransferFeeScheduleV1 {
                schema_version: 1,
                mint: "mint".to_owned(),
                token_program: TokenProgramKindV1::Token2022,
                epochs: vec![
                    TransferFeeEpochV1 {
                        effective_epoch: 0,
                        transfer_fee_basis_points: 25,
                        maximum_fee: amount(100),
                        source_signature: "initialize".to_owned(),
                        source_slot: 0,
                        decoded_instruction_sha256: "b".repeat(64),
                    },
                    TransferFeeEpochV1 {
                        effective_epoch: 2,
                        transfer_fee_basis_points: 100,
                        maximum_fee: amount(10_000),
                        source_signature: "update".to_owned(),
                        source_slot: 800_000,
                        decoded_instruction_sha256: "c".repeat(64),
                    },
                ],
            }],
        }
        .validate()
        .unwrap()
    }

    #[test]
    fn fee_uses_spl_ceiling_and_cap() {
        let registry = registry();
        assert_eq!(registry.transfer("mint", 1, 1).unwrap().fee_amount, 1);
        assert_eq!(
            registry.transfer("mint", 1, 1_000_000).unwrap().fee_amount,
            100
        );
        assert_eq!(
            registry
                .transfer("mint", 900_000, 10_001)
                .unwrap()
                .fee_amount,
            101
        );
    }

    #[test]
    fn factual_observation_is_an_assertion_not_a_fee_oracle() {
        let registry = registry();
        registry
            .assert_observed_credit("mint", 1, 10_000, 9_975)
            .unwrap();
        assert_eq!(
            registry.assert_observed_credit("mint", 1, 10_001, 9_976),
            Err(TransferFeeError::ObservationMismatch)
        );
    }

    #[test]
    fn maximum_gross_observation_accepts_unused_input_but_not_an_overrun() {
        let registry = registry();
        let outcome = registry
            .assert_observed_credit_with_maximum_gross("mint", 1, 10_001, 9_975)
            .unwrap();
        assert_eq!(outcome.gross_amount, 10_000);
        assert_eq!(outcome.net_amount, 9_975);
        assert_eq!(
            registry.assert_observed_credit_with_maximum_gross("mint", 1, 9_999, 9_975),
            Err(TransferFeeError::ObservationMismatch)
        );
    }

    #[test]
    fn inverse_fee_returns_smallest_exact_gross() {
        let registry = registry();
        let outcome = registry.gross_for_exact_net("mint", 1, 9_975).unwrap();
        assert_eq!(outcome.gross_amount, 10_000);
        assert_eq!(outcome.net_amount, 9_975);
    }

    #[test]
    fn missing_epoch_zero_fails_closed() {
        let mut schedule = registry().mints["mint"].clone();
        schedule.epochs.remove(0);
        assert!(schedule.validate().is_err());
    }

    #[test]
    fn warmup_epoch_schedule_fields_must_match_solana_derivation() {
        let valid = EpochScheduleV1 {
            slots_per_epoch: 432_000,
            leader_schedule_slot_offset: 432_000,
            warmup: true,
            first_normal_epoch: 14,
            first_normal_slot: 524_256,
            source_response_sha256: "d".repeat(64),
        };
        valid.validate().unwrap();
        assert_eq!(valid.epoch(31).unwrap(), 0);
        assert_eq!(valid.epoch(32).unwrap(), 1);

        let mut mismatched = valid;
        mismatched.first_normal_slot = 524_255;
        assert_eq!(
            mismatched.validate(),
            Err(TransferFeeError::InvalidEpochSchedule)
        );
    }
}
