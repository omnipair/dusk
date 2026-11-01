//! Slot-ranged reconstruction of archived V1 instructions and CPI events.
//!
//! Revision metadata selects and labels a range, but the current implementation
//! intentionally remains provisional until real historical decoder dispatch is
//! implemented and verified against deployed program binaries.

use std::{
    collections::{BTreeMap, BTreeSet},
    str::FromStr,
};

use carbon_core::instruction::InstructionDecoder;
use carbon_omnipair_decoder::{instructions::OmnipairInstruction, OmnipairDecoder};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;
use thiserror::Error;

use crate::{
    canonical::{sha256_hex, ChainOrder, SignedTokenAmount, TokenAmount},
    extract::{ArchivedBlock, ArchivedTransaction},
    token_cpi::{
        parse_compiled_token_instruction_v1, parse_deferred_token_account_adjustment_v1,
        resolve_deferred_token_account_adjustment_v1, CertifiedZeroNativeInitializationV1,
        DeferredTokenAccountAdjustmentCpiV1, ExactTokenAccountAdjustmentV1, ParsedTokenCpiV1,
        TokenAccountAdjustmentReasonV1, TokenAccountAdjustmentSourceV1, TokenCpiError,
        TokenSupplyChangeKindV1, TokenTransferVariantV1, ASSOCIATED_TOKEN_PROGRAM_ID,
        TOKEN_CPI_SCHEMA_VERSION,
    },
    transfer_fee::TokenProgramKindV1,
};

/// Serialized decoded transaction/provenance schema. Version 5 additionally
/// classifies Anchor IDL-account mutations as program metadata and decodes two
/// exact legacy event-CPI layouts.
pub const DECODE_SCHEMA_VERSION: u16 = 5;
/// Decoder range metadata schema. Version 2 separates the required hash of the
/// locally compiled candidate ELF from the optional hash of the exact deployed
/// ELF recovered from chain history.
pub const DECODER_REGISTRY_SCHEMA_VERSION: u16 = 2;

const LEGACY_TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const TOKEN_2022_PROGRAM_ID: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";
const SYSTEM_PROGRAM_ID: &str = "11111111111111111111111111111111";
/// Anchor `event-cpi` prefix followed by the `SwapEvent` discriminator. The
/// discriminator did not change when fee fields were appended.
const SWAP_EVENT_DISCRIMINATOR: [u8; 16] = [
    0xe4, 0x45, 0xa5, 0x2e, 0x51, 0xcb, 0x9a, 0x1d, 0x40, 0xc6, 0xcd, 0xe8, 0x26, 0x08, 0x71, 0xe2,
];
/// Historical layout at omnipair-rs `e26bc2e^`: six scalar fields followed by
/// `EventMetadata`. No per-beneficiary fee fields existed in this wire shape.
const SWAP_EVENT_LEGACY_V0_WIRE_LEN: usize = 129;
const ANCHOR_IDL_INSTRUCTION_PREFIX: [u8; 8] = [0x40, 0xf4, 0xbc, 0x78, 0xa7, 0xe9, 0x69, 0x0a];
const USER_POSITION_UPDATED_EVENT_LEGACY_V0_DISCRIMINATOR: [u8; 16] = [
    0xe4, 0x45, 0xa5, 0x2e, 0x51, 0xcb, 0x9a, 0x1d, 0x53, 0xa8, 0xc5, 0x58, 0x59, 0x2a, 0x3a, 0x66,
];
const USER_POSITION_UPDATED_EVENT_LEGACY_V0_WIRE_LEN: usize = 172;
const USER_LIQUIDITY_POSITION_UPDATED_EVENT_LEGACY_V0_DISCRIMINATOR: [u8; 16] = [
    0xe4, 0x45, 0xa5, 0x2e, 0x51, 0xcb, 0x9a, 0x1d, 0xff, 0xe3, 0x20, 0x6b, 0xd3, 0xf6, 0x27, 0x4e,
];
const USER_LIQUIDITY_POSITION_UPDATED_EVENT_LEGACY_V0_WIRE_LEN: usize = 208;

// Decoder metadata alone is not proof that the compiled implementation
// dispatches every historical layout. Keep headline eligibility fail-closed
// until real revision-specific implementations and deployed-binary checks are
// wired in.
pub(crate) const HISTORICAL_DECODER_DISPATCH_VERIFIED: bool = false;
// Carbon's indexer metadata uses a fixed five-level path stack.
const INDEXER_MAX_INSTRUCTION_STACK_DEPTH: usize = 5;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecoderRevisionV1 {
    pub decoder_version_id: String,
    pub valid_from_slot: u64,
    pub valid_to_slot_exclusive: Option<u64>,
    pub source_git_commit: String,
    pub idl_sha256: String,
    /// Raw SHA-256 of the locally compiled candidate ELF used by this decoder.
    /// This is required even while deployment certification is provisional.
    pub candidate_program_binary_sha256: String,
    /// Raw SHA-256 of the exact deployed ELF reconstructed from chain history.
    /// Absence means that deployment identity has not been certified.
    pub program_binary_sha256: Option<String>,
    /// A provisional range can support reconstructive and decision-grade
    /// evidence but cannot pass audit-grade deployment certification.
    pub provisional: bool,
}

impl DecoderRevisionV1 {
    fn contains(&self, slot: u64) -> bool {
        slot >= self.valid_from_slot
            && self
                .valid_to_slot_exclusive
                .is_none_or(|exclusive| slot < exclusive)
    }

    /// The minimum immutable binding required for reconstructive and
    /// decision-grade replay. Deployment certification is deliberately not
    /// part of this predicate.
    pub fn has_structural_candidate_binding(&self) -> bool {
        !self.decoder_version_id.is_empty()
            && valid_git_oid(&self.source_git_commit)
            && valid_lower_sha256(&self.idl_sha256)
            && valid_lower_sha256(&self.candidate_program_binary_sha256)
    }

    fn has_matching_deployed_binary(&self) -> bool {
        self.program_binary_sha256.as_ref().is_some_and(|deployed| {
            valid_lower_sha256(deployed) && deployed == &self.candidate_program_binary_sha256
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecoderRegistryV1 {
    pub schema_version: u16,
    pub revisions: Vec<DecoderRevisionV1>,
}

impl DecoderRegistryV1 {
    pub fn decoder_for_slot(&self, slot: u64) -> Result<&DecoderRevisionV1, DecodeError> {
        if self.schema_version != DECODER_REGISTRY_SCHEMA_VERSION {
            return Err(DecodeError::SchemaVersion(self.schema_version));
        }
        let matches: Vec<_> = self
            .revisions
            .iter()
            .filter(|revision| revision.contains(slot))
            .collect();
        match matches.as_slice() {
            [revision] => Ok(revision),
            [] => Err(DecodeError::NoDecoder(slot)),
            _ => Err(DecodeError::OverlappingDecoders(slot)),
        }
    }

    pub fn validate_complete_range(&self, start: u64, end: u64) -> Result<(), DecodeError> {
        if start > end {
            return Err(DecodeError::InvalidRange);
        }
        let mut boundaries = BTreeSet::from([start, end.saturating_add(1)]);
        for revision in &self.revisions {
            if (start..=end).contains(&revision.valid_from_slot) {
                boundaries.insert(revision.valid_from_slot);
            }
            if let Some(exclusive) = revision.valid_to_slot_exclusive {
                if exclusive > start && exclusive <= end {
                    boundaries.insert(exclusive);
                }
            }
        }
        for slot in boundaries.into_iter().filter(|slot| *slot <= end) {
            self.decoder_for_slot(slot)?;
        }
        Ok(())
    }

    pub fn is_headline_verified(&self) -> bool {
        self.schema_version == DECODER_REGISTRY_SCHEMA_VERSION
            && HISTORICAL_DECODER_DISPATCH_VERIFIED
            && !self.revisions.is_empty()
            && self.revisions.iter().all(|revision| {
                revision.has_structural_candidate_binding()
                    && !revision.provisional
                    && revision.has_matching_deployed_binary()
            })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecodedInstructionClass {
    StateChanging,
    ReadOnlyView,
    EmittedAssertion,
    /// Anchor IDL-account administration mutates program metadata but cannot
    /// mutate Omnipair economic state.
    ProgramMetadata,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DecodedInstructionV1 {
    pub schema_version: u16,
    pub decoder_version_id: String,
    pub signature: String,
    pub order: ChainOrder,
    pub block_time_unix_seconds: i64,
    pub class: DecodedInstructionClass,
    pub kind: String,
    pub data: JsonValue,
    pub accounts: Vec<String>,
    pub raw_data_sha256: String,
    pub stack_height: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DecodedTransactionV1 {
    pub schema_version: u16,
    pub decoder_version_id: String,
    pub signature: String,
    pub rpc_payload_sha256: String,
    pub slot: u64,
    pub transaction_index: u32,
    pub block_time_unix_seconds: i64,
    pub program_instruction_count: u32,
    pub state_changing_instruction_count: u32,
    pub emitted_assertion_count: u32,
    pub unknown_program_instruction_hashes: Vec<String>,
    pub non_target_outer_touched_accounts: Vec<String>,
    pub token_balances: Vec<TokenAccountBalanceV1>,
    pub instructions: Vec<DecodedInstructionV1>,
}

/// Whether an instruction's direct CPI parent can be recovered from finalized
/// `getBlock` execution metadata. Consumers must only bind an emitted assertion
/// to an operation when this is `complete` and `parent_order` equals the
/// operation's exact order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CpiFrameProvenanceV1 {
    Complete,
    MissingStackHeight,
    InvalidStackHeight,
    MissingParentFrame,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionExecutionFrameV1 {
    pub schema_version: u16,
    pub order: ChainOrder,
    pub program_id: String,
    pub is_target_program: bool,
    /// Top-level instructions are normalized to height 1. Inner instructions
    /// retain the archival RPC `stackHeight`; `None` is never guessed.
    pub stack_height: Option<u32>,
    pub parent_order: Option<ChainOrder>,
    /// Indexer-compatible `absolute_path`: outer instruction index followed by
    /// per-stack sibling counters. The DB text form pads each component to six
    /// digits and joins them with `.`.
    pub execution_path: Vec<u32>,
    pub provenance: CpiFrameProvenanceV1,
    /// Classification by the currently selected reconstructive decoder. `None`
    /// on a target instruction is an explicit, blocking unknown.
    pub decoded_target_class: Option<DecodedInstructionClass>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransactionExecutionProvenanceV1 {
    pub schema_version: u16,
    pub decoder_version_id: String,
    pub signature: String,
    pub slot: u64,
    pub transaction_index: u32,
    pub frames: Vec<InstructionExecutionFrameV1>,
    /// Canonically ordered SPL Token instructions proven to be descendants of
    /// a state-changing target instruction. Ownership is always the nearest
    /// such ancestor, so nested V1 operations cannot double-count a CPI.
    pub token_cpis: Vec<ParsedTokenCpiV1>,
}

impl TransactionExecutionProvenanceV1 {
    pub fn is_complete(&self) -> bool {
        self.frames
            .iter()
            .all(|frame| frame.provenance == CpiFrameProvenanceV1::Complete)
    }

    /// Returns a direct parent only when finalized RPC execution metadata proves
    /// the relationship. This deliberately returns `None` for missing/invalid
    /// historical `stackHeight` rather than inferring adjacency.
    pub fn verified_parent(&self, child: ChainOrder) -> Option<ChainOrder> {
        self.frames
            .iter()
            .find(|frame| frame.order == child)
            .filter(|frame| frame.provenance == CpiFrameProvenanceV1::Complete)
            .and_then(|frame| frame.parent_order)
    }

    pub fn verified_execution_path(&self, instruction: ChainOrder) -> Option<&[u32]> {
        self.frames
            .iter()
            .find(|frame| frame.order == instruction)
            .filter(|frame| frame.provenance == CpiFrameProvenanceV1::Complete)
            .map(|frame| frame.execution_path.as_slice())
    }

    /// Returns the nearest state-changing target ancestor for an instruction.
    /// Missing or invalid CPI ancestry never becomes inferred ownership.
    pub fn state_changing_owner(&self, instruction: ChainOrder) -> Option<ChainOrder> {
        let frames = structurally_valid_frame_map(&self.frames)?;
        nearest_state_changing_target_ancestor(instruction, &frames)
            .ok()
            .flatten()
    }

    /// Token CPI evidence owned exclusively by `operation` in canonical order.
    pub fn token_cpis_owned_by(&self, operation: ChainOrder) -> Vec<&ParsedTokenCpiV1> {
        self.token_cpis
            .iter()
            .filter(|token_cpi| self.state_changing_owner(token_cpi.order()) == Some(operation))
            .collect()
    }
}

fn unique_frame_map(
    frames: &[InstructionExecutionFrameV1],
) -> Option<BTreeMap<ChainOrder, &InstructionExecutionFrameV1>> {
    let mut by_order = BTreeMap::new();
    for frame in frames {
        if by_order.insert(frame.order, frame).is_some() {
            return None;
        }
    }
    Some(by_order)
}

fn structurally_valid_frame_map(
    frames: &[InstructionExecutionFrameV1],
) -> Option<BTreeMap<ChainOrder, &InstructionExecutionFrameV1>> {
    let by_order = unique_frame_map(frames)?;
    let mut complete_paths = BTreeSet::new();
    for frame in by_order.values() {
        if frame.order.event_sequence != 0
            || (!frame.is_target_program && frame.decoded_target_class.is_some())
        {
            return None;
        }
        if frame.provenance != CpiFrameProvenanceV1::Complete {
            if frame.parent_order.is_some() {
                return None;
            }
            continue;
        }
        if !complete_paths.insert(frame.execution_path.clone()) {
            return None;
        }
        match (
            frame.order.inner_execution_index,
            frame.stack_height,
            frame.parent_order,
        ) {
            (None, Some(1), None)
                if frame.execution_path == [frame.order.outer_instruction_index] => {}
            (Some(_), Some(height), Some(parent_order)) if height >= 2 => {
                let parent = by_order.get(&parent_order)?;
                let parent_height = parent.stack_height?;
                if parent.provenance != CpiFrameProvenanceV1::Complete
                    || parent_height.checked_add(1) != Some(height)
                    || frame.execution_path.len() != parent.execution_path.len() + 1
                    || !frame.execution_path.starts_with(&parent.execution_path)
                    || parent.order.slot != frame.order.slot
                    || parent.order.transaction_index != frame.order.transaction_index
                    || parent.order.outer_instruction_index != frame.order.outer_instruction_index
                    || parent.order >= frame.order
                {
                    return None;
                }
            }
            _ => return None,
        }
    }
    Some(by_order)
}

/// Finds one exclusive owner by walking direct, verified parents. The first
/// state-changing target frame wins; outer target frames are never also owners.
fn nearest_state_changing_target_ancestor(
    instruction: ChainOrder,
    frames: &BTreeMap<ChainOrder, &InstructionExecutionFrameV1>,
) -> Result<Option<ChainOrder>, ()> {
    let mut current = instruction;
    let mut visited = BTreeSet::new();
    loop {
        if !visited.insert(current) {
            return Err(());
        }
        let frame = frames.get(&current).ok_or(())?;
        if frame.provenance != CpiFrameProvenanceV1::Complete {
            return Ok(None);
        }
        let Some(parent_order) = frame.parent_order else {
            return Ok(None);
        };
        let parent = frames.get(&parent_order).ok_or(())?;
        if parent.provenance != CpiFrameProvenanceV1::Complete {
            return Ok(None);
        }
        if parent.is_target_program
            && parent.decoded_target_class == Some(DecodedInstructionClass::StateChanging)
        {
            return Ok(Some(parent.order));
        }
        current = parent_order;
    }
}

fn parsed_token_cpi_metadata(token_cpi: &ParsedTokenCpiV1) -> (u16, &str) {
    (token_cpi.schema_version(), token_cpi.raw_data_sha256())
}

fn valid_lower_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_git_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn token_cpis_are_internally_consistent(provenance: &TransactionExecutionProvenanceV1) -> bool {
    if provenance.schema_version != DECODE_SCHEMA_VERSION {
        return false;
    }
    let Some(frames) = structurally_valid_frame_map(&provenance.frames) else {
        return false;
    };
    if frames.values().any(|frame| {
        frame.schema_version != DECODE_SCHEMA_VERSION
            || frame.order.slot != provenance.slot
            || frame.order.transaction_index != provenance.transaction_index
            || frame.order.event_sequence != 0
    }) {
        return false;
    }

    let mut expected = BTreeMap::<ChainOrder, TokenProgramKindV1>::new();
    for frame in frames.values().filter(|frame| {
        frame.program_id == LEGACY_TOKEN_PROGRAM_ID || frame.program_id == TOKEN_2022_PROGRAM_ID
    }) {
        match nearest_state_changing_target_ancestor(frame.order, &frames) {
            Ok(Some(_owner)) => {
                let program = if frame.program_id == LEGACY_TOKEN_PROGRAM_ID {
                    TokenProgramKindV1::LegacyToken
                } else {
                    TokenProgramKindV1::Token2022
                };
                if expected.insert(frame.order, program).is_some() {
                    return false;
                }
            }
            Ok(None) => {}
            Err(()) => return false,
        }
    }

    let mut previous_order = None;
    for token_cpi in &provenance.token_cpis {
        let order = token_cpi.order();
        if previous_order.is_some_and(|previous| previous >= order) {
            return false;
        }
        previous_order = Some(order);
        let (schema_version, raw_data_sha256) = parsed_token_cpi_metadata(token_cpi);
        if schema_version != TOKEN_CPI_SCHEMA_VERSION || !valid_lower_sha256(raw_data_sha256) {
            return false;
        }
        if expected.remove(&order) != Some(token_cpi.token_program()) {
            return false;
        }
    }
    expected.is_empty()
}

impl InstructionExecutionFrameV1 {
    pub fn indexer_instruction_path(&self) -> String {
        self.execution_path
            .iter()
            .map(|component| format!("{component:06}"))
            .collect::<Vec<_>>()
            .join(".")
    }
}

/// Instruction-grained coverage. `expected` and `decoded` count the same unit:
/// state-changing target-program instructions recognized by the selected
/// reconstructive decoder. Unknown target instructions and transactions whose
/// raw inventory cannot be established are separate mandatory blockers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateChangingInstructionCoverageV1 {
    pub expected: u64,
    pub decoded: u64,
    pub unclassified_target_instructions: u64,
    pub transactions_with_incomplete_inventory: u64,
}

impl StateChangingInstructionCoverageV1 {
    pub fn is_exact(self) -> bool {
        self.expected == self.decoded
            && self.unclassified_target_instructions == 0
            && self.transactions_with_incomplete_inventory == 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionDecodeFailureCategoryV1 {
    InvalidTransactionShape,
    ParsedTargetInstruction,
    IndexOverflow,
    AccountIndex,
    InvalidBase58,
    InvalidAccountKey,
    DecodedSerialization,
    InvalidTokenBalance,
    ExactTokenCpiMalformed,
    ExactTokenCpiUnknown,
    ExactTokenCpiStateDependent,
    ExactTokenCpiUnsupported,
    ExactTokenCpiInvariant,
}

impl TransactionDecodeFailureCategoryV1 {
    fn from_error(error: &DecodeError) -> Self {
        match error {
            DecodeError::InvalidTransactionShape => Self::InvalidTransactionShape,
            DecodeError::ParsedTargetInstruction => Self::ParsedTargetInstruction,
            DecodeError::IndexOverflow => Self::IndexOverflow,
            DecodeError::AccountIndex => Self::AccountIndex,
            DecodeError::InvalidBase58 => Self::InvalidBase58,
            DecodeError::InvalidAccountKey => Self::InvalidAccountKey,
            DecodeError::DecodedSerialization => Self::DecodedSerialization,
            DecodeError::InvalidTokenBalance => Self::InvalidTokenBalance,
            DecodeError::ExactTokenCpiEvidence(error) => match error {
                TokenCpiError::UnknownInstruction { .. } => Self::ExactTokenCpiUnknown,
                TokenCpiError::RequiresExternalCustodyState { .. } => {
                    Self::ExactTokenCpiStateDependent
                }
                TokenCpiError::UnsupportedProgram(_)
                | TokenCpiError::UnsupportedValueMovement { .. }
                | TokenCpiError::UnsupportedToken2022Extension(_) => Self::ExactTokenCpiUnsupported,
                TokenCpiError::InvalidInstructionOrder
                | TokenCpiError::InvalidCompiledInstruction
                | TokenCpiError::InvalidCompiledField(_)
                | TokenCpiError::AccountIndexOutOfBounds(_)
                | TokenCpiError::AccountIndexOverflow(_)
                | TokenCpiError::InvalidAccountKey(_)
                | TokenCpiError::InvalidBase58
                | TokenCpiError::EmptyInstructionData
                | TokenCpiError::MalformedInstruction { .. }
                | TokenCpiError::NonCanonicalInstructionEncoding { .. }
                | TokenCpiError::TooFewTransferAccounts { .. }
                | TokenCpiError::TooFewSupplyChangeAccounts { .. } => Self::ExactTokenCpiMalformed,
                _ => Self::ExactTokenCpiInvariant,
            },
            DecodeError::SchemaVersion(_)
            | DecodeError::NoDecoder(_)
            | DecodeError::OverlappingDecoders(_)
            | DecodeError::InvalidRange
            | DecodeError::InvalidProgramId
            | DecodeError::MissingBlockTime(_) => {
                unreachable!("block-scoped decoder errors cannot become transaction failures")
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransactionDecodeFailureV1 {
    pub schema_version: u16,
    pub decoder_version_id: String,
    pub signature: String,
    pub rpc_payload_sha256: String,
    pub slot: u64,
    pub transaction_index: u32,
    pub category: TransactionDecodeFailureCategoryV1,
    pub expected_state_changing_instruction_count: u32,
    /// Counts usable decoded instructions committed to the output. A failed
    /// transaction is atomic, so this is zero rather than a partial prefix.
    pub decoded_state_changing_instruction_count: u32,
    pub unclassified_target_instruction_count: u32,
    pub instruction_inventory_available: bool,
    pub expected_count_complete: bool,
    pub cpi_provenance_complete: bool,
}

#[must_use = "persist valid transactions, transaction failures, provenance, and coverage together"]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BlockDecodeOutcomeV1 {
    pub schema_version: u16,
    pub slot: u64,
    pub transactions: Vec<DecodedTransactionV1>,
    pub transaction_failures: Vec<TransactionDecodeFailureV1>,
    /// One entry for each transaction in `transactions`, joined by signature and
    /// exact `(slot, transaction_index)` identity.
    pub execution_provenance: Vec<TransactionExecutionProvenanceV1>,
    pub state_changing_instruction_coverage: StateChangingInstructionCoverageV1,
}

impl BlockDecodeOutcomeV1 {
    pub fn provenance_for(
        &self,
        transaction: &DecodedTransactionV1,
    ) -> Option<&TransactionExecutionProvenanceV1> {
        self.execution_provenance.iter().find(|provenance| {
            provenance.signature == transaction.signature
                && provenance.slot == transaction.slot
                && provenance.transaction_index == transaction.transaction_index
        })
    }

    pub fn is_internally_consistent(&self) -> bool {
        if self.schema_version != DECODE_SCHEMA_VERSION
            || self.transactions.len() != self.execution_provenance.len()
        {
            return false;
        }
        let paired = self
            .transactions
            .iter()
            .zip(&self.execution_provenance)
            .all(|(transaction, provenance)| {
                let target_frames: Vec<_> = provenance
                    .frames
                    .iter()
                    .filter(|frame| frame.is_target_program)
                    .collect();
                let known_frames: BTreeMap<_, _> = target_frames
                    .iter()
                    .filter_map(|frame| {
                        frame.decoded_target_class.map(|class| (frame.order, class))
                    })
                    .collect();
                transaction.signature == provenance.signature
                    && transaction.schema_version == DECODE_SCHEMA_VERSION
                    && provenance.schema_version == DECODE_SCHEMA_VERSION
                    && transaction.slot == self.slot
                    && transaction.slot == provenance.slot
                    && transaction.transaction_index == provenance.transaction_index
                    && transaction.decoder_version_id == provenance.decoder_version_id
                    && usize::try_from(transaction.program_instruction_count).ok()
                        == Some(target_frames.len())
                    && usize::try_from(transaction.state_changing_instruction_count).ok()
                        == Some(
                            target_frames
                                .iter()
                                .filter(|frame| {
                                    frame.decoded_target_class
                                        == Some(DecodedInstructionClass::StateChanging)
                                })
                                .count(),
                        )
                    && usize::try_from(transaction.emitted_assertion_count).ok()
                        == Some(
                            target_frames
                                .iter()
                                .filter(|frame| {
                                    frame.decoded_target_class
                                        == Some(DecodedInstructionClass::EmittedAssertion)
                                })
                                .count(),
                        )
                    && transaction.unknown_program_instruction_hashes.len()
                        == target_frames
                            .iter()
                            .filter(|frame| frame.decoded_target_class.is_none())
                            .count()
                    && known_frames.len() == transaction.instructions.len()
                    && transaction.instructions.iter().all(|instruction| {
                        known_frames.get(&instruction.order).copied() == Some(instruction.class)
                    })
                    && token_cpis_are_internally_consistent(provenance)
            });
        if !paired {
            return false;
        }
        if self.transaction_failures.iter().any(|failure| {
            failure.schema_version != DECODE_SCHEMA_VERSION
                || failure.slot != self.slot
                || failure.decoded_state_changing_instruction_count != 0
        }) {
            return false;
        }
        if self.transaction_failures.iter().any(|failure| {
            failure.expected_count_complete
                != (failure.instruction_inventory_available
                    && failure.unclassified_target_instruction_count == 0)
                || (!failure.instruction_inventory_available
                    && (failure.expected_state_changing_instruction_count != 0
                        || failure.unclassified_target_instruction_count != 0
                        || failure.cpi_provenance_complete))
        }) {
            return false;
        }
        // Solana transaction signatures and positions are each block-unique.
        // Checking only the `(signature, transaction_index)` pair would allow
        // two records to alias either RPC identity independently.
        let mut signatures = BTreeSet::new();
        let mut transaction_indexes = BTreeSet::new();
        if !self.transactions.iter().all(|transaction| {
            signatures.insert(transaction.signature.as_str())
                && transaction_indexes.insert(transaction.transaction_index)
        }) || !self.transaction_failures.iter().all(|failure| {
            signatures.insert(failure.signature.as_str())
                && transaction_indexes.insert(failure.transaction_index)
        }) {
            return false;
        }
        let decoded = self
            .transactions
            .iter()
            .map(|transaction| u64::from(transaction.state_changing_instruction_count))
            .sum::<u64>();
        let expected = self
            .execution_provenance
            .iter()
            .flat_map(|provenance| &provenance.frames)
            .filter(|frame| {
                frame.is_target_program
                    && frame.decoded_target_class == Some(DecodedInstructionClass::StateChanging)
            })
            .count() as u64
            + self
                .transaction_failures
                .iter()
                .map(|failure| u64::from(failure.expected_state_changing_instruction_count))
                .sum::<u64>();
        let unclassified = self
            .execution_provenance
            .iter()
            .flat_map(|provenance| &provenance.frames)
            .filter(|frame| frame.is_target_program && frame.decoded_target_class.is_none())
            .count() as u64
            + self
                .transaction_failures
                .iter()
                .map(|failure| u64::from(failure.unclassified_target_instruction_count))
                .sum::<u64>();
        let incomplete = self
            .transaction_failures
            .iter()
            .filter(|failure| !failure.instruction_inventory_available)
            .count() as u64;
        self.state_changing_instruction_coverage
            == StateChangingInstructionCoverageV1 {
                expected,
                decoded,
                unclassified_target_instructions: unclassified,
                transactions_with_incomplete_inventory: incomplete,
            }
    }

    pub fn is_fully_decoded(&self) -> bool {
        self.is_internally_consistent()
            && self.transaction_failures.is_empty()
            && self.state_changing_instruction_coverage.is_exact()
            && self
                .execution_provenance
                .iter()
                .all(TransactionExecutionProvenanceV1::is_complete)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenAccountBalanceV1 {
    pub account_index: u32,
    pub account: String,
    pub mint: String,
    /// Exact owner program reported by finalized RPC token-balance metadata.
    /// This is the token-account program, not the wallet authority in `owner`.
    pub token_program: TokenProgramKindV1,
    pub owner: Option<String>,
    pub decimals: u8,
    pub pre_amount: TokenAmount,
    pub post_amount: TokenAmount,
}

impl DecodedTransactionV1 {
    pub fn is_state_changing(&self) -> bool {
        self.state_changing_instruction_count > 0
    }

    pub fn is_fully_decoded(&self) -> bool {
        self.unknown_program_instruction_hashes.is_empty()
    }
}

pub fn decode_archived_block(
    block: &ArchivedBlock,
    program_id: &str,
    registry: &DecoderRegistryV1,
) -> Result<BlockDecodeOutcomeV1, DecodeError> {
    let block_time = block
        .block_time_unix_seconds
        .ok_or(DecodeError::MissingBlockTime(block.slot))?;
    let revision = registry.decoder_for_slot(block.slot)?;
    let target_program = Pubkey::from_str(program_id).map_err(|_| DecodeError::InvalidProgramId)?;
    let mut outcome = BlockDecodeOutcomeV1 {
        schema_version: DECODE_SCHEMA_VERSION,
        slot: block.slot,
        transactions: Vec::new(),
        transaction_failures: Vec::new(),
        execution_provenance: Vec::new(),
        state_changing_instruction_coverage: StateChangingInstructionCoverageV1::default(),
    };

    for transaction in block
        .transactions
        .iter()
        .filter(|transaction| transaction.succeeded)
    {
        let inspection =
            match inspect_transaction_execution(block, transaction, target_program, revision) {
                Ok(inspection) => inspection,
                Err(error) => {
                    outcome
                        .state_changing_instruction_coverage
                        .transactions_with_incomplete_inventory += 1;
                    outcome.transaction_failures.push(transaction_failure(
                        block.slot,
                        transaction,
                        revision,
                        &error,
                        None,
                    ));
                    continue;
                }
            };
        outcome.state_changing_instruction_coverage.expected +=
            u64::from(inspection.expected_state_changing_instruction_count);
        outcome
            .state_changing_instruction_coverage
            .unclassified_target_instructions +=
            u64::from(inspection.unclassified_target_instruction_count);

        if let Some(token_cpi_error) = inspection.token_cpi_error.clone() {
            outcome.transaction_failures.push(transaction_failure(
                block.slot,
                transaction,
                revision,
                &DecodeError::ExactTokenCpiEvidence(token_cpi_error),
                Some(&inspection),
            ));
            continue;
        }

        match decode_transaction(block, block_time, transaction, target_program, revision) {
            Ok(decoded) if decoded.program_instruction_count > 0 => {
                outcome.state_changing_instruction_coverage.decoded +=
                    u64::from(decoded.state_changing_instruction_count);
                outcome.execution_provenance.push(inspection.provenance);
                outcome.transactions.push(decoded);
            }
            Ok(_) => {}
            Err(error) => outcome.transaction_failures.push(transaction_failure(
                block.slot,
                transaction,
                revision,
                &error,
                Some(&inspection),
            )),
        }
    }
    Ok(outcome)
}

#[derive(Clone, Debug)]
struct TransactionInspectionV1 {
    expected_state_changing_instruction_count: u32,
    unclassified_target_instruction_count: u32,
    provenance: TransactionExecutionProvenanceV1,
    token_cpi_error: Option<TokenCpiError>,
}

fn transaction_failure(
    slot: u64,
    transaction: &ArchivedTransaction,
    revision: &DecoderRevisionV1,
    error: &DecodeError,
    inspection: Option<&TransactionInspectionV1>,
) -> TransactionDecodeFailureV1 {
    TransactionDecodeFailureV1 {
        schema_version: DECODE_SCHEMA_VERSION,
        decoder_version_id: revision.decoder_version_id.clone(),
        signature: transaction.signature.clone(),
        rpc_payload_sha256: transaction.rpc_payload_sha256.clone(),
        slot,
        transaction_index: transaction.transaction_index,
        category: TransactionDecodeFailureCategoryV1::from_error(error),
        expected_state_changing_instruction_count: inspection
            .map(|value| value.expected_state_changing_instruction_count)
            .unwrap_or_default(),
        decoded_state_changing_instruction_count: 0,
        unclassified_target_instruction_count: inspection
            .map(|value| value.unclassified_target_instruction_count)
            .unwrap_or_default(),
        instruction_inventory_available: inspection.is_some(),
        expected_count_complete: inspection
            .is_some_and(|value| value.unclassified_target_instruction_count == 0),
        cpi_provenance_complete: inspection.is_some_and(|value| value.provenance.is_complete()),
    }
}

fn inspect_transaction_execution(
    block: &ArchivedBlock,
    transaction: &ArchivedTransaction,
    target_program: Pubkey,
    revision: &DecoderRevisionV1,
) -> Result<TransactionInspectionV1, DecodeError> {
    let account_keys = account_keys(transaction)?;
    let outer = transaction
        .transaction
        .get("message")
        .and_then(|message| message.get("instructions"))
        .and_then(JsonValue::as_array)
        .ok_or(DecodeError::InvalidTransactionShape)?;
    let inner_groups = inner_instruction_groups(transaction, outer.len())?;
    let target_program_text = target_program.to_string();
    let mut frames = Vec::new();
    let mut raw_instructions = Vec::new();
    let mut expected_state_changing_instruction_count = 0_u32;
    let mut unclassified_target_instruction_count = 0_u32;

    for (outer_index, raw) in outer.iter().enumerate() {
        let outer_instruction_index =
            u32::try_from(outer_index).map_err(|_| DecodeError::IndexOverflow)?;
        let outer_order = ChainOrder {
            slot: block.slot,
            transaction_index: transaction.transaction_index,
            outer_instruction_index,
            inner_execution_index: None,
            event_sequence: 0,
        };
        let outer_frame = execution_frame(
            raw,
            &account_keys,
            &target_program_text,
            outer_order,
            Some(1),
            None,
            vec![outer_instruction_index],
            CpiFrameProvenanceV1::Complete,
        )?;
        record_expected_state_changing(
            &outer_frame,
            &mut expected_state_changing_instruction_count,
            &mut unclassified_target_instruction_count,
        )?;
        frames.push(outer_frame);
        raw_instructions.push((outer_order, raw));

        // The map contains the most recent still-active invocation at each
        // height. Encountering a sibling or ancestor truncates completed frames.
        let mut active_frames = BTreeMap::from([(1_u32, (outer_order, true))]);
        let mut path_stack = [0_u32; INDEXER_MAX_INSTRUCTION_STACK_DEPTH];
        path_stack[0] = outer_instruction_index;
        let mut previous_indexer_height = 0_usize;
        if let Some(inner) = inner_groups.get(&outer_instruction_index) {
            for (inner_index, raw_inner) in inner.iter().enumerate() {
                let inner_execution_index =
                    u32::try_from(inner_index).map_err(|_| DecodeError::IndexOverflow)?;
                let order = ChainOrder {
                    slot: block.slot,
                    transaction_index: transaction.transaction_index,
                    outer_instruction_index,
                    inner_execution_index: Some(inner_execution_index),
                    event_sequence: 0,
                };
                let raw_height = raw_inner.get("stackHeight");
                let stack_height = match raw_height {
                    Some(JsonValue::Number(number)) => {
                        number.as_u64().and_then(|value| u32::try_from(value).ok())
                    }
                    Some(JsonValue::Null) | None => None,
                    Some(_) => None,
                };
                let execution_path = indexer_execution_path(
                    raw_height,
                    stack_height,
                    &mut path_stack,
                    &mut previous_indexer_height,
                )?;
                let (parent_order, provenance) = match raw_height {
                    Some(JsonValue::Number(_))
                        if stack_height.is_some_and(|height| {
                            height >= 2
                                && usize::try_from(height).is_ok_and(|height| {
                                    height <= INDEXER_MAX_INSTRUCTION_STACK_DEPTH
                                })
                        }) =>
                    {
                        let height = stack_height.expect("guard requires a valid stack height");
                        active_frames.retain(|active_height, _| *active_height < height);
                        match active_frames.get(&height.saturating_sub(1)).copied() {
                            Some((parent, true)) => {
                                active_frames.insert(height, (order, true));
                                (Some(parent), CpiFrameProvenanceV1::Complete)
                            }
                            _ => {
                                active_frames.insert(height, (order, false));
                                (None, CpiFrameProvenanceV1::MissingParentFrame)
                            }
                        }
                    }
                    Some(value) if !value.is_null() => {
                        active_frames.retain(|height, _| *height == 1);
                        (None, CpiFrameProvenanceV1::InvalidStackHeight)
                    }
                    _ => {
                        active_frames.retain(|height, _| *height == 1);
                        (None, CpiFrameProvenanceV1::MissingStackHeight)
                    }
                };
                let frame = execution_frame(
                    raw_inner,
                    &account_keys,
                    &target_program_text,
                    order,
                    stack_height,
                    parent_order,
                    execution_path,
                    provenance,
                )?;
                record_expected_state_changing(
                    &frame,
                    &mut expected_state_changing_instruction_count,
                    &mut unclassified_target_instruction_count,
                )?;
                frames.push(frame);
                raw_instructions.push((order, raw_inner));
            }
        }
    }

    let token_balances = token_account_balances(transaction, &account_keys)?;
    let account_key_strings = account_keys
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let (token_cpis, token_cpi_error) = match parse_in_scope_token_cpis(
        &frames,
        &raw_instructions,
        &account_key_strings,
        &token_balances,
    ) {
        Ok(token_cpis) => (token_cpis, None),
        Err(error) => (Vec::new(), Some(error)),
    };

    Ok(TransactionInspectionV1 {
        expected_state_changing_instruction_count,
        unclassified_target_instruction_count,
        provenance: TransactionExecutionProvenanceV1 {
            schema_version: DECODE_SCHEMA_VERSION,
            decoder_version_id: revision.decoder_version_id.clone(),
            signature: transaction.signature.clone(),
            slot: block.slot,
            transaction_index: transaction.transaction_index,
            frames,
            token_cpis,
        },
        token_cpi_error,
    })
}

fn parse_in_scope_token_cpis(
    frames: &[InstructionExecutionFrameV1],
    raw_instructions: &[(ChainOrder, &JsonValue)],
    account_keys: &[String],
    token_balances: &[TokenAccountBalanceV1],
) -> Result<Vec<ParsedTokenCpiV1>, TokenCpiError> {
    if !frames.iter().any(|frame| {
        frame.is_target_program
            && frame.decoded_target_class == Some(DecodedInstructionClass::StateChanging)
    }) {
        return Ok(Vec::new());
    }
    let frames_by_order =
        structurally_valid_frame_map(frames).ok_or(TokenCpiError::InvalidCompiledInstruction)?;
    let mut raw_by_order = BTreeMap::new();
    for (order, raw) in raw_instructions {
        if raw_by_order.insert(*order, *raw).is_some() {
            return Err(TokenCpiError::InvalidCompiledInstruction);
        }
    }
    if raw_by_order.len() != frames_by_order.len()
        || frames_by_order
            .keys()
            .any(|order| !raw_by_order.contains_key(order))
    {
        return Err(TokenCpiError::InvalidCompiledInstruction);
    }

    let mut parsed_by_order = BTreeMap::<ChainOrder, ParsedTokenCpiV1>::new();
    let mut deferred_by_order = BTreeMap::<ChainOrder, DeferredTokenAccountAdjustmentCpiV1>::new();
    let mut unresolved_external_touches = Vec::<(ChainOrder, Vec<String>)>::new();
    for frame in frames_by_order.values().filter(|frame| {
        frame.program_id == LEGACY_TOKEN_PROGRAM_ID || frame.program_id == TOKEN_2022_PROGRAM_ID
    }) {
        let owner = nearest_state_changing_target_ancestor(frame.order, &frames_by_order)
            .map_err(|()| TokenCpiError::MalformedFrameAncestry(frame.order))?;
        let raw = raw_by_order
            .get(&frame.order)
            .ok_or(TokenCpiError::InvalidCompiledInstruction)?;
        match parse_compiled_token_instruction_v1(frame.order, raw, account_keys) {
            Ok(parsed) => {
                let expected_program = if frame.program_id == LEGACY_TOKEN_PROGRAM_ID {
                    TokenProgramKindV1::LegacyToken
                } else {
                    TokenProgramKindV1::Token2022
                };
                if parsed.token_program() != expected_program {
                    return Err(TokenCpiError::ParsedProgramConflict(frame.order));
                }
                parsed_by_order.insert(frame.order, parsed);
            }
            Err(original_error) => {
                match parse_deferred_token_account_adjustment_v1(frame.order, raw, account_keys) {
                    Ok(deferred) => {
                        deferred_by_order.insert(frame.order, deferred);
                    }
                    Err(_) if owner.is_some() => return Err(original_error),
                    Err(_) => unresolved_external_touches
                        .push((frame.order, compiled_account_keys(raw, account_keys)?)),
                }
            }
        }
    }

    // Canonical ATA creation is the only external token setup accepted as a
    // zero native-initialization touch. Bare native initialization remains
    // deferred and must instead close through the RPC balance identity below.
    let mut certified_external_zeroes =
        BTreeMap::<String, Vec<CertifiedZeroNativeInitializationV1>>::new();
    for (order, candidate) in deferred_by_order.clone() {
        let Some(parent_order) = certified_associated_token_parent(
            &candidate,
            &frames_by_order,
            &raw_by_order,
            account_keys,
        )?
        else {
            continue;
        };
        let adjustment = ExactTokenAccountAdjustmentV1 {
            reason: TokenAccountAdjustmentReasonV1::NativeAccountInitialization,
            source: TokenAccountAdjustmentSourceV1::AssociatedTokenAccountCreation { parent_order },
            token_account: candidate.token_account.clone(),
            mint: candidate.mint.clone(),
            decimals: 9,
            signed_amount: SignedTokenAmount::zero(),
        };
        let owner = nearest_state_changing_target_ancestor(order, &frames_by_order)
            .map_err(|()| TokenCpiError::MalformedFrameAncestry(order))?;
        if owner.is_none() {
            certified_external_zeroes
                .entry(candidate.token_account.clone())
                .or_default()
                .push(CertifiedZeroNativeInitializationV1 {
                    order,
                    associated_token_parent_order: parent_order,
                    raw_data_sha256: candidate.raw_data_sha256.clone(),
                });
        }
        parsed_by_order.insert(
            order,
            resolve_deferred_token_account_adjustment_v1(candidate, adjustment)?,
        );
        deferred_by_order.remove(&order);
    }

    let mut known_deltas = BTreeMap::<String, i128>::new();
    let mut unknown_effect_accounts = BTreeSet::<String>::new();
    for parsed in parsed_by_order.values() {
        record_exact_token_effects(parsed, &mut known_deltas, &mut unknown_effect_accounts)?;
    }
    for (_order, accounts) in &unresolved_external_touches {
        unknown_effect_accounts.extend(accounts.iter().cloned());
    }

    for (order, candidate) in deferred_by_order.clone() {
        let Some(owner) = nearest_state_changing_target_ancestor(order, &frames_by_order)
            .map_err(|()| TokenCpiError::MalformedFrameAncestry(order))?
        else {
            continue;
        };
        let unresolved_on_account = deferred_by_order
            .values()
            .filter(|item| item.token_account == candidate.token_account)
            .count();
        if unresolved_on_account != 1
            || unknown_effect_accounts.contains(&candidate.token_account)
            || sibling_state_changing_target_touches_account(
                owner,
                &candidate.token_account,
                &frames_by_order,
                &raw_by_order,
                account_keys,
            )?
            || has_uncertified_non_target_outer_touch(
                &candidate.token_account,
                &frames_by_order,
                &raw_by_order,
                account_keys,
                certified_external_zeroes
                    .get(&candidate.token_account)
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
            )?
        {
            return Err(TokenCpiError::RequiresExternalCustodyState {
                program: candidate.token_program,
                discriminant: candidate.top_level_discriminant,
            });
        }
        let mut balance_matches = token_balances
            .iter()
            .filter(|balance| balance.account == candidate.token_account);
        let balance =
            balance_matches
                .next()
                .ok_or(TokenCpiError::RequiresExternalCustodyState {
                    program: candidate.token_program,
                    discriminant: candidate.top_level_discriminant,
                })?;
        if balance_matches.next().is_some()
            || balance.token_program != candidate.token_program
            || balance.mint != candidate.mint
            || balance.decimals != 9
        {
            return Err(TokenCpiError::RequiresExternalCustodyState {
                program: candidate.token_program,
                discriminant: candidate.top_level_discriminant,
            });
        }
        let pre = token_amount_i128(&balance.pre_amount, order)?;
        let post = token_amount_i128(&balance.post_amount, order)?;
        let other_exact_delta = known_deltas
            .get(&candidate.token_account)
            .copied()
            .unwrap_or_default();
        let residual = post
            .checked_sub(pre)
            .and_then(|net| net.checked_sub(other_exact_delta))
            .ok_or(TokenCpiError::InvalidAccountAdjustment(order))?;
        if (candidate.reason == TokenAccountAdjustmentReasonV1::NativeAccountInitialization
            && residual != 0)
            || (candidate.reason == TokenAccountAdjustmentReasonV1::SyncNative && residual < 0)
        {
            return Err(TokenCpiError::RequiresExternalCustodyState {
                program: candidate.token_program,
                discriminant: candidate.top_level_discriminant,
            });
        }
        let adjustment = ExactTokenAccountAdjustmentV1 {
            reason: candidate.reason,
            source: TokenAccountAdjustmentSourceV1::RpcTokenBalanceResidual {
                transaction_pre_amount: balance.pre_amount.clone(),
                transaction_post_amount: balance.post_amount.clone(),
                other_exact_delta: SignedTokenAmount::new(other_exact_delta.to_string())
                    .map_err(|_| TokenCpiError::InvalidAccountAdjustment(order))?,
                certified_zero_external_cpis: certified_external_zeroes
                    .get(&candidate.token_account)
                    .cloned()
                    .unwrap_or_default(),
            },
            token_account: candidate.token_account.clone(),
            mint: candidate.mint.clone(),
            decimals: balance.decimals,
            signed_amount: SignedTokenAmount::new(residual.to_string())
                .map_err(|_| TokenCpiError::InvalidAccountAdjustment(order))?,
        };
        parsed_by_order.insert(
            order,
            resolve_deferred_token_account_adjustment_v1(candidate, adjustment)?,
        );
        deferred_by_order.remove(&order);
    }

    if let Some(candidate) = deferred_by_order.values().find(|candidate| {
        nearest_state_changing_target_ancestor(candidate.order, &frames_by_order)
            .ok()
            .flatten()
            .is_some()
    }) {
        return Err(TokenCpiError::RequiresExternalCustodyState {
            program: candidate.token_program,
            discriminant: candidate.top_level_discriminant,
        });
    }

    let mut token_cpis = Vec::new();
    for (order, parsed) in parsed_by_order {
        if nearest_state_changing_target_ancestor(order, &frames_by_order)
            .map_err(|()| TokenCpiError::MalformedFrameAncestry(order))?
            .is_some()
        {
            token_cpis.push(parsed);
        }
    }
    token_cpis.sort_by_key(ParsedTokenCpiV1::order);
    Ok(token_cpis)
}

fn compiled_account_keys(
    raw: &JsonValue,
    account_keys: &[String],
) -> Result<Vec<String>, TokenCpiError> {
    raw.get("accounts")
        .and_then(JsonValue::as_array)
        .ok_or(TokenCpiError::InvalidCompiledField("accounts"))?
        .iter()
        .map(|value| {
            let index = value
                .as_u64()
                .ok_or(TokenCpiError::InvalidCompiledField("accounts"))?;
            let index = usize::try_from(index)
                .map_err(|_| TokenCpiError::AccountIndexOutOfBounds(index))?;
            account_keys
                .get(index)
                .cloned()
                .ok_or(TokenCpiError::AccountIndexOutOfBounds(index as u64))
        })
        .collect()
}

fn certified_associated_token_parent(
    candidate: &DeferredTokenAccountAdjustmentCpiV1,
    frames: &BTreeMap<ChainOrder, &InstructionExecutionFrameV1>,
    raw_by_order: &BTreeMap<ChainOrder, &JsonValue>,
    account_keys: &[String],
) -> Result<Option<ChainOrder>, TokenCpiError> {
    if candidate.reason != TokenAccountAdjustmentReasonV1::NativeAccountInitialization
        || candidate.top_level_discriminant != 18
    {
        return Ok(None);
    }
    let frame = frames
        .get(&candidate.order)
        .ok_or(TokenCpiError::MalformedFrameAncestry(candidate.order))?;
    let Some(parent_order) = frame.parent_order else {
        return Ok(None);
    };
    let parent = frames
        .get(&parent_order)
        .ok_or(TokenCpiError::MalformedFrameAncestry(candidate.order))?;
    if parent.program_id != ASSOCIATED_TOKEN_PROGRAM_ID {
        return Ok(None);
    }
    let parent_raw = raw_by_order
        .get(&parent_order)
        .ok_or(TokenCpiError::InvalidCompiledInstruction)?;
    let parent_accounts = compiled_account_keys(parent_raw, account_keys)?;
    let canonical_roles = parent_accounts.len() >= 6
        && parent_accounts[1] == candidate.token_account
        && parent_accounts[3] == candidate.mint
        && parent_accounts[4] == SYSTEM_PROGRAM_ID
        && parent_accounts[5] == LEGACY_TOKEN_PROGRAM_ID;
    Ok(canonical_roles.then_some(parent_order))
}

fn token_amount_i128(amount: &TokenAmount, order: ChainOrder) -> Result<i128, TokenCpiError> {
    amount
        .as_str()
        .parse::<u64>()
        .map(i128::from)
        .map_err(|_| TokenCpiError::InvalidAccountAdjustment(order))
}

fn accumulate_known_delta(
    output: &mut BTreeMap<String, i128>,
    account: &str,
    delta: i128,
    order: ChainOrder,
) -> Result<(), TokenCpiError> {
    let updated = output
        .get(account)
        .copied()
        .unwrap_or_default()
        .checked_add(delta)
        .ok_or(TokenCpiError::InvalidAccountAdjustment(order))?;
    output.insert(account.to_owned(), updated);
    Ok(())
}

fn record_exact_token_effects(
    parsed: &ParsedTokenCpiV1,
    known_deltas: &mut BTreeMap<String, i128>,
    unknown_effect_accounts: &mut BTreeSet<String>,
) -> Result<(), TokenCpiError> {
    let order = parsed.order();
    match parsed {
        ParsedTokenCpiV1::Transfer { evidence } => {
            let gross = token_amount_i128(&evidence.gross_amount, order)?;
            accumulate_known_delta(known_deltas, &evidence.source_token_account, -gross, order)?;
            let destination_credit = match evidence.variant {
                TokenTransferVariantV1::LegacyTransfer
                | TokenTransferVariantV1::LegacyTransferChecked => Some(gross),
                TokenTransferVariantV1::Token2022TransferCheckedWithFee => {
                    let fee = evidence
                        .explicit_fee
                        .as_ref()
                        .ok_or(TokenCpiError::InvalidAccountAdjustment(order))
                        .and_then(|fee| token_amount_i128(fee, order))?;
                    gross.checked_sub(fee)
                }
                TokenTransferVariantV1::Token2022TransferChecked => None,
            };
            if let Some(destination_credit) = destination_credit {
                if destination_credit < 0 {
                    return Err(TokenCpiError::InvalidAccountAdjustment(order));
                }
                accumulate_known_delta(
                    known_deltas,
                    &evidence.destination_token_account,
                    destination_credit,
                    order,
                )?;
            } else {
                unknown_effect_accounts.insert(evidence.destination_token_account.clone());
            }
        }
        ParsedTokenCpiV1::SupplyChange { evidence } => {
            let magnitude = token_amount_i128(&evidence.amount, order)?;
            let signed = match evidence.kind {
                TokenSupplyChangeKindV1::Mint => magnitude,
                TokenSupplyChangeKindV1::Burn => -magnitude,
            };
            accumulate_known_delta(known_deltas, &evidence.token_account, signed, order)?;
        }
        ParsedTokenCpiV1::RecognizedNonTransfer { evidence } => {
            if let Some(adjustment) = &evidence.exact_account_adjustment {
                let signed = adjustment
                    .signed_amount
                    .as_str()
                    .parse::<i128>()
                    .map_err(|_| TokenCpiError::InvalidAccountAdjustment(order))?;
                accumulate_known_delta(known_deltas, &adjustment.token_account, signed, order)?;
            }
        }
    }
    Ok(())
}

fn sibling_state_changing_target_touches_account(
    owner: ChainOrder,
    account: &str,
    frames: &BTreeMap<ChainOrder, &InstructionExecutionFrameV1>,
    raw_by_order: &BTreeMap<ChainOrder, &JsonValue>,
    account_keys: &[String],
) -> Result<bool, TokenCpiError> {
    for frame in frames.values().filter(|frame| {
        frame.order != owner
            && frame.is_target_program
            && frame.decoded_target_class == Some(DecodedInstructionClass::StateChanging)
    }) {
        let raw = raw_by_order
            .get(&frame.order)
            .ok_or(TokenCpiError::InvalidCompiledInstruction)?;
        if compiled_account_keys(raw, account_keys)?
            .iter()
            .any(|candidate| candidate == account)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn has_uncertified_non_target_outer_touch(
    account: &str,
    frames: &BTreeMap<ChainOrder, &InstructionExecutionFrameV1>,
    raw_by_order: &BTreeMap<ChainOrder, &JsonValue>,
    account_keys: &[String],
    certified_zeroes: &[CertifiedZeroNativeInitializationV1],
) -> Result<bool, TokenCpiError> {
    for frame in frames
        .values()
        .filter(|frame| frame.order.inner_execution_index.is_none() && !frame.is_target_program)
    {
        let raw = raw_by_order
            .get(&frame.order)
            .ok_or(TokenCpiError::InvalidCompiledInstruction)?;
        if !compiled_account_keys(raw, account_keys)?
            .iter()
            .any(|candidate| candidate == account)
        {
            continue;
        }
        let certified_ata_touch = frame.program_id == ASSOCIATED_TOKEN_PROGRAM_ID
            && certified_zeroes
                .iter()
                .any(|zero| zero.associated_token_parent_order == frame.order);
        if !certified_ata_touch {
            return Ok(true);
        }
    }
    Ok(false)
}

fn inner_instruction_groups<'a>(
    transaction: &'a ArchivedTransaction,
    outer_instruction_count: usize,
) -> Result<BTreeMap<u32, &'a [JsonValue]>, DecodeError> {
    let Some(raw_groups) = transaction.meta.get("innerInstructions") else {
        return Ok(BTreeMap::new());
    };
    if raw_groups.is_null() {
        return Ok(BTreeMap::new());
    }
    let groups = raw_groups
        .as_array()
        .ok_or(DecodeError::InvalidTransactionShape)?;
    let mut output = BTreeMap::new();
    for group in groups {
        let index = group
            .get("index")
            .and_then(JsonValue::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or(DecodeError::IndexOverflow)?;
        if usize::try_from(index)
            .ok()
            .is_none_or(|value| value >= outer_instruction_count)
        {
            return Err(DecodeError::InvalidTransactionShape);
        }
        let instructions = group
            .get("instructions")
            .and_then(JsonValue::as_array)
            .ok_or(DecodeError::InvalidTransactionShape)?;
        if output.insert(index, instructions.as_slice()).is_some() {
            return Err(DecodeError::InvalidTransactionShape);
        }
    }
    Ok(output)
}

#[allow(clippy::too_many_arguments)]
fn execution_frame(
    raw: &JsonValue,
    account_keys: &[Pubkey],
    target_program: &str,
    order: ChainOrder,
    stack_height: Option<u32>,
    parent_order: Option<ChainOrder>,
    execution_path: Vec<u32>,
    provenance: CpiFrameProvenanceV1,
) -> Result<InstructionExecutionFrameV1, DecodeError> {
    let (program_id, compiled) = instruction_program_id(raw, account_keys)?;
    let is_target_program = program_id == target_program;
    let decoded_target_class = if is_target_program && compiled {
        classify_target_instruction(raw, &program_id)
    } else {
        None
    };
    Ok(InstructionExecutionFrameV1 {
        schema_version: DECODE_SCHEMA_VERSION,
        order,
        program_id,
        is_target_program,
        stack_height,
        parent_order,
        execution_path,
        provenance,
        decoded_target_class,
    })
}

fn indexer_execution_path(
    raw_height: Option<&JsonValue>,
    stack_height: Option<u32>,
    path_stack: &mut [u32; INDEXER_MAX_INSTRUCTION_STACK_DEPTH],
    previous_height: &mut usize,
) -> Result<Vec<u32>, DecodeError> {
    // This deliberately mirrors Carbon's `absolute_path` convention, including
    // its historical `None => 1` fallback. Provenance remains marked missing,
    // so the fallback path can be reconciled to DB evidence but cannot prove a
    // CPI parent relationship.
    let height = match raw_height {
        Some(JsonValue::Number(_)) => stack_height.and_then(|height| usize::try_from(height).ok()),
        Some(JsonValue::Null) | None => Some(1),
        Some(_) => None,
    };
    let Some(height) =
        height.filter(|height| *height > 0 && *height <= INDEXER_MAX_INSTRUCTION_STACK_DEPTH)
    else {
        return Ok(Vec::new());
    };
    if height > *previous_height {
        path_stack[height - 1] = 0;
    } else {
        path_stack[height - 1] = path_stack[height - 1]
            .checked_add(1)
            .ok_or(DecodeError::IndexOverflow)?;
    }
    *previous_height = height;
    Ok(path_stack[..height].to_vec())
}

fn instruction_program_id(
    raw: &JsonValue,
    account_keys: &[Pubkey],
) -> Result<(String, bool), DecodeError> {
    if let Some(index) = raw.get("programIdIndex") {
        let index = index
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .ok_or(DecodeError::AccountIndex)?;
        return Ok((
            account_keys
                .get(index)
                .ok_or(DecodeError::AccountIndex)?
                .to_string(),
            true,
        ));
    }
    raw.get("programId")
        .and_then(JsonValue::as_str)
        .map(|program_id| (program_id.to_owned(), false))
        .ok_or(DecodeError::InvalidTransactionShape)
}

fn classify_target_instruction(
    raw: &JsonValue,
    target_program: &str,
) -> Option<DecodedInstructionClass> {
    let data = raw
        .get("data")
        .and_then(JsonValue::as_str)
        .and_then(|value| bs58::decode(value).into_vec().ok())?;
    if decode_anchor_idl_metadata_v1(&data)
        .ok()
        .flatten()
        .is_some()
    {
        return Some(DecodedInstructionClass::ProgramMetadata);
    }
    if data.len() == SWAP_EVENT_LEGACY_V0_WIRE_LEN && data.starts_with(&SWAP_EVENT_DISCRIMINATOR) {
        return Some(DecodedInstructionClass::EmittedAssertion);
    }
    if is_legacy_position_event(&data) || is_legacy_lp_position_event(&data) {
        return Some(DecodedInstructionClass::EmittedAssertion);
    }
    let instruction = Instruction {
        program_id: Pubkey::from_str(target_program).ok()?,
        accounts: Vec::new(),
        data,
    };
    OmnipairDecoder
        .decode_instruction(&instruction)
        .map(|decoded| instruction_kind_and_class(&decoded.data).1)
}

fn record_expected_state_changing(
    frame: &InstructionExecutionFrameV1,
    expected: &mut u32,
    unclassified: &mut u32,
) -> Result<(), DecodeError> {
    if !frame.is_target_program {
        return Ok(());
    }
    match frame.decoded_target_class {
        Some(DecodedInstructionClass::StateChanging) => {
            *expected = expected.checked_add(1).ok_or(DecodeError::IndexOverflow)?;
        }
        Some(
            DecodedInstructionClass::ReadOnlyView
            | DecodedInstructionClass::EmittedAssertion
            | DecodedInstructionClass::ProgramMetadata,
        ) => {}
        None => {
            *unclassified = unclassified
                .checked_add(1)
                .ok_or(DecodeError::IndexOverflow)?;
        }
    }
    Ok(())
}

fn decode_transaction(
    block: &ArchivedBlock,
    block_time: i64,
    transaction: &ArchivedTransaction,
    target_program: Pubkey,
    revision: &DecoderRevisionV1,
) -> Result<DecodedTransactionV1, DecodeError> {
    let account_keys = account_keys(transaction)?;
    let token_balances = token_account_balances(transaction, &account_keys)?;
    let message = transaction
        .transaction
        .get("message")
        .ok_or(DecodeError::InvalidTransactionShape)?;
    let outer = message
        .get("instructions")
        .and_then(JsonValue::as_array)
        .ok_or(DecodeError::InvalidTransactionShape)?;
    let mut instructions = Vec::new();
    let mut unknown = Vec::new();
    let mut program_instruction_count = 0_u32;
    let mut non_target_outer_touched_accounts = BTreeSet::new();

    for (outer_index, raw) in outer.iter().enumerate() {
        record_non_target_outer_accounts(
            raw,
            &account_keys,
            target_program,
            &mut non_target_outer_touched_accounts,
        )?;
        decode_one(
            raw,
            &account_keys,
            target_program,
            revision,
            transaction,
            block.slot,
            block_time,
            u32::try_from(outer_index).map_err(|_| DecodeError::IndexOverflow)?,
            None,
            &mut program_instruction_count,
            &mut instructions,
            &mut unknown,
        )?;

        if let Some(groups) = transaction
            .meta
            .get("innerInstructions")
            .and_then(JsonValue::as_array)
        {
            for group in groups.iter().filter(|group| {
                group.get("index").and_then(JsonValue::as_u64) == Some(outer_index as u64)
            }) {
                let inner = group
                    .get("instructions")
                    .and_then(JsonValue::as_array)
                    .ok_or(DecodeError::InvalidTransactionShape)?;
                for (inner_index, raw_inner) in inner.iter().enumerate() {
                    decode_one(
                        raw_inner,
                        &account_keys,
                        target_program,
                        revision,
                        transaction,
                        block.slot,
                        block_time,
                        u32::try_from(outer_index).map_err(|_| DecodeError::IndexOverflow)?,
                        Some(u32::try_from(inner_index).map_err(|_| DecodeError::IndexOverflow)?),
                        &mut program_instruction_count,
                        &mut instructions,
                        &mut unknown,
                    )?;
                }
            }
        }
    }
    let state_changing_instruction_count = u32::try_from(
        instructions
            .iter()
            .filter(|instruction| instruction.class == DecodedInstructionClass::StateChanging)
            .count(),
    )
    .map_err(|_| DecodeError::IndexOverflow)?;
    let emitted_assertion_count = u32::try_from(
        instructions
            .iter()
            .filter(|instruction| instruction.class == DecodedInstructionClass::EmittedAssertion)
            .count(),
    )
    .map_err(|_| DecodeError::IndexOverflow)?;
    Ok(DecodedTransactionV1 {
        schema_version: DECODE_SCHEMA_VERSION,
        decoder_version_id: revision.decoder_version_id.clone(),
        signature: transaction.signature.clone(),
        rpc_payload_sha256: transaction.rpc_payload_sha256.clone(),
        slot: block.slot,
        transaction_index: transaction.transaction_index,
        block_time_unix_seconds: block_time,
        program_instruction_count,
        state_changing_instruction_count,
        emitted_assertion_count,
        unknown_program_instruction_hashes: unknown,
        non_target_outer_touched_accounts: non_target_outer_touched_accounts.into_iter().collect(),
        token_balances,
        instructions,
    })
}

fn record_non_target_outer_accounts(
    raw: &JsonValue,
    account_keys: &[Pubkey],
    target_program: Pubkey,
    output: &mut BTreeSet<String>,
) -> Result<(), DecodeError> {
    if let Some(program_index) = raw.get("programIdIndex").and_then(JsonValue::as_u64) {
        let program = account_keys
            .get(usize::try_from(program_index).map_err(|_| DecodeError::IndexOverflow)?)
            .ok_or(DecodeError::AccountIndex)?;
        if *program == target_program {
            return Ok(());
        }
        let accounts = raw
            .get("accounts")
            .and_then(JsonValue::as_array)
            .ok_or(DecodeError::InvalidTransactionShape)?;
        for index in accounts {
            let index = index
                .as_u64()
                .and_then(|value| usize::try_from(value).ok())
                .ok_or(DecodeError::AccountIndex)?;
            output.insert(
                account_keys
                    .get(index)
                    .ok_or(DecodeError::AccountIndex)?
                    .to_string(),
            );
        }
        return Ok(());
    }
    let Some(program) = raw.get("programId").and_then(JsonValue::as_str) else {
        return Err(DecodeError::InvalidTransactionShape);
    };
    if program == target_program.to_string() {
        return Ok(());
    }
    if let Some(accounts) = raw.get("accounts").and_then(JsonValue::as_array) {
        for account in accounts {
            if let Some(account) = account
                .as_str()
                .or_else(|| account.get("pubkey").and_then(JsonValue::as_str))
            {
                output.insert(account.to_owned());
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn decode_one(
    raw: &JsonValue,
    account_keys: &[Pubkey],
    target_program: Pubkey,
    revision: &DecoderRevisionV1,
    transaction: &ArchivedTransaction,
    slot: u64,
    block_time: i64,
    outer_instruction_index: u32,
    inner_execution_index: Option<u32>,
    program_instruction_count: &mut u32,
    output: &mut Vec<DecodedInstructionV1>,
    unknown: &mut Vec<String>,
) -> Result<(), DecodeError> {
    let Some(program_index) = raw.get("programIdIndex").and_then(JsonValue::as_u64) else {
        // Parsed instructions are only accepted as irrelevant. The V1 program
        // itself must remain raw so instruction bytes can be hashed and decoded.
        if raw.get("programId").and_then(JsonValue::as_str)
            == Some(target_program.to_string().as_str())
        {
            return Err(DecodeError::ParsedTargetInstruction);
        }
        return Ok(());
    };
    let program_index = usize::try_from(program_index).map_err(|_| DecodeError::IndexOverflow)?;
    let program = *account_keys
        .get(program_index)
        .ok_or(DecodeError::AccountIndex)?;
    if program != target_program {
        return Ok(());
    }
    *program_instruction_count = program_instruction_count
        .checked_add(1)
        .ok_or(DecodeError::IndexOverflow)?;
    let data_text = raw
        .get("data")
        .and_then(JsonValue::as_str)
        .ok_or(DecodeError::InvalidTransactionShape)?;
    let data = bs58::decode(data_text)
        .into_vec()
        .map_err(|_| DecodeError::InvalidBase58)?;
    let account_indices = raw
        .get("accounts")
        .and_then(JsonValue::as_array)
        .ok_or(DecodeError::InvalidTransactionShape)?;
    let mut accounts = Vec::with_capacity(account_indices.len());
    let mut metas = Vec::with_capacity(account_indices.len());
    for index in account_indices {
        let index = index
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .ok_or(DecodeError::AccountIndex)?;
        let key = *account_keys.get(index).ok_or(DecodeError::AccountIndex)?;
        accounts.push(key.to_string());
        metas.push(AccountMeta::new_readonly(key, false));
    }
    let instruction = Instruction {
        program_id: target_program,
        accounts: metas,
        data: data.clone(),
    };
    let decoder = OmnipairDecoder;
    let order = ChainOrder {
        slot,
        transaction_index: transaction.transaction_index,
        outer_instruction_index,
        inner_execution_index,
        event_sequence: 0,
    };
    let decoded = if let Some((kind, decoded_data)) = decode_anchor_idl_metadata_v1(&data)? {
        Some((kind, DecodedInstructionClass::ProgramMetadata, decoded_data))
    } else if let Some(decoded_data) = decode_swap_event_legacy_v0(&data)? {
        Some((
            "swap_event_legacy_v0",
            DecodedInstructionClass::EmittedAssertion,
            decoded_data,
        ))
    } else if let Some(decoded_data) = decode_user_position_updated_event_legacy_v0(&data)? {
        Some((
            "user_position_updated_event_legacy_v0",
            DecodedInstructionClass::EmittedAssertion,
            decoded_data,
        ))
    } else if let Some(decoded_data) =
        decode_user_liquidity_position_updated_event_legacy_v0(&data)?
    {
        Some((
            "user_liquidity_position_updated_event_legacy_v0",
            DecodedInstructionClass::EmittedAssertion,
            decoded_data,
        ))
    } else if let Some(decoded) = decoder.decode_instruction(&instruction) {
        let (kind, class) = instruction_kind_and_class(&decoded.data);
        let decoded_data =
            serde_json::to_value(decoded.data).map_err(|_| DecodeError::DecodedSerialization)?;
        Some((kind, class, decoded_data))
    } else {
        None
    };
    if let Some((kind, class, mut decoded_data)) = decoded {
        normalize_schema_pubkeys(kind, class, &mut decoded_data)?;
        stringify_decoded_json_numbers(kind, &mut decoded_data)?;
        output.push(DecodedInstructionV1 {
            schema_version: DECODE_SCHEMA_VERSION,
            decoder_version_id: revision.decoder_version_id.clone(),
            signature: transaction.signature.clone(),
            order,
            block_time_unix_seconds: block_time,
            class,
            kind: kind.to_owned(),
            data: decoded_data,
            accounts,
            raw_data_sha256: sha256_hex(&data),
            stack_height: raw
                .get("stackHeight")
                .and_then(JsonValue::as_u64)
                .and_then(|value| u32::try_from(value).ok()),
        });
    } else {
        unknown.push(sha256_hex(&data));
    }
    Ok(())
}

fn decode_anchor_idl_metadata_v1(
    data: &[u8],
) -> Result<Option<(&'static str, JsonValue)>, DecodeError> {
    if !data.starts_with(&ANCHOR_IDL_INSTRUCTION_PREFIX) {
        return Ok(None);
    }
    let tag = *data
        .get(ANCHOR_IDL_INSTRUCTION_PREFIX.len())
        .ok_or(DecodeError::DecodedSerialization)?;
    let body = &data[ANCHOR_IDL_INSTRUCTION_PREFIX.len() + 1..];
    let (kind, fields) = match tag {
        0 if body.len() == 8 => (
            "anchor_idl_create_account",
            serde_json::json!({"data_len": u64::from_le_bytes(body.try_into().map_err(|_| DecodeError::DecodedSerialization)?)}),
        ),
        1 if body.is_empty() => ("anchor_idl_create_buffer", serde_json::json!({})),
        2 if body.len() >= 4 => {
            let declared = u32::from_le_bytes(
                body[..4]
                    .try_into()
                    .map_err(|_| DecodeError::DecodedSerialization)?,
            );
            let payload = &body[4..];
            if usize::try_from(declared).ok() != Some(payload.len()) {
                return Err(DecodeError::DecodedSerialization);
            }
            (
                "anchor_idl_write",
                serde_json::json!({
                    "payload_length": declared,
                    "payload_sha256": sha256_hex(payload),
                }),
            )
        }
        3 if body.is_empty() => ("anchor_idl_set_buffer", serde_json::json!({})),
        5 if body.is_empty() => ("anchor_idl_close_account", serde_json::json!({})),
        6 if body.len() == 8 => (
            "anchor_idl_resize_account",
            serde_json::json!({"data_len": u64::from_le_bytes(body.try_into().map_err(|_| DecodeError::DecodedSerialization)?)}),
        ),
        _ => return Ok(None),
    };
    Ok(Some((
        kind,
        serde_json::json!({"AnchorIdlMetadataV1": fields}),
    )))
}

fn is_legacy_position_event(data: &[u8]) -> bool {
    data.len() == USER_POSITION_UPDATED_EVENT_LEGACY_V0_WIRE_LEN
        && data.starts_with(&USER_POSITION_UPDATED_EVENT_LEGACY_V0_DISCRIMINATOR)
}

fn is_legacy_lp_position_event(data: &[u8]) -> bool {
    data.len() == USER_LIQUIDITY_POSITION_UPDATED_EVENT_LEGACY_V0_WIRE_LEN
        && data.starts_with(&USER_LIQUIDITY_POSITION_UPDATED_EVENT_LEGACY_V0_DISCRIMINATOR)
}

fn decode_user_position_updated_event_legacy_v0(
    data: &[u8],
) -> Result<Option<JsonValue>, DecodeError> {
    if !is_legacy_position_event(data) {
        return Ok(None);
    }
    let mut offset = USER_POSITION_UPDATED_EVENT_LEGACY_V0_DISCRIMINATOR.len();
    let position = Pubkey::new_from_array(read_wire_bytes::<32>(data, &mut offset)?);
    let collateral0 = read_wire_u64(data, &mut offset)?;
    let collateral1 = read_wire_u64(data, &mut offset)?;
    let debt0_shares = read_wire_u128(data, &mut offset)?;
    let debt1_shares = read_wire_u128(data, &mut offset)?;
    let collateral0_applied_min_cf_bps = read_wire_u16(data, &mut offset)?;
    let collateral1_applied_min_cf_bps = read_wire_u16(data, &mut offset)?;
    let signer = Pubkey::new_from_array(read_wire_bytes::<32>(data, &mut offset)?);
    let pair = Pubkey::new_from_array(read_wire_bytes::<32>(data, &mut offset)?);
    let metadata_slot = read_wire_u64(data, &mut offset)?;
    if offset != data.len() {
        return Err(DecodeError::DecodedSerialization);
    }
    Ok(Some(serde_json::json!({
        "UserPositionUpdatedEventLegacyV0": {
            "position": position.to_string(),
            "collateral0": collateral0,
            "collateral1": collateral1,
            "debt0_shares": debt0_shares.to_string(),
            "debt1_shares": debt1_shares.to_string(),
            "collateral0_applied_min_cf_bps": collateral0_applied_min_cf_bps,
            "collateral1_applied_min_cf_bps": collateral1_applied_min_cf_bps,
            "metadata": {
                "signer": signer.to_string(),
                "pair": pair.to_string(),
                "slot": metadata_slot,
            },
        }
    })))
}

fn decode_user_liquidity_position_updated_event_legacy_v0(
    data: &[u8],
) -> Result<Option<JsonValue>, DecodeError> {
    if !is_legacy_lp_position_event(data) {
        return Ok(None);
    }
    let mut offset = USER_LIQUIDITY_POSITION_UPDATED_EVENT_LEGACY_V0_DISCRIMINATOR.len();
    let token0_amount = read_wire_u64(data, &mut offset)?;
    let token1_amount = read_wire_u64(data, &mut offset)?;
    let lp_amount = read_wire_u64(data, &mut offset)?;
    let token0_mint = Pubkey::new_from_array(read_wire_bytes::<32>(data, &mut offset)?);
    let token1_mint = Pubkey::new_from_array(read_wire_bytes::<32>(data, &mut offset)?);
    let lp_mint = Pubkey::new_from_array(read_wire_bytes::<32>(data, &mut offset)?);
    let signer = Pubkey::new_from_array(read_wire_bytes::<32>(data, &mut offset)?);
    let pair = Pubkey::new_from_array(read_wire_bytes::<32>(data, &mut offset)?);
    let metadata_slot = read_wire_u64(data, &mut offset)?;
    if offset != data.len() {
        return Err(DecodeError::DecodedSerialization);
    }
    Ok(Some(serde_json::json!({
        "UserLiquidityPositionUpdatedEventLegacyV0": {
            "token0_amount": token0_amount,
            "token1_amount": token1_amount,
            "lp_amount": lp_amount,
            "token0_mint": token0_mint.to_string(),
            "token1_mint": token1_mint.to_string(),
            "lp_mint": lp_mint.to_string(),
            "metadata": {
                "signer": signer.to_string(),
                "pair": pair.to_string(),
                "slot": metadata_slot,
            },
        }
    })))
}

fn decode_swap_event_legacy_v0(data: &[u8]) -> Result<Option<JsonValue>, DecodeError> {
    if data.len() != SWAP_EVENT_LEGACY_V0_WIRE_LEN || !data.starts_with(&SWAP_EVENT_DISCRIMINATOR) {
        return Ok(None);
    }
    let mut offset = SWAP_EVENT_DISCRIMINATOR.len();
    let reserve0 = read_wire_u64(data, &mut offset)?;
    let reserve1 = read_wire_u64(data, &mut offset)?;
    let is_token0_in = match read_wire_bytes::<1>(data, &mut offset)?[0] {
        0 => false,
        1 => true,
        _ => return Err(DecodeError::DecodedSerialization),
    };
    let amount_in = read_wire_u64(data, &mut offset)?;
    let amount_out = read_wire_u64(data, &mut offset)?;
    let amount_in_after_fee = read_wire_u64(data, &mut offset)?;
    let signer = Pubkey::new_from_array(read_wire_bytes::<32>(data, &mut offset)?);
    let pair = Pubkey::new_from_array(read_wire_bytes::<32>(data, &mut offset)?);
    let metadata_slot = read_wire_u64(data, &mut offset)?;
    if offset != data.len() {
        return Err(DecodeError::DecodedSerialization);
    }
    Ok(Some(serde_json::json!({
        "SwapEventLegacyV0": {
            "reserve0": reserve0,
            "reserve1": reserve1,
            "is_token0_in": is_token0_in,
            "amount_in": amount_in,
            "amount_out": amount_out,
            "amount_in_after_fee": amount_in_after_fee,
            "metadata": {
                "signer": signer.to_string(),
                "pair": pair.to_string(),
                "slot": metadata_slot,
            },
        }
    })))
}

fn read_wire_u64(data: &[u8], offset: &mut usize) -> Result<u64, DecodeError> {
    Ok(u64::from_le_bytes(read_wire_bytes::<8>(data, offset)?))
}

fn read_wire_u16(data: &[u8], offset: &mut usize) -> Result<u16, DecodeError> {
    Ok(u16::from_le_bytes(read_wire_bytes::<2>(data, offset)?))
}

fn read_wire_u128(data: &[u8], offset: &mut usize) -> Result<u128, DecodeError> {
    Ok(u128::from_le_bytes(read_wire_bytes::<16>(data, offset)?))
}

fn read_wire_bytes<const N: usize>(
    data: &[u8],
    offset: &mut usize,
) -> Result<[u8; N], DecodeError> {
    let end = offset
        .checked_add(N)
        .ok_or(DecodeError::DecodedSerialization)?;
    let bytes = data
        .get(*offset..end)
        .ok_or(DecodeError::DecodedSerialization)?
        .try_into()
        .map_err(|_| DecodeError::DecodedSerialization)?;
    *offset = end;
    Ok(bytes)
}

/// Canonicalize only fields whose generated decoder schema declares a Pubkey.
/// A generic 32-byte conversion would corrupt non-key hashes such as
/// `PairCreatedEvent.params_hash`.
fn normalize_schema_pubkeys(
    kind: &str,
    class: DecodedInstructionClass,
    value: &mut JsonValue,
) -> Result<(), DecodeError> {
    match class {
        DecodedInstructionClass::EmittedAssertion => {
            normalize_assertion_schema_pubkeys(kind, value)
        }
        DecodedInstructionClass::StateChanging => {
            normalize_config_instruction_schema_pubkeys(kind, value)
        }
        DecodedInstructionClass::ReadOnlyView | DecodedInstructionClass::ProgramMetadata => Ok(()),
    }
}

fn normalize_assertion_schema_pubkeys(
    kind: &str,
    value: &mut JsonValue,
) -> Result<(), DecodeError> {
    let body = decoded_variant_body_mut(value)?;
    let metadata = body
        .get_mut("metadata")
        .and_then(JsonValue::as_object_mut)
        .ok_or(DecodeError::DecodedSerialization)?;
    normalize_pubkey_field(metadata, "signer")?;
    normalize_pubkey_field(metadata, "pair")?;

    let fields: &[&str] = match kind {
        "pair_created_event" => &["token0", "token1", "lp_mint", "rate_model"],
        "user_liquidity_position_updated_event"
        | "user_liquidity_position_updated_event_legacy_v0" => {
            &["token0_mint", "token1_mint", "lp_mint"]
        }
        "user_position_created_event"
        | "user_position_updated_event"
        | "user_position_updated_event_legacy_v0" => &["position"],
        "user_position_liquidated_event" => &["position", "liquidator"],
        "flashloan_event" => &["receiver"],
        "claim_protocol_fees_event" => &["token0", "token1"],
        "adjust_collateral_event"
        | "adjust_debt_event"
        | "adjust_liquidity_event"
        | "burn_event"
        | "mint_event"
        | "swap_event"
        | "swap_event_legacy_v0"
        | "update_pair_event" => &[],
        _ => return Err(DecodeError::DecodedSerialization),
    };
    for field in fields {
        normalize_pubkey_field(body, field)?;
    }
    Ok(())
}

/// Global configuration instructions are the only state-changing instruction
/// arguments copied into configuration history as public keys. Convert their
/// generated-decoder byte representation before recursive number
/// stringification so future canonical history contains base58 keys.
fn normalize_config_instruction_schema_pubkeys(
    kind: &str,
    value: &mut JsonValue,
) -> Result<(), DecodeError> {
    let fields: &[(&str, bool)] = match kind {
        "init_futarchy_authority" => &[
            ("authority", false),
            ("futarchy_treasury", false),
            ("buybacks_vault", false),
            ("team_treasury", false),
        ],
        "update_futarchy_authority" => &[("new_authority", false)],
        "update_revenue_recipients" => &[
            ("futarchy_treasury", true),
            ("buybacks_vault", true),
            ("team_treasury", true),
        ],
        _ => return Ok(()),
    };
    let args = decoded_variant_body_mut(value)?
        .get_mut("args")
        .and_then(JsonValue::as_object_mut)
        .ok_or(DecodeError::DecodedSerialization)?;
    for (field, optional) in fields {
        let value = args
            .get_mut(*field)
            .ok_or(DecodeError::DecodedSerialization)?;
        if *optional && value.is_null() {
            continue;
        }
        let pubkey = pubkey_from_schema_json(value)?;
        *value = JsonValue::String(pubkey.to_string());
    }
    Ok(())
}

fn decoded_variant_body_mut(
    value: &mut JsonValue,
) -> Result<&mut serde_json::Map<String, JsonValue>, DecodeError> {
    let variants = value
        .as_object_mut()
        .filter(|variants| variants.len() == 1)
        .ok_or(DecodeError::DecodedSerialization)?;
    variants
        .values_mut()
        .next()
        .and_then(JsonValue::as_object_mut)
        .ok_or(DecodeError::DecodedSerialization)
}

fn normalize_pubkey_field(
    fields: &mut serde_json::Map<String, JsonValue>,
    field: &str,
) -> Result<(), DecodeError> {
    let value = fields
        .get_mut(field)
        .ok_or(DecodeError::DecodedSerialization)?;
    let pubkey = pubkey_from_schema_json(value)?;
    *value = JsonValue::String(pubkey.to_string());
    Ok(())
}

fn pubkey_from_schema_json(value: &JsonValue) -> Result<Pubkey, DecodeError> {
    if let Some(text) = value.as_str() {
        return Pubkey::from_str(text).map_err(|_| DecodeError::DecodedSerialization);
    }
    exact_json_bytes::<32>(value)
        .map(Pubkey::new_from_array)
        .ok_or(DecodeError::DecodedSerialization)
}

fn exact_json_bytes<const N: usize>(value: &JsonValue) -> Option<[u8; N]> {
    let values = value.as_array()?;
    if values.len() != N {
        return None;
    }
    let mut bytes = [0_u8; N];
    for (output, value) in bytes.iter_mut().zip(values) {
        *output = u8::try_from(value.as_u64()?).ok()?;
    }
    Some(bytes)
}

fn stringify_decoded_json_numbers(kind: &str, value: &mut JsonValue) -> Result<(), DecodeError> {
    // This is the one schema-known byte array that must not be rewritten into
    // a base58 key or an array of decimal strings.
    let params_hash = if kind == "pair_created_event" {
        let params_hash = decoded_variant_body_mut(value)?
            .remove("params_hash")
            .ok_or(DecodeError::DecodedSerialization)?;
        exact_json_bytes::<32>(&params_hash).ok_or(DecodeError::DecodedSerialization)?;
        Some(params_hash)
    } else {
        None
    };
    stringify_json_numbers(value);
    if let Some(params_hash) = params_hash {
        decoded_variant_body_mut(value)?.insert("params_hash".to_owned(), params_hash);
    }
    Ok(())
}

/// Decoder artifacts are analysis inputs too. Converting every JSON number to
/// a canonical decimal string prevents u64/u128 token fields from being
/// rounded by JavaScript, notebooks, or Parquet conversion. Typed canonical
/// builders may parse non-amount configuration fields back to bounded ints.
fn stringify_json_numbers(value: &mut JsonValue) {
    match value {
        JsonValue::Number(number) => *value = JsonValue::String(number.to_string()),
        JsonValue::Array(items) => {
            for item in items {
                stringify_json_numbers(item);
            }
        }
        JsonValue::Object(fields) => {
            for field in fields.values_mut() {
                stringify_json_numbers(field);
            }
        }
        JsonValue::Null | JsonValue::Bool(_) | JsonValue::String(_) => {}
    }
}

fn instruction_kind_and_class(
    instruction: &OmnipairInstruction,
) -> (&'static str, DecodedInstructionClass) {
    use DecodedInstructionClass::{EmittedAssertion, ReadOnlyView, StateChanging};
    match instruction {
        OmnipairInstruction::AddCollateral(_) => ("add_collateral", StateChanging),
        OmnipairInstruction::AddLiquidity(_) => ("add_liquidity", StateChanging),
        OmnipairInstruction::Borrow(_) => ("borrow", StateChanging),
        OmnipairInstruction::ClaimProtocolFees(_) => ("claim_protocol_fees", StateChanging),
        OmnipairInstruction::Flashloan(_) => ("flashloan", StateChanging),
        OmnipairInstruction::InitFutarchyAuthority(_) => ("init_futarchy_authority", StateChanging),
        OmnipairInstruction::Initialize(_) => ("initialize", StateChanging),
        OmnipairInstruction::Liquidate(_) => ("liquidate", StateChanging),
        OmnipairInstruction::RemoveCollateral(_) => ("remove_collateral", StateChanging),
        OmnipairInstruction::RemoveLiquidity(_) => ("remove_liquidity", StateChanging),
        OmnipairInstruction::Repay(_) => ("repay", StateChanging),
        OmnipairInstruction::SetGlobalReduceOnly(_) => ("set_global_reduce_only", StateChanging),
        OmnipairInstruction::SetPairReduceOnly(_) => ("set_pair_reduce_only", StateChanging),
        OmnipairInstruction::Swap(_) => ("swap", StateChanging),
        OmnipairInstruction::UpdatePair(_) => ("update_pair", StateChanging),
        OmnipairInstruction::UpdateFutarchyAuthority(_) => {
            ("update_futarchy_authority", StateChanging)
        }
        OmnipairInstruction::UpdateProtocolRevenue(_) => ("update_protocol_revenue", StateChanging),
        OmnipairInstruction::UpdateRevenueRecipients(_) => {
            ("update_revenue_recipients", StateChanging)
        }
        OmnipairInstruction::ViewPairData(_) => ("view_pair_data", ReadOnlyView),
        OmnipairInstruction::ViewUserPositionData(_) => ("view_user_position_data", ReadOnlyView),
        OmnipairInstruction::AdjustCollateralEvent(_) => {
            ("adjust_collateral_event", EmittedAssertion)
        }
        OmnipairInstruction::AdjustDebtEvent(_) => ("adjust_debt_event", EmittedAssertion),
        OmnipairInstruction::AdjustLiquidityEvent(_) => {
            ("adjust_liquidity_event", EmittedAssertion)
        }
        OmnipairInstruction::BurnEvent(_) => ("burn_event", EmittedAssertion),
        OmnipairInstruction::ClaimProtocolFeesEvent(_) => {
            ("claim_protocol_fees_event", EmittedAssertion)
        }
        OmnipairInstruction::FlashloanEvent(_) => ("flashloan_event", EmittedAssertion),
        OmnipairInstruction::MintEvent(_) => ("mint_event", EmittedAssertion),
        OmnipairInstruction::PairCreatedEvent(_) => ("pair_created_event", EmittedAssertion),
        OmnipairInstruction::SwapEvent(_) => ("swap_event", EmittedAssertion),
        OmnipairInstruction::UpdatePairEvent(_) => ("update_pair_event", EmittedAssertion),
        OmnipairInstruction::UserLiquidityPositionUpdatedEvent(_) => {
            ("user_liquidity_position_updated_event", EmittedAssertion)
        }
        OmnipairInstruction::UserPositionCreatedEvent(_) => {
            ("user_position_created_event", EmittedAssertion)
        }
        OmnipairInstruction::UserPositionLiquidatedEvent(_) => {
            ("user_position_liquidated_event", EmittedAssertion)
        }
        OmnipairInstruction::UserPositionUpdatedEvent(_) => {
            ("user_position_updated_event", EmittedAssertion)
        }
    }
}

fn account_keys(transaction: &ArchivedTransaction) -> Result<Vec<Pubkey>, DecodeError> {
    let message = transaction
        .transaction
        .get("message")
        .ok_or(DecodeError::InvalidTransactionShape)?;
    let static_keys = message
        .get("accountKeys")
        .and_then(JsonValue::as_array)
        .ok_or(DecodeError::InvalidTransactionShape)?;
    let mut keys = static_keys
        .iter()
        .map(parse_account_key)
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(loaded) = transaction.meta.get("loadedAddresses") {
        for lane in ["writable", "readonly"] {
            if let Some(addresses) = loaded.get(lane).and_then(JsonValue::as_array) {
                keys.extend(
                    addresses
                        .iter()
                        .map(parse_account_key)
                        .collect::<Result<Vec<_>, _>>()?,
                );
            }
        }
    }
    Ok(keys)
}

#[derive(Clone)]
struct PartialTokenBalance {
    mint: String,
    token_program: TokenProgramKindV1,
    owner: Option<String>,
    decimals: u8,
    pre_amount: Option<TokenAmount>,
    post_amount: Option<TokenAmount>,
}

fn token_account_balances(
    transaction: &ArchivedTransaction,
    account_keys: &[Pubkey],
) -> Result<Vec<TokenAccountBalanceV1>, DecodeError> {
    let mut balances = BTreeMap::<u32, PartialTokenBalance>::new();
    for (lane, is_pre) in [("preTokenBalances", true), ("postTokenBalances", false)] {
        let Some(rows) = transaction.meta.get(lane).and_then(JsonValue::as_array) else {
            continue;
        };
        for row in rows {
            let account_index = row
                .get("accountIndex")
                .and_then(JsonValue::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .ok_or(DecodeError::InvalidTokenBalance)?;
            let ui = row
                .get("uiTokenAmount")
                .ok_or(DecodeError::InvalidTokenBalance)?;
            let amount = ui
                .get("amount")
                .and_then(JsonValue::as_str)
                .ok_or(DecodeError::InvalidTokenBalance)?
                .parse::<TokenAmount>()
                .map_err(|_| DecodeError::InvalidTokenBalance)?;
            let decimals = ui
                .get("decimals")
                .and_then(JsonValue::as_u64)
                .and_then(|value| u8::try_from(value).ok())
                .ok_or(DecodeError::InvalidTokenBalance)?;
            let mint = row
                .get("mint")
                .and_then(JsonValue::as_str)
                .ok_or(DecodeError::InvalidTokenBalance)?
                .to_owned();
            let token_program = match row.get("programId").and_then(JsonValue::as_str) {
                Some(LEGACY_TOKEN_PROGRAM_ID) => TokenProgramKindV1::LegacyToken,
                Some(TOKEN_2022_PROGRAM_ID) => TokenProgramKindV1::Token2022,
                _ => return Err(DecodeError::InvalidTokenBalance),
            };
            let owner = row
                .get("owner")
                .and_then(JsonValue::as_str)
                .map(str::to_owned);
            let entry = balances
                .entry(account_index)
                .or_insert_with(|| PartialTokenBalance {
                    mint: mint.clone(),
                    token_program,
                    owner: owner.clone(),
                    decimals,
                    pre_amount: None,
                    post_amount: None,
                });
            if entry.mint != mint
                || entry.token_program != token_program
                || entry.decimals != decimals
            {
                return Err(DecodeError::InvalidTokenBalance);
            }
            if entry.owner.is_none() {
                entry.owner = owner;
            }
            let target = if is_pre {
                &mut entry.pre_amount
            } else {
                &mut entry.post_amount
            };
            if target.replace(amount).is_some() {
                return Err(DecodeError::InvalidTokenBalance);
            }
        }
    }
    balances
        .into_iter()
        .map(|(account_index, balance)| {
            let account = account_keys
                .get(account_index as usize)
                .ok_or(DecodeError::AccountIndex)?
                .to_string();
            Ok(TokenAccountBalanceV1 {
                account_index,
                account,
                mint: balance.mint,
                token_program: balance.token_program,
                owner: balance.owner,
                decimals: balance.decimals,
                pre_amount: balance.pre_amount.unwrap_or_else(TokenAmount::zero),
                post_amount: balance.post_amount.unwrap_or_else(TokenAmount::zero),
            })
        })
        .collect()
}

fn parse_account_key(value: &JsonValue) -> Result<Pubkey, DecodeError> {
    let text = value
        .as_str()
        .or_else(|| value.get("pubkey").and_then(JsonValue::as_str))
        .ok_or(DecodeError::InvalidTransactionShape)?;
    Pubkey::from_str(text).map_err(|_| DecodeError::InvalidAccountKey)
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DecodeError {
    #[error("unsupported decoder registry schema {0}")]
    SchemaVersion(u16),
    #[error("no decoder is active at slot {0}")]
    NoDecoder(u64),
    #[error("more than one decoder is active at slot {0}")]
    OverlappingDecoders(u64),
    #[error("decoder range is invalid")]
    InvalidRange,
    #[error("program id is invalid")]
    InvalidProgramId,
    #[error("archival block {0} lacks canonical block time")]
    MissingBlockTime(u64),
    #[error("archived transaction JSON has an unsupported shape")]
    InvalidTransactionShape,
    #[error("target-program instruction was returned in parsed form")]
    ParsedTargetInstruction,
    #[error("instruction or account index exceeds the supported domain")]
    IndexOverflow,
    #[error("compiled instruction account index is invalid")]
    AccountIndex,
    #[error("instruction data is not valid base58")]
    InvalidBase58,
    #[error("account key is not a Solana public key")]
    InvalidAccountKey,
    #[error("decoded instruction could not be serialized")]
    DecodedSerialization,
    #[error("transaction token-balance metadata is malformed or inconsistent")]
    InvalidTokenBalance,
    #[error("exact token CPI evidence failed: {0}")]
    ExactTokenCpiEvidence(TokenCpiError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::{ArchivedBlockProofBasisV1, ArchivedTransactionProofBasisV1};

    fn revision(from: u64, to: Option<u64>, id: &str) -> DecoderRevisionV1 {
        DecoderRevisionV1 {
            decoder_version_id: id.into(),
            valid_from_slot: from,
            valid_to_slot_exclusive: to,
            source_git_commit: "1".repeat(40),
            idl_sha256: "2".repeat(64),
            candidate_program_binary_sha256: "3".repeat(64),
            program_binary_sha256: Some("3".repeat(64)),
            provisional: false,
        }
    }

    #[test]
    fn registry_requires_exactly_one_decoder() {
        let registry = DecoderRegistryV1 {
            schema_version: DECODER_REGISTRY_SCHEMA_VERSION,
            revisions: vec![revision(10, Some(20), "a"), revision(20, Some(30), "b")],
        };
        assert_eq!(
            registry.decoder_for_slot(10).unwrap().decoder_version_id,
            "a"
        );
        assert_eq!(
            registry.decoder_for_slot(20).unwrap().decoder_version_id,
            "b"
        );
        assert_eq!(
            registry.decoder_for_slot(30),
            Err(DecodeError::NoDecoder(30))
        );
    }

    #[test]
    fn metadata_cannot_claim_unimplemented_historical_dispatch() {
        let mut item = revision(0, None, "current");
        let registry = DecoderRegistryV1 {
            schema_version: DECODER_REGISTRY_SCHEMA_VERSION,
            revisions: vec![item.clone()],
        };
        assert!(!registry.is_headline_verified());
        item.provisional = true;
        assert!(!DecoderRegistryV1 {
            schema_version: DECODER_REGISTRY_SCHEMA_VERSION,
            revisions: vec![item]
        }
        .is_headline_verified());
    }

    #[test]
    fn registry_v2_requires_a_valid_candidate_binding() {
        let mut item = revision(0, None, "current");
        assert!(item.has_structural_candidate_binding());

        item.source_git_commit = "working-tree".to_owned();
        assert!(!item.has_structural_candidate_binding());
        item.source_git_commit = "1".repeat(40);

        item.candidate_program_binary_sha256 = "unresolved".to_owned();
        assert!(!item.has_structural_candidate_binding());
    }

    #[test]
    fn checked_in_registry_truthfully_identifies_only_the_candidate_elf() {
        let registry: DecoderRegistryV1 =
            toml::from_str(include_str!("../decoder-revisions.toml")).unwrap();
        assert_eq!(registry.schema_version, DECODER_REGISTRY_SCHEMA_VERSION);
        let revision = registry.decoder_for_slot(440_459_364).unwrap();
        assert_eq!(
            revision.source_git_commit,
            "84a8ef7a2a3a81e750435b86d7191bf43413d6a4"
        );
        assert_eq!(
            revision.idl_sha256,
            "42181773707e3726e2b95d1b28d8aeeabf1cb50b84e7c027bf7a5bb8c940aa46"
        );
        assert_eq!(
            revision.candidate_program_binary_sha256,
            "ca676f1763be474a0035e1c9f5dc4003be41250e134c366ff8d91d8cdc7452eb"
        );
        assert!(revision.program_binary_sha256.is_none());
        assert!(revision.provisional);
        assert!(revision.has_structural_candidate_binding());
        assert!(!registry.is_headline_verified());
    }

    #[test]
    fn decoded_numeric_values_are_strings_recursively() {
        let mut value = serde_json::json!({
            "amount": 9_007_199_254_740_993_u64,
            "nested": [1, {"bps": 25}],
            "flag": true
        });
        stringify_json_numbers(&mut value);
        assert_eq!(value["amount"], "9007199254740993");
        assert_eq!(value["nested"][0], "1");
        assert_eq!(value["nested"][1]["bps"], "25");
        assert_eq!(value["flag"], true);
    }

    fn update_pair_data() -> String {
        bs58::encode([0xb0, 0x3e, 0x24, 0xd7, 0xff, 0xce, 0x23, 0x0c]).into_string()
    }

    fn transfer_checked_data(amount: u64) -> String {
        let mut data = vec![12];
        data.extend_from_slice(&amount.to_le_bytes());
        data.push(6);
        bs58::encode(data).into_string()
    }

    fn test_order(
        transaction_index: u32,
        outer_instruction_index: u32,
        inner_execution_index: Option<u32>,
    ) -> ChainOrder {
        ChainOrder {
            slot: 100,
            transaction_index,
            outer_instruction_index,
            inner_execution_index,
            event_sequence: 0,
        }
    }

    fn test_transaction(
        signature: &str,
        transaction_index: u32,
        account_keys: &[String],
        outer: Vec<JsonValue>,
        inner: Vec<JsonValue>,
    ) -> ArchivedTransaction {
        ArchivedTransaction {
            signature: signature.to_owned(),
            transaction_index,
            succeeded: true,
            rpc_payload_sha256: sha256_hex(signature.as_bytes()),
            proof_basis: ArchivedTransactionProofBasisV1::FullBlockPayloadV1,
            transaction: serde_json::json!({
                "signatures": [signature],
                "message": {
                    "accountKeys": account_keys,
                    "instructions": outer,
                }
            }),
            meta: serde_json::json!({
                "err": null,
                "innerInstructions": inner,
            }),
        }
    }

    fn rpc_token_balance(
        account_index: u32,
        mint: &str,
        program_id: Option<&str>,
        amount: &str,
    ) -> JsonValue {
        let mut row = serde_json::json!({
            "accountIndex": account_index,
            "mint": mint,
            "owner": Pubkey::new_from_array([10; 32]).to_string(),
            "uiTokenAmount": {
                "amount": amount,
                "decimals": 6,
                "uiAmount": null,
                "uiAmountString": amount,
            },
        });
        if let Some(program_id) = program_id {
            row["programId"] = JsonValue::String(program_id.to_owned());
        }
        row
    }

    #[test]
    fn token_balances_preserve_exact_rpc_program_and_require_pre_post_agreement() {
        let token_account = Pubkey::new_from_array([11; 32]).to_string();
        let mint = Pubkey::new_from_array([12; 32]).to_string();
        let mut transaction = test_transaction(
            "token-program",
            0,
            std::slice::from_ref(&token_account),
            Vec::new(),
            Vec::new(),
        );
        transaction.meta["preTokenBalances"] = serde_json::json!([rpc_token_balance(
            0,
            &mint,
            Some(LEGACY_TOKEN_PROGRAM_ID),
            "1"
        )]);
        transaction.meta["postTokenBalances"] = serde_json::json!([rpc_token_balance(
            0,
            &mint,
            Some(LEGACY_TOKEN_PROGRAM_ID),
            "2"
        )]);
        let keys = account_keys(&transaction).unwrap();
        let balances = token_account_balances(&transaction, &keys).unwrap();
        assert_eq!(balances.len(), 1);
        assert_eq!(balances[0].mint, mint);
        assert_eq!(balances[0].token_program, TokenProgramKindV1::LegacyToken);

        transaction.meta["postTokenBalances"][0]["programId"] =
            JsonValue::String(TOKEN_2022_PROGRAM_ID.to_owned());
        assert_eq!(
            token_account_balances(&transaction, &keys),
            Err(DecodeError::InvalidTokenBalance)
        );
    }

    #[test]
    fn token_balances_reject_missing_or_unknown_rpc_program() {
        let token_account = Pubkey::new_from_array([13; 32]).to_string();
        let mint = Pubkey::new_from_array([14; 32]).to_string();
        for program_id in [None, Some(Pubkey::new_from_array([15; 32]).to_string())] {
            let mut transaction = test_transaction(
                "unsupported-token-program",
                0,
                std::slice::from_ref(&token_account),
                Vec::new(),
                Vec::new(),
            );
            transaction.meta["preTokenBalances"] =
                serde_json::json!([rpc_token_balance(0, &mint, program_id.as_deref(), "1")]);
            let keys = account_keys(&transaction).unwrap();
            assert_eq!(
                token_account_balances(&transaction, &keys),
                Err(DecodeError::InvalidTokenBalance)
            );
        }
    }

    fn test_block(transactions: Vec<ArchivedTransaction>) -> ArchivedBlock {
        let full_transaction_count = transactions.len() as u64;
        ArchivedBlock {
            slot: 100,
            block_time_unix_seconds: Some(1_700_000_000),
            blockhash: "block-100".to_owned(),
            previous_blockhash: "block-99".to_owned(),
            parent_slot: 99,
            full_transaction_count,
            full_transaction_inventory_sha256: sha256_hex(b"test-inventory"),
            proof_basis: ArchivedBlockProofBasisV1::FullPayloadV1,
            transactions,
        }
    }

    fn provisional_registry() -> DecoderRegistryV1 {
        let mut revision = revision(0, None, "reconstructive-current");
        revision.provisional = true;
        DecoderRegistryV1 {
            schema_version: DECODER_REGISTRY_SCHEMA_VERSION,
            revisions: vec![revision],
        }
    }

    fn append_u64(output: &mut Vec<u8>, value: u64) {
        output.extend_from_slice(&value.to_le_bytes());
    }

    fn append_u16(output: &mut Vec<u8>, value: u16) {
        output.extend_from_slice(&value.to_le_bytes());
    }

    fn append_u128(output: &mut Vec<u8>, value: u128) {
        output.extend_from_slice(&value.to_le_bytes());
    }

    fn append_metadata(output: &mut Vec<u8>, signer: Pubkey, pair: Pubkey, slot: u64) {
        output.extend_from_slice(signer.as_ref());
        output.extend_from_slice(pair.as_ref());
        append_u64(output, slot);
    }

    fn swap_event_wire(signer: Pubkey, pair: Pubkey, fees: Option<(u64, u64)>) -> Vec<u8> {
        let mut output = SWAP_EVENT_DISCRIMINATOR.to_vec();
        append_u64(&mut output, 11);
        append_u64(&mut output, 22);
        output.push(1);
        append_u64(&mut output, 33);
        append_u64(&mut output, 44);
        append_u64(&mut output, 31);
        if let Some((lp_fee, protocol_fee)) = fees {
            append_u64(&mut output, lp_fee);
            append_u64(&mut output, protocol_fee);
        }
        append_metadata(&mut output, signer, pair, 77);
        output
    }

    fn user_position_updated_event_legacy_v0_wire(
        position: Pubkey,
        signer: Pubkey,
        pair: Pubkey,
    ) -> Vec<u8> {
        let mut output = USER_POSITION_UPDATED_EVENT_LEGACY_V0_DISCRIMINATOR.to_vec();
        output.extend_from_slice(position.as_ref());
        append_u64(&mut output, 101);
        append_u64(&mut output, 202);
        append_u128(&mut output, u128::from(u64::MAX) + 3);
        append_u128(&mut output, u128::from(u64::MAX) + 4);
        append_u16(&mut output, 7_501);
        append_u16(&mut output, 7_502);
        append_metadata(&mut output, signer, pair, 303);
        output
    }

    fn user_liquidity_position_updated_event_legacy_v0_wire(
        token0_mint: Pubkey,
        token1_mint: Pubkey,
        lp_mint: Pubkey,
        signer: Pubkey,
        pair: Pubkey,
    ) -> Vec<u8> {
        let mut output = USER_LIQUIDITY_POSITION_UPDATED_EVENT_LEGACY_V0_DISCRIMINATOR.to_vec();
        append_u64(&mut output, 11);
        append_u64(&mut output, 22);
        append_u64(&mut output, 33);
        output.extend_from_slice(token0_mint.as_ref());
        output.extend_from_slice(token1_mint.as_ref());
        output.extend_from_slice(lp_mint.as_ref());
        append_metadata(&mut output, signer, pair, 44);
        output
    }

    fn liquidation_event_wire(
        position: Pubkey,
        liquidator: Pubkey,
        signer: Pubkey,
        pair: Pubkey,
        shortfall: u128,
        k0: u128,
        k1: u128,
    ) -> Vec<u8> {
        let mut output = vec![
            0xe4, 0x45, 0xa5, 0x2e, 0x51, 0xcb, 0x9a, 0x1d, 0xdc, 0x89, 0xd9, 0x03, 0xf2, 0xbe,
            0xee, 0xd8,
        ];
        output.extend_from_slice(position.as_ref());
        output.extend_from_slice(liquidator.as_ref());
        for value in [101, 102, 103, 104, 105] {
            append_u64(&mut output, value);
        }
        append_u128(&mut output, shortfall);
        append_u64(&mut output, 106);
        append_u128(&mut output, k0);
        append_u128(&mut output, k1);
        append_metadata(&mut output, signer, pair, 88);
        output
    }

    fn target_transaction(
        signature: &str,
        transaction_index: u32,
        data: &[u8],
    ) -> ArchivedTransaction {
        let target = Pubkey::new_from_array([7; 32]).to_string();
        test_transaction(
            signature,
            transaction_index,
            &[target],
            vec![serde_json::json!({
                "programIdIndex": 0,
                "accounts": [],
                "data": bs58::encode(data).into_string(),
            })],
            vec![],
        )
    }

    #[test]
    fn liquidation_u128_and_event_pubkeys_are_lossless_and_canonical() {
        let position = Pubkey::new_from_array([21; 32]);
        let liquidator = Pubkey::new_from_array([22; 32]);
        let signer = Pubkey::new_from_array([23; 32]);
        let pair = Pubkey::new_from_array([24; 32]);
        let shortfall = u128::from(u64::MAX) + 1;
        let k0 = u128::from(u64::MAX) + 2;
        let k1 = u128::MAX;
        let wire = liquidation_event_wire(position, liquidator, signer, pair, shortfall, k0, k1);
        assert_eq!(wire.len(), 248);

        let target = Pubkey::new_from_array([7; 32]).to_string();
        let outcome = decode_archived_block(
            &test_block(vec![target_transaction("liquidation-golden", 0, &wire)]),
            &target,
            &provisional_registry(),
        )
        .unwrap();
        assert!(outcome.is_fully_decoded());
        let instruction = &outcome.transactions[0].instructions[0];
        assert_eq!(instruction.schema_version, DECODE_SCHEMA_VERSION);
        assert_eq!(instruction.kind, "user_position_liquidated_event");
        assert_eq!(instruction.class, DecodedInstructionClass::EmittedAssertion);
        let body = instruction.data["UserPositionLiquidatedEvent"]
            .as_object()
            .unwrap();
        assert_eq!(body["shortfall"], shortfall.to_string());
        assert_eq!(body["k0"], k0.to_string());
        assert_eq!(body["k1"], k1.to_string());
        assert_eq!(body["position"], position.to_string());
        assert_eq!(body["liquidator"], liquidator.to_string());
        assert_eq!(body["metadata"]["signer"], signer.to_string());
        assert_eq!(body["metadata"]["pair"], pair.to_string());
        assert_eq!(body["metadata"]["slot"], "88");
    }

    #[test]
    fn legacy_and_current_swap_wire_layouts_decode_without_fee_inference() {
        let signer = Pubkey::new_from_array([31; 32]);
        let pair = Pubkey::new_from_array([32; 32]);
        let legacy = swap_event_wire(signer, pair, None);
        let current = swap_event_wire(signer, pair, Some((2, 0)));
        assert_eq!(legacy.len(), SWAP_EVENT_LEGACY_V0_WIRE_LEN);
        assert_eq!(current.len(), 145);

        let target = Pubkey::new_from_array([7; 32]).to_string();
        let outcome = decode_archived_block(
            &test_block(vec![
                target_transaction("legacy-swap-golden", 0, &legacy),
                target_transaction("current-swap-golden", 1, &current),
            ]),
            &target,
            &provisional_registry(),
        )
        .unwrap();
        assert!(outcome.is_fully_decoded());
        let legacy = &outcome.transactions[0].instructions[0];
        assert_eq!(legacy.kind, "swap_event_legacy_v0");
        assert_eq!(legacy.class, DecodedInstructionClass::EmittedAssertion);
        let legacy_body = legacy.data["SwapEventLegacyV0"].as_object().unwrap();
        assert_eq!(legacy_body["reserve0"], "11");
        assert_eq!(legacy_body["reserve1"], "22");
        assert_eq!(legacy_body["amount_in"], "33");
        assert_eq!(legacy_body["amount_out"], "44");
        assert_eq!(legacy_body["amount_in_after_fee"], "31");
        assert_eq!(legacy_body["metadata"]["signer"], signer.to_string());
        assert_eq!(legacy_body["metadata"]["pair"], pair.to_string());
        assert!(!legacy_body.contains_key("lp_fee"));
        assert!(!legacy_body.contains_key("protocol_fee"));

        let current = &outcome.transactions[1].instructions[0];
        assert_eq!(current.kind, "swap_event");
        let current_body = current.data["SwapEvent"].as_object().unwrap();
        assert_eq!(current_body["lp_fee"], "2");
        assert_eq!(current_body["protocol_fee"], "0");
    }

    #[test]
    fn legacy_swap_decoder_requires_exact_discriminator_length_and_bool_encoding() {
        let signer = Pubkey::new_from_array([33; 32]);
        let pair = Pubkey::new_from_array([34; 32]);
        let wire = swap_event_wire(signer, pair, None);

        let mut wrong_discriminator = wire.clone();
        wrong_discriminator[0] ^= 1;
        assert_eq!(
            decode_swap_event_legacy_v0(&wrong_discriminator).unwrap(),
            None
        );

        let mut wrong_length = wire.clone();
        wrong_length.push(0);
        assert_eq!(decode_swap_event_legacy_v0(&wrong_length).unwrap(), None);

        let mut invalid_bool = wire;
        invalid_bool[SWAP_EVENT_DISCRIMINATOR.len() + 16] = 2;
        assert_eq!(
            decode_swap_event_legacy_v0(&invalid_bool),
            Err(DecodeError::DecodedSerialization)
        );
    }

    #[test]
    fn anchor_idl_mutations_are_typed_program_metadata() {
        let payload = b"exact-idl-fragment";
        let mut create = ANCHOR_IDL_INSTRUCTION_PREFIX.to_vec();
        create.push(0);
        append_u64(&mut create, 4_096);
        let mut create_buffer = ANCHOR_IDL_INSTRUCTION_PREFIX.to_vec();
        create_buffer.push(1);
        let mut write = ANCHOR_IDL_INSTRUCTION_PREFIX.to_vec();
        write.push(2);
        write.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        write.extend_from_slice(payload);
        let mut set_buffer = ANCHOR_IDL_INSTRUCTION_PREFIX.to_vec();
        set_buffer.push(3);
        let mut close = ANCHOR_IDL_INSTRUCTION_PREFIX.to_vec();
        close.push(5);
        let mut resize = ANCHOR_IDL_INSTRUCTION_PREFIX.to_vec();
        resize.push(6);
        append_u64(&mut resize, 8_192);

        let target = Pubkey::new_from_array([7; 32]).to_string();
        let wires = [create, create_buffer, write, set_buffer, close, resize];
        let transactions = wires
            .iter()
            .enumerate()
            .map(|(index, wire)| target_transaction(&format!("idl-{index}"), index as u32, wire))
            .collect();
        let outcome =
            decode_archived_block(&test_block(transactions), &target, &provisional_registry())
                .unwrap();
        assert!(outcome.is_fully_decoded());
        assert_eq!(outcome.state_changing_instruction_coverage.expected, 0);
        assert_eq!(
            outcome
                .transactions
                .iter()
                .map(|transaction| transaction.instructions[0].kind.as_str())
                .collect::<Vec<_>>(),
            vec![
                "anchor_idl_create_account",
                "anchor_idl_create_buffer",
                "anchor_idl_write",
                "anchor_idl_set_buffer",
                "anchor_idl_close_account",
                "anchor_idl_resize_account",
            ]
        );
        assert!(outcome.transactions.iter().all(|transaction| {
            transaction.instructions[0].class == DecodedInstructionClass::ProgramMetadata
                && transaction.unknown_program_instruction_hashes.is_empty()
        }));
        assert_eq!(
            outcome.transactions[2].instructions[0].data["AnchorIdlMetadataV1"]["payload_sha256"],
            sha256_hex(payload)
        );
    }

    #[test]
    fn exact_legacy_position_assertion_layouts_decode_without_field_invention() {
        let position = Pubkey::new_from_array([40; 32]);
        let signer = Pubkey::new_from_array([41; 32]);
        let pair = Pubkey::new_from_array([42; 32]);
        let token0 = Pubkey::new_from_array([43; 32]);
        let token1 = Pubkey::new_from_array([44; 32]);
        let lp_mint = Pubkey::new_from_array([45; 32]);
        let position_wire = user_position_updated_event_legacy_v0_wire(position, signer, pair);
        let lp_wire = user_liquidity_position_updated_event_legacy_v0_wire(
            token0, token1, lp_mint, signer, pair,
        );
        assert_eq!(
            position_wire.len(),
            USER_POSITION_UPDATED_EVENT_LEGACY_V0_WIRE_LEN
        );
        assert_eq!(
            lp_wire.len(),
            USER_LIQUIDITY_POSITION_UPDATED_EVENT_LEGACY_V0_WIRE_LEN
        );

        let target = Pubkey::new_from_array([7; 32]).to_string();
        let outcome = decode_archived_block(
            &test_block(vec![
                target_transaction("legacy-position", 0, &position_wire),
                target_transaction("legacy-lp-position", 1, &lp_wire),
            ]),
            &target,
            &provisional_registry(),
        )
        .unwrap();
        assert!(outcome.is_fully_decoded());
        let position_body = outcome.transactions[0].instructions[0].data
            ["UserPositionUpdatedEventLegacyV0"]
            .as_object()
            .unwrap();
        assert_eq!(position_body["position"], position.to_string());
        assert_eq!(position_body["collateral0_applied_min_cf_bps"], "7501");
        assert_eq!(position_body["collateral1_applied_min_cf_bps"], "7502");
        assert!(!position_body.contains_key("collateral0_max_cf_bps"));
        assert!(!position_body.contains_key("collateral0_liquidation_cf_bps"));

        let lp_body = outcome.transactions[1].instructions[0].data
            ["UserLiquidityPositionUpdatedEventLegacyV0"]
            .as_object()
            .unwrap();
        assert_eq!(lp_body["token0_mint"], token0.to_string());
        assert_eq!(lp_body["token1_mint"], token1.to_string());
        assert_eq!(lp_body["lp_mint"], lp_mint.to_string());
        assert!(!lp_body.contains_key("cash_reserve0"));
        assert!(!lp_body.contains_key("cash_reserve1"));
    }

    #[test]
    fn schema_known_pubkeys_accept_exact_bytes_and_leave_params_hash_as_bytes() {
        let key = Pubkey::new_from_array([41; 32]);
        let params_hash: Vec<u8> = (0..32).collect();
        let mut pair_created = serde_json::json!({
            "PairCreatedEvent": {
                "token0": key.to_bytes(),
                "token1": key.to_string(),
                "lp_mint": key.to_bytes(),
                "rate_model": key.to_bytes(),
                "params_hash": params_hash,
                "metadata": {
                    "signer": key.to_bytes(),
                    "pair": key.to_bytes(),
                    "slot": 99,
                }
            }
        });
        normalize_schema_pubkeys(
            "pair_created_event",
            DecodedInstructionClass::EmittedAssertion,
            &mut pair_created,
        )
        .unwrap();
        stringify_decoded_json_numbers("pair_created_event", &mut pair_created).unwrap();
        let body = pair_created["PairCreatedEvent"].as_object().unwrap();
        for field in ["token0", "token1", "lp_mint", "rate_model"] {
            assert_eq!(body[field], key.to_string());
        }
        assert_eq!(body["metadata"]["signer"], key.to_string());
        assert_eq!(body["metadata"]["pair"], key.to_string());
        assert_eq!(body["metadata"]["slot"], "99");
        assert!(body["params_hash"]
            .as_array()
            .unwrap()
            .iter()
            .all(JsonValue::is_number));

        let mut invalid = pair_created.clone();
        invalid["PairCreatedEvent"]["token0"] = serde_json::to_value([0_u8; 31]).unwrap();
        assert_eq!(
            normalize_schema_pubkeys(
                "pair_created_event",
                DecodedInstructionClass::EmittedAssertion,
                &mut invalid,
            ),
            Err(DecodeError::DecodedSerialization)
        );
    }

    #[test]
    fn every_schema_declared_event_pubkey_field_is_canonicalized() {
        let key = Pubkey::new_from_array([42; 32]);
        for (kind, fields) in [
            (
                "user_liquidity_position_updated_event",
                &["token0_mint", "token1_mint", "lp_mint"][..],
            ),
            ("user_position_created_event", &["position"][..]),
            ("user_position_updated_event", &["position"][..]),
            (
                "user_position_liquidated_event",
                &["position", "liquidator"][..],
            ),
            ("flashloan_event", &["receiver"][..]),
            ("claim_protocol_fees_event", &["token0", "token1"][..]),
        ] {
            let mut body = serde_json::Map::from_iter([(
                "metadata".to_owned(),
                serde_json::json!({
                    "signer": key.to_bytes(),
                    "pair": key.to_bytes(),
                    "slot": 1,
                }),
            )]);
            for field in fields {
                body.insert((*field).to_owned(), serde_json::json!(key.to_bytes()));
            }
            let mut value = JsonValue::Object(serde_json::Map::from_iter([(
                "Event".to_owned(),
                JsonValue::Object(body),
            )]));
            normalize_schema_pubkeys(kind, DecodedInstructionClass::EmittedAssertion, &mut value)
                .unwrap();
            let body = value["Event"].as_object().unwrap();
            for field in fields {
                assert_eq!(body[*field], key.to_string());
            }
            assert_eq!(body["metadata"]["signer"], key.to_string());
            assert_eq!(body["metadata"]["pair"], key.to_string());
        }
    }

    #[test]
    fn every_schema_declared_config_pubkey_field_is_canonicalized() {
        let key = Pubkey::new_from_array([43; 32]);
        for (kind, fields, optional) in [
            (
                "init_futarchy_authority",
                &[
                    "authority",
                    "futarchy_treasury",
                    "buybacks_vault",
                    "team_treasury",
                ][..],
                false,
            ),
            ("update_futarchy_authority", &["new_authority"][..], false),
            (
                "update_revenue_recipients",
                &["futarchy_treasury", "buybacks_vault", "team_treasury"][..],
                true,
            ),
        ] {
            let mut args = serde_json::Map::new();
            for field in fields {
                args.insert((*field).to_owned(), serde_json::json!(key.to_bytes()));
            }
            let mut value = serde_json::json!({"Instruction": {"args": args}});
            normalize_schema_pubkeys(kind, DecodedInstructionClass::StateChanging, &mut value)
                .unwrap();
            stringify_decoded_json_numbers(kind, &mut value).unwrap();
            for field in fields {
                assert_eq!(value["Instruction"]["args"][*field], key.to_string());
            }

            if optional {
                value["Instruction"]["args"][fields[0]] = JsonValue::Null;
                normalize_schema_pubkeys(kind, DecodedInstructionClass::StateChanging, &mut value)
                    .unwrap();
                assert!(value["Instruction"]["args"][fields[0]].is_null());
            }

            value["Instruction"]["args"][fields[0]] = serde_json::to_value([0_u8; 31]).unwrap();
            assert_eq!(
                normalize_schema_pubkeys(kind, DecodedInstructionClass::StateChanging, &mut value,),
                Err(DecodeError::DecodedSerialization)
            );
        }
    }

    #[test]
    fn transaction_failure_preserves_valid_siblings_and_instruction_grain() {
        let target = Pubkey::new_from_array([7; 32]).to_string();
        let keys = vec![target.clone()];
        let valid = test_transaction(
            "valid",
            0,
            &keys,
            vec![serde_json::json!({
                "programIdIndex": 0,
                "accounts": [],
                "data": update_pair_data(),
            })],
            vec![],
        );
        let malformed = test_transaction(
            "malformed",
            1,
            &keys,
            vec![serde_json::json!({
                "programIdIndex": 0,
                "accounts": "not-an-array",
                "data": update_pair_data(),
            })],
            vec![],
        );

        let outcome = decode_archived_block(
            &test_block(vec![valid, malformed]),
            &target,
            &provisional_registry(),
        )
        .unwrap();
        assert_eq!(outcome.transactions.len(), 1);
        assert_eq!(outcome.transactions[0].signature, "valid");
        assert_eq!(outcome.transaction_failures.len(), 1);
        let failure = &outcome.transaction_failures[0];
        assert_eq!(failure.signature, "malformed");
        assert_eq!(failure.slot, 100);
        assert_eq!(failure.transaction_index, 1);
        assert_eq!(
            failure.category,
            TransactionDecodeFailureCategoryV1::InvalidTransactionShape
        );
        assert_eq!(failure.expected_state_changing_instruction_count, 1);
        assert_eq!(failure.decoded_state_changing_instruction_count, 0);
        assert!(failure.expected_count_complete);
        assert_eq!(
            outcome.state_changing_instruction_coverage,
            StateChangingInstructionCoverageV1 {
                expected: 2,
                decoded: 1,
                unclassified_target_instructions: 0,
                transactions_with_incomplete_inventory: 0,
            }
        );
        assert!(outcome.is_internally_consistent());
        assert!(!outcome.is_fully_decoded());
        assert!(!provisional_registry().is_headline_verified());

        let mut duplicate_signature = outcome.clone();
        duplicate_signature.transaction_failures[0].signature = "valid".to_owned();
        assert!(!duplicate_signature.is_internally_consistent());

        let mut duplicate_transaction_index = outcome.clone();
        duplicate_transaction_index.transaction_failures[0].transaction_index = 0;
        assert!(!duplicate_transaction_index.is_internally_consistent());
    }

    #[test]
    fn cpi_frames_reproduce_indexer_paths_and_direct_parents() {
        let target = Pubkey::new_from_array([7; 32]).to_string();
        let other = Pubkey::new_from_array([8; 32]).to_string();
        let keys = vec![target.clone(), other];
        let compiled = |program_index: u32, stack_height: u32| {
            serde_json::json!({
                "programIdIndex": program_index,
                "accounts": [],
                "data": update_pair_data(),
                "stackHeight": stack_height,
            })
        };
        let transaction = test_transaction(
            "nested",
            4,
            &keys,
            vec![serde_json::json!({
                "programIdIndex": 1,
                "accounts": [],
                "data": "",
            })],
            vec![serde_json::json!({
                "index": 0,
                "instructions": [
                    compiled(0, 2),
                    compiled(1, 3),
                    compiled(0, 3),
                    compiled(0, 2),
                ]
            })],
        );
        let outcome = decode_archived_block(
            &test_block(vec![transaction]),
            &target,
            &provisional_registry(),
        )
        .unwrap();
        let provenance = &outcome.execution_provenance[0];
        let paths: Vec<Vec<u32>> = provenance
            .frames
            .iter()
            .map(|frame| frame.execution_path.clone())
            .collect();
        assert_eq!(
            paths,
            vec![
                vec![0],
                vec![0, 0],
                vec![0, 0, 0],
                vec![0, 0, 1],
                vec![0, 1]
            ]
        );
        assert_eq!(
            provenance.frames[3].indexer_instruction_path(),
            "000000.000000.000001"
        );
        assert_eq!(
            provenance.verified_parent(provenance.frames[3].order),
            Some(provenance.frames[1].order)
        );
        assert_eq!(
            provenance.verified_parent(provenance.frames[4].order),
            Some(provenance.frames[0].order)
        );
        assert!(provenance.is_complete());
        assert!(outcome.is_internally_consistent());
        assert!(outcome.is_fully_decoded());
    }

    #[test]
    fn routed_token_cpis_are_nearest_owned_and_irrelevant_siblings_are_ignored() {
        let target = Pubkey::new_from_array([7; 32]).to_string();
        let router = Pubkey::new_from_array([8; 32]).to_string();
        let source = Pubkey::new_from_array([61; 32]).to_string();
        let mint = Pubkey::new_from_array([62; 32]).to_string();
        let destination = Pubkey::new_from_array([63; 32]).to_string();
        let authority = Pubkey::new_from_array([64; 32]).to_string();
        let keys = vec![
            router,
            target.clone(),
            LEGACY_TOKEN_PROGRAM_ID.to_owned(),
            source,
            mint,
            destination,
            authority,
        ];
        let transaction = test_transaction(
            "routed-token-cpi",
            0,
            &keys,
            vec![serde_json::json!({
                "programIdIndex": 0,
                "accounts": [],
                "data": "",
            })],
            vec![serde_json::json!({
                "index": 0,
                "instructions": [
                    {
                        "programIdIndex": 1,
                        "accounts": [],
                        "data": update_pair_data(),
                        "stackHeight": 2,
                    },
                    {
                        "programIdIndex": 2,
                        "accounts": [3, 4, 5, 6],
                        "data": transfer_checked_data(100),
                        "stackHeight": 3,
                    },
                    {
                        "programIdIndex": 2,
                        "accounts": [3, 6],
                        "data": bs58::encode([5_u8]).into_string(),
                        "stackHeight": 3,
                    },
                    {
                        "programIdIndex": 2,
                        "accounts": [],
                        "data": bs58::encode([255_u8]).into_string(),
                        "stackHeight": 2,
                    },
                ]
            })],
        );
        let outcome = decode_archived_block(
            &test_block(vec![transaction]),
            &target,
            &provisional_registry(),
        )
        .unwrap();
        assert!(outcome.is_fully_decoded());
        let provenance = &outcome.execution_provenance[0];
        assert_eq!(provenance.token_cpis.len(), 2);
        assert_eq!(
            provenance
                .token_cpis
                .iter()
                .map(ParsedTokenCpiV1::order)
                .collect::<Vec<_>>(),
            [test_order(0, 0, Some(1)), test_order(0, 0, Some(2))]
        );
        let target_order = test_order(0, 0, Some(0));
        assert_eq!(provenance.token_cpis_owned_by(target_order).len(), 2);
        assert_eq!(
            provenance.state_changing_owner(test_order(0, 0, Some(3))),
            None
        );

        let mut missing = outcome.clone();
        missing.execution_provenance[0].token_cpis.pop();
        assert!(!missing.is_internally_consistent());

        let mut out_of_scope = outcome.clone();
        let mut extra = out_of_scope.execution_provenance[0].token_cpis[0].clone();
        let ParsedTokenCpiV1::Transfer { evidence } = &mut extra else {
            panic!("first routed token CPI must be the checked transfer");
        };
        evidence.order = test_order(0, 0, Some(3));
        out_of_scope.execution_provenance[0].token_cpis.push(extra);
        assert!(!out_of_scope.is_internally_consistent());

        let mut wrong_program = outcome.clone();
        let ParsedTokenCpiV1::Transfer { evidence } =
            &mut wrong_program.execution_provenance[0].token_cpis[0]
        else {
            panic!("first routed token CPI must be the checked transfer");
        };
        evidence.token_program = TokenProgramKindV1::Token2022;
        assert!(!wrong_program.is_internally_consistent());

        let mut wrong_schema = outcome.clone();
        let ParsedTokenCpiV1::Transfer { evidence } =
            &mut wrong_schema.execution_provenance[0].token_cpis[0]
        else {
            panic!("first routed token CPI must be the checked transfer");
        };
        evidence.schema_version = TOKEN_CPI_SCHEMA_VERSION + 1;
        assert!(!wrong_schema.is_internally_consistent());

        let mut fabricated_parent = outcome;
        fabricated_parent.execution_provenance[0]
            .frames
            .iter_mut()
            .find(|frame| frame.order == test_order(0, 0, Some(1)))
            .unwrap()
            .parent_order = Some(test_order(0, 0, None));
        assert!(!fabricated_parent.is_internally_consistent());
    }

    #[test]
    fn shared_accounts_across_v1_operations_keep_instruction_local_evidence() {
        let target = Pubkey::new_from_array([7; 32]).to_string();
        let source = Pubkey::new_from_array([71; 32]).to_string();
        let mint = Pubkey::new_from_array([72; 32]).to_string();
        let destination = Pubkey::new_from_array([73; 32]).to_string();
        let authority = Pubkey::new_from_array([74; 32]).to_string();
        let keys = vec![
            target.clone(),
            LEGACY_TOKEN_PROGRAM_ID.to_owned(),
            source,
            mint,
            destination,
            authority,
        ];
        let transaction = test_transaction(
            "shared-token-account",
            0,
            &keys,
            vec![
                serde_json::json!({
                    "programIdIndex": 0,
                    "accounts": [],
                    "data": update_pair_data(),
                }),
                serde_json::json!({
                    "programIdIndex": 0,
                    "accounts": [],
                    "data": update_pair_data(),
                }),
            ],
            vec![
                serde_json::json!({
                    "index": 0,
                    "instructions": [{
                        "programIdIndex": 1,
                        "accounts": [2, 3, 4, 5],
                        "data": transfer_checked_data(10),
                        "stackHeight": 2,
                    }]
                }),
                serde_json::json!({
                    "index": 1,
                    "instructions": [{
                        "programIdIndex": 1,
                        "accounts": [2, 3, 4, 5],
                        "data": transfer_checked_data(20),
                        "stackHeight": 2,
                    }]
                }),
            ],
        );
        let outcome = decode_archived_block(
            &test_block(vec![transaction]),
            &target,
            &provisional_registry(),
        )
        .unwrap();
        assert!(outcome.is_fully_decoded());
        let provenance = &outcome.execution_provenance[0];
        assert_eq!(provenance.token_cpis.len(), 2);
        assert_eq!(
            provenance.token_cpis_owned_by(test_order(0, 0, None)).len(),
            1
        );
        assert_eq!(
            provenance.token_cpis_owned_by(test_order(0, 1, None)).len(),
            1
        );
    }

    #[test]
    fn native_ata_setup_and_sync_close_through_unique_rpc_balance_residual() {
        let target = Pubkey::new_from_array([7; 32]).to_string();
        let payer = Pubkey::new_from_array([70; 32]).to_string();
        let source = Pubkey::new_from_array([71; 32]).to_string();
        let owner = Pubkey::new_from_array([72; 32]).to_string();
        let destination = Pubkey::new_from_array([73; 32]).to_string();
        let authority = Pubkey::new_from_array([74; 32]).to_string();
        let native_mint = "So11111111111111111111111111111111111111112".to_owned();
        let keys = vec![
            target.clone(),
            ASSOCIATED_TOKEN_PROGRAM_ID.to_owned(),
            LEGACY_TOKEN_PROGRAM_ID.to_owned(),
            SYSTEM_PROGRAM_ID.to_owned(),
            native_mint.clone(),
            payer,
            source.clone(),
            owner,
            destination.clone(),
            authority,
        ];
        let mut initialize_account3 = vec![18_u8];
        initialize_account3.extend_from_slice(Pubkey::new_from_array([72; 32]).as_ref());
        let mut checked_transfer = vec![12_u8];
        checked_transfer.extend_from_slice(&100_u64.to_le_bytes());
        checked_transfer.push(9);
        let mut transaction = test_transaction(
            "native-residual",
            0,
            &keys,
            vec![
                serde_json::json!({
                    "programIdIndex": 1,
                    "accounts": [5, 6, 7, 4, 3, 2],
                    "data": "1",
                }),
                serde_json::json!({
                    "programIdIndex": 0,
                    "accounts": [6, 8],
                    "data": update_pair_data(),
                }),
            ],
            vec![
                serde_json::json!({
                    "index": 0,
                    "instructions": [{
                        "programIdIndex": 2,
                        "accounts": [6, 4],
                        "data": bs58::encode(initialize_account3).into_string(),
                        "stackHeight": 2,
                    }]
                }),
                serde_json::json!({
                    "index": 1,
                    "instructions": [
                        {
                            "programIdIndex": 3,
                            "accounts": [5, 6],
                            "data": "3Bxs3zz3fjzUYuEP",
                            "stackHeight": 2,
                        },
                        {
                            "programIdIndex": 2,
                            "accounts": [6],
                            "data": bs58::encode([17_u8]).into_string(),
                            "stackHeight": 2,
                        },
                        {
                            "programIdIndex": 2,
                            "accounts": [6, 4, 8, 9],
                            "data": bs58::encode(checked_transfer).into_string(),
                            "stackHeight": 2,
                        }
                    ]
                }),
            ],
        );
        let raw_balance = |account_index: u32, amount: &str| {
            serde_json::json!({
                "accountIndex": account_index,
                "mint": native_mint,
                "owner": Pubkey::new_from_array([72; 32]).to_string(),
                "programId": LEGACY_TOKEN_PROGRAM_ID,
                "uiTokenAmount": {
                    "amount": amount,
                    "decimals": 9,
                    "uiAmount": null,
                    "uiAmountString": amount,
                },
            })
        };
        transaction.meta["postTokenBalances"] =
            serde_json::json!([raw_balance(6, "0"), raw_balance(8, "100"),]);
        let outcome = decode_archived_block(
            &test_block(vec![transaction.clone()]),
            &target,
            &provisional_registry(),
        )
        .unwrap();
        assert!(outcome.is_fully_decoded());
        let provenance = &outcome.execution_provenance[0];
        assert_eq!(provenance.token_cpis.len(), 2);
        let ParsedTokenCpiV1::RecognizedNonTransfer { evidence } = &provenance.token_cpis[0] else {
            panic!("first owned CPI must be the resolved SyncNative");
        };
        let adjustment = evidence.exact_account_adjustment.as_ref().unwrap();
        assert_eq!(
            adjustment.reason,
            TokenAccountAdjustmentReasonV1::SyncNative
        );
        assert_eq!(adjustment.signed_amount.as_str(), "100");
        let TokenAccountAdjustmentSourceV1::RpcTokenBalanceResidual {
            other_exact_delta,
            certified_zero_external_cpis,
            ..
        } = &adjustment.source
        else {
            panic!("SyncNative must be proven by the RPC residual identity");
        };
        assert_eq!(other_exact_delta.as_str(), "-100");
        assert_eq!(certified_zero_external_cpis.len(), 1);

        transaction.meta["postTokenBalances"] = serde_json::json!([raw_balance(8, "100")]);
        let rejected = decode_archived_block(
            &test_block(vec![transaction]),
            &target,
            &provisional_registry(),
        )
        .unwrap();
        assert_eq!(rejected.transaction_failures.len(), 1);
        assert_eq!(
            rejected.transaction_failures[0].category,
            TransactionDecodeFailureCategoryV1::ExactTokenCpiStateDependent
        );
    }

    #[test]
    fn native_initialization_is_zero_only_with_residual_or_canonical_ata_proof() {
        let target = Pubkey::new_from_array([7; 32]).to_string();
        let native_mint = "So11111111111111111111111111111111111111112".to_owned();
        let native_account = Pubkey::new_from_array([81; 32]).to_string();
        let source = Pubkey::new_from_array([82; 32]).to_string();
        let authority = Pubkey::new_from_array([83; 32]).to_string();
        let payer = Pubkey::new_from_array([84; 32]).to_string();
        let owner = Pubkey::new_from_array([85; 32]).to_string();
        let keys = vec![
            target.clone(),
            LEGACY_TOKEN_PROGRAM_ID.to_owned(),
            native_mint.clone(),
            native_account.clone(),
            source.clone(),
            authority,
            ASSOCIATED_TOKEN_PROGRAM_ID.to_owned(),
            SYSTEM_PROGRAM_ID.to_owned(),
            payer,
            owner,
        ];
        let mut initialize_account3 = vec![18_u8];
        initialize_account3.extend_from_slice(Pubkey::new_from_array([85; 32]).as_ref());
        let mut checked_transfer = vec![12_u8];
        checked_transfer.extend_from_slice(&100_u64.to_le_bytes());
        checked_transfer.push(9);
        let balance = |account_index: u32, amount: &str| {
            serde_json::json!({
                "accountIndex": account_index,
                "mint": native_mint,
                "owner": Pubkey::new_from_array([85; 32]).to_string(),
                "programId": LEGACY_TOKEN_PROGRAM_ID,
                "uiTokenAmount": {
                    "amount": amount,
                    "decimals": 9,
                    "uiAmount": null,
                    "uiAmountString": amount,
                },
            })
        };

        let mut direct = test_transaction(
            "native-init-residual",
            0,
            &keys,
            vec![serde_json::json!({
                "programIdIndex": 0,
                "accounts": [3, 4],
                "data": update_pair_data(),
            })],
            vec![serde_json::json!({
                "index": 0,
                "instructions": [
                    {
                        "programIdIndex": 1,
                        "accounts": [3, 2],
                        "data": bs58::encode(&initialize_account3).into_string(),
                        "stackHeight": 2,
                    },
                    {
                        "programIdIndex": 1,
                        "accounts": [4, 2, 3, 5],
                        "data": bs58::encode(&checked_transfer).into_string(),
                        "stackHeight": 2,
                    }
                ]
            })],
        );
        direct.meta["preTokenBalances"] = serde_json::json!([balance(4, "100")]);
        direct.meta["postTokenBalances"] = serde_json::json!([balance(3, "100"), balance(4, "0"),]);
        let direct_outcome =
            decode_archived_block(&test_block(vec![direct]), &target, &provisional_registry())
                .unwrap();
        assert!(direct_outcome.is_fully_decoded());
        let ParsedTokenCpiV1::RecognizedNonTransfer { evidence } =
            &direct_outcome.execution_provenance[0].token_cpis[0]
        else {
            panic!("native initialization must retain typed evidence");
        };
        let adjustment = evidence.exact_account_adjustment.as_ref().unwrap();
        assert_eq!(adjustment.signed_amount.as_str(), "0");
        assert!(matches!(
            adjustment.source,
            TokenAccountAdjustmentSourceV1::RpcTokenBalanceResidual { .. }
        ));

        let mut ata_owned = test_transaction(
            "native-init-ata-owned",
            0,
            &keys,
            vec![serde_json::json!({
                "programIdIndex": 0,
                "accounts": [3, 4],
                "data": update_pair_data(),
            })],
            vec![serde_json::json!({
                "index": 0,
                "instructions": [
                    {
                        "programIdIndex": 6,
                        "accounts": [8, 3, 9, 2, 7, 1],
                        "data": "1",
                        "stackHeight": 2,
                    },
                    {
                        "programIdIndex": 1,
                        "accounts": [3, 2],
                        "data": bs58::encode(initialize_account3).into_string(),
                        "stackHeight": 3,
                    },
                    {
                        "programIdIndex": 1,
                        "accounts": [4, 2, 3, 5],
                        "data": bs58::encode(checked_transfer).into_string(),
                        "stackHeight": 2,
                    }
                ]
            })],
        );
        ata_owned.meta["preTokenBalances"] = serde_json::json!([balance(4, "100")]);
        ata_owned.meta["postTokenBalances"] =
            serde_json::json!([balance(3, "100"), balance(4, "0"),]);
        let ata_outcome = decode_archived_block(
            &test_block(vec![ata_owned]),
            &target,
            &provisional_registry(),
        )
        .unwrap();
        assert!(ata_outcome.is_fully_decoded());
        let ParsedTokenCpiV1::RecognizedNonTransfer { evidence } =
            &ata_outcome.execution_provenance[0].token_cpis[0]
        else {
            panic!("ATA-owned initialization must retain typed evidence");
        };
        let adjustment = evidence.exact_account_adjustment.as_ref().unwrap();
        assert!(matches!(
            adjustment.source,
            TokenAccountAdjustmentSourceV1::AssociatedTokenAccountCreation { .. }
        ));
    }

    #[test]
    fn nested_state_changing_targets_assign_each_token_cpi_to_nearest_owner() {
        let target = Pubkey::new_from_array([7; 32]).to_string();
        let source = Pubkey::new_from_array([81; 32]).to_string();
        let mint = Pubkey::new_from_array([82; 32]).to_string();
        let destination = Pubkey::new_from_array([83; 32]).to_string();
        let authority = Pubkey::new_from_array([84; 32]).to_string();
        let keys = vec![
            target.clone(),
            LEGACY_TOKEN_PROGRAM_ID.to_owned(),
            source,
            mint,
            destination,
            authority,
        ];
        let transaction = test_transaction(
            "nearest-v1-owner",
            0,
            &keys,
            vec![serde_json::json!({
                "programIdIndex": 0,
                "accounts": [],
                "data": update_pair_data(),
            })],
            vec![serde_json::json!({
                "index": 0,
                "instructions": [
                    {
                        "programIdIndex": 0,
                        "accounts": [],
                        "data": update_pair_data(),
                        "stackHeight": 2,
                    },
                    {
                        "programIdIndex": 1,
                        "accounts": [2, 3, 4, 5],
                        "data": transfer_checked_data(30),
                        "stackHeight": 3,
                    },
                ]
            })],
        );
        let outcome = decode_archived_block(
            &test_block(vec![transaction]),
            &target,
            &provisional_registry(),
        )
        .unwrap();
        assert!(outcome.is_fully_decoded());
        let provenance = &outcome.execution_provenance[0];
        assert_eq!(provenance.token_cpis.len(), 1);
        assert!(provenance
            .token_cpis_owned_by(test_order(0, 0, None))
            .is_empty());
        assert_eq!(
            provenance
                .token_cpis_owned_by(test_order(0, 0, Some(0)))
                .len(),
            1
        );
    }

    #[test]
    fn malformed_token_cpi_fails_atomically_without_losing_v1_inventory() {
        let target = Pubkey::new_from_array([7; 32]).to_string();
        let keys = vec![target.clone(), LEGACY_TOKEN_PROGRAM_ID.to_owned()];
        let transaction = test_transaction(
            "malformed-token-cpi",
            0,
            &keys,
            vec![serde_json::json!({
                "programIdIndex": 0,
                "accounts": [],
                "data": update_pair_data(),
            })],
            vec![serde_json::json!({
                "index": 0,
                "instructions": [{
                    "programIdIndex": 1,
                    "accounts": [],
                    "data": bs58::encode([12_u8]).into_string(),
                    "stackHeight": 2,
                }]
            })],
        );
        let outcome = decode_archived_block(
            &test_block(vec![transaction]),
            &target,
            &provisional_registry(),
        )
        .unwrap();
        assert!(outcome.transactions.is_empty());
        assert!(outcome.execution_provenance.is_empty());
        assert_eq!(outcome.transaction_failures.len(), 1);
        let failure = &outcome.transaction_failures[0];
        assert_eq!(
            failure.category,
            TransactionDecodeFailureCategoryV1::ExactTokenCpiMalformed
        );
        assert_eq!(failure.expected_state_changing_instruction_count, 1);
        assert!(failure.instruction_inventory_available);
        assert!(failure.expected_count_complete);
        assert!(failure.cpi_provenance_complete);
        assert_eq!(
            outcome.state_changing_instruction_coverage,
            StateChangingInstructionCoverageV1 {
                expected: 1,
                decoded: 0,
                unclassified_target_instructions: 0,
                transactions_with_incomplete_inventory: 0,
            }
        );
        assert!(outcome.is_internally_consistent());
        assert!(!outcome.is_fully_decoded());
    }

    #[test]
    fn token_cpi_failure_categories_preserve_the_exact_blocker_class() {
        let program = TokenProgramKindV1::LegacyToken;
        assert_eq!(
            TransactionDecodeFailureCategoryV1::from_error(&DecodeError::ExactTokenCpiEvidence(
                TokenCpiError::UnknownInstruction {
                    program,
                    discriminant: 255,
                }
            )),
            TransactionDecodeFailureCategoryV1::ExactTokenCpiUnknown
        );
        assert_eq!(
            TransactionDecodeFailureCategoryV1::from_error(&DecodeError::ExactTokenCpiEvidence(
                TokenCpiError::RequiresExternalCustodyState {
                    program,
                    discriminant: 1,
                }
            )),
            TransactionDecodeFailureCategoryV1::ExactTokenCpiStateDependent
        );
        assert_eq!(
            TransactionDecodeFailureCategoryV1::from_error(&DecodeError::ExactTokenCpiEvidence(
                TokenCpiError::UnsupportedValueMovement {
                    program,
                    discriminant: 17,
                }
            )),
            TransactionDecodeFailureCategoryV1::ExactTokenCpiUnsupported
        );
    }

    #[test]
    fn missing_inner_stack_height_never_proves_a_parent() {
        let target = Pubkey::new_from_array([7; 32]).to_string();
        let transaction = test_transaction(
            "missing-stack",
            0,
            std::slice::from_ref(&target),
            vec![serde_json::json!({
                "programIdIndex": 0,
                "accounts": [],
                "data": update_pair_data(),
            })],
            vec![serde_json::json!({
                "index": 0,
                "instructions": [{
                    "programIdIndex": 0,
                    "accounts": [],
                    "data": update_pair_data(),
                }]
            })],
        );
        let outcome = decode_archived_block(
            &test_block(vec![transaction]),
            &target,
            &provisional_registry(),
        )
        .unwrap();
        let provenance = &outcome.execution_provenance[0];
        let inner = &provenance.frames[1];
        assert_eq!(inner.provenance, CpiFrameProvenanceV1::MissingStackHeight);
        assert!(provenance.verified_parent(inner.order).is_none());
        assert!(provenance.verified_execution_path(inner.order).is_none());
        assert!(outcome.is_internally_consistent());
        assert!(!outcome.is_fully_decoded());
    }

    #[test]
    fn unknown_target_instruction_is_an_explicit_coverage_blocker() {
        let target = Pubkey::new_from_array([7; 32]).to_string();
        let transaction = test_transaction(
            "unknown",
            0,
            std::slice::from_ref(&target),
            vec![serde_json::json!({
                "programIdIndex": 0,
                "accounts": [],
                "data": bs58::encode([1, 2, 3, 4]).into_string(),
            })],
            vec![],
        );
        let outcome = decode_archived_block(
            &test_block(vec![transaction]),
            &target,
            &provisional_registry(),
        )
        .unwrap();
        assert_eq!(outcome.transactions.len(), 1);
        assert_eq!(
            outcome.state_changing_instruction_coverage,
            StateChangingInstructionCoverageV1 {
                expected: 0,
                decoded: 0,
                unclassified_target_instructions: 1,
                transactions_with_incomplete_inventory: 0,
            }
        );
        assert!(!outcome.state_changing_instruction_coverage.is_exact());
        assert!(outcome.is_internally_consistent());
        assert!(!outcome.is_fully_decoded());
    }
}
