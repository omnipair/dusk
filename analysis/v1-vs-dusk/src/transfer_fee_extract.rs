//! Finalized, history-complete mint ownership and Token-2022 fee provenance.
//!
//! Audit-grade extraction never substitutes current account state for history.
//! A narrow DecisionGrade exception accepts a finalized post-cutoff
//! `getAccountInfo` observation for an initialized mint owned by the immutable
//! legacy SPL Token program, and only when selected-operation evidence proves
//! the mint existed by the cutoff and every relevant program is compatible.
//! It is never labeled historical cutoff state. Token-2022 and ambiguous cases
//! retain the complete address-history crawl.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    future::Future,
    io::{BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
    pin::Pin,
    str::FromStr,
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};
use solana_pubkey::Pubkey;
use spl_token_2022::{
    extension::transfer_fee::instruction::TransferFeeInstruction, instruction::TokenInstruction,
    solana_program::program_pack::Pack, state::Mint as SplTokenMint,
};
use spl_token_metadata_interface::instruction::TokenMetadataInstruction;
use thiserror::Error;

use crate::{
    audit::EvidenceTier,
    canonical::{sha256_hex, ArtifactChecksum, SanitizedSource, TokenAmount},
    decode::{DecodedTransactionV1, TransactionExecutionProvenanceV1, DECODE_SCHEMA_VERSION},
    extract::{
        finalized_get_block_request, ArchivedBlock, ArchivedBlockProofBasisV1, ArchivedTransaction,
        ArchivedTransactionProofBasisV1, RpcFailureKind,
    },
    rpc::{
        finalized_account_info_request_sha256, BoundRpcResponseProofV1, FinalizedAccountInfoV1,
        HttpArchivalRpc, ProgramSignatureV1, RpcResponseProofV1,
    },
    transfer_fee::{
        EpochScheduleV1, MintTransferFeeScheduleV1, Token2022MintExtensionV1, TokenProgramKindV1,
        TransferFeeEpochV1, TransferFeeError, TransferFeeRegistryV1, TRANSFER_FEE_SCHEMA_VERSION,
    },
};

pub const TRANSFER_FEE_HISTORY_SCHEMA_VERSION: u16 = 1;
pub const TRANSFER_FEE_HISTORY_DIR: &str = "transfer-fee-history";
pub const TRANSFER_FEE_PROVENANCE_JSON: &str = "transfer-fee-provenance.json";
pub const TRANSFER_FEE_REGISTRY_JSON: &str = "transfer-fee-registry.json";
pub const SELECTED_OPERATION_TOKEN_PROGRAM_EVIDENCE_JSON: &str =
    "selected-operation-token-program-evidence.json";
pub const DECISION_GRADE_LEGACY_MINT_PROOFS_JSON: &str = "decision-grade-legacy-mint-proofs.json";
pub const TRANSFER_FEE_EXACT_FALLBACK_DIR: &str = "transfer-fee-exact-fallback";
pub const DEFAULT_MINT_SIGNATURE_PAGE_LIMIT: u16 = 1_000;
pub const LEGACY_TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
pub const TOKEN_2022_PROGRAM_ID: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";

const PAGE_HASH_DOMAIN: &[u8] = b"omnipair:v1-vs-dusk:mint-signature-page:v1";
const LEGACY_BLOCK_HASH_DOMAIN: &[u8] = b"omnipair:v1-vs-dusk:mint-history-block:v1";
const BLOCK_HASH_DOMAIN: &[u8] = b"omnipair:v1-vs-dusk:mint-history-block:v2";
const MINT_HISTORY_BLOCK_SCHEMA_VERSION: u16 = 2;
const MINT_HISTORY_BLOCK_ZSTD_LEVEL: i32 = 3;
const EPOCH_HASH_DOMAIN: &[u8] = b"omnipair:v1-vs-dusk:epoch-schedule:v1";
const PROVENANCE_HASH_DOMAIN: &[u8] = b"omnipair:v1-vs-dusk:transfer-fee-provenance:v1";
const DECODED_MINT_HISTORY_HASH_DOMAIN: &[u8] = b"omnipair:v1-vs-dusk:decoded-mint-history:v1";
const DECISION_GRADE_LEGACY_MINT_HASH_DOMAIN: &[u8] =
    b"omnipair:v1-vs-dusk:decision-grade-legacy-mint:v1";
const SELECTED_OPERATION_PROGRAM_EVIDENCE_HASH_DOMAIN: &[u8] =
    b"omnipair:v1-vs-dusk:selected-operation-token-program-evidence:v1";
const SELECTED_OPERATION_PROGRAM_SOURCE_HASH_DOMAIN: &[u8] =
    b"omnipair:v1-vs-dusk:selected-operation-token-program-source:v1";
const DECISION_GRADE_LEGACY_MINT_PROOF_SET_HASH_DOMAIN: &[u8] =
    b"omnipair:v1-vs-dusk:decision-grade-legacy-mint-proof-set:v1";
const DECISION_GRADE_DECODED_MINT_HASH_DOMAIN: &[u8] =
    b"omnipair:v1-vs-dusk:decision-grade-decoded-mint:v1";
static PARTIAL_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketMintBindingV1 {
    pub pair_address: String,
    pub token0: String,
    pub token1: String,
    pub lp_mint: String,
}

impl MarketMintBindingV1 {
    fn mints(&self) -> [&str; 3] {
        [&self.token0, &self.token1, &self.lp_mint]
    }

    fn validate(&self) -> Result<(), TransferFeeExtractionError> {
        if self.pair_address.trim().is_empty() {
            return Err(TransferFeeExtractionError::InvalidBinding(
                "market pair address is empty".to_owned(),
            ));
        }
        for mint in self.mints() {
            parse_pubkey(mint).map_err(|_| {
                TransferFeeExtractionError::InvalidBinding(format!(
                    "market {} contains an invalid mint",
                    self.pair_address
                ))
            })?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct TransferFeeExtractionRequestV1<'a> {
    pub run_id: &'a str,
    pub cutoff_slot: u64,
    pub provider: &'a SanitizedSource,
    pub markets: &'a [MarketMintBindingV1],
    pub output_dir: &'a Path,
    pub signature_page_limit: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferFeeExtractionOutputV1 {
    pub provenance_path: PathBuf,
    pub registry_path: Option<PathBuf>,
    pub artifact: TransferFeeProvenanceArtifactV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectedMintRoleV1 {
    Token0,
    Token1,
    LpMint,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectedMintUseV1 {
    pub pair_address: String,
    pub role: SelectedMintRoleV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectedMintBindingV1 {
    pub mint: String,
    pub selected_uses: Vec<SelectedMintUseV1>,
}

/// Expands every selected market into a deduplicated mint inventory. LP mints
/// are first-class rows, never silently omitted from the shortcut decision.
pub fn selected_mint_bindings(
    markets: &[MarketMintBindingV1],
) -> Result<Vec<SelectedMintBindingV1>, TransferFeeExtractionError> {
    if markets.is_empty() {
        return Err(TransferFeeExtractionError::InvalidBinding(
            "selected market inventory is empty".to_owned(),
        ));
    }
    let mut seen_markets = BTreeSet::new();
    let mut uses_by_mint = BTreeMap::<String, BTreeSet<SelectedMintUseV1>>::new();
    for market in markets {
        market.validate()?;
        if !seen_markets.insert(market.pair_address.clone()) {
            return Err(TransferFeeExtractionError::InvalidBinding(
                "selected market inventory contains a duplicate pair".to_owned(),
            ));
        }
        for (mint, role) in [
            (&market.token0, SelectedMintRoleV1::Token0),
            (&market.token1, SelectedMintRoleV1::Token1),
            (&market.lp_mint, SelectedMintRoleV1::LpMint),
        ] {
            uses_by_mint
                .entry(mint.clone())
                .or_default()
                .insert(SelectedMintUseV1 {
                    pair_address: market.pair_address.clone(),
                    role,
                });
        }
    }
    Ok(uses_by_mint
        .into_iter()
        .map(|(mint, selected_uses)| SelectedMintBindingV1 {
            mint,
            selected_uses: selected_uses.into_iter().collect(),
        })
        .collect())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectedOperationTokenProgramEvidenceV1 {
    pub mint: String,
    pub expected_relevant_operation_count: u64,
    pub classified_relevant_operation_count: u64,
    pub earliest_relevant_slot: Option<u64>,
    pub latest_relevant_slot: Option<u64>,
    pub observed_programs: Vec<TokenProgramKindV1>,
    pub source_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectedOperationTokenProgramEvidenceArtifactV1 {
    pub schema_version: u16,
    pub run_id: String,
    pub cutoff_slot: u64,
    pub decoded_transactions_artifact_sha256: String,
    pub decoded_transactions_jsonl_sha256: String,
    pub decoded_transactions_normalized_sha256: String,
    pub execution_provenance_artifact_sha256: String,
    pub execution_provenance_jsonl_sha256: String,
    pub execution_provenance_normalized_sha256: String,
    pub selected_bindings_sha256: String,
    pub selected_bindings: Vec<SelectedMintBindingV1>,
    pub mint_evidence: Vec<SelectedOperationTokenProgramEvidenceV1>,
    pub artifact_sha256: String,
}

impl SelectedOperationTokenProgramEvidenceArtifactV1 {
    fn seal(mut self) -> Result<Self, TransferFeeExtractionError> {
        self.artifact_sha256.clear();
        self.artifact_sha256 =
            hash_serialized(SELECTED_OPERATION_PROGRAM_EVIDENCE_HASH_DOMAIN, &self)?;
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), TransferFeeExtractionError> {
        if self.schema_version != TRANSFER_FEE_HISTORY_SCHEMA_VERSION
            || self.run_id.trim().is_empty()
            || !valid_sha256(&self.decoded_transactions_artifact_sha256)
            || !valid_sha256(&self.decoded_transactions_jsonl_sha256)
            || !valid_sha256(&self.decoded_transactions_normalized_sha256)
            || !valid_sha256(&self.execution_provenance_artifact_sha256)
            || !valid_sha256(&self.execution_provenance_jsonl_sha256)
            || !valid_sha256(&self.execution_provenance_normalized_sha256)
            || !valid_sha256(&self.selected_bindings_sha256)
            || !valid_sha256(&self.artifact_sha256)
            || self.selected_bindings.is_empty()
        {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "selected operation token-program evidence identity is invalid".to_owned(),
            ));
        }
        validate_selected_binding_inventory(&self.selected_bindings)?;
        if self.selected_bindings_sha256
            != hash_serialized(
                SELECTED_OPERATION_PROGRAM_SOURCE_HASH_DOMAIN,
                &self.selected_bindings,
            )?
            || self.mint_evidence.len() != self.selected_bindings.len()
        {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "selected mint binding hash or evidence count mismatch".to_owned(),
            ));
        }
        for (binding, evidence) in self.selected_bindings.iter().zip(&self.mint_evidence) {
            let slots_valid = match (
                evidence.earliest_relevant_slot,
                evidence.latest_relevant_slot,
                evidence.expected_relevant_operation_count,
            ) {
                (None, None, 0) => true,
                (Some(first), Some(last), count) => {
                    count > 0 && first <= last && last <= self.cutoff_slot
                }
                _ => false,
            };
            if evidence.mint != binding.mint
                || evidence.classified_relevant_operation_count
                    > evidence.expected_relevant_operation_count
                || !slots_valid
                || !strictly_sorted_unique(&evidence.observed_programs)
                || evidence.source_sha256
                    != selected_operation_program_source_sha256(
                        &self.run_id,
                        self.cutoff_slot,
                        &binding.mint,
                        &self.decoded_transactions_artifact_sha256,
                        &self.decoded_transactions_jsonl_sha256,
                        &self.decoded_transactions_normalized_sha256,
                        &self.execution_provenance_artifact_sha256,
                        &self.execution_provenance_jsonl_sha256,
                        &self.execution_provenance_normalized_sha256,
                        &self.selected_bindings_sha256,
                    )?
            {
                return Err(TransferFeeExtractionError::InvalidArtifact(
                    "selected mint token-program evidence is inconsistent".to_owned(),
                ));
            }
        }
        let mut unsigned = self.clone();
        unsigned.artifact_sha256.clear();
        if self.artifact_sha256
            != hash_serialized(SELECTED_OPERATION_PROGRAM_EVIDENCE_HASH_DOMAIN, &unsigned)?
        {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "selected operation token-program evidence self-hash mismatch".to_owned(),
            ));
        }
        Ok(())
    }

    pub fn evidence_for(&self, mint: &str) -> Option<&SelectedOperationTokenProgramEvidenceV1> {
        self.mint_evidence
            .binary_search_by(|row| row.mint.as_str().cmp(mint))
            .ok()
            .map(|index| &self.mint_evidence[index])
    }
}

#[allow(clippy::too_many_arguments)]
pub fn build_selected_operation_token_program_evidence(
    run_id: &str,
    cutoff_slot: u64,
    decoded_transactions_artifact_sha256: &str,
    execution_provenance_artifact_sha256: &str,
    selected_bindings: &[SelectedMintBindingV1],
    decoded_transactions: &[DecodedTransactionV1],
    execution_provenance: &[TransactionExecutionProvenanceV1],
) -> Result<SelectedOperationTokenProgramEvidenceArtifactV1, TransferFeeExtractionError> {
    if run_id.trim().is_empty()
        || !valid_sha256(decoded_transactions_artifact_sha256)
        || !valid_sha256(execution_provenance_artifact_sha256)
    {
        return Err(TransferFeeExtractionError::InvalidBinding(
            "selected operation evidence source identity is invalid".to_owned(),
        ));
    }
    let mut bindings = selected_bindings.to_vec();
    bindings.sort_by(|left, right| left.mint.cmp(&right.mint));
    validate_selected_binding_inventory(&bindings)?;

    let mut transactions = decoded_transactions.to_vec();
    transactions.sort_by(|left, right| {
        (left.slot, left.transaction_index, left.signature.as_str()).cmp(&(
            right.slot,
            right.transaction_index,
            right.signature.as_str(),
        ))
    });
    let mut transaction_orders = BTreeSet::new();
    let mut transaction_signatures = BTreeSet::new();
    for transaction in &transactions {
        if transaction.schema_version != DECODE_SCHEMA_VERSION
            || transaction.slot > cutoff_slot
            || !transaction_orders.insert((transaction.slot, transaction.transaction_index))
            || !transaction_signatures.insert(transaction.signature.clone())
        {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "decoded transaction inventory is invalid or exceeds cutoff".to_owned(),
            ));
        }
    }
    let mut provenance_rows = execution_provenance.to_vec();
    provenance_rows.sort_by(|left, right| {
        (left.slot, left.transaction_index, left.signature.as_str()).cmp(&(
            right.slot,
            right.transaction_index,
            right.signature.as_str(),
        ))
    });
    let mut provenance_by_order = BTreeMap::new();
    let mut provenance_signatures = BTreeSet::new();
    for provenance in &provenance_rows {
        if provenance.schema_version != DECODE_SCHEMA_VERSION
            || provenance.slot > cutoff_slot
            || !provenance_signatures.insert(provenance.signature.clone())
            || provenance_by_order
                .insert((provenance.slot, provenance.transaction_index), provenance)
                .is_some()
        {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "execution provenance inventory is invalid or exceeds cutoff".to_owned(),
            ));
        }
    }
    if transaction_orders != provenance_by_order.keys().copied().collect()
        || transaction_signatures != provenance_signatures
        || transactions.iter().any(|transaction| {
            provenance_by_order
                .get(&(transaction.slot, transaction.transaction_index))
                .is_none_or(|provenance| {
                    provenance.signature != transaction.signature
                        || provenance.decoder_version_id != transaction.decoder_version_id
                })
        })
    {
        return Err(TransferFeeExtractionError::InvalidArtifact(
            "decoded transaction and execution provenance inventories do not match".to_owned(),
        ));
    }

    let decoded_transactions_jsonl_sha256 = canonical_jsonl_sha256(&transactions)?;
    let decoded_transactions_normalized_sha256 = sha256_hex(&serde_json::to_vec(&transactions)?);
    let execution_provenance_jsonl_sha256 = canonical_jsonl_sha256(&provenance_rows)?;
    let execution_provenance_normalized_sha256 = sha256_hex(&serde_json::to_vec(&provenance_rows)?);
    let selected_bindings_sha256 =
        hash_serialized(SELECTED_OPERATION_PROGRAM_SOURCE_HASH_DOMAIN, &bindings)?;
    let mut mint_evidence = Vec::with_capacity(bindings.len());
    for binding in &bindings {
        let mut expected = 0_u64;
        let mut classified = 0_u64;
        let mut earliest = None::<u64>;
        let mut latest = None::<u64>;
        let mut observed_programs = BTreeSet::new();
        for transaction in &transactions {
            // A mixed-mint V1 operation can invoke both token programs. The
            // finalized RPC balance row, not the transaction-wide frame union,
            // is the exact program attribution for this mint/account.
            let relevant_programs = transaction
                .token_balances
                .iter()
                .filter(|balance| {
                    balance.mint == binding.mint && balance.pre_amount != balance.post_amount
                })
                .map(|balance| balance.token_program)
                .collect::<BTreeSet<_>>();
            if relevant_programs.is_empty() {
                continue;
            }
            if relevant_programs.len() != 1 {
                return Err(TransferFeeExtractionError::InvalidArtifact(format!(
                    "changed token-balance rows disagree on the token program for mint {} in transaction {}",
                    binding.mint, transaction.signature
                )));
            }
            let relevant_program = *relevant_programs
                .iter()
                .next()
                .expect("non-empty relevant program set was checked above");
            expected = expected.checked_add(1).ok_or_else(|| {
                TransferFeeExtractionError::InvalidArtifact(
                    "selected operation evidence count overflow".to_owned(),
                )
            })?;
            earliest = Some(earliest.map_or(transaction.slot, |slot| slot.min(transaction.slot)));
            latest = Some(latest.map_or(transaction.slot, |slot| slot.max(transaction.slot)));
            let provenance = provenance_by_order
                .get(&(transaction.slot, transaction.transaction_index))
                .copied()
                .expect("matching inventories were validated above");
            if !provenance.is_complete() {
                continue;
            }
            let frame_programs = provenance
                .frames
                .iter()
                .filter_map(|frame| match frame.program_id.as_str() {
                    LEGACY_TOKEN_PROGRAM_ID => Some(TokenProgramKindV1::LegacyToken),
                    TOKEN_2022_PROGRAM_ID => Some(TokenProgramKindV1::Token2022),
                    _ => None,
                })
                .collect::<BTreeSet<_>>();
            if !frame_programs.contains(&relevant_program) {
                continue;
            }
            classified = classified.checked_add(1).ok_or_else(|| {
                TransferFeeExtractionError::InvalidArtifact(
                    "selected operation classification count overflow".to_owned(),
                )
            })?;
            observed_programs.insert(relevant_program);
        }
        mint_evidence.push(SelectedOperationTokenProgramEvidenceV1 {
            mint: binding.mint.clone(),
            expected_relevant_operation_count: expected,
            classified_relevant_operation_count: classified,
            earliest_relevant_slot: earliest,
            latest_relevant_slot: latest,
            observed_programs: observed_programs.into_iter().collect(),
            source_sha256: selected_operation_program_source_sha256(
                run_id,
                cutoff_slot,
                &binding.mint,
                decoded_transactions_artifact_sha256,
                &decoded_transactions_jsonl_sha256,
                &decoded_transactions_normalized_sha256,
                execution_provenance_artifact_sha256,
                &execution_provenance_jsonl_sha256,
                &execution_provenance_normalized_sha256,
                &selected_bindings_sha256,
            )?,
        });
    }
    SelectedOperationTokenProgramEvidenceArtifactV1 {
        schema_version: TRANSFER_FEE_HISTORY_SCHEMA_VERSION,
        run_id: run_id.to_owned(),
        cutoff_slot,
        decoded_transactions_artifact_sha256: decoded_transactions_artifact_sha256.to_owned(),
        decoded_transactions_jsonl_sha256,
        decoded_transactions_normalized_sha256,
        execution_provenance_artifact_sha256: execution_provenance_artifact_sha256.to_owned(),
        execution_provenance_jsonl_sha256,
        execution_provenance_normalized_sha256,
        selected_bindings_sha256,
        selected_bindings: bindings,
        mint_evidence,
        artifact_sha256: String::new(),
    }
    .seal()
}

/// Streaming equivalent of `build_selected_operation_token_program_evidence`.
/// Both inputs must be compact canonical JSONL in strict chain order and carry
/// exactly one matching provenance row per decoded transaction. Only the small
/// selected-mint accumulator is retained in memory.
#[allow(clippy::too_many_arguments)]
pub fn build_selected_operation_token_program_evidence_from_jsonl(
    run_id: &str,
    cutoff_slot: u64,
    decoded_transactions_artifact_sha256: &str,
    decoded_transactions_jsonl: &Path,
    execution_provenance_artifact_sha256: &str,
    execution_provenance_jsonl: &Path,
    selected_bindings: &[SelectedMintBindingV1],
) -> Result<SelectedOperationTokenProgramEvidenceArtifactV1, TransferFeeExtractionError> {
    if run_id.trim().is_empty()
        || !valid_sha256(decoded_transactions_artifact_sha256)
        || !valid_sha256(execution_provenance_artifact_sha256)
    {
        return Err(TransferFeeExtractionError::InvalidBinding(
            "selected operation evidence source identity is invalid".to_owned(),
        ));
    }
    let mut bindings = selected_bindings.to_vec();
    bindings.sort_by(|left, right| left.mint.cmp(&right.mint));
    validate_selected_binding_inventory(&bindings)?;
    let selected_bindings_sha256 =
        hash_serialized(SELECTED_OPERATION_PROGRAM_SOURCE_HASH_DOMAIN, &bindings)?;

    #[derive(Default)]
    struct Accumulator {
        expected: u64,
        classified: u64,
        earliest: Option<u64>,
        latest: Option<u64>,
        programs: BTreeSet<TokenProgramKindV1>,
    }

    let mut accumulators = bindings
        .iter()
        .map(|binding| (binding.mint.clone(), Accumulator::default()))
        .collect::<BTreeMap<_, _>>();
    let mut decoded_reader = BufReader::new(File::open(decoded_transactions_jsonl)?);
    let mut provenance_reader = BufReader::new(File::open(execution_provenance_jsonl)?);
    let mut decoded_exact = Sha256::new();
    let mut provenance_exact = Sha256::new();
    let mut decoded_normalized = JsonArrayHasher::new();
    let mut provenance_normalized = JsonArrayHasher::new();
    let mut decoded_line = Vec::new();
    let mut provenance_line = Vec::new();
    let mut previous_order = None::<(u64, u32)>;
    let mut signatures = BTreeSet::new();

    loop {
        decoded_line.clear();
        provenance_line.clear();
        let decoded_bytes = decoded_reader.read_until(b'\n', &mut decoded_line)?;
        let provenance_bytes = provenance_reader.read_until(b'\n', &mut provenance_line)?;
        match (decoded_bytes, provenance_bytes) {
            (0, 0) => break,
            (0, _) | (_, 0) => {
                return Err(TransferFeeExtractionError::InvalidArtifact(
                    "decoded transaction and execution provenance JSONL lengths differ".to_owned(),
                ));
            }
            _ => {}
        }
        decoded_exact.update(&decoded_line);
        provenance_exact.update(&provenance_line);
        let (transaction, transaction_json) =
            parse_canonical_jsonl_row::<DecodedTransactionV1>(&decoded_line)?;
        let (provenance, provenance_json) =
            parse_canonical_jsonl_row::<TransactionExecutionProvenanceV1>(&provenance_line)?;
        decoded_normalized.push(&transaction_json);
        provenance_normalized.push(&provenance_json);

        let order = (transaction.slot, transaction.transaction_index);
        if transaction.schema_version != DECODE_SCHEMA_VERSION
            || provenance.schema_version != DECODE_SCHEMA_VERSION
            || transaction.slot > cutoff_slot
            || previous_order.is_some_and(|previous| order <= previous)
            || !signatures.insert(transaction.signature.clone())
            || provenance.slot != transaction.slot
            || provenance.transaction_index != transaction.transaction_index
            || provenance.signature != transaction.signature
            || provenance.decoder_version_id != transaction.decoder_version_id
        {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "canonical JSONL order, identity, decoder, or cutoff binding is invalid".to_owned(),
            ));
        }
        previous_order = Some(order);

        let frame_programs = provenance
            .frames
            .iter()
            .filter_map(|frame| match frame.program_id.as_str() {
                LEGACY_TOKEN_PROGRAM_ID => Some(TokenProgramKindV1::LegacyToken),
                TOKEN_2022_PROGRAM_ID => Some(TokenProgramKindV1::Token2022),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        // Merge by mint from exact per-account RPC program identities. CPI
        // frames only confirm that the attributed program actually executed.
        let mut changed_mint_programs = BTreeMap::<&str, TokenProgramKindV1>::new();
        for balance in transaction
            .token_balances
            .iter()
            .filter(|balance| balance.pre_amount != balance.post_amount)
        {
            if let Some(previous) =
                changed_mint_programs.insert(balance.mint.as_str(), balance.token_program)
            {
                if previous != balance.token_program {
                    return Err(TransferFeeExtractionError::InvalidArtifact(format!(
                        "changed token-balance rows disagree on the token program for mint {} in transaction {}",
                        balance.mint, transaction.signature
                    )));
                }
            }
        }
        for (mint, token_program) in changed_mint_programs {
            let Some(accumulator) = accumulators.get_mut(mint) else {
                continue;
            };
            accumulator.expected = accumulator.expected.checked_add(1).ok_or_else(|| {
                TransferFeeExtractionError::InvalidArtifact(
                    "selected operation evidence count overflow".to_owned(),
                )
            })?;
            accumulator.earliest = Some(
                accumulator
                    .earliest
                    .map_or(transaction.slot, |slot| slot.min(transaction.slot)),
            );
            accumulator.latest = Some(
                accumulator
                    .latest
                    .map_or(transaction.slot, |slot| slot.max(transaction.slot)),
            );
            if provenance.is_complete() && frame_programs.contains(&token_program) {
                accumulator.classified =
                    accumulator.classified.checked_add(1).ok_or_else(|| {
                        TransferFeeExtractionError::InvalidArtifact(
                            "selected operation classification count overflow".to_owned(),
                        )
                    })?;
                accumulator.programs.insert(token_program);
            }
        }
    }

    let decoded_transactions_jsonl_sha256 = hex::encode(decoded_exact.finalize());
    let execution_provenance_jsonl_sha256 = hex::encode(provenance_exact.finalize());
    let decoded_transactions_normalized_sha256 = decoded_normalized.finish();
    let execution_provenance_normalized_sha256 = provenance_normalized.finish();
    let mint_evidence = bindings
        .iter()
        .map(|binding| {
            let accumulator = accumulators
                .remove(&binding.mint)
                .expect("accumulator was initialized from selected bindings");
            Ok(SelectedOperationTokenProgramEvidenceV1 {
                mint: binding.mint.clone(),
                expected_relevant_operation_count: accumulator.expected,
                classified_relevant_operation_count: accumulator.classified,
                earliest_relevant_slot: accumulator.earliest,
                latest_relevant_slot: accumulator.latest,
                observed_programs: accumulator.programs.into_iter().collect(),
                source_sha256: selected_operation_program_source_sha256(
                    run_id,
                    cutoff_slot,
                    &binding.mint,
                    decoded_transactions_artifact_sha256,
                    &decoded_transactions_jsonl_sha256,
                    &decoded_transactions_normalized_sha256,
                    execution_provenance_artifact_sha256,
                    &execution_provenance_jsonl_sha256,
                    &execution_provenance_normalized_sha256,
                    &selected_bindings_sha256,
                )?,
            })
        })
        .collect::<Result<Vec<_>, TransferFeeExtractionError>>()?;
    SelectedOperationTokenProgramEvidenceArtifactV1 {
        schema_version: TRANSFER_FEE_HISTORY_SCHEMA_VERSION,
        run_id: run_id.to_owned(),
        cutoff_slot,
        decoded_transactions_artifact_sha256: decoded_transactions_artifact_sha256.to_owned(),
        decoded_transactions_jsonl_sha256,
        decoded_transactions_normalized_sha256,
        execution_provenance_artifact_sha256: execution_provenance_artifact_sha256.to_owned(),
        execution_provenance_jsonl_sha256,
        execution_provenance_normalized_sha256,
        selected_bindings_sha256,
        selected_bindings: bindings,
        mint_evidence,
        artifact_sha256: String::new(),
    }
    .seal()
}

struct JsonArrayHasher {
    digest: Sha256,
    empty: bool,
}

impl JsonArrayHasher {
    fn new() -> Self {
        let mut digest = Sha256::new();
        digest.update(b"[");
        Self {
            digest,
            empty: true,
        }
    }

    fn push(&mut self, canonical_json: &[u8]) {
        if !self.empty {
            self.digest.update(b",");
        }
        self.digest.update(canonical_json);
        self.empty = false;
    }

    fn finish(mut self) -> String {
        self.digest.update(b"]");
        hex::encode(self.digest.finalize())
    }
}

fn parse_canonical_jsonl_row<T>(line: &[u8]) -> Result<(T, Vec<u8>), TransferFeeExtractionError>
where
    T: DeserializeOwned + Serialize,
{
    let Some(json) = line.strip_suffix(b"\n") else {
        return Err(TransferFeeExtractionError::InvalidArtifact(
            "canonical JSONL row lacks its required newline terminator".to_owned(),
        ));
    };
    if json.ends_with(b"\r") || json.is_empty() {
        return Err(TransferFeeExtractionError::InvalidArtifact(
            "canonical JSONL row is empty or uses a non-canonical line ending".to_owned(),
        ));
    }
    let row: T = serde_json::from_slice(json)?;
    let canonical = serde_json::to_vec(&row)?;
    if canonical != json {
        return Err(TransferFeeExtractionError::InvalidArtifact(
            "JSONL row is not compact canonical serde JSON".to_owned(),
        ));
    }
    Ok((row, canonical))
}

#[allow(clippy::too_many_arguments)]
fn selected_operation_program_source_sha256(
    run_id: &str,
    cutoff_slot: u64,
    mint: &str,
    decoded_transactions_artifact_sha256: &str,
    decoded_transactions_jsonl_sha256: &str,
    decoded_transactions_normalized_sha256: &str,
    execution_provenance_artifact_sha256: &str,
    execution_provenance_jsonl_sha256: &str,
    execution_provenance_normalized_sha256: &str,
    selected_bindings_sha256: &str,
) -> Result<String, TransferFeeExtractionError> {
    hash_serialized(
        SELECTED_OPERATION_PROGRAM_SOURCE_HASH_DOMAIN,
        &(
            run_id,
            cutoff_slot,
            mint,
            decoded_transactions_artifact_sha256,
            decoded_transactions_jsonl_sha256,
            decoded_transactions_normalized_sha256,
            execution_provenance_artifact_sha256,
            execution_provenance_jsonl_sha256,
            execution_provenance_normalized_sha256,
            selected_bindings_sha256,
        ),
    )
}

fn validate_selected_binding_inventory(
    bindings: &[SelectedMintBindingV1],
) -> Result<(), TransferFeeExtractionError> {
    if bindings.is_empty()
        || !bindings
            .windows(2)
            .all(|bindings| bindings[0].mint < bindings[1].mint)
        || bindings.iter().any(|binding| {
            parse_pubkey(&binding.mint).is_err()
                || binding.selected_uses.is_empty()
                || !strictly_sorted_unique(&binding.selected_uses)
                || binding
                    .selected_uses
                    .iter()
                    .any(|use_| use_.pair_address.trim().is_empty())
        })
    {
        return Err(TransferFeeExtractionError::InvalidBinding(
            "selected mint binding inventory is invalid".to_owned(),
        ));
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct SelectedMintProofRequestV1<'a> {
    pub run_id: &'a str,
    pub cutoff_slot: u64,
    pub provider: &'a SanitizedSource,
    pub evidence_tier: EvidenceTier,
    pub binding: &'a SelectedMintBindingV1,
    pub operation_program_evidence: &'a SelectedOperationTokenProgramEvidenceV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionGradeLegacyMintProofBasisV1 {
    FinalizedPostCutoffInitializedImmutableLegacyMint,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionGradeLegacyMintProofV1 {
    pub schema_version: u16,
    pub run_id: String,
    pub cutoff_slot: u64,
    pub evidence_tier: EvidenceTier,
    pub proof_basis: DecisionGradeLegacyMintProofBasisV1,
    pub provider: SanitizedSource,
    pub mint: String,
    pub selected_uses: Vec<SelectedMintUseV1>,
    pub commitment: String,
    pub rpc_method: String,
    pub canonical_request_sha256: String,
    pub raw_response_sha256: String,
    /// This finalized observation can be newer than the cutoff. It is never
    /// represented as historical account state at the cutoff.
    pub observed_context_slot: u64,
    pub account_owner_program: String,
    pub account_lamports: u64,
    pub account_executable: bool,
    pub account_rent_epoch: u64,
    /// Canonical lowercase encoding retained so standalone validation can
    /// re-decode decimals and recompute the raw-data hash.
    pub account_data_hex: String,
    pub account_data_sha256: String,
    pub decimals: u8,
    pub mint_initialized: bool,
    pub expected_relevant_operation_count: u64,
    pub classified_relevant_operation_count: u64,
    pub earliest_relevant_slot: u64,
    pub latest_relevant_slot: u64,
    pub observed_operation_programs: Vec<TokenProgramKindV1>,
    pub operation_program_evidence_sha256: String,
    pub artifact_sha256: String,
}

impl DecisionGradeLegacyMintProofV1 {
    fn seal(mut self) -> Result<Self, TransferFeeExtractionError> {
        self.artifact_sha256.clear();
        self.artifact_sha256 = hash_serialized(DECISION_GRADE_LEGACY_MINT_HASH_DOMAIN, &self)?;
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), TransferFeeExtractionError> {
        if self.schema_version != TRANSFER_FEE_HISTORY_SCHEMA_VERSION
            || self.run_id.trim().is_empty()
            || self.evidence_tier != EvidenceTier::DecisionGrade
            || self.proof_basis
                != DecisionGradeLegacyMintProofBasisV1::FinalizedPostCutoffInitializedImmutableLegacyMint
            || self.commitment != "finalized"
            || self.rpc_method != "getAccountInfo"
            || self.observed_context_slot < self.cutoff_slot
            || self.account_owner_program != LEGACY_TOKEN_PROGRAM_ID
            || self.account_lamports == 0
            || self.account_executable
            || !self.mint_initialized
            || self.expected_relevant_operation_count == 0
            || self.expected_relevant_operation_count != self.classified_relevant_operation_count
            || self.earliest_relevant_slot > self.latest_relevant_slot
            || self.latest_relevant_slot > self.cutoff_slot
            || self.observed_operation_programs != [TokenProgramKindV1::LegacyToken]
            || !valid_sha256(&self.canonical_request_sha256)
            || !valid_sha256(&self.raw_response_sha256)
            || !valid_sha256(&self.account_data_sha256)
            || !valid_sha256(&self.operation_program_evidence_sha256)
            || !valid_sha256(&self.artifact_sha256)
            || !valid_sanitized_provider(&self.provider)
            || parse_pubkey(&self.mint).is_err()
            || self.selected_uses.is_empty()
            || !strictly_sorted_unique(&self.selected_uses)
            || self
                .selected_uses
                .iter()
                .any(|use_| use_.pair_address.trim().is_empty())
        {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "decision-grade legacy mint proof identity is invalid".to_owned(),
            ));
        }
        if self.canonical_request_sha256
            != finalized_account_info_request_sha256(&self.mint, self.cutoff_slot).map_err(
                |kind| TransferFeeExtractionError::Rpc {
                    operation: "build_finalized_account_request",
                    kind,
                },
            )?
        {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "decision-grade legacy mint request hash mismatch".to_owned(),
            ));
        }
        let account_data = hex::decode(&self.account_data_hex).map_err(|_| {
            TransferFeeExtractionError::InvalidArtifact(
                "decision-grade legacy mint data is not hex".to_owned(),
            )
        })?;
        if hex::encode(&account_data) != self.account_data_hex
            || account_data.len() != SplTokenMint::LEN
            || sha256_hex(&account_data) != self.account_data_sha256
        {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "decision-grade legacy mint raw-data binding is invalid".to_owned(),
            ));
        }
        let mint = SplTokenMint::unpack(&account_data).map_err(|_| {
            TransferFeeExtractionError::InvalidArtifact(
                "decision-grade legacy mint state cannot be decoded".to_owned(),
            )
        })?;
        if !mint.is_initialized || mint.decimals != self.decimals {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "decision-grade legacy mint decoded fields mismatch".to_owned(),
            ));
        }
        let mut unsigned = self.clone();
        unsigned.artifact_sha256.clear();
        if self.artifact_sha256
            != hash_serialized(DECISION_GRADE_LEGACY_MINT_HASH_DOMAIN, &unsigned)?
        {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "decision-grade legacy mint proof self-hash mismatch".to_owned(),
            ));
        }
        Ok(())
    }

    /// The immutable legacy program cannot levy a transfer fee, so a valid
    /// proof yields an exact zero-fee schedule without reconstructing mint
    /// address history. Its source identifier is explicitly not a transaction.
    pub fn zero_fee_schedule(
        &self,
    ) -> Result<MintTransferFeeScheduleV1, TransferFeeExtractionError> {
        self.validate()?;
        let schedule = MintTransferFeeScheduleV1 {
            schema_version: TRANSFER_FEE_SCHEMA_VERSION,
            mint: self.mint.clone(),
            token_program: TokenProgramKindV1::LegacyToken,
            epochs: vec![TransferFeeEpochV1 {
                effective_epoch: 0,
                transfer_fee_basis_points: 0,
                maximum_fee: TokenAmount::zero(),
                source_signature: format!(
                    "decision-grade-legacy-account-proof:{}",
                    self.artifact_sha256
                ),
                source_slot: self.cutoff_slot,
                decoded_instruction_sha256: self.account_data_sha256.clone(),
            }],
        };
        schedule.validate()?;
        Ok(schedule)
    }
}

fn decision_grade_decoded_mint_sha256(
    proof: &DecisionGradeLegacyMintProofV1,
) -> Result<String, TransferFeeExtractionError> {
    hash_serialized(
        DECISION_GRADE_DECODED_MINT_HASH_DOMAIN,
        &(
            proof.run_id.as_str(),
            proof.cutoff_slot,
            proof.mint.as_str(),
            proof.decimals,
            proof.account_data_sha256.as_str(),
            proof.operation_program_evidence_sha256.as_str(),
            proof.artifact_sha256.as_str(),
        ),
    )
}

fn decision_grade_legacy_mint_history(
    proof: &DecisionGradeLegacyMintProofV1,
) -> Result<MintTransferFeeHistoryV1, TransferFeeExtractionError> {
    proof.validate()?;
    Ok(MintTransferFeeHistoryV1 {
        mint: proof.mint.clone(),
        proof_basis: MintTransferFeeProofBasisV1::DecisionGradePostCutoffImmutableLegacy {
            proof: proof.clone(),
        },
        history_root_proven: false,
        signature_page_count: 0,
        signatures_through_cutoff: 0,
        successful_signatures_through_cutoff: 0,
        failed_signatures_through_cutoff: 0,
        block_slots_expected: 0,
        block_slots_fetched: 0,
        token_program: Some(TokenProgramKindV1::LegacyToken),
        decimals: Some(proof.decimals),
        initialized_token_2022_extensions: Some(Vec::new()),
        transfer_fee_config_ever_exists: Some(false),
        initialization_signature: None,
        initialization_slot: None,
        decoded_mint_history_sha256: decision_grade_decoded_mint_sha256(proof)?,
        transfer_fee_epoch_count: 1,
        signature_page_artifacts: Vec::new(),
        block_artifacts: Vec::new(),
        blockers: Vec::new(),
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionGradeLegacyMintProofSetV1 {
    pub schema_version: u16,
    pub run_id: String,
    pub cutoff_slot: u64,
    pub provider: SanitizedSource,
    pub operation_evidence_artifact_sha256: String,
    pub proofs: Vec<DecisionGradeLegacyMintProofV1>,
    pub artifact_sha256: String,
}

impl DecisionGradeLegacyMintProofSetV1 {
    fn seal(mut self) -> Result<Self, TransferFeeExtractionError> {
        self.proofs
            .sort_by(|left, right| left.mint.cmp(&right.mint));
        self.artifact_sha256.clear();
        self.artifact_sha256 =
            hash_serialized(DECISION_GRADE_LEGACY_MINT_PROOF_SET_HASH_DOMAIN, &self)?;
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), TransferFeeExtractionError> {
        if self.schema_version != TRANSFER_FEE_HISTORY_SCHEMA_VERSION
            || self.run_id.trim().is_empty()
            || !valid_sanitized_provider(&self.provider)
            || !valid_sha256(&self.operation_evidence_artifact_sha256)
            || !valid_sha256(&self.artifact_sha256)
            || !strictly_sorted_unique(
                &self
                    .proofs
                    .iter()
                    .map(|proof| proof.mint.clone())
                    .collect::<Vec<_>>(),
            )
        {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "decision-grade legacy mint proof-set identity is invalid".to_owned(),
            ));
        }
        for proof in &self.proofs {
            proof.validate()?;
            if proof.run_id != self.run_id
                || proof.cutoff_slot != self.cutoff_slot
                || proof.provider != self.provider
            {
                return Err(TransferFeeExtractionError::InvalidArtifact(
                    "legacy mint proof belongs to a different run, cutoff, or provider".to_owned(),
                ));
            }
        }
        let mut unsigned = self.clone();
        unsigned.artifact_sha256.clear();
        if self.artifact_sha256
            != hash_serialized(DECISION_GRADE_LEGACY_MINT_PROOF_SET_HASH_DOMAIN, &unsigned)?
        {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "decision-grade legacy mint proof-set self-hash mismatch".to_owned(),
            ));
        }
        Ok(())
    }

    pub fn validate_against_operation_evidence(
        &self,
        evidence: &SelectedOperationTokenProgramEvidenceArtifactV1,
    ) -> Result<(), TransferFeeExtractionError> {
        self.validate()?;
        evidence.validate()?;
        if self.run_id != evidence.run_id
            || self.cutoff_slot != evidence.cutoff_slot
            || self.operation_evidence_artifact_sha256 != evidence.artifact_sha256
        {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "legacy proof set and selected-operation evidence identity differ".to_owned(),
            ));
        }
        for proof in &self.proofs {
            let row = evidence.evidence_for(&proof.mint).ok_or_else(|| {
                TransferFeeExtractionError::InvalidArtifact(
                    "legacy proof mint is absent from selected-operation evidence".to_owned(),
                )
            })?;
            if proof.expected_relevant_operation_count != row.expected_relevant_operation_count
                || proof.classified_relevant_operation_count
                    != row.classified_relevant_operation_count
                || Some(proof.earliest_relevant_slot) != row.earliest_relevant_slot
                || Some(proof.latest_relevant_slot) != row.latest_relevant_slot
                || proof.observed_operation_programs != row.observed_programs
                || proof.operation_program_evidence_sha256 != row.source_sha256
            {
                return Err(TransferFeeExtractionError::InvalidArtifact(
                    "legacy proof and selected-operation mint evidence differ".to_owned(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectedMintFullHistoryReasonV1 {
    AuditGradeRequiresFullHistory,
    OperationProgramEvidenceIncomplete,
    OperationProgramEvidenceNotLegacyOnly,
    Token2022AccountOwner,
    UnsupportedAccountOwner,
    ExecutableAccount,
    InvalidLegacyMintState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "resolution", rename_all = "snake_case", deny_unknown_fields)]
pub enum SelectedMintTransferFeeResolutionV1 {
    DecisionGradeLegacy {
        proof: DecisionGradeLegacyMintProofV1,
    },
    FullHistoryRequired {
        mint: String,
        reason: SelectedMintFullHistoryReasonV1,
    },
}

pub trait FinalizedMintAccountRpc: Send + Sync {
    fn finalized_account_at_or_after_cutoff<'a>(
        &'a self,
        mint: &'a str,
        cutoff_slot: u64,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<
                        BoundRpcResponseProofV1<FinalizedAccountInfoV1>,
                        RpcFailureKind,
                    >,
                > + Send
                + 'a,
        >,
    >;
}

impl FinalizedMintAccountRpc for HttpArchivalRpc {
    fn finalized_account_at_or_after_cutoff<'a>(
        &'a self,
        mint: &'a str,
        cutoff_slot: u64,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<
                        BoundRpcResponseProofV1<FinalizedAccountInfoV1>,
                        RpcFailureKind,
                    >,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            self.finalized_account_info_at_or_after_slot_with_proof(mint, cutoff_slot)
                .await
        })
    }
}

/// Resolves one selected mint. Only a complete DecisionGrade legacy proof can
/// bypass the existing history crawl; all other outcomes are explicit routes
/// back to `extract_transfer_fee_provenance`.
pub async fn resolve_selected_mint_transfer_fee_evidence<R: FinalizedMintAccountRpc>(
    rpc: &R,
    request: &SelectedMintProofRequestV1<'_>,
) -> Result<SelectedMintTransferFeeResolutionV1, TransferFeeExtractionError> {
    validate_selected_mint_proof_request(request)?;
    if request.evidence_tier == EvidenceTier::AuditGrade {
        return Ok(full_history(
            request.binding,
            SelectedMintFullHistoryReasonV1::AuditGradeRequiresFullHistory,
        ));
    }
    let operation_evidence = request.operation_program_evidence;
    let (Some(earliest_relevant_slot), Some(latest_relevant_slot)) = (
        operation_evidence.earliest_relevant_slot,
        operation_evidence.latest_relevant_slot,
    ) else {
        return Ok(full_history(
            request.binding,
            SelectedMintFullHistoryReasonV1::OperationProgramEvidenceIncomplete,
        ));
    };
    if operation_evidence.expected_relevant_operation_count == 0
        || operation_evidence.expected_relevant_operation_count
            != operation_evidence.classified_relevant_operation_count
        || earliest_relevant_slot > latest_relevant_slot
        || latest_relevant_slot > request.cutoff_slot
    {
        return Ok(full_history(
            request.binding,
            SelectedMintFullHistoryReasonV1::OperationProgramEvidenceIncomplete,
        ));
    }
    if operation_evidence.observed_programs != [TokenProgramKindV1::LegacyToken] {
        return Ok(full_history(
            request.binding,
            SelectedMintFullHistoryReasonV1::OperationProgramEvidenceNotLegacyOnly,
        ));
    }

    let response = rpc
        .finalized_account_at_or_after_cutoff(&request.binding.mint, request.cutoff_slot)
        .await
        .map_err(|kind| TransferFeeExtractionError::Rpc {
            operation: "get_finalized_post_cutoff_mint_account",
            kind,
        })?;
    let expected_request_sha256 =
        finalized_account_info_request_sha256(&request.binding.mint, request.cutoff_slot).map_err(
            |kind| TransferFeeExtractionError::Rpc {
                operation: "build_finalized_account_request",
                kind,
            },
        )?;
    if response.canonical_request_sha256 != expected_request_sha256
        || !valid_sha256(&response.raw_response_sha256)
        || response.value.context_slot < request.cutoff_slot
        || response.value.account_data_sha256 != sha256_hex(&response.value.data)
    {
        return Err(TransferFeeExtractionError::InvalidArtifact(
            "post-cutoff finalized account response binding is invalid".to_owned(),
        ));
    }
    let account = response.value;
    let reason = if account.owner == TOKEN_2022_PROGRAM_ID {
        Some(SelectedMintFullHistoryReasonV1::Token2022AccountOwner)
    } else if account.owner != LEGACY_TOKEN_PROGRAM_ID {
        Some(SelectedMintFullHistoryReasonV1::UnsupportedAccountOwner)
    } else if account.executable {
        Some(SelectedMintFullHistoryReasonV1::ExecutableAccount)
    } else if account.lamports == 0 || account.data.len() != SplTokenMint::LEN {
        Some(SelectedMintFullHistoryReasonV1::InvalidLegacyMintState)
    } else {
        None
    };
    if let Some(reason) = reason {
        return Ok(full_history(request.binding, reason));
    }
    let mint_state = match SplTokenMint::unpack(&account.data) {
        Ok(mint) if mint.is_initialized => mint,
        _ => {
            return Ok(full_history(
                request.binding,
                SelectedMintFullHistoryReasonV1::InvalidLegacyMintState,
            ));
        }
    };
    let proof = DecisionGradeLegacyMintProofV1 {
        schema_version: TRANSFER_FEE_HISTORY_SCHEMA_VERSION,
        run_id: request.run_id.to_owned(),
        cutoff_slot: request.cutoff_slot,
        evidence_tier: request.evidence_tier,
        proof_basis:
            DecisionGradeLegacyMintProofBasisV1::FinalizedPostCutoffInitializedImmutableLegacyMint,
        provider: request.provider.clone(),
        mint: request.binding.mint.clone(),
        selected_uses: request.binding.selected_uses.clone(),
        commitment: "finalized".to_owned(),
        rpc_method: "getAccountInfo".to_owned(),
        canonical_request_sha256: response.canonical_request_sha256,
        raw_response_sha256: response.raw_response_sha256,
        observed_context_slot: account.context_slot,
        account_owner_program: account.owner,
        account_lamports: account.lamports,
        account_executable: account.executable,
        account_rent_epoch: account.rent_epoch,
        account_data_hex: hex::encode(&account.data),
        account_data_sha256: account.account_data_sha256,
        decimals: mint_state.decimals,
        mint_initialized: mint_state.is_initialized,
        expected_relevant_operation_count: operation_evidence.expected_relevant_operation_count,
        classified_relevant_operation_count: operation_evidence.classified_relevant_operation_count,
        earliest_relevant_slot,
        latest_relevant_slot,
        observed_operation_programs: operation_evidence.observed_programs.clone(),
        operation_program_evidence_sha256: operation_evidence.source_sha256.clone(),
        artifact_sha256: String::new(),
    }
    .seal()?;
    Ok(SelectedMintTransferFeeResolutionV1::DecisionGradeLegacy { proof })
}

fn validate_selected_mint_proof_request(
    request: &SelectedMintProofRequestV1<'_>,
) -> Result<(), TransferFeeExtractionError> {
    if request.run_id.trim().is_empty()
        || !valid_sanitized_provider(request.provider)
        || parse_pubkey(&request.binding.mint).is_err()
        || request.binding.selected_uses.is_empty()
        || !strictly_sorted_unique(&request.binding.selected_uses)
        || request
            .binding
            .selected_uses
            .iter()
            .any(|use_| use_.pair_address.trim().is_empty())
        || request.operation_program_evidence.mint != request.binding.mint
        || !valid_sha256(&request.operation_program_evidence.source_sha256)
        || !request
            .operation_program_evidence
            .observed_programs
            .windows(2)
            .all(|programs| programs[0] < programs[1])
    {
        return Err(TransferFeeExtractionError::InvalidBinding(
            "selected mint proof request is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn full_history(
    binding: &SelectedMintBindingV1,
    reason: SelectedMintFullHistoryReasonV1,
) -> SelectedMintTransferFeeResolutionV1 {
    SelectedMintTransferFeeResolutionV1::FullHistoryRequired {
        mint: binding.mint.clone(),
        reason,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectedMintFullHistoryRequirementV1 {
    pub mint: String,
    pub reason: SelectedMintFullHistoryReasonV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectedMintResolutionSetV1 {
    pub legacy_proofs: Vec<DecisionGradeLegacyMintProofV1>,
    pub legacy_zero_fee_registry: Option<TransferFeeRegistryV1>,
    /// Exact inventory that must be passed to the existing complete history
    /// crawl. Resolved legacy mints are intentionally absent.
    pub full_history_mints: Vec<SelectedMintFullHistoryRequirementV1>,
}

#[derive(Clone, Debug)]
pub struct SelectedScopeTransferFeeExtractionRequestV1<'a> {
    pub run_id: &'a str,
    pub cutoff_slot: u64,
    pub provider: &'a SanitizedSource,
    pub evidence_tier: EvidenceTier,
    pub markets: &'a [MarketMintBindingV1],
    pub output_dir: &'a Path,
    pub signature_page_limit: u16,
    pub decoded_transactions_jsonl: &'a Path,
    pub decoded_transactions_artifact_sha256: &'a str,
    pub execution_provenance_jsonl: &'a Path,
    pub execution_provenance_artifact_sha256: &'a str,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SelectedScopeTransferFeeExtractionOutputV1 {
    pub operation_evidence_path: PathBuf,
    pub legacy_proofs_path: PathBuf,
    pub fallback_provenance_path: Option<PathBuf>,
    pub provenance_path: PathBuf,
    pub registry_path: Option<PathBuf>,
    pub operation_evidence: SelectedOperationTokenProgramEvidenceArtifactV1,
    pub legacy_proof_set: DecisionGradeLegacyMintProofSetV1,
    pub artifact: TransferFeeProvenanceArtifactV1,
}

#[allow(clippy::too_many_arguments)]
pub async fn resolve_selected_mint_set<R: FinalizedMintAccountRpc>(
    rpc: &R,
    run_id: &str,
    cutoff_slot: u64,
    provider: &SanitizedSource,
    evidence_tier: EvidenceTier,
    epoch_schedule: &EpochScheduleV1,
    operation_evidence: &SelectedOperationTokenProgramEvidenceArtifactV1,
) -> Result<SelectedMintResolutionSetV1, TransferFeeExtractionError> {
    operation_evidence.validate()?;
    epoch_schedule.validate()?;
    if operation_evidence.run_id != run_id || operation_evidence.cutoff_slot != cutoff_slot {
        return Err(TransferFeeExtractionError::InvalidBinding(
            "selected mint set belongs to a different run or cutoff".to_owned(),
        ));
    }
    let mut legacy_proofs = Vec::new();
    let mut legacy_schedules = Vec::new();
    let mut full_history_mints = Vec::new();
    for binding in &operation_evidence.selected_bindings {
        let mint_evidence = operation_evidence
            .evidence_for(&binding.mint)
            .ok_or_else(|| {
                TransferFeeExtractionError::InvalidArtifact(
                    "selected mint has no operation program evidence".to_owned(),
                )
            })?;
        match resolve_selected_mint_transfer_fee_evidence(
            rpc,
            &SelectedMintProofRequestV1 {
                run_id,
                cutoff_slot,
                provider,
                evidence_tier,
                binding,
                operation_program_evidence: mint_evidence,
            },
        )
        .await?
        {
            SelectedMintTransferFeeResolutionV1::DecisionGradeLegacy { proof } => {
                legacy_schedules.push(proof.zero_fee_schedule()?);
                legacy_proofs.push(proof);
            }
            SelectedMintTransferFeeResolutionV1::FullHistoryRequired { mint, reason } => {
                full_history_mints.push(SelectedMintFullHistoryRequirementV1 { mint, reason });
            }
        }
    }
    let legacy_zero_fee_registry = if legacy_schedules.is_empty() {
        None
    } else {
        let registry = TransferFeeRegistryV1 {
            schema_version: TRANSFER_FEE_SCHEMA_VERSION,
            epoch_schedule: epoch_schedule.clone(),
            mints: legacy_schedules,
        };
        registry.clone().validate()?;
        Some(registry)
    };
    Ok(SelectedMintResolutionSetV1 {
        legacy_proofs,
        legacy_zero_fee_registry,
        full_history_mints,
    })
}

/// End-to-end selected-scope transfer-fee extraction. The full finalized mint
/// history crawler is invoked only for the explicit fallback inventory. The
/// canonical output remains the same `TransferFeeProvenanceArtifactV1` and one
/// merged `TransferFeeRegistryV1` consumed by audit and replay.
pub async fn extract_selected_scope_transfer_fee_provenance<R>(
    rpc: &R,
    request: &SelectedScopeTransferFeeExtractionRequestV1<'_>,
) -> Result<SelectedScopeTransferFeeExtractionOutputV1, TransferFeeExtractionError>
where
    R: TransferFeeHistoryRpc + FinalizedMintAccountRpc,
{
    let base_request = TransferFeeExtractionRequestV1 {
        run_id: request.run_id,
        cutoff_slot: request.cutoff_slot,
        provider: request.provider,
        markets: request.markets,
        output_dir: request.output_dir,
        signature_page_limit: request.signature_page_limit,
    };
    let normalized_markets = validate_request(&base_request)?;
    let bindings = selected_mint_bindings(request.markets)?;
    fs::create_dir_all(request.output_dir)?;

    let operation_evidence = build_selected_operation_token_program_evidence_from_jsonl(
        request.run_id,
        request.cutoff_slot,
        request.decoded_transactions_artifact_sha256,
        request.decoded_transactions_jsonl,
        request.execution_provenance_artifact_sha256,
        request.execution_provenance_jsonl,
        &bindings,
    )?;
    let operation_evidence_path = request
        .output_dir
        .join(SELECTED_OPERATION_TOKEN_PROGRAM_EVIDENCE_JSON);
    write_or_validate_json(&operation_evidence_path, &operation_evidence)?;
    let operation_evidence_checksum = checksum_path(
        &operation_evidence_path,
        SELECTED_OPERATION_TOKEN_PROGRAM_EVIDENCE_JSON.to_owned(),
    )?;

    let history_root = request.output_dir.join(TRANSFER_FEE_HISTORY_DIR);
    fs::create_dir_all(&history_root)?;
    let epoch_path = history_root.join("epoch-schedule.json");
    let epoch_artifact = load_or_fetch_epoch_schedule(rpc, &base_request, &epoch_path).await?;
    let epoch_checksum = checksum_path(
        &epoch_path,
        format!("{TRANSFER_FEE_HISTORY_DIR}/epoch-schedule.json"),
    )?;
    let resolutions = resolve_selected_mint_set(
        rpc,
        request.run_id,
        request.cutoff_slot,
        request.provider,
        request.evidence_tier,
        &epoch_artifact.schedule,
        &operation_evidence,
    )
    .await?;
    let legacy_proof_set = DecisionGradeLegacyMintProofSetV1 {
        schema_version: TRANSFER_FEE_HISTORY_SCHEMA_VERSION,
        run_id: request.run_id.to_owned(),
        cutoff_slot: request.cutoff_slot,
        provider: request.provider.clone(),
        operation_evidence_artifact_sha256: operation_evidence.artifact_sha256.clone(),
        proofs: resolutions.legacy_proofs.clone(),
        artifact_sha256: String::new(),
    }
    .seal()?;
    legacy_proof_set.validate_against_operation_evidence(&operation_evidence)?;
    let legacy_proofs_path = request
        .output_dir
        .join(DECISION_GRADE_LEGACY_MINT_PROOFS_JSON);
    write_or_validate_json(&legacy_proofs_path, &legacy_proof_set)?;
    let legacy_proofs_checksum = checksum_path(
        &legacy_proofs_path,
        DECISION_GRADE_LEGACY_MINT_PROOFS_JSON.to_owned(),
    )?;

    let mut histories = legacy_proof_set
        .proofs
        .iter()
        .map(decision_grade_legacy_mint_history)
        .collect::<Result<Vec<_>, _>>()?;
    let mut schedules = resolutions
        .legacy_zero_fee_registry
        .as_ref()
        .map_or_else(Vec::new, |registry| registry.mints.clone());
    let fallback_provenance_path = if resolutions.full_history_mints.is_empty() {
        None
    } else {
        let fallback_output_dir = request.output_dir.join(TRANSFER_FEE_EXACT_FALLBACK_DIR);
        let fallback_epoch_path = fallback_output_dir
            .join(TRANSFER_FEE_HISTORY_DIR)
            .join("epoch-schedule.json");
        write_or_validate_json(&fallback_epoch_path, &epoch_artifact)?;
        let fallback_markets = resolutions
            .full_history_mints
            .iter()
            .enumerate()
            .map(|(index, fallback)| MarketMintBindingV1 {
                pair_address: format!("selected-fallback-{index}-{}", fallback.mint),
                token0: fallback.mint.clone(),
                token1: fallback.mint.clone(),
                lp_mint: fallback.mint.clone(),
            })
            .collect::<Vec<_>>();
        let fallback_request = TransferFeeExtractionRequestV1 {
            run_id: request.run_id,
            cutoff_slot: request.cutoff_slot,
            provider: request.provider,
            markets: &fallback_markets,
            output_dir: &fallback_output_dir,
            signature_page_limit: request.signature_page_limit,
        };
        let fallback_output = extract_transfer_fee_provenance(rpc, &fallback_request).await?;
        if fallback_output.artifact.epoch_schedule != epoch_artifact.schedule {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "fallback history epoch schedule differs from selected-scope schedule".to_owned(),
            ));
        }
        let prefix = format!("{TRANSFER_FEE_EXACT_FALLBACK_DIR}/");
        for mut history in fallback_output.artifact.mint_histories.clone() {
            for checksum in history
                .signature_page_artifacts
                .iter_mut()
                .chain(history.block_artifacts.iter_mut())
            {
                checksum.logical_name = format!("{prefix}{}", checksum.logical_name);
            }
            histories.push(history);
        }
        if let Some(registry) = &fallback_output.artifact.registry {
            schedules.extend(registry.mints.clone());
        }
        Some(fallback_output.provenance_path)
    };

    histories.sort_by(|left, right| left.mint.cmp(&right.mint));
    schedules.sort_by(|left, right| left.mint.cmp(&right.mint));
    let expected_mints = bindings
        .iter()
        .map(|binding| binding.mint.clone())
        .collect::<Vec<_>>();
    let history_mints = histories
        .iter()
        .map(|history| history.mint.clone())
        .collect::<Vec<_>>();
    if history_mints != expected_mints {
        return Err(TransferFeeExtractionError::InvalidArtifact(
            "selected-scope proof and fallback histories do not cover the exact mint inventory"
                .to_owned(),
        ));
    }
    let history_by_mint = histories
        .iter()
        .map(|history| (history.mint.as_str(), history))
        .collect::<BTreeMap<_, _>>();
    let market_coverage = normalized_markets
        .iter()
        .map(|market| {
            let blocking_mints = market
                .mints
                .iter()
                .filter(|mint| {
                    history_by_mint
                        .get(mint.as_str())
                        .is_none_or(|history| !history.ready())
                })
                .cloned()
                .collect::<Vec<_>>();
            MarketTransferFeeCoverageV1 {
                pair_address: market.pair_address.clone(),
                mints: market.mints.clone(),
                ready: blocking_mints.is_empty(),
                blocking_mints,
            }
        })
        .collect::<Vec<_>>();
    let blockers = histories
        .iter()
        .flat_map(|history| history.blockers.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let registry = blockers.is_empty().then_some(TransferFeeRegistryV1 {
        schema_version: TRANSFER_FEE_SCHEMA_VERSION,
        epoch_schedule: epoch_artifact.schedule.clone(),
        mints: schedules,
    });
    if let Some(registry) = &registry {
        registry.clone().validate()?;
    }
    let artifact = TransferFeeProvenanceArtifactV1 {
        schema_version: TRANSFER_FEE_HISTORY_SCHEMA_VERSION,
        run_id: request.run_id.to_owned(),
        cutoff_slot: request.cutoff_slot,
        provider: request.provider.clone(),
        epoch_schedule: epoch_artifact.schedule,
        epoch_schedule_artifact: epoch_checksum,
        selected_operation_evidence_artifact: Some(operation_evidence_checksum),
        decision_grade_legacy_proofs_artifact: Some(legacy_proofs_checksum),
        selected_operation_evidence: Some(operation_evidence.clone()),
        decision_grade_legacy_proofs: Some(legacy_proof_set.clone()),
        market_coverage,
        mint_histories: histories,
        registry,
        blockers,
        artifact_sha256: String::new(),
    }
    .seal()?;
    artifact.validate()?;

    let provenance_path = request.output_dir.join(TRANSFER_FEE_PROVENANCE_JSON);
    write_or_validate_json(&provenance_path, &artifact)?;
    let registry_path = if let Some(registry) = &artifact.registry {
        let path = request.output_dir.join(TRANSFER_FEE_REGISTRY_JSON);
        write_or_validate_json(&path, registry)?;
        Some(path)
    } else {
        let stale = request.output_dir.join(TRANSFER_FEE_REGISTRY_JSON);
        if stale.exists() {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "a stale registry exists while selected-scope provenance is blocked".to_owned(),
            ));
        }
        None
    };
    Ok(SelectedScopeTransferFeeExtractionOutputV1 {
        operation_evidence_path,
        legacy_proofs_path,
        fallback_provenance_path,
        provenance_path,
        registry_path,
        operation_evidence,
        legacy_proof_set,
        artifact,
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MintSignaturePageArtifactV1 {
    pub schema_version: u16,
    pub run_id: String,
    pub cutoff_slot: u64,
    pub provider_fingerprint_sha256: String,
    pub mint: String,
    pub commitment: String,
    pub page_index: u64,
    pub request_before: Option<String>,
    pub request_limit: u16,
    pub raw_response_sha256: String,
    pub signatures: Vec<ProgramSignatureV1>,
    pub artifact_sha256: String,
}

impl MintSignaturePageArtifactV1 {
    fn seal(mut self) -> Result<Self, TransferFeeExtractionError> {
        self.artifact_sha256.clear();
        self.artifact_sha256 = hash_serialized(PAGE_HASH_DOMAIN, &self)?;
        Ok(self)
    }

    fn validate(&self) -> Result<(), TransferFeeExtractionError> {
        if self.schema_version != TRANSFER_FEE_HISTORY_SCHEMA_VERSION
            || self.run_id.trim().is_empty()
            || parse_pubkey(&self.mint).is_err()
            || self.commitment != "finalized"
            || self.request_limit == 0
            || self.request_limit > DEFAULT_MINT_SIGNATURE_PAGE_LIMIT
            || self.signatures.len() > usize::from(self.request_limit)
            || !valid_sha256(&self.provider_fingerprint_sha256)
            || !valid_sha256(&self.raw_response_sha256)
            || !valid_sha256(&self.artifact_sha256)
        {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "invalid mint signature page identity".to_owned(),
            ));
        }
        let mut signatures = BTreeSet::new();
        let mut previous_slot = None;
        for row in &self.signatures {
            if !valid_solana_signature(&row.signature)
                || !signatures.insert(row.signature.clone())
                || previous_slot.is_some_and(|slot| row.slot > slot)
            {
                return Err(TransferFeeExtractionError::InvalidArtifact(
                    "mint signature page is duplicated or not newest-first".to_owned(),
                ));
            }
            previous_slot = Some(row.slot);
        }
        let mut unsigned = self.clone();
        unsigned.artifact_sha256.clear();
        if self.artifact_sha256 != hash_serialized(PAGE_HASH_DOMAIN, &unsigned)? {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "mint signature page self-hash mismatch".to_owned(),
            ));
        }
        Ok(())
    }

    fn terminal(&self) -> bool {
        self.signatures.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MintHistoryBlockArtifactV1 {
    pub schema_version: u16,
    pub run_id: String,
    pub cutoff_slot: u64,
    pub provider_fingerprint_sha256: String,
    pub slot: u64,
    pub commitment: String,
    pub encoding: String,
    pub transaction_details: String,
    pub max_supported_transaction_version: u8,
    /// Every mint whose address-history crawl requires this full block.
    pub mints: Vec<String>,
    /// Exact independently crawled address-history signatures retained from
    /// the full finalized block. This list includes failed transactions as
    /// coverage evidence and is sorted lexicographically for stable binding.
    pub target_signatures: Vec<String>,
    pub raw_response_sha256: String,
    /// Full-block count and ordered inventory hash remain in this envelope,
    /// while `transactions` retains only `target_signatures`.
    pub block: Option<ArchivedBlock>,
    pub artifact_sha256: String,
}

impl Eq for MintHistoryBlockArtifactV1 {}

impl MintHistoryBlockArtifactV1 {
    fn seal(mut self) -> Result<Self, TransferFeeExtractionError> {
        self.artifact_sha256.clear();
        self.artifact_sha256 = hash_serialized(BLOCK_HASH_DOMAIN, &self)?;
        Ok(self)
    }

    fn validate(&self) -> Result<(), TransferFeeExtractionError> {
        if self.schema_version != MINT_HISTORY_BLOCK_SCHEMA_VERSION
            || self.run_id.trim().is_empty()
            || !valid_sha256(&self.provider_fingerprint_sha256)
            || self.commitment != "finalized"
            || self.encoding != "json"
            || self.transaction_details != "full"
            || self.max_supported_transaction_version != 0
            || !valid_sha256(&self.raw_response_sha256)
            || !valid_sha256(&self.artifact_sha256)
            || !strictly_sorted_unique(&self.mints)
            || self.mints.iter().any(|mint| parse_pubkey(mint).is_err())
            || !strictly_sorted_unique(&self.target_signatures)
            || self
                .target_signatures
                .iter()
                .any(|signature| !valid_solana_signature(signature))
        {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "invalid mint-history block identity".to_owned(),
            ));
        }
        if let Some(block) = &self.block {
            validate_retained_block(block, self.slot, &self.target_signatures)?;
        } else if !self.target_signatures.is_empty() {
            // An unavailable finalized block can still bind the exact requested
            // inventory; it simply cannot satisfy mint-history coverage.
        }
        let mut unsigned = self.clone();
        unsigned.artifact_sha256.clear();
        if self.artifact_sha256 != hash_serialized(BLOCK_HASH_DOMAIN, &unsigned)? {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "mint-history block self-hash mismatch".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Read-only migration shape for the former uncompressed artifact that stored
/// every transaction payload in a block. It is never written again.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyMintHistoryBlockArtifactV1 {
    schema_version: u16,
    run_id: String,
    cutoff_slot: u64,
    provider_fingerprint_sha256: String,
    slot: u64,
    commitment: String,
    encoding: String,
    transaction_details: String,
    max_supported_transaction_version: u8,
    mints: Vec<String>,
    raw_response_sha256: String,
    block: Option<ArchivedBlock>,
    artifact_sha256: String,
}

impl LegacyMintHistoryBlockArtifactV1 {
    fn validate(&self) -> Result<(), TransferFeeExtractionError> {
        if self.schema_version != TRANSFER_FEE_HISTORY_SCHEMA_VERSION
            || self.run_id.trim().is_empty()
            || !valid_sha256(&self.provider_fingerprint_sha256)
            || self.commitment != "finalized"
            || self.encoding != "json"
            || self.transaction_details != "full"
            || self.max_supported_transaction_version != 0
            || !valid_sha256(&self.raw_response_sha256)
            || !valid_sha256(&self.artifact_sha256)
            || !strictly_sorted_unique(&self.mints)
            || self.mints.iter().any(|mint| parse_pubkey(mint).is_err())
        {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "invalid legacy mint-history block identity".to_owned(),
            ));
        }
        if let Some(block) = &self.block {
            validate_full_block(block, self.slot)?;
        }
        let mut unsigned = self.clone();
        unsigned.artifact_sha256.clear();
        if self.artifact_sha256 != hash_serialized(LEGACY_BLOCK_HASH_DOMAIN, &unsigned)? {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "legacy mint-history block self-hash mismatch".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EpochScheduleArtifactV1 {
    pub schema_version: u16,
    pub run_id: String,
    pub cutoff_slot: u64,
    pub provider_fingerprint_sha256: String,
    pub raw_response_sha256: String,
    pub schedule: EpochScheduleV1,
    pub artifact_sha256: String,
}

impl EpochScheduleArtifactV1 {
    fn seal(mut self) -> Result<Self, TransferFeeExtractionError> {
        self.artifact_sha256.clear();
        self.artifact_sha256 = hash_serialized(EPOCH_HASH_DOMAIN, &self)?;
        Ok(self)
    }

    fn validate(&self) -> Result<(), TransferFeeExtractionError> {
        if self.schema_version != TRANSFER_FEE_HISTORY_SCHEMA_VERSION
            || self.run_id.trim().is_empty()
            || !valid_sha256(&self.provider_fingerprint_sha256)
            || !valid_sha256(&self.raw_response_sha256)
            || self.schedule.source_response_sha256 != self.raw_response_sha256
            || !valid_sha256(&self.artifact_sha256)
        {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "invalid epoch-schedule proof".to_owned(),
            ));
        }
        self.schedule.validate()?;
        let mut unsigned = self.clone();
        unsigned.artifact_sha256.clear();
        if self.artifact_sha256 != hash_serialized(EPOCH_HASH_DOMAIN, &unsigned)? {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "epoch-schedule proof self-hash mismatch".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferFeeHistoryBlockerCodeV1 {
    ArchivedBlockUnavailable,
    SignatureMissingFromBlock,
    MintMissingFromTransaction,
    SignatureStatusMismatch,
    MintInitializationMissing,
    MultipleMintInitializations,
    AmbiguousTokenProgram,
    MintClosedOrReinitialized,
    InvalidTokenInstruction,
    TransferFeeInitializationDuplicated,
    TransferFeeSetBeforeInitialization,
    UnsupportedTransferHook,
    UnknownMintExtension,
    UnsupportedMintExtension,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransferFeeHistoryBlockerV1 {
    pub mint: String,
    pub code: TransferFeeHistoryBlockerCodeV1,
    pub signature: Option<String>,
    pub slot: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "basis", rename_all = "snake_case", deny_unknown_fields)]
pub enum MintTransferFeeProofBasisV1 {
    CompleteFinalizedAddressHistory,
    DecisionGradePostCutoffImmutableLegacy {
        proof: DecisionGradeLegacyMintProofV1,
    },
}

impl Default for MintTransferFeeProofBasisV1 {
    fn default() -> Self {
        Self::CompleteFinalizedAddressHistory
    }
}

fn is_complete_history_basis(basis: &MintTransferFeeProofBasisV1) -> bool {
    matches!(
        basis,
        MintTransferFeeProofBasisV1::CompleteFinalizedAddressHistory
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MintTransferFeeHistoryV1 {
    pub mint: String,
    /// Omitted for historical schema-v1 exact-crawl artifacts so their
    /// existing self-hashes remain stable.
    #[serde(default, skip_serializing_if = "is_complete_history_basis")]
    pub proof_basis: MintTransferFeeProofBasisV1,
    pub history_root_proven: bool,
    pub signature_page_count: u64,
    pub signatures_through_cutoff: u64,
    pub successful_signatures_through_cutoff: u64,
    pub failed_signatures_through_cutoff: u64,
    pub block_slots_expected: u64,
    pub block_slots_fetched: u64,
    pub token_program: Option<TokenProgramKindV1>,
    pub decimals: Option<u8>,
    /// `Some` is a complete initialized mint-extension inventory. `None`
    /// means decoding encountered an unknown/unsupported successful mutation.
    pub initialized_token_2022_extensions: Option<Vec<Token2022MintExtensionV1>>,
    /// Never inferred from a zero-fee schedule: this is set only from the
    /// complete decoded extension/mutation history.
    pub transfer_fee_config_ever_exists: Option<bool>,
    pub initialization_signature: Option<String>,
    pub initialization_slot: Option<u64>,
    pub decoded_mint_history_sha256: String,
    pub transfer_fee_epoch_count: u64,
    pub signature_page_artifacts: Vec<ArtifactChecksum>,
    pub block_artifacts: Vec<ArtifactChecksum>,
    pub blockers: Vec<TransferFeeHistoryBlockerV1>,
}

impl MintTransferFeeHistoryV1 {
    pub fn ready(&self) -> bool {
        match &self.proof_basis {
            MintTransferFeeProofBasisV1::CompleteFinalizedAddressHistory => {
                self.history_root_proven
                    && self.block_slots_expected == self.block_slots_fetched
                    && self.token_program.is_some()
                    && self.decimals.is_some()
                    && self.initialized_token_2022_extensions.is_some()
                    && self.transfer_fee_config_ever_exists.is_some()
                    && self.initialization_signature.is_some()
                    && self.initialization_slot.is_some()
                    && self.blockers.is_empty()
            }
            MintTransferFeeProofBasisV1::DecisionGradePostCutoffImmutableLegacy { .. } => {
                !self.history_root_proven
                    && self.signature_page_count == 0
                    && self.signatures_through_cutoff == 0
                    && self.successful_signatures_through_cutoff == 0
                    && self.failed_signatures_through_cutoff == 0
                    && self.block_slots_expected == 0
                    && self.block_slots_fetched == 0
                    && self.token_program == Some(TokenProgramKindV1::LegacyToken)
                    && self.decimals.is_some()
                    && self.initialized_token_2022_extensions.as_deref() == Some(&[])
                    && self.transfer_fee_config_ever_exists == Some(false)
                    && self.initialization_signature.is_none()
                    && self.initialization_slot.is_none()
                    && self.transfer_fee_epoch_count == 1
                    && self.signature_page_artifacts.is_empty()
                    && self.block_artifacts.is_empty()
                    && self.blockers.is_empty()
            }
        }
    }
}

/// Sanitized projection that can be mapped directly into
/// `LeverageMintEligibilityEvidenceV1` without exposing wallet-level data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MintLeverageEligibilityProjectionV1 {
    pub mint: String,
    pub decoded_mint_sha256: String,
    /// Exact result of Dusk's two native mint guards from reconstructed state.
    pub dusk_native_guards_pass: bool,
    /// Native guards plus benchmark-specific exact-history/TransferHook gates.
    pub native_leverage_eligible: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketTransferFeeCoverageV1 {
    pub pair_address: String,
    pub mints: Vec<String>,
    pub ready: bool,
    pub blocking_mints: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransferFeeProvenanceArtifactV1 {
    pub schema_version: u16,
    pub run_id: String,
    pub cutoff_slot: u64,
    pub provider: SanitizedSource,
    pub epoch_schedule: EpochScheduleV1,
    pub epoch_schedule_artifact: ArtifactChecksum,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_operation_evidence_artifact: Option<ArtifactChecksum>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_grade_legacy_proofs_artifact: Option<ArtifactChecksum>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_operation_evidence: Option<SelectedOperationTokenProgramEvidenceArtifactV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_grade_legacy_proofs: Option<DecisionGradeLegacyMintProofSetV1>,
    pub market_coverage: Vec<MarketTransferFeeCoverageV1>,
    pub mint_histories: Vec<MintTransferFeeHistoryV1>,
    pub registry: Option<TransferFeeRegistryV1>,
    pub blockers: Vec<TransferFeeHistoryBlockerV1>,
    pub artifact_sha256: String,
}

impl TransferFeeProvenanceArtifactV1 {
    fn seal(mut self) -> Result<Self, TransferFeeExtractionError> {
        self.artifact_sha256.clear();
        self.artifact_sha256 = hash_serialized(PROVENANCE_HASH_DOMAIN, &self)?;
        Ok(self)
    }

    pub fn ready_mint_count(&self) -> u64 {
        self.mint_histories
            .iter()
            .filter(|history| history.ready())
            .count() as u64
    }

    pub fn expected_mint_count(&self) -> u64 {
        self.mint_histories.len() as u64
    }

    pub fn market_ready(&self, pair_address: &str) -> bool {
        self.market_coverage
            .iter()
            .find(|market| market.pair_address == pair_address)
            .is_some_and(|market| market.ready)
    }

    pub fn leverage_mint_eligibility_projections(
        &self,
    ) -> Result<Vec<MintLeverageEligibilityProjectionV1>, TransferFeeExtractionError> {
        self.validate()?;
        Ok(self
            .mint_histories
            .iter()
            .map(|history| {
                let decimals_supported = history
                    .decimals
                    .is_some_and(|decimals| decimals <= dusk::constants::NAD_DECIMALS);
                let extension_guard = match (
                    history.token_program,
                    history.initialized_token_2022_extensions.as_deref(),
                    history.transfer_fee_config_ever_exists,
                ) {
                    (Some(TokenProgramKindV1::LegacyToken), Some([]), Some(false)) => true,
                    (Some(TokenProgramKindV1::Token2022), Some(extensions), Some(false)) => {
                        extensions.iter().all(|extension| {
                            matches!(
                                extension,
                                Token2022MintExtensionV1::MetadataPointer
                                    | Token2022MintExtensionV1::TokenMetadata
                                    | Token2022MintExtensionV1::TransferHook
                            )
                        })
                    }
                    _ => false,
                };
                let dusk_native_guards_pass = decimals_supported && extension_guard;
                MintLeverageEligibilityProjectionV1 {
                    mint: history.mint.clone(),
                    decoded_mint_sha256: history.decoded_mint_history_sha256.clone(),
                    dusk_native_guards_pass,
                    native_leverage_eligible: dusk_native_guards_pass && history.ready(),
                }
            })
            .collect())
    }

    pub fn validate(&self) -> Result<(), TransferFeeExtractionError> {
        if self.schema_version != TRANSFER_FEE_HISTORY_SCHEMA_VERSION
            || self.run_id.trim().is_empty()
            || !valid_sha256(&self.provider.secret_fingerprint_sha256)
            || !valid_sha256(&self.artifact_sha256)
            || self.market_coverage.is_empty()
            || self.mint_histories.is_empty()
            || self.provider.kind.contains("://")
            || self.provider.label.contains("://")
            || self.provider.label.contains('@')
            || self.provider.label.contains('=')
        {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "invalid transfer-fee provenance identity".to_owned(),
            ));
        }
        self.epoch_schedule.validate()?;
        if self.epoch_schedule_artifact.logical_name
            != format!("{TRANSFER_FEE_HISTORY_DIR}/epoch-schedule.json")
            || !valid_artifact_checksum(&self.epoch_schedule_artifact)
        {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "epoch-schedule artifact checksum is invalid".to_owned(),
            ));
        }
        if self
            .selected_operation_evidence_artifact
            .as_ref()
            .is_some_and(|checksum| !valid_artifact_checksum(checksum))
            || self
                .decision_grade_legacy_proofs_artifact
                .as_ref()
                .is_some_and(|checksum| !valid_artifact_checksum(checksum))
            || self.selected_operation_evidence_artifact.is_some()
                != self.decision_grade_legacy_proofs_artifact.is_some()
            || self.selected_operation_evidence.is_some()
                != self.decision_grade_legacy_proofs.is_some()
            || self.selected_operation_evidence_artifact.is_some()
                != self.selected_operation_evidence.is_some()
        {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "selected-scope decision proof checksums are invalid or incomplete".to_owned(),
            ));
        }
        if self
            .selected_operation_evidence_artifact
            .as_ref()
            .is_some_and(|checksum| {
                checksum.logical_name != SELECTED_OPERATION_TOKEN_PROGRAM_EVIDENCE_JSON
            })
            || self
                .decision_grade_legacy_proofs_artifact
                .as_ref()
                .is_some_and(|checksum| {
                    checksum.logical_name != DECISION_GRADE_LEGACY_MINT_PROOFS_JSON
                })
        {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "selected-scope proof checksum paths are not canonical".to_owned(),
            ));
        }
        if let (Some(evidence), Some(proofs)) = (
            &self.selected_operation_evidence,
            &self.decision_grade_legacy_proofs,
        ) {
            proofs.validate_against_operation_evidence(evidence)?;
            if evidence.run_id != self.run_id
                || evidence.cutoff_slot != self.cutoff_slot
                || proofs.provider != self.provider
            {
                return Err(TransferFeeExtractionError::InvalidArtifact(
                    "selected-scope nested proof identity differs from provenance".to_owned(),
                ));
            }
        }
        let history_mints: Vec<_> = self
            .mint_histories
            .iter()
            .map(|history| history.mint.clone())
            .collect();
        if !strictly_sorted_unique(&history_mints)
            || self
                .mint_histories
                .iter()
                .any(|history| parse_pubkey(&history.mint).is_err())
        {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "mint histories are not a sorted unique mint inventory".to_owned(),
            ));
        }
        for history in &self.mint_histories {
            if history
                .blockers
                .iter()
                .any(|blocker| blocker.mint != history.mint)
                || !valid_sha256(&history.decoded_mint_history_sha256)
            {
                return Err(TransferFeeExtractionError::InvalidArtifact(
                    "mint history coverage/checksum counts are inconsistent".to_owned(),
                ));
            }
            match &history.proof_basis {
                MintTransferFeeProofBasisV1::CompleteFinalizedAddressHistory => {
                    if history.signature_page_count != history.signature_page_artifacts.len() as u64
                        || history.block_slots_expected != history.block_artifacts.len() as u64
                        || history.block_slots_fetched > history.block_slots_expected
                        || history
                            .successful_signatures_through_cutoff
                            .checked_add(history.failed_signatures_through_cutoff)
                            != Some(history.signatures_through_cutoff)
                        || history.signature_page_artifacts.is_empty()
                        || history
                            .signature_page_artifacts
                            .iter()
                            .any(|checksum| !valid_artifact_checksum(checksum))
                        || history
                            .block_artifacts
                            .iter()
                            .any(|checksum| !valid_artifact_checksum(checksum))
                    {
                        return Err(TransferFeeExtractionError::InvalidArtifact(
                            "complete mint history coverage/checksum counts are inconsistent"
                                .to_owned(),
                        ));
                    }
                }
                MintTransferFeeProofBasisV1::DecisionGradePostCutoffImmutableLegacy { proof } => {
                    proof.validate()?;
                    if proof.run_id != self.run_id
                        || proof.cutoff_slot != self.cutoff_slot
                        || proof.provider != self.provider
                        || proof.mint != history.mint
                        || Some(proof.decimals) != history.decimals
                        || decision_grade_decoded_mint_sha256(proof)?
                            != history.decoded_mint_history_sha256
                        || !history.ready()
                    {
                        return Err(TransferFeeExtractionError::InvalidArtifact(
                            "decision-grade immutable legacy mint history is inconsistent"
                                .to_owned(),
                        ));
                    }
                }
            }
            match (
                history.token_program,
                history.initialized_token_2022_extensions.as_deref(),
                history.transfer_fee_config_ever_exists,
            ) {
                (Some(TokenProgramKindV1::LegacyToken), Some([]), Some(false)) => {}
                (Some(TokenProgramKindV1::Token2022), Some(extensions), Some(has_fee))
                    if extensions.windows(2).all(|pair| pair[0] < pair[1])
                        && has_fee
                            == extensions
                                .contains(&Token2022MintExtensionV1::TransferFeeConfig) => {}
                (None, None, None) => {}
                (Some(TokenProgramKindV1::Token2022), None, None)
                    if history.blockers.iter().any(|blocker| {
                        blocker.code == TransferFeeHistoryBlockerCodeV1::UnknownMintExtension
                    }) => {}
                _ => {
                    return Err(TransferFeeExtractionError::InvalidArtifact(
                        "mint owner/extension/transfer-fee evidence is inconsistent".to_owned(),
                    ));
                }
            }
            if history.ready() && history.decimals.is_none() {
                return Err(TransferFeeExtractionError::InvalidArtifact(
                    "ready mint history omits decimals".to_owned(),
                ));
            }
        }
        if self.mint_histories.iter().any(|history| {
            matches!(
                &history.proof_basis,
                MintTransferFeeProofBasisV1::DecisionGradePostCutoffImmutableLegacy { .. }
            )
        }) && self.selected_operation_evidence_artifact.is_none()
        {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "decision-grade mint history lacks selected-operation proof artifacts".to_owned(),
            ));
        }
        if let (Some(evidence), Some(proofs)) = (
            &self.selected_operation_evidence,
            &self.decision_grade_legacy_proofs,
        ) {
            let evidence_mints = evidence
                .selected_bindings
                .iter()
                .map(|binding| binding.mint.clone())
                .collect::<Vec<_>>();
            let history_proofs = self
                .mint_histories
                .iter()
                .filter_map(|history| match &history.proof_basis {
                    MintTransferFeeProofBasisV1::DecisionGradePostCutoffImmutableLegacy {
                        proof,
                    } => Some(proof.clone()),
                    MintTransferFeeProofBasisV1::CompleteFinalizedAddressHistory => None,
                })
                .collect::<Vec<_>>();
            if evidence_mints != history_mints || history_proofs != proofs.proofs {
                return Err(TransferFeeExtractionError::InvalidArtifact(
                    "selected-scope nested proofs do not cover the provenance mint inventory"
                        .to_owned(),
                ));
            }
        }
        let market_ids: Vec<_> = self
            .market_coverage
            .iter()
            .map(|market| market.pair_address.clone())
            .collect();
        if !strictly_sorted_unique(&market_ids) {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "market transfer-fee coverage is not sorted and unique".to_owned(),
            ));
        }
        let history_by_mint: BTreeMap<_, _> = self
            .mint_histories
            .iter()
            .map(|history| (history.mint.as_str(), history))
            .collect();
        let market_mints = self
            .market_coverage
            .iter()
            .flat_map(|market| market.mints.iter().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        if market_mints != history_mints {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "market bindings do not cover the exact mint history inventory".to_owned(),
            ));
        }
        for market in &self.market_coverage {
            if !strictly_sorted_unique(&market.mints) {
                return Err(TransferFeeExtractionError::InvalidArtifact(
                    "market mint inventory is not sorted and unique".to_owned(),
                ));
            }
            let expected_blocking: Vec<String> = market
                .mints
                .iter()
                .filter(|mint| {
                    history_by_mint
                        .get(mint.as_str())
                        .is_none_or(|row| !row.ready())
                })
                .cloned()
                .collect();
            if market.blocking_mints != expected_blocking
                || market.ready != expected_blocking.is_empty()
            {
                return Err(TransferFeeExtractionError::InvalidArtifact(
                    "market transfer-fee readiness is internally inconsistent".to_owned(),
                ));
            }
        }
        let expected_blockers: Vec<_> = self
            .mint_histories
            .iter()
            .flat_map(|history| history.blockers.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if self.blockers != expected_blockers {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "global transfer-fee blockers do not match mint-grain blockers".to_owned(),
            ));
        }
        match (&self.registry, self.blockers.is_empty()) {
            (Some(registry), true) => {
                registry.clone().validate()?;
                let registry_mints: Vec<_> = registry
                    .mints
                    .iter()
                    .map(|mint| mint.mint.clone())
                    .collect();
                if registry_mints != history_mints
                    || self.ready_mint_count() != self.expected_mint_count()
                {
                    return Err(TransferFeeExtractionError::InvalidArtifact(
                        "complete registry does not cover the exact mint inventory".to_owned(),
                    ));
                }
            }
            (None, false) => {}
            _ => {
                return Err(TransferFeeExtractionError::InvalidArtifact(
                    "registry presence disagrees with transfer-fee blockers".to_owned(),
                ));
            }
        }
        let mut unsigned = self.clone();
        unsigned.artifact_sha256.clear();
        if self.artifact_sha256 != hash_serialized(PROVENANCE_HASH_DOMAIN, &unsigned)? {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "transfer-fee provenance self-hash mismatch".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum TransferFeeExtractionError {
    #[error("I/O failure while persisting transfer-fee history")]
    Io(#[from] std::io::Error),
    #[error("invalid JSON in transfer-fee history artifact")]
    Json(#[from] serde_json::Error),
    #[error("transfer-fee registry is invalid: {0}")]
    Registry(#[from] TransferFeeError),
    #[error("invalid market/mint binding: {0}")]
    InvalidBinding(String),
    #[error("invalid transfer-fee extraction artifact: {0}")]
    InvalidArtifact(String),
    #[error("archival RPC {operation} failed categorically: {kind:?}")]
    Rpc {
        operation: &'static str,
        kind: RpcFailureKind,
    },
}

pub trait TransferFeeHistoryRpc: Send + Sync {
    fn epoch_schedule<'a>(
        &'a self,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<RpcResponseProofV1<EpochScheduleV1>, RpcFailureKind>>
                + Send
                + 'a,
        >,
    >;

    fn signatures_page<'a>(
        &'a self,
        mint: &'a str,
        before: Option<&'a str>,
        limit: u16,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<RpcResponseProofV1<Vec<ProgramSignatureV1>>, RpcFailureKind>>
                + Send
                + 'a,
        >,
    >;

    fn full_block<'a>(
        &'a self,
        slot: u64,
        request_id: u64,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<RpcResponseProofV1<Option<ArchivedBlock>>, RpcFailureKind>>
                + Send
                + 'a,
        >,
    >;
}

impl TransferFeeHistoryRpc for HttpArchivalRpc {
    fn epoch_schedule<'a>(
        &'a self,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<RpcResponseProofV1<EpochScheduleV1>, RpcFailureKind>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move { self.epoch_schedule_with_proof().await })
    }

    fn signatures_page<'a>(
        &'a self,
        mint: &'a str,
        before: Option<&'a str>,
        limit: u16,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<RpcResponseProofV1<Vec<ProgramSignatureV1>>, RpcFailureKind>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            self.signatures_for_address_page_with_proof(mint, before, limit)
                .await
        })
    }

    fn full_block<'a>(
        &'a self,
        slot: u64,
        request_id: u64,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<RpcResponseProofV1<Option<ArchivedBlock>>, RpcFailureKind>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            self.finalized_block_with_proof(finalized_get_block_request(slot, request_id))
                .await
        })
    }
}

#[derive(Clone, Debug)]
struct MintCrawlV1 {
    mint: String,
    pages: Vec<ArtifactChecksum>,
    signatures: Vec<ProgramSignatureV1>,
}

/// Extracts and atomically persists exact historical transfer-fee provenance.
/// A returned artifact can be incomplete only by carrying explicit blockers;
/// no partial registry is emitted.
pub async fn extract_transfer_fee_provenance<R: TransferFeeHistoryRpc>(
    rpc: &R,
    request: &TransferFeeExtractionRequestV1<'_>,
) -> Result<TransferFeeExtractionOutputV1, TransferFeeExtractionError> {
    let normalized_markets = validate_request(request)?;
    let history_root = request.output_dir.join(TRANSFER_FEE_HISTORY_DIR);
    fs::create_dir_all(history_root.join("signatures"))?;
    fs::create_dir_all(history_root.join("blocks"))?;

    let epoch_path = history_root.join("epoch-schedule.json");
    let epoch_artifact = load_or_fetch_epoch_schedule(rpc, request, &epoch_path).await?;
    let epoch_checksum = checksum_path(
        &epoch_path,
        format!("{TRANSFER_FEE_HISTORY_DIR}/epoch-schedule.json"),
    )?;

    let all_mints: Vec<String> = normalized_markets
        .iter()
        .flat_map(|market| market.mints.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut crawls = BTreeMap::new();
    for mint in &all_mints {
        let crawl = crawl_mint_signatures(rpc, request, &history_root, mint).await?;
        crawls.insert(mint.clone(), crawl);
    }

    let mut mints_by_slot = BTreeMap::<u64, BTreeSet<String>>::new();
    let mut signatures_by_slot = BTreeMap::<u64, BTreeSet<String>>::new();
    for crawl in crawls.values() {
        for signature in crawl
            .signatures
            .iter()
            .filter(|signature| signature.slot <= request.cutoff_slot)
        {
            mints_by_slot
                .entry(signature.slot)
                .or_default()
                .insert(crawl.mint.clone());
            signatures_by_slot
                .entry(signature.slot)
                .or_default()
                .insert(signature.signature.clone());
        }
    }
    let mut block_artifacts = BTreeMap::<u64, MintHistoryBlockArtifactV1>::new();
    let mut block_checksums = BTreeMap::<u64, ArtifactChecksum>::new();
    for (request_index, (slot, mints)) in mints_by_slot.iter().enumerate() {
        let shard = slot / 1_000_000;
        let path = history_root
            .join("blocks")
            .join(shard.to_string())
            .join(format!("{slot}.json.zst"));
        let legacy_path = history_root.join("blocks").join(format!("{slot}.json"));
        let target_signatures = signatures_by_slot
            .get(slot)
            .expect("signature inventory is built with the mint inventory");
        let artifact = load_or_fetch_block(
            rpc,
            request,
            &path,
            &legacy_path,
            *slot,
            mints,
            target_signatures,
            u64::try_from(request_index)
                .unwrap_or(u64::MAX)
                .saturating_add(10_000),
        )
        .await?;
        let checksum = checksum_path(
            &path,
            format!("{TRANSFER_FEE_HISTORY_DIR}/blocks/{shard}/{slot}.json.zst"),
        )?;
        block_artifacts.insert(*slot, artifact);
        block_checksums.insert(*slot, checksum);
    }

    let mut histories = Vec::with_capacity(all_mints.len());
    let mut schedules = Vec::new();
    for mint in &all_mints {
        let crawl = crawls
            .get(mint)
            .expect("all_mints is derived from the crawl map");
        let (history, schedule) = build_mint_history(
            mint,
            crawl,
            request.cutoff_slot,
            &epoch_artifact.schedule,
            &block_artifacts,
            &block_checksums,
        )?;
        if let Some(schedule) = schedule {
            schedules.push(schedule);
        }
        histories.push(history);
    }
    histories.sort_by(|left, right| left.mint.cmp(&right.mint));
    schedules.sort_by(|left, right| left.mint.cmp(&right.mint));
    let history_by_mint: BTreeMap<_, _> = histories
        .iter()
        .map(|history| (history.mint.as_str(), history))
        .collect();
    let market_coverage = normalized_markets
        .iter()
        .map(|market| {
            let blocking_mints = market
                .mints
                .iter()
                .filter(|mint| {
                    history_by_mint
                        .get(mint.as_str())
                        .is_none_or(|history| !history.ready())
                })
                .cloned()
                .collect::<Vec<_>>();
            MarketTransferFeeCoverageV1 {
                pair_address: market.pair_address.clone(),
                mints: market.mints.clone(),
                ready: blocking_mints.is_empty(),
                blocking_mints,
            }
        })
        .collect::<Vec<_>>();
    let blockers = histories
        .iter()
        .flat_map(|history| history.blockers.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let registry = blockers.is_empty().then_some(TransferFeeRegistryV1 {
        schema_version: TRANSFER_FEE_SCHEMA_VERSION,
        epoch_schedule: epoch_artifact.schedule.clone(),
        mints: schedules,
    });
    if let Some(registry) = &registry {
        registry.clone().validate()?;
    }
    let artifact = TransferFeeProvenanceArtifactV1 {
        schema_version: TRANSFER_FEE_HISTORY_SCHEMA_VERSION,
        run_id: request.run_id.to_owned(),
        cutoff_slot: request.cutoff_slot,
        provider: request.provider.clone(),
        epoch_schedule: epoch_artifact.schedule,
        epoch_schedule_artifact: epoch_checksum,
        selected_operation_evidence_artifact: None,
        decision_grade_legacy_proofs_artifact: None,
        selected_operation_evidence: None,
        decision_grade_legacy_proofs: None,
        market_coverage,
        mint_histories: histories,
        registry,
        blockers,
        artifact_sha256: String::new(),
    }
    .seal()?;
    artifact.validate()?;

    let provenance_path = request.output_dir.join(TRANSFER_FEE_PROVENANCE_JSON);
    write_json_atomic(&provenance_path, &artifact)?;
    let registry_path = if let Some(registry) = &artifact.registry {
        let path = request.output_dir.join(TRANSFER_FEE_REGISTRY_JSON);
        if path.exists() {
            let existing: TransferFeeRegistryV1 = read_json(&path)?;
            if &existing != registry {
                return Err(TransferFeeExtractionError::InvalidArtifact(
                    "existing transfer-fee registry differs from proven history".to_owned(),
                ));
            }
        } else {
            write_json_atomic(&path, registry)?;
        }
        Some(path)
    } else {
        let stale = request.output_dir.join(TRANSFER_FEE_REGISTRY_JSON);
        if stale.exists() {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "a stale registry exists while current provenance is blocked".to_owned(),
            ));
        }
        None
    };
    Ok(TransferFeeExtractionOutputV1 {
        provenance_path,
        registry_path,
        artifact,
    })
}

#[derive(Clone, Debug)]
struct NormalizedMarketBindingV1 {
    pair_address: String,
    mints: Vec<String>,
}

fn validate_request(
    request: &TransferFeeExtractionRequestV1<'_>,
) -> Result<Vec<NormalizedMarketBindingV1>, TransferFeeExtractionError> {
    if request.run_id.trim().is_empty()
        || request.markets.is_empty()
        || request.signature_page_limit == 0
        || request.signature_page_limit > DEFAULT_MINT_SIGNATURE_PAGE_LIMIT
        || !valid_sha256(&request.provider.secret_fingerprint_sha256)
    {
        return Err(TransferFeeExtractionError::InvalidBinding(
            "run, provider, market inventory, or page limit is invalid".to_owned(),
        ));
    }
    let mut markets = BTreeMap::<String, Vec<String>>::new();
    for market in request.markets {
        market.validate()?;
        let mut mints = market
            .mints()
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        mints.sort();
        mints.dedup();
        if markets.insert(market.pair_address.clone(), mints).is_some() {
            return Err(TransferFeeExtractionError::InvalidBinding(
                "duplicate market pair address".to_owned(),
            ));
        }
    }
    Ok(markets
        .into_iter()
        .map(|(pair_address, mints)| NormalizedMarketBindingV1 {
            pair_address,
            mints,
        })
        .collect())
}

async fn load_or_fetch_epoch_schedule<R: TransferFeeHistoryRpc>(
    rpc: &R,
    request: &TransferFeeExtractionRequestV1<'_>,
    path: &Path,
) -> Result<EpochScheduleArtifactV1, TransferFeeExtractionError> {
    if path.exists() {
        let artifact: EpochScheduleArtifactV1 = read_json(path)?;
        artifact.validate()?;
        validate_common_binding(
            artifact.run_id.as_str(),
            artifact.cutoff_slot,
            artifact.provider_fingerprint_sha256.as_str(),
            request,
        )?;
        return Ok(artifact);
    }
    let response = rpc
        .epoch_schedule()
        .await
        .map_err(|kind| TransferFeeExtractionError::Rpc {
            operation: "get_epoch_schedule",
            kind,
        })?;
    if response.value.source_response_sha256 != response.raw_response_sha256 {
        return Err(TransferFeeExtractionError::InvalidArtifact(
            "epoch schedule does not bind its raw response".to_owned(),
        ));
    }
    let artifact = EpochScheduleArtifactV1 {
        schema_version: TRANSFER_FEE_HISTORY_SCHEMA_VERSION,
        run_id: request.run_id.to_owned(),
        cutoff_slot: request.cutoff_slot,
        provider_fingerprint_sha256: request.provider.secret_fingerprint_sha256.clone(),
        raw_response_sha256: response.raw_response_sha256,
        schedule: response.value,
        artifact_sha256: String::new(),
    }
    .seal()?;
    artifact.validate()?;
    write_json_atomic(path, &artifact)?;
    Ok(artifact)
}

async fn crawl_mint_signatures<R: TransferFeeHistoryRpc>(
    rpc: &R,
    request: &TransferFeeExtractionRequestV1<'_>,
    history_root: &Path,
    mint: &str,
) -> Result<MintCrawlV1, TransferFeeExtractionError> {
    let directory = history_root.join("signatures").join(mint);
    fs::create_dir_all(&directory)?;
    let mut before = None::<String>;
    let mut page_index = 0_u64;
    let mut previous_slot = None;
    let mut seen = BTreeSet::new();
    let mut signatures = Vec::new();
    let mut checksums = Vec::new();
    loop {
        let path = directory.join(format!("{page_index:08}.json"));
        let artifact = if path.exists() {
            read_json::<MintSignaturePageArtifactV1>(&path)?
        } else {
            let response = rpc
                .signatures_page(mint, before.as_deref(), request.signature_page_limit)
                .await
                .map_err(|kind| TransferFeeExtractionError::Rpc {
                    operation: "get_signatures_for_mint",
                    kind,
                })?;
            let artifact = MintSignaturePageArtifactV1 {
                schema_version: TRANSFER_FEE_HISTORY_SCHEMA_VERSION,
                run_id: request.run_id.to_owned(),
                cutoff_slot: request.cutoff_slot,
                provider_fingerprint_sha256: request.provider.secret_fingerprint_sha256.clone(),
                mint: mint.to_owned(),
                commitment: "finalized".to_owned(),
                page_index,
                request_before: before.clone(),
                request_limit: request.signature_page_limit,
                raw_response_sha256: response.raw_response_sha256,
                signatures: response.value,
                artifact_sha256: String::new(),
            }
            .seal()?;
            artifact.validate()?;
            write_json_atomic(&path, &artifact)?;
            artifact
        };
        artifact.validate()?;
        validate_common_binding(
            artifact.run_id.as_str(),
            artifact.cutoff_slot,
            artifact.provider_fingerprint_sha256.as_str(),
            request,
        )?;
        if artifact.mint != mint
            || artifact.page_index != page_index
            || artifact.request_before != before
            || artifact.request_limit != request.signature_page_limit
        {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "resumed mint signature page does not match its request chain".to_owned(),
            ));
        }
        for row in &artifact.signatures {
            if previous_slot.is_some_and(|slot| row.slot > slot)
                || !seen.insert(row.signature.clone())
            {
                return Err(TransferFeeExtractionError::InvalidArtifact(
                    "mint signature pagination regressed or duplicated a signature".to_owned(),
                ));
            }
            previous_slot = Some(row.slot);
            if row.slot <= request.cutoff_slot {
                signatures.push(row.clone());
            }
        }
        checksums.push(checksum_path(
            &path,
            format!("{TRANSFER_FEE_HISTORY_DIR}/signatures/{mint}/{page_index:08}.json"),
        )?);
        if artifact.terminal() {
            reject_trailing_signature_pages(&directory, page_index)?;
            break;
        }
        let last = artifact
            .signatures
            .last()
            .expect("non-terminal page is non-empty")
            .signature
            .clone();
        if before.as_deref() == Some(last.as_str()) {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "mint signature pagination cursor did not advance".to_owned(),
            ));
        }
        before = Some(last);
        page_index = page_index.checked_add(1).ok_or_else(|| {
            TransferFeeExtractionError::InvalidArtifact(
                "mint signature page index overflow".to_owned(),
            )
        })?;
    }
    Ok(MintCrawlV1 {
        mint: mint.to_owned(),
        pages: checksums,
        signatures,
    })
}

async fn load_or_fetch_block<R: TransferFeeHistoryRpc>(
    rpc: &R,
    request: &TransferFeeExtractionRequestV1<'_>,
    path: &Path,
    legacy_path: &Path,
    slot: u64,
    mints: &BTreeSet<String>,
    target_signatures: &BTreeSet<String>,
    request_id: u64,
) -> Result<MintHistoryBlockArtifactV1, TransferFeeExtractionError> {
    let expected_mints = mints.iter().cloned().collect::<Vec<_>>();
    let expected_signatures = target_signatures.iter().cloned().collect::<Vec<_>>();
    if path.exists() {
        let existing: MintHistoryBlockArtifactV1 = read_json_zstd(path)?;
        existing.validate()?;
        validate_common_binding(
            existing.run_id.as_str(),
            existing.cutoff_slot,
            existing.provider_fingerprint_sha256.as_str(),
            request,
        )?;
        if existing.slot != slot
            || existing.mints != expected_mints
            || existing.target_signatures != expected_signatures
        {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "resumed block proof does not match its mint/slot/signature request".to_owned(),
            ));
        }
        if existing.block.is_some() {
            remove_legacy_full_block_after_migration(legacy_path)?;
            return Ok(existing);
        }
        // An explicit unavailable response is coverage evidence, but retry it
        // on resume so a repaired archival provider can complete the same run.
    } else if legacy_path.exists() {
        let legacy: LegacyMintHistoryBlockArtifactV1 = read_json(legacy_path)?;
        legacy.validate()?;
        validate_common_binding(
            legacy.run_id.as_str(),
            legacy.cutoff_slot,
            legacy.provider_fingerprint_sha256.as_str(),
            request,
        )?;
        if legacy.slot != slot || legacy.mints != expected_mints {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "legacy block proof does not match its mint/slot request".to_owned(),
            ));
        }
        let migrated_block = legacy
            .block
            .map(|block| retain_target_transactions(block, target_signatures))
            .transpose()?;
        let migrated = MintHistoryBlockArtifactV1 {
            schema_version: MINT_HISTORY_BLOCK_SCHEMA_VERSION,
            run_id: legacy.run_id,
            cutoff_slot: legacy.cutoff_slot,
            provider_fingerprint_sha256: legacy.provider_fingerprint_sha256,
            slot: legacy.slot,
            commitment: legacy.commitment,
            encoding: legacy.encoding,
            transaction_details: legacy.transaction_details,
            max_supported_transaction_version: legacy.max_supported_transaction_version,
            mints: legacy.mints,
            target_signatures: expected_signatures.clone(),
            raw_response_sha256: legacy.raw_response_sha256,
            block: migrated_block,
            artifact_sha256: String::new(),
        }
        .seal()?;
        migrated.validate()?;
        write_json_zstd_atomic(path, &migrated)?;
        let persisted: MintHistoryBlockArtifactV1 = read_json_zstd(path)?;
        persisted.validate()?;
        if persisted != migrated {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "compressed migrated block differs after round trip".to_owned(),
            ));
        }
        remove_legacy_full_block_after_migration(legacy_path)?;
        if persisted.block.is_some() {
            return Ok(persisted);
        }
        // Preserve the unavailable proof but retry below.
    }
    let response =
        rpc.full_block(slot, request_id)
            .await
            .map_err(|kind| TransferFeeExtractionError::Rpc {
                operation: "get_full_mint_history_block",
                kind,
            })?;
    let retained_block = response
        .value
        .map(|block| retain_target_transactions(block, target_signatures))
        .transpose()?;
    let artifact = MintHistoryBlockArtifactV1 {
        schema_version: MINT_HISTORY_BLOCK_SCHEMA_VERSION,
        run_id: request.run_id.to_owned(),
        cutoff_slot: request.cutoff_slot,
        provider_fingerprint_sha256: request.provider.secret_fingerprint_sha256.clone(),
        slot,
        commitment: "finalized".to_owned(),
        encoding: "json".to_owned(),
        transaction_details: "full".to_owned(),
        max_supported_transaction_version: 0,
        mints: expected_mints,
        target_signatures: expected_signatures,
        raw_response_sha256: response.raw_response_sha256,
        block: retained_block,
        artifact_sha256: String::new(),
    }
    .seal()?;
    artifact.validate()?;
    write_json_zstd_atomic(path, &artifact)?;
    remove_legacy_full_block_after_migration(legacy_path)?;
    Ok(artifact)
}

fn build_mint_history(
    mint: &str,
    crawl: &MintCrawlV1,
    cutoff_slot: u64,
    epoch_schedule: &EpochScheduleV1,
    block_artifacts: &BTreeMap<u64, MintHistoryBlockArtifactV1>,
    block_checksums: &BTreeMap<u64, ArtifactChecksum>,
) -> Result<(MintTransferFeeHistoryV1, Option<MintTransferFeeScheduleV1>), TransferFeeExtractionError>
{
    let signatures = crawl
        .signatures
        .iter()
        .filter(|signature| signature.slot <= cutoff_slot)
        .collect::<Vec<_>>();
    let expected_slots = signatures
        .iter()
        .map(|signature| signature.slot)
        .collect::<BTreeSet<_>>();
    let mut fetched_slots = BTreeSet::new();
    let mut blockers = BTreeSet::new();
    let mut successful_transactions = Vec::<ProvenTransactionV1<'_>>::new();

    for signature in &signatures {
        let Some(block_artifact) = block_artifacts.get(&signature.slot) else {
            blockers.insert(blocker(
                mint,
                TransferFeeHistoryBlockerCodeV1::ArchivedBlockUnavailable,
                Some(signature.signature.as_str()),
                Some(signature.slot),
            ));
            continue;
        };
        let Some(block) = block_artifact.block.as_ref() else {
            blockers.insert(blocker(
                mint,
                TransferFeeHistoryBlockerCodeV1::ArchivedBlockUnavailable,
                Some(signature.signature.as_str()),
                Some(signature.slot),
            ));
            continue;
        };
        fetched_slots.insert(signature.slot);
        let matching = block
            .transactions
            .iter()
            .filter(|transaction| transaction.signature == signature.signature)
            .collect::<Vec<_>>();
        let [transaction] = matching.as_slice() else {
            blockers.insert(blocker(
                mint,
                TransferFeeHistoryBlockerCodeV1::SignatureMissingFromBlock,
                Some(signature.signature.as_str()),
                Some(signature.slot),
            ));
            continue;
        };
        match transaction_account_keys(transaction) {
            Ok(keys) if keys.iter().any(|key| key == mint) => {}
            _ => {
                blockers.insert(blocker(
                    mint,
                    TransferFeeHistoryBlockerCodeV1::MintMissingFromTransaction,
                    Some(signature.signature.as_str()),
                    Some(signature.slot),
                ));
                continue;
            }
        }
        if transaction.succeeded != signature.succeeded {
            blockers.insert(blocker(
                mint,
                TransferFeeHistoryBlockerCodeV1::SignatureStatusMismatch,
                Some(signature.signature.as_str()),
                Some(signature.slot),
            ));
            continue;
        }
        if signature.succeeded {
            successful_transactions.push(ProvenTransactionV1 {
                slot: signature.slot,
                transaction,
            });
        }
    }
    successful_transactions
        .sort_by_key(|transaction| (transaction.slot, transaction.transaction.transaction_index));
    let reconstruction = reconstruct_mint_schedule(mint, epoch_schedule, &successful_transactions)?;
    blockers.extend(reconstruction.blockers.iter().cloned());
    let blocker_rows = blockers.into_iter().collect::<Vec<_>>();
    let exact_schedule = blocker_rows
        .is_empty()
        .then_some(reconstruction.schedule.clone())
        .flatten();
    let block_artifacts_for_mint = expected_slots
        .iter()
        .filter_map(|slot| block_checksums.get(slot).cloned())
        .collect::<Vec<_>>();
    let decoded_mint_history_sha256 = hash_serialized(
        DECODED_MINT_HISTORY_HASH_DOMAIN,
        &DecodedMintHistoryDigestV1 {
            schema_version: TRANSFER_FEE_HISTORY_SCHEMA_VERSION,
            mint,
            cutoff_slot,
            token_program: reconstruction.token_program,
            decimals: reconstruction.decimals,
            initialized_token_2022_extensions: reconstruction
                .initialized_token_2022_extensions
                .as_deref(),
            transfer_fee_config_ever_exists: reconstruction.transfer_fee_config_ever_exists,
            initialization_signature: reconstruction.initialization_signature.as_deref(),
            initialization_slot: reconstruction.initialization_slot,
            signature_page_artifacts: &crawl.pages,
            block_artifacts: &block_artifacts_for_mint,
            mutations: &reconstruction.mutations,
            blockers: &blocker_rows,
        },
    )?;
    let history = MintTransferFeeHistoryV1 {
        mint: mint.to_owned(),
        proof_basis: MintTransferFeeProofBasisV1::CompleteFinalizedAddressHistory,
        history_root_proven: true,
        signature_page_count: crawl.pages.len() as u64,
        signatures_through_cutoff: signatures.len() as u64,
        successful_signatures_through_cutoff: signatures
            .iter()
            .filter(|signature| signature.succeeded)
            .count() as u64,
        failed_signatures_through_cutoff: signatures
            .iter()
            .filter(|signature| !signature.succeeded)
            .count() as u64,
        block_slots_expected: expected_slots.len() as u64,
        block_slots_fetched: fetched_slots.len() as u64,
        token_program: reconstruction.token_program,
        decimals: reconstruction.decimals,
        initialized_token_2022_extensions: reconstruction.initialized_token_2022_extensions,
        transfer_fee_config_ever_exists: reconstruction.transfer_fee_config_ever_exists,
        initialization_signature: reconstruction.initialization_signature,
        initialization_slot: reconstruction.initialization_slot,
        decoded_mint_history_sha256,
        transfer_fee_epoch_count: exact_schedule
            .as_ref()
            .map_or(0, |schedule| schedule.epochs.len() as u64),
        signature_page_artifacts: crawl.pages.clone(),
        block_artifacts: block_artifacts_for_mint,
        blockers: blocker_rows,
    };
    Ok((history, exact_schedule))
}

#[derive(Clone, Copy)]
struct ProvenTransactionV1<'a> {
    slot: u64,
    transaction: &'a ArchivedTransaction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct MintInstructionOrderV1 {
    slot: u64,
    transaction_index: u32,
    outer_instruction_index: u32,
    inner_execution_index: Option<u32>,
}

#[derive(Clone, Debug, Serialize)]
struct MintMutationV1 {
    order: MintInstructionOrderV1,
    signature: String,
    program: TokenProgramKindV1,
    raw_data_sha256: String,
    kind: MintMutationKindV1,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum MintMutationKindV1 {
    InitializeMint {
        decimals: u8,
    },
    CloseMint,
    InitializeTransferFee {
        transfer_fee_basis_points: u16,
        maximum_fee: u64,
    },
    SetTransferFee {
        transfer_fee_basis_points: u16,
        maximum_fee: u64,
    },
    TransferHook,
    MintExtension {
        extension: Token2022MintExtensionV1,
    },
}

struct MintReconstructionV1 {
    token_program: Option<TokenProgramKindV1>,
    decimals: Option<u8>,
    initialized_token_2022_extensions: Option<Vec<Token2022MintExtensionV1>>,
    transfer_fee_config_ever_exists: Option<bool>,
    initialization_signature: Option<String>,
    initialization_slot: Option<u64>,
    schedule: Option<MintTransferFeeScheduleV1>,
    mutations: Vec<MintMutationV1>,
    blockers: Vec<TransferFeeHistoryBlockerV1>,
}

#[derive(Serialize)]
struct DecodedMintHistoryDigestV1<'a> {
    schema_version: u16,
    mint: &'a str,
    cutoff_slot: u64,
    token_program: Option<TokenProgramKindV1>,
    decimals: Option<u8>,
    initialized_token_2022_extensions: Option<&'a [Token2022MintExtensionV1]>,
    transfer_fee_config_ever_exists: Option<bool>,
    initialization_signature: Option<&'a str>,
    initialization_slot: Option<u64>,
    signature_page_artifacts: &'a [ArtifactChecksum],
    block_artifacts: &'a [ArtifactChecksum],
    mutations: &'a [MintMutationV1],
    blockers: &'a [TransferFeeHistoryBlockerV1],
}

fn reconstruct_mint_schedule(
    mint: &str,
    epoch_schedule: &EpochScheduleV1,
    transactions: &[ProvenTransactionV1<'_>],
) -> Result<MintReconstructionV1, TransferFeeExtractionError> {
    let mut blockers = BTreeSet::new();
    let mut mutations = Vec::new();
    for transaction in transactions {
        match decode_mint_mutations(mint, *transaction) {
            Ok(mut decoded) => mutations.append(&mut decoded),
            Err(code) => {
                blockers.insert(blocker(
                    mint,
                    code,
                    Some(transaction.transaction.signature.as_str()),
                    Some(transaction.slot),
                ));
            }
        }
    }
    mutations.sort_by_key(|mutation| mutation.order);
    let initializations = mutations
        .iter()
        .filter(|mutation| matches!(mutation.kind, MintMutationKindV1::InitializeMint { .. }))
        .collect::<Vec<_>>();
    if initializations.is_empty() {
        blockers.insert(blocker(
            mint,
            TransferFeeHistoryBlockerCodeV1::MintInitializationMissing,
            None,
            None,
        ));
    } else if initializations.len() > 1 {
        blockers.insert(blocker(
            mint,
            TransferFeeHistoryBlockerCodeV1::MultipleMintInitializations,
            initializations.last().map(|row| row.signature.as_str()),
            initializations.last().map(|row| row.order.slot),
        ));
    }
    let initialization = initializations.first().copied();
    let token_program = initialization.map(|row| row.program);
    let decimals = initialization.and_then(|row| match row.kind {
        MintMutationKindV1::InitializeMint { decimals } => Some(decimals),
        _ => None,
    });
    if let Some(owner) = token_program {
        if mutations.iter().any(|mutation| {
            matches!(
                mutation.kind,
                MintMutationKindV1::InitializeTransferFee { .. }
                    | MintMutationKindV1::SetTransferFee { .. }
                    | MintMutationKindV1::TransferHook
                    | MintMutationKindV1::MintExtension { .. }
            ) && mutation.program != owner
        }) {
            blockers.insert(blocker(
                mint,
                TransferFeeHistoryBlockerCodeV1::AmbiguousTokenProgram,
                None,
                None,
            ));
        }
    }
    if let Some(close) = mutations
        .iter()
        .find(|mutation| matches!(mutation.kind, MintMutationKindV1::CloseMint))
    {
        blockers.insert(blocker(
            mint,
            TransferFeeHistoryBlockerCodeV1::MintClosedOrReinitialized,
            Some(close.signature.as_str()),
            Some(close.order.slot),
        ));
    }
    if let Some(hook) = mutations
        .iter()
        .find(|mutation| matches!(mutation.kind, MintMutationKindV1::TransferHook))
    {
        blockers.insert(blocker(
            mint,
            TransferFeeHistoryBlockerCodeV1::UnsupportedTransferHook,
            Some(hook.signature.as_str()),
            Some(hook.order.slot),
        ));
    }

    let decoded_extensions = mutations
        .iter()
        .filter_map(|mutation| match mutation.kind {
            MintMutationKindV1::InitializeTransferFee { .. }
            | MintMutationKindV1::SetTransferFee { .. } => {
                Some(Token2022MintExtensionV1::TransferFeeConfig)
            }
            MintMutationKindV1::TransferHook => Some(Token2022MintExtensionV1::TransferHook),
            MintMutationKindV1::MintExtension { extension } => Some(extension),
            _ => None,
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if let Some(unsupported) = mutations.iter().find(|mutation| {
        matches!(
            mutation.kind,
            MintMutationKindV1::MintExtension { extension }
                if !matches!(
                    extension,
                    Token2022MintExtensionV1::MetadataPointer
                        | Token2022MintExtensionV1::TokenMetadata
                )
        )
    }) {
        blockers.insert(blocker(
            mint,
            TransferFeeHistoryBlockerCodeV1::UnsupportedMintExtension,
            Some(unsupported.signature.as_str()),
            Some(unsupported.order.slot),
        ));
    }
    let (initialized_token_2022_extensions, transfer_fee_config_ever_exists) = match token_program {
        Some(TokenProgramKindV1::LegacyToken) => (Some(Vec::new()), Some(false)),
        Some(TokenProgramKindV1::Token2022)
            if !blockers.iter().any(|blocker| {
                blocker.code == TransferFeeHistoryBlockerCodeV1::UnknownMintExtension
            }) =>
        {
            let has_fee = decoded_extensions.contains(&Token2022MintExtensionV1::TransferFeeConfig);
            (Some(decoded_extensions), Some(has_fee))
        }
        Some(TokenProgramKindV1::Token2022) => (None, None),
        None => (None, None),
    };

    let schedule = match (initialization, token_program) {
        (Some(initialization), Some(TokenProgramKindV1::LegacyToken)) => {
            if mutations.iter().any(|mutation| {
                matches!(
                    mutation.kind,
                    MintMutationKindV1::InitializeTransferFee { .. }
                        | MintMutationKindV1::SetTransferFee { .. }
                )
            }) {
                blockers.insert(blocker(
                    mint,
                    TransferFeeHistoryBlockerCodeV1::AmbiguousTokenProgram,
                    None,
                    None,
                ));
                None
            } else {
                Some(MintTransferFeeScheduleV1 {
                    schema_version: TRANSFER_FEE_SCHEMA_VERSION,
                    mint: mint.to_owned(),
                    token_program: TokenProgramKindV1::LegacyToken,
                    epochs: vec![fee_epoch(0, 0, 0, initialization)?],
                })
            }
        }
        (Some(initialization), Some(TokenProgramKindV1::Token2022)) => {
            let fee_initializations = mutations
                .iter()
                .filter(|mutation| {
                    matches!(
                        mutation.kind,
                        MintMutationKindV1::InitializeTransferFee { .. }
                    )
                })
                .collect::<Vec<_>>();
            if fee_initializations.len() > 1 {
                let duplicate = fee_initializations[1];
                blockers.insert(blocker(
                    mint,
                    TransferFeeHistoryBlockerCodeV1::TransferFeeInitializationDuplicated,
                    Some(duplicate.signature.as_str()),
                    Some(duplicate.order.slot),
                ));
                None
            } else {
                let fee_initialization = fee_initializations.first().copied();
                let initial_fee_source = fee_initialization.unwrap_or(initialization);
                let (initial_bps, initial_cap) = fee_initialization
                    .map(|mutation| match mutation.kind {
                        MintMutationKindV1::InitializeTransferFee {
                            transfer_fee_basis_points,
                            maximum_fee,
                        } => (transfer_fee_basis_points, maximum_fee),
                        _ => unreachable!("filtered to fee initialization"),
                    })
                    .unwrap_or((0, 0));
                let mut epochs = vec![fee_epoch(0, initial_bps, initial_cap, initial_fee_source)?];
                for mutation in mutations.iter().filter(|mutation| {
                    matches!(mutation.kind, MintMutationKindV1::SetTransferFee { .. })
                }) {
                    let Some(fee_initialization) = fee_initialization else {
                        blockers.insert(blocker(
                            mint,
                            TransferFeeHistoryBlockerCodeV1::TransferFeeSetBeforeInitialization,
                            Some(mutation.signature.as_str()),
                            Some(mutation.order.slot),
                        ));
                        continue;
                    };
                    if mutation.order <= fee_initialization.order {
                        blockers.insert(blocker(
                            mint,
                            TransferFeeHistoryBlockerCodeV1::TransferFeeSetBeforeInitialization,
                            Some(mutation.signature.as_str()),
                            Some(mutation.order.slot),
                        ));
                        continue;
                    }
                    let MintMutationKindV1::SetTransferFee {
                        transfer_fee_basis_points,
                        maximum_fee,
                    } = mutation.kind
                    else {
                        unreachable!("filtered to fee update")
                    };
                    let current_epoch = epoch_schedule.epoch(mutation.order.slot)?;
                    // SPL Token-2022 retains an activated newer value, but a
                    // still-pending value is overwritten and never becomes
                    // economic history.
                    epochs.retain(|epoch| epoch.effective_epoch <= current_epoch);
                    let effective_epoch = current_epoch.saturating_add(2);
                    epochs.push(fee_epoch(
                        effective_epoch,
                        transfer_fee_basis_points,
                        maximum_fee,
                        mutation,
                    )?);
                }
                Some(MintTransferFeeScheduleV1 {
                    schema_version: TRANSFER_FEE_SCHEMA_VERSION,
                    mint: mint.to_owned(),
                    token_program: TokenProgramKindV1::Token2022,
                    epochs,
                })
            }
        }
        _ => None,
    };
    Ok(MintReconstructionV1 {
        token_program,
        decimals,
        initialized_token_2022_extensions,
        transfer_fee_config_ever_exists,
        initialization_signature: initialization.map(|row| row.signature.clone()),
        initialization_slot: initialization.map(|row| row.order.slot),
        schedule,
        mutations,
        blockers: blockers.into_iter().collect(),
    })
}

fn fee_epoch(
    effective_epoch: u64,
    transfer_fee_basis_points: u16,
    maximum_fee: u64,
    source: &MintMutationV1,
) -> Result<TransferFeeEpochV1, TransferFeeExtractionError> {
    if transfer_fee_basis_points > 10_000 {
        return Err(TransferFeeExtractionError::InvalidArtifact(
            "successful transfer-fee mutation exceeds 10,000 bps".to_owned(),
        ));
    }
    Ok(TransferFeeEpochV1 {
        effective_epoch,
        transfer_fee_basis_points,
        maximum_fee: TokenAmount::new(maximum_fee.to_string()).map_err(|_| {
            TransferFeeExtractionError::InvalidArtifact(
                "transfer-fee maximum is not an exact token integer".to_owned(),
            )
        })?,
        source_signature: source.signature.clone(),
        source_slot: source.order.slot,
        decoded_instruction_sha256: source.raw_data_sha256.clone(),
    })
}

fn decode_mint_mutations(
    mint: &str,
    transaction: ProvenTransactionV1<'_>,
) -> Result<Vec<MintMutationV1>, TransferFeeHistoryBlockerCodeV1> {
    let account_keys = transaction_account_keys(transaction.transaction)?;
    let message = transaction
        .transaction
        .transaction
        .get("message")
        .ok_or(TransferFeeHistoryBlockerCodeV1::InvalidTokenInstruction)?;
    let outer = message
        .get("instructions")
        .and_then(JsonValue::as_array)
        .ok_or(TransferFeeHistoryBlockerCodeV1::InvalidTokenInstruction)?;
    let inner = transaction
        .transaction
        .meta
        .get("innerInstructions")
        .ok_or(TransferFeeHistoryBlockerCodeV1::InvalidTokenInstruction)?;
    let inner_groups = if inner.is_null() {
        // A null inner-instruction inventory cannot prove absence of a CPI fee
        // or hook mutation for a transaction selected by the mint address.
        return Err(TransferFeeHistoryBlockerCodeV1::InvalidTokenInstruction);
    } else {
        inner
            .as_array()
            .ok_or(TransferFeeHistoryBlockerCodeV1::InvalidTokenInstruction)?
    };
    let mut grouped = BTreeMap::<u32, &[JsonValue]>::new();
    for group in inner_groups {
        let index = group
            .get("index")
            .and_then(JsonValue::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or(TransferFeeHistoryBlockerCodeV1::InvalidTokenInstruction)?;
        if usize::try_from(index)
            .ok()
            .is_none_or(|index| index >= outer.len())
            || grouped
                .insert(
                    index,
                    group
                        .get("instructions")
                        .and_then(JsonValue::as_array)
                        .map(Vec::as_slice)
                        .ok_or(TransferFeeHistoryBlockerCodeV1::InvalidTokenInstruction)?,
                )
                .is_some()
        {
            return Err(TransferFeeHistoryBlockerCodeV1::InvalidTokenInstruction);
        }
    }
    let mut mutations = Vec::new();
    for (outer_index, instruction) in outer.iter().enumerate() {
        decode_token_instruction(
            mint,
            transaction,
            instruction,
            &account_keys,
            u32::try_from(outer_index)
                .map_err(|_| TransferFeeHistoryBlockerCodeV1::InvalidTokenInstruction)?,
            None,
            &mut mutations,
        )?;
        if let Some(instructions) = grouped.get(&(outer_index as u32)) {
            for (inner_index, instruction) in instructions.iter().enumerate() {
                decode_token_instruction(
                    mint,
                    transaction,
                    instruction,
                    &account_keys,
                    u32::try_from(outer_index)
                        .map_err(|_| TransferFeeHistoryBlockerCodeV1::InvalidTokenInstruction)?,
                    Some(
                        u32::try_from(inner_index).map_err(|_| {
                            TransferFeeHistoryBlockerCodeV1::InvalidTokenInstruction
                        })?,
                    ),
                    &mut mutations,
                )?;
            }
        }
    }
    Ok(mutations)
}

#[allow(clippy::too_many_arguments)]
fn decode_token_instruction(
    mint: &str,
    transaction: ProvenTransactionV1<'_>,
    raw: &JsonValue,
    account_keys: &[String],
    outer_instruction_index: u32,
    inner_execution_index: Option<u32>,
    output: &mut Vec<MintMutationV1>,
) -> Result<(), TransferFeeHistoryBlockerCodeV1> {
    let Some(program_index) = raw.get("programIdIndex") else {
        if raw
            .get("programId")
            .and_then(JsonValue::as_str)
            .is_some_and(|program| {
                program == LEGACY_TOKEN_PROGRAM_ID || program == TOKEN_2022_PROGRAM_ID
            })
        {
            return Err(TransferFeeHistoryBlockerCodeV1::InvalidTokenInstruction);
        }
        return Ok(());
    };
    let program_index = program_index
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .ok_or(TransferFeeHistoryBlockerCodeV1::InvalidTokenInstruction)?;
    let Some(program) = account_keys.get(program_index) else {
        return Err(TransferFeeHistoryBlockerCodeV1::InvalidTokenInstruction);
    };
    let program_kind = match program.as_str() {
        LEGACY_TOKEN_PROGRAM_ID => TokenProgramKindV1::LegacyToken,
        TOKEN_2022_PROGRAM_ID => TokenProgramKindV1::Token2022,
        _ => return Ok(()),
    };
    let indices = raw
        .get("accounts")
        .and_then(JsonValue::as_array)
        .ok_or(TransferFeeHistoryBlockerCodeV1::InvalidTokenInstruction)?;
    let accounts = indices
        .iter()
        .map(|index| {
            index
                .as_u64()
                .and_then(|value| usize::try_from(value).ok())
                .and_then(|index| account_keys.get(index))
                .cloned()
                .ok_or(TransferFeeHistoryBlockerCodeV1::InvalidTokenInstruction)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let data = raw
        .get("data")
        .and_then(JsonValue::as_str)
        .ok_or(TransferFeeHistoryBlockerCodeV1::InvalidTokenInstruction)
        .and_then(|encoded| {
            bs58::decode(encoded)
                .into_vec()
                .map_err(|_| TransferFeeHistoryBlockerCodeV1::InvalidTokenInstruction)
        })?;
    let first_is_mint = accounts.first().is_some_and(|account| account == mint);
    let decoded = match TokenInstruction::unpack(&data) {
        Ok(decoded) => decoded,
        Err(_) if program_kind == TokenProgramKindV1::Token2022 => {
            if TokenMetadataInstruction::unpack(&data).is_ok() {
                if first_is_mint {
                    output.push(MintMutationV1 {
                        order: MintInstructionOrderV1 {
                            slot: transaction.slot,
                            transaction_index: transaction.transaction.transaction_index,
                            outer_instruction_index,
                            inner_execution_index,
                        },
                        signature: transaction.transaction.signature.clone(),
                        program: program_kind,
                        raw_data_sha256: sha256_hex(&data),
                        kind: MintMutationKindV1::MintExtension {
                            extension: Token2022MintExtensionV1::TokenMetadata,
                        },
                    });
                }
                return Ok(());
            }
            if accounts.iter().any(|account| account == mint) {
                return Err(TransferFeeHistoryBlockerCodeV1::UnknownMintExtension);
            }
            return Ok(());
        }
        Err(_) if accounts.iter().any(|account| account == mint) => {
            return Err(TransferFeeHistoryBlockerCodeV1::InvalidTokenInstruction);
        }
        Err(_) => return Ok(()),
    };
    let kind = match decoded {
        TokenInstruction::InitializeMint { decimals, .. }
        | TokenInstruction::InitializeMint2 { decimals, .. }
            if first_is_mint =>
        {
            Some(MintMutationKindV1::InitializeMint { decimals })
        }
        TokenInstruction::CloseAccount if first_is_mint => Some(MintMutationKindV1::CloseMint),
        TokenInstruction::TransferHookExtension if first_is_mint => {
            Some(MintMutationKindV1::TransferHook)
        }
        TokenInstruction::TransferFeeExtension if first_is_mint => {
            let extension = TransferFeeInstruction::unpack(
                data.get(1..)
                    .ok_or(TransferFeeHistoryBlockerCodeV1::InvalidTokenInstruction)?,
            )
            .map_err(|_| TransferFeeHistoryBlockerCodeV1::InvalidTokenInstruction)?;
            match extension {
                TransferFeeInstruction::InitializeTransferFeeConfig {
                    transfer_fee_basis_points,
                    maximum_fee,
                    ..
                } => Some(MintMutationKindV1::InitializeTransferFee {
                    transfer_fee_basis_points,
                    maximum_fee,
                }),
                TransferFeeInstruction::SetTransferFee {
                    transfer_fee_basis_points,
                    maximum_fee,
                } => Some(MintMutationKindV1::SetTransferFee {
                    transfer_fee_basis_points,
                    maximum_fee,
                }),
                _ => Some(MintMutationKindV1::MintExtension {
                    extension: Token2022MintExtensionV1::TransferFeeConfig,
                }),
            }
        }
        TokenInstruction::InitializeMintCloseAuthority { .. } if first_is_mint => {
            Some(MintMutationKindV1::MintExtension {
                extension: Token2022MintExtensionV1::MintCloseAuthority,
            })
        }
        TokenInstruction::ConfidentialTransferExtension if first_is_mint => {
            Some(MintMutationKindV1::MintExtension {
                extension: Token2022MintExtensionV1::ConfidentialTransferMint,
            })
        }
        TokenInstruction::DefaultAccountStateExtension if first_is_mint => {
            Some(MintMutationKindV1::MintExtension {
                extension: Token2022MintExtensionV1::DefaultAccountState,
            })
        }
        TokenInstruction::InitializeNonTransferableMint if first_is_mint => {
            Some(MintMutationKindV1::MintExtension {
                extension: Token2022MintExtensionV1::NonTransferable,
            })
        }
        TokenInstruction::InterestBearingMintExtension if first_is_mint => {
            Some(MintMutationKindV1::MintExtension {
                extension: Token2022MintExtensionV1::InterestBearingConfig,
            })
        }
        TokenInstruction::InitializePermanentDelegate { .. } if first_is_mint => {
            Some(MintMutationKindV1::MintExtension {
                extension: Token2022MintExtensionV1::PermanentDelegate,
            })
        }
        TokenInstruction::ConfidentialTransferFeeExtension if first_is_mint => {
            Some(MintMutationKindV1::MintExtension {
                extension: Token2022MintExtensionV1::ConfidentialTransferFeeConfig,
            })
        }
        TokenInstruction::MetadataPointerExtension if first_is_mint => {
            Some(MintMutationKindV1::MintExtension {
                extension: Token2022MintExtensionV1::MetadataPointer,
            })
        }
        TokenInstruction::GroupPointerExtension if first_is_mint => {
            Some(MintMutationKindV1::MintExtension {
                extension: Token2022MintExtensionV1::GroupPointer,
            })
        }
        TokenInstruction::GroupMemberPointerExtension if first_is_mint => {
            Some(MintMutationKindV1::MintExtension {
                extension: Token2022MintExtensionV1::GroupMemberPointer,
            })
        }
        TokenInstruction::ConfidentialMintBurnExtension if first_is_mint => {
            Some(MintMutationKindV1::MintExtension {
                extension: Token2022MintExtensionV1::ConfidentialMintBurn,
            })
        }
        _ => None,
    };
    if let Some(kind) = kind {
        output.push(MintMutationV1 {
            order: MintInstructionOrderV1 {
                slot: transaction.slot,
                transaction_index: transaction.transaction.transaction_index,
                outer_instruction_index,
                inner_execution_index,
            },
            signature: transaction.transaction.signature.clone(),
            program: program_kind,
            raw_data_sha256: sha256_hex(&data),
            kind,
        });
    }
    Ok(())
}

fn transaction_account_keys(
    transaction: &ArchivedTransaction,
) -> Result<Vec<String>, TransferFeeHistoryBlockerCodeV1> {
    let message = transaction
        .transaction
        .get("message")
        .ok_or(TransferFeeHistoryBlockerCodeV1::InvalidTokenInstruction)?;
    let static_keys = message
        .get("accountKeys")
        .and_then(JsonValue::as_array)
        .ok_or(TransferFeeHistoryBlockerCodeV1::InvalidTokenInstruction)?;
    let mut keys = static_keys
        .iter()
        .map(parse_json_account_key)
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(loaded) = transaction.meta.get("loadedAddresses") {
        for lane in ["writable", "readonly"] {
            if let Some(addresses) = loaded.get(lane).and_then(JsonValue::as_array) {
                keys.extend(
                    addresses
                        .iter()
                        .map(parse_json_account_key)
                        .collect::<Result<Vec<_>, _>>()?,
                );
            }
        }
    }
    Ok(keys)
}

fn parse_json_account_key(value: &JsonValue) -> Result<String, TransferFeeHistoryBlockerCodeV1> {
    let text = value
        .as_str()
        .or_else(|| value.get("pubkey").and_then(JsonValue::as_str));
    let text = text.ok_or(TransferFeeHistoryBlockerCodeV1::InvalidTokenInstruction)?;
    parse_pubkey(text).map_err(|_| TransferFeeHistoryBlockerCodeV1::InvalidTokenInstruction)?;
    Ok(text.to_owned())
}

fn validate_full_block(
    block: &ArchivedBlock,
    expected_slot: u64,
) -> Result<(), TransferFeeExtractionError> {
    if block.slot != expected_slot
        || !matches!(&block.proof_basis, ArchivedBlockProofBasisV1::FullPayloadV1)
        || block.blockhash.trim().is_empty()
        || block.previous_blockhash.trim().is_empty()
        || block.full_transaction_count != block.transactions.len() as u64
        || !valid_sha256(&block.full_transaction_inventory_sha256)
        || block.full_transaction_inventory_sha256
            != transaction_inventory_sha256(&block.transactions)?
    {
        return Err(TransferFeeExtractionError::InvalidArtifact(
            "full mint-history block identity is invalid".to_owned(),
        ));
    }
    let mut signatures = BTreeSet::new();
    for (expected_index, transaction) in block.transactions.iter().enumerate() {
        if transaction.transaction_index as usize != expected_index
            || !matches!(
                &transaction.proof_basis,
                ArchivedTransactionProofBasisV1::FullBlockPayloadV1
            )
            || !valid_solana_signature(&transaction.signature)
            || !signatures.insert(transaction.signature.clone())
            || !valid_sha256(&transaction.rpc_payload_sha256)
        {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "full mint-history transaction inventory is invalid".to_owned(),
            ));
        }
        let err = transaction.meta.get("err").ok_or_else(|| {
            TransferFeeExtractionError::InvalidArtifact(
                "full block transaction metadata omits err status".to_owned(),
            )
        })?;
        if transaction.succeeded != err.is_null() {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "full block transaction status disagrees with metadata".to_owned(),
            ));
        }
    }
    Ok(())
}

fn retain_target_transactions(
    mut block: ArchivedBlock,
    target_signatures: &BTreeSet<String>,
) -> Result<ArchivedBlock, TransferFeeExtractionError> {
    let slot = block.slot;
    validate_full_block(&block, slot)?;
    block
        .transactions
        .retain(|transaction| target_signatures.contains(&transaction.signature));
    let expected = target_signatures.iter().cloned().collect::<Vec<_>>();
    validate_retained_block(&block, slot, &expected)?;
    Ok(block)
}

fn validate_retained_block(
    block: &ArchivedBlock,
    expected_slot: u64,
    target_signatures: &[String],
) -> Result<(), TransferFeeExtractionError> {
    if block.slot != expected_slot
        || !matches!(&block.proof_basis, ArchivedBlockProofBasisV1::FullPayloadV1)
        || block.blockhash.trim().is_empty()
        || block.previous_blockhash.trim().is_empty()
        || block.full_transaction_count < block.transactions.len() as u64
        || !valid_sha256(&block.full_transaction_inventory_sha256)
    {
        return Err(TransferFeeExtractionError::InvalidArtifact(
            "retained mint-history block identity is invalid".to_owned(),
        ));
    }
    let expected = target_signatures.iter().cloned().collect::<BTreeSet<_>>();
    if expected.len() != target_signatures.len() {
        return Err(TransferFeeExtractionError::InvalidArtifact(
            "target mint-history signatures are duplicated".to_owned(),
        ));
    }
    let mut indexes = BTreeSet::new();
    let mut signatures = BTreeSet::new();
    let mut previous_index = None;
    for transaction in &block.transactions {
        if u64::from(transaction.transaction_index) >= block.full_transaction_count
            || !matches!(
                &transaction.proof_basis,
                ArchivedTransactionProofBasisV1::FullBlockPayloadV1
            )
            || previous_index.is_some_and(|index| transaction.transaction_index <= index)
            || !indexes.insert(transaction.transaction_index)
            || !valid_solana_signature(&transaction.signature)
            || !signatures.insert(transaction.signature.clone())
            || !valid_sha256(&transaction.rpc_payload_sha256)
        {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "retained mint-history transaction inventory is invalid".to_owned(),
            ));
        }
        previous_index = Some(transaction.transaction_index);
        let err = transaction.meta.get("err").ok_or_else(|| {
            TransferFeeExtractionError::InvalidArtifact(
                "retained block transaction metadata omits err status".to_owned(),
            )
        })?;
        if transaction.succeeded != err.is_null() {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "retained block transaction status disagrees with metadata".to_owned(),
            ));
        }
    }
    if signatures != expected {
        return Err(TransferFeeExtractionError::InvalidArtifact(
            "retained block signatures differ from the independent address crawl".to_owned(),
        ));
    }
    Ok(())
}

fn transaction_inventory_sha256(
    transactions: &[ArchivedTransaction],
) -> Result<String, TransferFeeExtractionError> {
    let inventory = transactions
        .iter()
        .map(|transaction| {
            serde_json::json!({
                "transaction_index": transaction.transaction_index,
                "signature": transaction.signature,
                "rpc_payload_sha256": transaction.rpc_payload_sha256,
            })
        })
        .collect::<Vec<_>>();
    Ok(sha256_hex(&serde_json::to_vec(&inventory)?))
}

fn blocker(
    mint: &str,
    code: TransferFeeHistoryBlockerCodeV1,
    signature: Option<&str>,
    slot: Option<u64>,
) -> TransferFeeHistoryBlockerV1 {
    TransferFeeHistoryBlockerV1 {
        mint: mint.to_owned(),
        code,
        signature: signature.map(str::to_owned),
        slot,
    }
}

fn validate_common_binding(
    run_id: &str,
    cutoff_slot: u64,
    provider_fingerprint_sha256: &str,
    request: &TransferFeeExtractionRequestV1<'_>,
) -> Result<(), TransferFeeExtractionError> {
    if run_id != request.run_id
        || cutoff_slot != request.cutoff_slot
        || provider_fingerprint_sha256 != request.provider.secret_fingerprint_sha256
    {
        return Err(TransferFeeExtractionError::InvalidArtifact(
            "resumed artifact belongs to a different run, cutoff, or provider".to_owned(),
        ));
    }
    Ok(())
}

fn reject_trailing_signature_pages(
    directory: &Path,
    terminal_index: u64,
) -> Result<(), TransferFeeExtractionError> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(index) = name
            .strip_suffix(".json")
            .and_then(|stem| stem.parse::<u64>().ok())
        else {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "unexpected file in mint signature-page directory".to_owned(),
            ));
        };
        if index > terminal_index {
            return Err(TransferFeeExtractionError::InvalidArtifact(
                "signature pages exist after the explicit empty terminal page".to_owned(),
            ));
        }
    }
    Ok(())
}

fn checksum_path(
    path: &Path,
    logical_name: String,
) -> Result<ArtifactChecksum, TransferFeeExtractionError> {
    Ok(ArtifactChecksum::from_bytes(logical_name, &fs::read(path)?))
}

fn hash_serialized<T: Serialize>(
    domain: &[u8],
    value: &T,
) -> Result<String, TransferFeeExtractionError> {
    let bytes = serde_json::to_vec(value)?;
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
    Ok(hex::encode(digest.finalize()))
}

fn canonical_jsonl_sha256<T: Serialize>(rows: &[T]) -> Result<String, TransferFeeExtractionError> {
    let mut digest = Sha256::new();
    for row in rows {
        digest.update(serde_json::to_vec(row)?);
        digest.update(b"\n");
    }
    Ok(hex::encode(digest.finalize()))
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, TransferFeeExtractionError> {
    Ok(serde_json::from_reader(BufReader::new(File::open(path)?))?)
}

fn write_or_validate_json<T>(path: &Path, value: &T) -> Result<(), TransferFeeExtractionError>
where
    T: Serialize + DeserializeOwned + PartialEq,
{
    if path.exists() {
        let existing: T = read_json(path)?;
        if existing != *value {
            return Err(TransferFeeExtractionError::InvalidArtifact(format!(
                "resumed artifact differs at {}",
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("sanitized-output")
            )));
        }
        Ok(())
    } else {
        write_json_atomic(path, value)
    }
}

fn read_json_zstd<T: DeserializeOwned>(path: &Path) -> Result<T, TransferFeeExtractionError> {
    let decoder = zstd::Decoder::new(BufReader::new(File::open(path)?))?;
    Ok(serde_json::from_reader(decoder)?)
}

fn write_json_zstd_atomic<T: Serialize>(
    target: &Path,
    value: &T,
) -> Result<(), TransferFeeExtractionError> {
    let parent = parent_directory(target);
    fs::create_dir_all(parent)?;
    let target_name = target
        .file_name()
        .ok_or_else(|| {
            TransferFeeExtractionError::InvalidArtifact("atomic output has no file name".to_owned())
        })?
        .to_string_lossy();
    let mut reserved = None;
    for _ in 0..32 {
        let sequence = PARTIAL_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(
            ".{target_name}.{}.{}.partial",
            std::process::id(),
            sequence
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => {
                reserved = Some((path, file));
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    let (partial, file) = reserved.ok_or_else(|| {
        TransferFeeExtractionError::InvalidArtifact(
            "could not reserve an atomic compressed transfer-fee output".to_owned(),
        )
    })?;
    let mut guard = PartialFileGuard {
        path: partial.clone(),
        committed: false,
    };
    let writer = BufWriter::new(file);
    let mut encoder = zstd::Encoder::new(writer, MINT_HISTORY_BLOCK_ZSTD_LEVEL)?;
    serde_json::to_writer(&mut encoder, value)?;
    encoder.write_all(b"\n")?;
    let mut writer = encoder.finish()?;
    writer.flush()?;
    writer.get_ref().sync_all()?;
    drop(writer);
    fs::rename(&partial, target)?;
    guard.committed = true;
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn remove_legacy_full_block_after_migration(
    legacy_path: &Path,
) -> Result<(), TransferFeeExtractionError> {
    match fs::remove_file(legacy_path) {
        Ok(()) => {
            File::open(parent_directory(legacy_path))?.sync_all()?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn write_json_atomic<T: Serialize>(
    target: &Path,
    value: &T,
) -> Result<(), TransferFeeExtractionError> {
    let parent = parent_directory(target);
    fs::create_dir_all(parent)?;
    let target_name = target
        .file_name()
        .ok_or_else(|| {
            TransferFeeExtractionError::InvalidArtifact("atomic output has no file name".to_owned())
        })?
        .to_string_lossy();
    let mut reserved = None;
    for _ in 0..32 {
        let sequence = PARTIAL_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(
            ".{target_name}.{}.{}.partial",
            std::process::id(),
            sequence
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => {
                reserved = Some((path, file));
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    let (partial, file) = reserved.ok_or_else(|| {
        TransferFeeExtractionError::InvalidArtifact(
            "could not reserve an atomic transfer-fee output".to_owned(),
        )
    })?;
    let mut guard = PartialFileGuard {
        path: partial.clone(),
        committed: false,
    };
    let mut writer = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    writer.get_ref().sync_all()?;
    drop(writer);
    fs::rename(&partial, target)?;
    guard.committed = true;
    File::open(parent)?.sync_all()?;
    Ok(())
}

struct PartialFileGuard {
    path: PathBuf,
    committed: bool,
}

impl Drop for PartialFileGuard {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn parent_directory(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn valid_sanitized_provider(provider: &SanitizedSource) -> bool {
    !provider.kind.trim().is_empty()
        && !provider.label.trim().is_empty()
        && !provider.kind.contains("://")
        && !provider.label.contains("://")
        && !provider.label.contains('@')
        && !provider.label.contains('=')
        && valid_sha256(&provider.secret_fingerprint_sha256)
}

fn valid_solana_signature(value: &str) -> bool {
    bs58::decode(value)
        .into_vec()
        .is_ok_and(|decoded| decoded.len() == 64)
}

fn valid_artifact_checksum(checksum: &ArtifactChecksum) -> bool {
    !checksum.logical_name.trim().is_empty() && valid_sha256(&checksum.sha256)
}

fn strictly_sorted_unique<T: Ord>(values: &[T]) -> bool {
    values.windows(2).all(|pair| pair[0] < pair[1])
}

fn parse_pubkey(value: &str) -> Result<Pubkey, ()> {
    Pubkey::from_str(value).map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        sync::{Arc, Mutex},
    };

    use serde_json::json;
    use solana_instruction::{AccountMeta, Instruction};
    use spl_token_2022::{
        extension::{
            metadata_pointer::instruction as metadata_pointer_instruction,
            transfer_fee::instruction as fee_instruction,
            transfer_hook::instruction as hook_instruction,
        },
        instruction as token_instruction,
    };

    use super::*;
    use crate::{
        canonical::ChainOrder,
        decode::{CpiFrameProvenanceV1, InstructionExecutionFrameV1, TokenAccountBalanceV1},
    };

    static TEST_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let sequence = TEST_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "v1-vs-dusk-transfer-fee-history-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Default)]
    struct RpcCalls {
        epoch: u64,
        pages: Vec<(String, Option<String>)>,
        blocks: Vec<u64>,
    }

    #[derive(Clone)]
    struct FakeRpc {
        epoch: RpcResponseProofV1<EpochScheduleV1>,
        pages: Arc<BTreeMap<(String, Option<String>), RpcResponseProofV1<Vec<ProgramSignatureV1>>>>,
        blocks: Arc<BTreeMap<u64, RpcResponseProofV1<Option<ArchivedBlock>>>>,
        calls: Arc<Mutex<RpcCalls>>,
    }

    #[derive(Clone)]
    struct FakeFinalizedMintRpc {
        responses: Arc<
            BTreeMap<
                String,
                Result<BoundRpcResponseProofV1<FinalizedAccountInfoV1>, RpcFailureKind>,
            >,
        >,
        calls: Arc<Mutex<Vec<(String, u64)>>>,
    }

    impl FinalizedMintAccountRpc for FakeFinalizedMintRpc {
        fn finalized_account_at_or_after_cutoff<'a>(
            &'a self,
            mint: &'a str,
            cutoff_slot: u64,
        ) -> Pin<
            Box<
                dyn Future<
                        Output = Result<
                            BoundRpcResponseProofV1<FinalizedAccountInfoV1>,
                            RpcFailureKind,
                        >,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                self.calls
                    .lock()
                    .unwrap()
                    .push((mint.to_owned(), cutoff_slot));
                self.responses
                    .get(mint)
                    .cloned()
                    .unwrap_or(Err(RpcFailureKind::InvalidResponse))
            })
        }
    }

    #[derive(Clone)]
    struct SelectedScopeRpc {
        epoch: RpcResponseProofV1<EpochScheduleV1>,
        accounts: FakeFinalizedMintRpc,
        epoch_calls: Arc<Mutex<u64>>,
    }

    impl TransferFeeHistoryRpc for SelectedScopeRpc {
        fn epoch_schedule<'a>(
            &'a self,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<RpcResponseProofV1<EpochScheduleV1>, RpcFailureKind>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                *self.epoch_calls.lock().unwrap() += 1;
                Ok(self.epoch.clone())
            })
        }

        fn signatures_page<'a>(
            &'a self,
            _mint: &'a str,
            _before: Option<&'a str>,
            _limit: u16,
        ) -> Pin<
            Box<
                dyn Future<
                        Output = Result<
                            RpcResponseProofV1<Vec<ProgramSignatureV1>>,
                            RpcFailureKind,
                        >,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async { Err(RpcFailureKind::InvalidResponse) })
        }

        fn full_block<'a>(
            &'a self,
            _slot: u64,
            _request_id: u64,
        ) -> Pin<
            Box<
                dyn Future<
                        Output = Result<RpcResponseProofV1<Option<ArchivedBlock>>, RpcFailureKind>,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async { Err(RpcFailureKind::InvalidResponse) })
        }
    }

    impl FinalizedMintAccountRpc for SelectedScopeRpc {
        fn finalized_account_at_or_after_cutoff<'a>(
            &'a self,
            mint: &'a str,
            cutoff_slot: u64,
        ) -> Pin<
            Box<
                dyn Future<
                        Output = Result<
                            BoundRpcResponseProofV1<FinalizedAccountInfoV1>,
                            RpcFailureKind,
                        >,
                    > + Send
                    + 'a,
            >,
        > {
            self.accounts
                .finalized_account_at_or_after_cutoff(mint, cutoff_slot)
        }
    }

    impl TransferFeeHistoryRpc for FakeRpc {
        fn epoch_schedule<'a>(
            &'a self,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<RpcResponseProofV1<EpochScheduleV1>, RpcFailureKind>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                self.calls.lock().unwrap().epoch += 1;
                Ok(self.epoch.clone())
            })
        }

        fn signatures_page<'a>(
            &'a self,
            mint: &'a str,
            before: Option<&'a str>,
            _limit: u16,
        ) -> Pin<
            Box<
                dyn Future<
                        Output = Result<
                            RpcResponseProofV1<Vec<ProgramSignatureV1>>,
                            RpcFailureKind,
                        >,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                let key = (mint.to_owned(), before.map(str::to_owned));
                self.calls.lock().unwrap().pages.push(key.clone());
                self.pages
                    .get(&key)
                    .cloned()
                    .ok_or(RpcFailureKind::InvalidResponse)
            })
        }

        fn full_block<'a>(
            &'a self,
            slot: u64,
            _request_id: u64,
        ) -> Pin<
            Box<
                dyn Future<
                        Output = Result<RpcResponseProofV1<Option<ArchivedBlock>>, RpcFailureKind>,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                self.calls.lock().unwrap().blocks.push(slot);
                self.blocks
                    .get(&slot)
                    .cloned()
                    .ok_or(RpcFailureKind::InvalidResponse)
            })
        }
    }

    fn pubkey(seed: u8) -> Pubkey {
        Pubkey::new_from_array([seed; 32])
    }

    fn signature(seed: u8) -> String {
        bs58::encode([seed; 64]).into_string()
    }

    fn rpc_hash(seed: char) -> String {
        seed.to_string().repeat(64)
    }

    fn epoch_proof() -> RpcResponseProofV1<EpochScheduleV1> {
        let hash = rpc_hash('a');
        RpcResponseProofV1 {
            raw_response_sha256: hash.clone(),
            value: EpochScheduleV1 {
                slots_per_epoch: 100,
                leader_schedule_slot_offset: 100,
                warmup: false,
                first_normal_epoch: 0,
                first_normal_slot: 0,
                source_response_sha256: hash,
            },
        }
    }

    fn signature_row(seed: u8, slot: u64, succeeded: bool) -> ProgramSignatureV1 {
        ProgramSignatureV1 {
            signature: signature(seed),
            slot,
            succeeded,
            rpc_block_time_hint: Some(slot as i64),
        }
    }

    fn add_pages(
        pages: &mut BTreeMap<(String, Option<String>), RpcResponseProofV1<Vec<ProgramSignatureV1>>>,
        mint: &str,
        rows: Vec<ProgramSignatureV1>,
        hash_seed: char,
    ) {
        let terminal_before = rows.last().map(|row| row.signature.clone());
        pages.insert(
            (mint.to_owned(), None),
            RpcResponseProofV1 {
                raw_response_sha256: rpc_hash(hash_seed),
                value: rows,
            },
        );
        pages.insert(
            (mint.to_owned(), terminal_before),
            RpcResponseProofV1 {
                raw_response_sha256: rpc_hash(char::from_u32(hash_seed as u32 + 1).unwrap()),
                value: Vec::new(),
            },
        );
    }

    fn archived_transaction(
        signature: String,
        succeeded: bool,
        instructions: &[Instruction],
    ) -> ArchivedTransaction {
        let mut keys = Vec::<Pubkey>::new();
        for instruction in instructions {
            if !keys.contains(&instruction.program_id) {
                keys.push(instruction.program_id);
            }
            for account in &instruction.accounts {
                if !keys.contains(&account.pubkey) {
                    keys.push(account.pubkey);
                }
            }
        }
        let compiled = instructions
            .iter()
            .map(|instruction| {
                json!({
                    "programIdIndex": keys.iter().position(|key| key == &instruction.program_id).unwrap(),
                    "accounts": instruction.accounts.iter().map(|account| {
                        keys.iter().position(|key| key == &account.pubkey).unwrap()
                    }).collect::<Vec<_>>(),
                    "data": bs58::encode(&instruction.data).into_string(),
                })
            })
            .collect::<Vec<_>>();
        let transaction = json!({
            "signatures": [signature.clone()],
            "message": {
                "accountKeys": keys.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "instructions": compiled,
            }
        });
        let meta = json!({
            "err": if succeeded { JsonValue::Null } else { json!({"InstructionError": [0, "Custom"]}) },
            "innerInstructions": [],
            "loadedAddresses": {"writable": [], "readonly": []}
        });
        let payload = json!({"transaction": transaction, "meta": meta});
        ArchivedTransaction {
            signature,
            transaction_index: 0,
            succeeded,
            rpc_payload_sha256: sha256_hex(&serde_json::to_vec(&payload).unwrap()),
            proof_basis: ArchivedTransactionProofBasisV1::FullBlockPayloadV1,
            transaction: payload["transaction"].clone(),
            meta: payload["meta"].clone(),
        }
    }

    fn block(slot: u64, transaction: ArchivedTransaction) -> ArchivedBlock {
        block_with_transactions(slot, vec![transaction])
    }

    fn block_with_transactions(
        slot: u64,
        mut transactions: Vec<ArchivedTransaction>,
    ) -> ArchivedBlock {
        for (index, transaction) in transactions.iter_mut().enumerate() {
            transaction.transaction_index = u32::try_from(index).unwrap();
        }
        let mut block = ArchivedBlock {
            slot,
            block_time_unix_seconds: Some(slot as i64),
            blockhash: format!("block-{slot}"),
            previous_blockhash: format!("block-{}", slot.saturating_sub(1)),
            parent_slot: slot.saturating_sub(1),
            full_transaction_count: transactions.len() as u64,
            full_transaction_inventory_sha256: String::new(),
            proof_basis: ArchivedBlockProofBasisV1::FullPayloadV1,
            transactions,
        };
        block.full_transaction_inventory_sha256 =
            transaction_inventory_sha256(&block.transactions).unwrap();
        block
    }

    fn add_block(
        blocks: &mut BTreeMap<u64, RpcResponseProofV1<Option<ArchivedBlock>>>,
        slot: u64,
        transaction: ArchivedTransaction,
    ) {
        blocks.insert(
            slot,
            RpcResponseProofV1 {
                raw_response_sha256: rpc_hash('b'),
                value: Some(block(slot, transaction)),
            },
        );
    }

    fn provider() -> SanitizedSource {
        SanitizedSource::from_secret("solana_rpc", "fixture_provider", "fixture-endpoint").unwrap()
    }

    fn selected_binding(mint: String, seed: u8, role: SelectedMintRoleV1) -> SelectedMintBindingV1 {
        SelectedMintBindingV1 {
            mint,
            selected_uses: vec![SelectedMintUseV1 {
                pair_address: pubkey(seed).to_string(),
                role,
            }],
        }
    }

    fn operation_evidence(
        mint: String,
        program: TokenProgramKindV1,
    ) -> SelectedOperationTokenProgramEvidenceV1 {
        SelectedOperationTokenProgramEvidenceV1 {
            mint,
            expected_relevant_operation_count: 1,
            classified_relevant_operation_count: 1,
            earliest_relevant_slot: Some(10),
            latest_relevant_slot: Some(10),
            observed_programs: vec![program],
            source_sha256: rpc_hash('b'),
        }
    }

    fn initialized_legacy_mint_data(decimals: u8) -> Vec<u8> {
        let mut state = SplTokenMint::default();
        state.decimals = decimals;
        state.is_initialized = true;
        let mut data = vec![0_u8; SplTokenMint::LEN];
        SplTokenMint::pack(state, &mut data).unwrap();
        data
    }

    fn finalized_mint_response(
        mint: &str,
        cutoff_slot: u64,
        observed_context_slot: u64,
        owner: &str,
        data: Vec<u8>,
    ) -> BoundRpcResponseProofV1<FinalizedAccountInfoV1> {
        BoundRpcResponseProofV1 {
            canonical_request_sha256: finalized_account_info_request_sha256(mint, cutoff_slot)
                .unwrap(),
            raw_response_sha256: rpc_hash('c'),
            value: FinalizedAccountInfoV1 {
                context_slot: observed_context_slot,
                owner: owner.to_owned(),
                lamports: 1,
                executable: false,
                rent_epoch: 7,
                account_data_sha256: sha256_hex(&data),
                data,
            },
        }
    }

    fn finalized_mint_rpc(
        mint: &str,
        response: BoundRpcResponseProofV1<FinalizedAccountInfoV1>,
    ) -> FakeFinalizedMintRpc {
        FakeFinalizedMintRpc {
            responses: Arc::new(BTreeMap::from([(mint.to_owned(), Ok(response))])),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn decoded_transaction_and_provenance(
        mint: &str,
        slot: u64,
        seed: u8,
        program_id: &str,
    ) -> (DecodedTransactionV1, TransactionExecutionProvenanceV1) {
        let transaction = DecodedTransactionV1 {
            schema_version: DECODE_SCHEMA_VERSION,
            decoder_version_id: "fixture-decoder".to_owned(),
            signature: signature(seed),
            rpc_payload_sha256: rpc_hash('d'),
            slot,
            transaction_index: 0,
            block_time_unix_seconds: slot as i64,
            program_instruction_count: 0,
            state_changing_instruction_count: 0,
            emitted_assertion_count: 0,
            unknown_program_instruction_hashes: Vec::new(),
            non_target_outer_touched_accounts: Vec::new(),
            token_balances: vec![TokenAccountBalanceV1 {
                account_index: 0,
                account: pubkey(seed.wrapping_add(1)).to_string(),
                mint: mint.to_owned(),
                token_program: match program_id {
                    LEGACY_TOKEN_PROGRAM_ID => TokenProgramKindV1::LegacyToken,
                    TOKEN_2022_PROGRAM_ID => TokenProgramKindV1::Token2022,
                    _ => panic!("fixture token program must be supported"),
                },
                owner: None,
                decimals: 6,
                pre_amount: TokenAmount::zero(),
                post_amount: TokenAmount::new("1").unwrap(),
            }],
            instructions: Vec::new(),
        };
        let provenance = TransactionExecutionProvenanceV1 {
            schema_version: DECODE_SCHEMA_VERSION,
            decoder_version_id: transaction.decoder_version_id.clone(),
            signature: transaction.signature.clone(),
            slot,
            transaction_index: 0,
            frames: vec![InstructionExecutionFrameV1 {
                schema_version: DECODE_SCHEMA_VERSION,
                order: ChainOrder {
                    slot,
                    transaction_index: 0,
                    outer_instruction_index: 0,
                    inner_execution_index: None,
                    event_sequence: 0,
                },
                program_id: program_id.to_owned(),
                is_target_program: false,
                stack_height: Some(1),
                parent_order: None,
                execution_path: vec![0],
                provenance: CpiFrameProvenanceV1::Complete,
                decoded_target_class: None,
            }],
            token_cpis: Vec::new(),
        };
        (transaction, provenance)
    }

    fn write_canonical_jsonl<T: Serialize>(path: &Path, rows: &[T]) {
        let mut writer = BufWriter::new(File::create(path).unwrap());
        for row in rows {
            serde_json::to_writer(&mut writer, row).unwrap();
            writer.write_all(b"\n").unwrap();
        }
        writer.flush().unwrap();
    }

    #[test]
    fn selected_mint_inventory_includes_lp_mints_and_all_roles() {
        let shared = pubkey(61).to_string();
        let lp = pubkey(62).to_string();
        let bindings = selected_mint_bindings(&[MarketMintBindingV1 {
            pair_address: pubkey(60).to_string(),
            token0: shared.clone(),
            token1: pubkey(63).to_string(),
            lp_mint: lp.clone(),
        }])
        .unwrap();
        assert_eq!(bindings.len(), 3);
        assert_eq!(
            bindings
                .iter()
                .find(|binding| binding.mint == shared)
                .unwrap()
                .selected_uses[0]
                .role,
            SelectedMintRoleV1::Token0
        );
        assert_eq!(
            bindings
                .iter()
                .find(|binding| binding.mint == lp)
                .unwrap()
                .selected_uses[0]
                .role,
            SelectedMintRoleV1::LpMint
        );
    }

    #[test]
    fn selected_operation_builder_binds_complete_legacy_execution_provenance() {
        let mint = pubkey(64).to_string();
        let binding = selected_binding(mint.clone(), 65, SelectedMintRoleV1::Token0);
        let transaction = DecodedTransactionV1 {
            schema_version: DECODE_SCHEMA_VERSION,
            decoder_version_id: "fixture-decoder".to_owned(),
            signature: signature(64),
            rpc_payload_sha256: rpc_hash('d'),
            slot: 10,
            transaction_index: 0,
            block_time_unix_seconds: 1,
            program_instruction_count: 0,
            state_changing_instruction_count: 0,
            emitted_assertion_count: 0,
            unknown_program_instruction_hashes: Vec::new(),
            non_target_outer_touched_accounts: Vec::new(),
            token_balances: vec![TokenAccountBalanceV1 {
                account_index: 0,
                account: pubkey(66).to_string(),
                mint: mint.clone(),
                token_program: TokenProgramKindV1::LegacyToken,
                owner: None,
                decimals: 6,
                pre_amount: TokenAmount::zero(),
                post_amount: TokenAmount::new("1").unwrap(),
            }],
            instructions: Vec::new(),
        };
        let provenance = TransactionExecutionProvenanceV1 {
            schema_version: DECODE_SCHEMA_VERSION,
            decoder_version_id: "fixture-decoder".to_owned(),
            signature: transaction.signature.clone(),
            slot: transaction.slot,
            transaction_index: transaction.transaction_index,
            frames: vec![InstructionExecutionFrameV1 {
                schema_version: DECODE_SCHEMA_VERSION,
                order: ChainOrder {
                    slot: transaction.slot,
                    transaction_index: transaction.transaction_index,
                    outer_instruction_index: 0,
                    inner_execution_index: None,
                    event_sequence: 0,
                },
                program_id: LEGACY_TOKEN_PROGRAM_ID.to_owned(),
                is_target_program: false,
                stack_height: Some(1),
                parent_order: None,
                execution_path: vec![0],
                provenance: CpiFrameProvenanceV1::Complete,
                decoded_target_class: None,
            }],
            token_cpis: Vec::new(),
        };
        let artifact = build_selected_operation_token_program_evidence(
            "run",
            20,
            &rpc_hash('e'),
            &rpc_hash('f'),
            &[binding],
            &[transaction],
            &[provenance],
        )
        .unwrap();
        artifact.validate().unwrap();
        let evidence = artifact.evidence_for(&mint).unwrap();
        assert_eq!(evidence.expected_relevant_operation_count, 1);
        assert_eq!(evidence.classified_relevant_operation_count, 1);
        assert_eq!(evidence.earliest_relevant_slot, Some(10));
        assert_eq!(evidence.latest_relevant_slot, Some(10));
        assert_eq!(
            evidence.observed_programs,
            [TokenProgramKindV1::LegacyToken]
        );

        let mut tampered = artifact;
        tampered.mint_evidence[0].latest_relevant_slot = Some(21);
        assert!(tampered.validate().is_err());
    }

    fn mixed_program_transaction_fixture() -> (
        Vec<SelectedMintBindingV1>,
        DecodedTransactionV1,
        TransactionExecutionProvenanceV1,
    ) {
        let pair = pubkey(90).to_string();
        let legacy_mint = pubkey(91).to_string();
        let token_2022_mint = pubkey(92).to_string();
        let bindings = vec![
            SelectedMintBindingV1 {
                mint: legacy_mint.clone(),
                selected_uses: vec![SelectedMintUseV1 {
                    pair_address: pair.clone(),
                    role: SelectedMintRoleV1::Token0,
                }],
            },
            SelectedMintBindingV1 {
                mint: token_2022_mint.clone(),
                selected_uses: vec![SelectedMintUseV1 {
                    pair_address: pair,
                    role: SelectedMintRoleV1::Token1,
                }],
            },
        ];
        let transaction = DecodedTransactionV1 {
            schema_version: DECODE_SCHEMA_VERSION,
            decoder_version_id: "fixture-decoder".to_owned(),
            signature: signature(90),
            rpc_payload_sha256: rpc_hash('d'),
            slot: 10,
            transaction_index: 0,
            block_time_unix_seconds: 1,
            program_instruction_count: 0,
            state_changing_instruction_count: 0,
            emitted_assertion_count: 0,
            unknown_program_instruction_hashes: Vec::new(),
            non_target_outer_touched_accounts: Vec::new(),
            token_balances: vec![
                TokenAccountBalanceV1 {
                    account_index: 0,
                    account: pubkey(93).to_string(),
                    mint: legacy_mint,
                    token_program: TokenProgramKindV1::LegacyToken,
                    owner: None,
                    decimals: 6,
                    pre_amount: TokenAmount::zero(),
                    post_amount: TokenAmount::new("1").unwrap(),
                },
                TokenAccountBalanceV1 {
                    account_index: 1,
                    account: pubkey(94).to_string(),
                    mint: token_2022_mint,
                    token_program: TokenProgramKindV1::Token2022,
                    owner: None,
                    decimals: 6,
                    pre_amount: TokenAmount::zero(),
                    post_amount: TokenAmount::new("1").unwrap(),
                },
            ],
            instructions: Vec::new(),
        };
        let frame = |outer_instruction_index, program_id: &str| InstructionExecutionFrameV1 {
            schema_version: DECODE_SCHEMA_VERSION,
            order: ChainOrder {
                slot: transaction.slot,
                transaction_index: transaction.transaction_index,
                outer_instruction_index,
                inner_execution_index: None,
                event_sequence: 0,
            },
            program_id: program_id.to_owned(),
            is_target_program: false,
            stack_height: Some(1),
            parent_order: None,
            execution_path: vec![outer_instruction_index],
            provenance: CpiFrameProvenanceV1::Complete,
            decoded_target_class: None,
        };
        let provenance = TransactionExecutionProvenanceV1 {
            schema_version: DECODE_SCHEMA_VERSION,
            decoder_version_id: transaction.decoder_version_id.clone(),
            signature: transaction.signature.clone(),
            slot: transaction.slot,
            transaction_index: transaction.transaction_index,
            frames: vec![
                frame(0, LEGACY_TOKEN_PROGRAM_ID),
                frame(1, TOKEN_2022_PROGRAM_ID),
            ],
            token_cpis: Vec::new(),
        };
        (bindings, transaction, provenance)
    }

    #[test]
    fn mixed_legacy_and_token_2022_transaction_is_attributed_per_mint() {
        let directory = TestDirectory::new();
        let (bindings, transaction, provenance) = mixed_program_transaction_fixture();
        let memory = build_selected_operation_token_program_evidence(
            "mixed-run",
            20,
            &rpc_hash('e'),
            &rpc_hash('f'),
            &bindings,
            std::slice::from_ref(&transaction),
            std::slice::from_ref(&provenance),
        )
        .unwrap();
        assert_eq!(
            memory
                .evidence_for(&bindings[0].mint)
                .unwrap()
                .observed_programs,
            [TokenProgramKindV1::LegacyToken]
        );
        assert_eq!(
            memory
                .evidence_for(&bindings[1].mint)
                .unwrap()
                .observed_programs,
            [TokenProgramKindV1::Token2022]
        );

        let decoded_path = directory.0.join("mixed-decoded.jsonl");
        let provenance_path = directory.0.join("mixed-provenance.jsonl");
        write_canonical_jsonl(&decoded_path, std::slice::from_ref(&transaction));
        write_canonical_jsonl(&provenance_path, std::slice::from_ref(&provenance));
        let streamed = build_selected_operation_token_program_evidence_from_jsonl(
            "mixed-run",
            20,
            &rpc_hash('e'),
            &decoded_path,
            &rpc_hash('f'),
            &provenance_path,
            &bindings,
        )
        .unwrap();
        assert_eq!(streamed, memory);
    }

    #[test]
    fn conflicting_programs_for_one_changed_mint_fail_closed() {
        let directory = TestDirectory::new();
        let (bindings, mut transaction, provenance) = mixed_program_transaction_fixture();
        transaction.token_balances.push(TokenAccountBalanceV1 {
            account_index: 2,
            account: pubkey(95).to_string(),
            mint: bindings[0].mint.clone(),
            token_program: TokenProgramKindV1::Token2022,
            owner: None,
            decimals: 6,
            pre_amount: TokenAmount::zero(),
            post_amount: TokenAmount::new("1").unwrap(),
        });
        assert!(build_selected_operation_token_program_evidence(
            "ambiguous-run",
            20,
            &rpc_hash('e'),
            &rpc_hash('f'),
            &bindings,
            std::slice::from_ref(&transaction),
            std::slice::from_ref(&provenance),
        )
        .is_err());

        let decoded_path = directory.0.join("ambiguous-decoded.jsonl");
        let provenance_path = directory.0.join("ambiguous-provenance.jsonl");
        write_canonical_jsonl(&decoded_path, std::slice::from_ref(&transaction));
        write_canonical_jsonl(&provenance_path, std::slice::from_ref(&provenance));
        assert!(build_selected_operation_token_program_evidence_from_jsonl(
            "ambiguous-run",
            20,
            &rpc_hash('e'),
            &decoded_path,
            &rpc_hash('f'),
            &provenance_path,
            &bindings,
        )
        .is_err());
    }

    #[test]
    fn streaming_operation_builder_matches_memory_and_rejects_order_or_join_gaps() {
        let directory = TestDirectory::new();
        let first_mint = pubkey(73).to_string();
        let second_mint = pubkey(74).to_string();
        let bindings = vec![
            selected_binding(first_mint.clone(), 75, SelectedMintRoleV1::Token0),
            selected_binding(second_mint.clone(), 76, SelectedMintRoleV1::Token1),
        ];
        let first =
            decoded_transaction_and_provenance(&first_mint, 10, 73, LEGACY_TOKEN_PROGRAM_ID);
        let second =
            decoded_transaction_and_provenance(&second_mint, 11, 74, LEGACY_TOKEN_PROGRAM_ID);
        let transactions = vec![first.0.clone(), second.0.clone()];
        let provenance = vec![first.1.clone(), second.1.clone()];
        let decoded_path = directory.0.join("decoded.jsonl");
        let provenance_path = directory.0.join("provenance.jsonl");
        write_canonical_jsonl(&decoded_path, &transactions);
        write_canonical_jsonl(&provenance_path, &provenance);

        let memory = build_selected_operation_token_program_evidence(
            "stream-run",
            20,
            &rpc_hash('e'),
            &rpc_hash('f'),
            &bindings,
            &transactions,
            &provenance,
        )
        .unwrap();
        let streamed = build_selected_operation_token_program_evidence_from_jsonl(
            "stream-run",
            20,
            &rpc_hash('e'),
            &decoded_path,
            &rpc_hash('f'),
            &provenance_path,
            &bindings,
        )
        .unwrap();
        assert_eq!(streamed, memory);

        write_canonical_jsonl(&decoded_path, &[second.0, first.0]);
        write_canonical_jsonl(&provenance_path, &[second.1, first.1]);
        assert!(build_selected_operation_token_program_evidence_from_jsonl(
            "stream-run",
            20,
            &rpc_hash('e'),
            &decoded_path,
            &rpc_hash('f'),
            &provenance_path,
            &bindings,
        )
        .is_err());

        write_canonical_jsonl(&decoded_path, &transactions);
        write_canonical_jsonl::<TransactionExecutionProvenanceV1>(&provenance_path, &[]);
        assert!(build_selected_operation_token_program_evidence_from_jsonl(
            "stream-run",
            20,
            &rpc_hash('e'),
            &decoded_path,
            &rpc_hash('f'),
            &provenance_path,
            &bindings,
        )
        .is_err());
    }

    #[tokio::test]
    async fn selected_scope_orchestrator_is_deterministic_resumable_and_tamper_evident() {
        let directory = TestDirectory::new();
        let cutoff_slot = 20;
        let pair = pubkey(77).to_string();
        let mints = [
            pubkey(78).to_string(),
            pubkey(79).to_string(),
            pubkey(80).to_string(),
        ];
        let market = MarketMintBindingV1 {
            pair_address: pair.clone(),
            token0: mints[0].clone(),
            token1: mints[1].clone(),
            lp_mint: mints[2].clone(),
        };
        let rows = mints
            .iter()
            .enumerate()
            .map(|(index, mint)| {
                decoded_transaction_and_provenance(
                    mint,
                    10 + index as u64,
                    78 + index as u8,
                    LEGACY_TOKEN_PROGRAM_ID,
                )
            })
            .collect::<Vec<_>>();
        let transactions = rows
            .iter()
            .map(|(transaction, _)| transaction.clone())
            .collect::<Vec<_>>();
        let provenance = rows
            .iter()
            .map(|(_, provenance)| provenance.clone())
            .collect::<Vec<_>>();
        let decoded_path = directory.0.join("decoded.jsonl");
        let provenance_path = directory.0.join("provenance.jsonl");
        write_canonical_jsonl(&decoded_path, &transactions);
        write_canonical_jsonl(&provenance_path, &provenance);
        let responses = mints
            .iter()
            .map(|mint| {
                (
                    mint.clone(),
                    Ok(finalized_mint_response(
                        mint,
                        cutoff_slot,
                        cutoff_slot + 5,
                        LEGACY_TOKEN_PROGRAM_ID,
                        initialized_legacy_mint_data(6),
                    )),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let account_calls = Arc::new(Mutex::new(Vec::new()));
        let rpc = SelectedScopeRpc {
            epoch: epoch_proof(),
            accounts: FakeFinalizedMintRpc {
                responses: Arc::new(responses),
                calls: account_calls.clone(),
            },
            epoch_calls: Arc::new(Mutex::new(0)),
        };
        let provider = provider();
        let output_dir = directory.0.join("selected-output");
        let decoded_artifact_sha256 = rpc_hash('e');
        let provenance_artifact_sha256 = rpc_hash('f');
        let request = SelectedScopeTransferFeeExtractionRequestV1 {
            run_id: "selected-run",
            cutoff_slot,
            provider: &provider,
            evidence_tier: EvidenceTier::DecisionGrade,
            markets: std::slice::from_ref(&market),
            output_dir: &output_dir,
            signature_page_limit: DEFAULT_MINT_SIGNATURE_PAGE_LIMIT,
            decoded_transactions_jsonl: &decoded_path,
            decoded_transactions_artifact_sha256: &decoded_artifact_sha256,
            execution_provenance_jsonl: &provenance_path,
            execution_provenance_artifact_sha256: &provenance_artifact_sha256,
        };
        let first = extract_selected_scope_transfer_fee_provenance(&rpc, &request)
            .await
            .unwrap();
        assert_eq!(first.artifact.expected_mint_count(), 3);
        assert_eq!(first.artifact.ready_mint_count(), 3);
        assert!(first.artifact.market_ready(&pair));
        assert_eq!(first.artifact.registry.as_ref().unwrap().mints.len(), 3);
        assert!(first.fallback_provenance_path.is_none());
        assert!(first.operation_evidence_path.exists());
        assert!(first.legacy_proofs_path.exists());
        assert!(first.provenance_path.exists());
        assert!(first.registry_path.as_ref().unwrap().exists());
        assert!(first.artifact.mint_histories.iter().all(|history| matches!(
            &history.proof_basis,
            MintTransferFeeProofBasisV1::DecisionGradePostCutoffImmutableLegacy { .. }
        )));
        assert_eq!(*rpc.epoch_calls.lock().unwrap(), 1);

        let resumed = extract_selected_scope_transfer_fee_provenance(&rpc, &request)
            .await
            .unwrap();
        assert_eq!(resumed.artifact, first.artifact);
        assert_eq!(resumed.operation_evidence, first.operation_evidence);
        assert_eq!(*rpc.epoch_calls.lock().unwrap(), 1);
        assert_eq!(account_calls.lock().unwrap().len(), 6);

        let mut tampered = first.operation_evidence;
        tampered.mint_evidence[0].classified_relevant_operation_count = 0;
        write_json_atomic(&first.operation_evidence_path, &tampered).unwrap();
        assert!(
            extract_selected_scope_transfer_fee_provenance(&rpc, &request)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn decision_grade_accepts_only_a_bound_post_cutoff_immutable_legacy_mint() {
        let cutoff_slot = 20;
        let observed_context_slot = 25;
        let mint = pubkey(67).to_string();
        let binding = selected_binding(mint.clone(), 68, SelectedMintRoleV1::LpMint);
        let evidence = operation_evidence(mint.clone(), TokenProgramKindV1::LegacyToken);
        let response = finalized_mint_response(
            &mint,
            cutoff_slot,
            observed_context_slot,
            LEGACY_TOKEN_PROGRAM_ID,
            initialized_legacy_mint_data(6),
        );
        let rpc = finalized_mint_rpc(&mint, response);
        let provider = provider();
        let resolution = resolve_selected_mint_transfer_fee_evidence(
            &rpc,
            &SelectedMintProofRequestV1 {
                run_id: "run",
                cutoff_slot,
                provider: &provider,
                evidence_tier: EvidenceTier::DecisionGrade,
                binding: &binding,
                operation_program_evidence: &evidence,
            },
        )
        .await
        .unwrap();
        let SelectedMintTransferFeeResolutionV1::DecisionGradeLegacy { proof } = resolution else {
            panic!("expected decision-grade immutable legacy proof")
        };
        proof.validate().unwrap();
        assert_eq!(proof.observed_context_slot, observed_context_slot);
        assert!(proof.observed_context_slot > proof.cutoff_slot);
        assert_eq!(proof.decimals, 6);
        let zero = proof.zero_fee_schedule().unwrap();
        assert_eq!(zero.token_program, TokenProgramKindV1::LegacyToken);
        assert_eq!(zero.epochs[0].transfer_fee_basis_points, 0);
        assert_eq!(rpc.calls.lock().unwrap().as_slice(), [(mint, cutoff_slot)]);
    }

    #[tokio::test]
    async fn audit_grade_and_incomplete_operation_evidence_never_call_the_shortcut_rpc() {
        let mint = pubkey(69).to_string();
        let binding = selected_binding(mint.clone(), 70, SelectedMintRoleV1::Token1);
        let response = finalized_mint_response(
            &mint,
            20,
            20,
            LEGACY_TOKEN_PROGRAM_ID,
            initialized_legacy_mint_data(6),
        );
        let rpc = finalized_mint_rpc(&mint, response);
        let provider = provider();
        let evidence = operation_evidence(mint.clone(), TokenProgramKindV1::LegacyToken);
        let audit = resolve_selected_mint_transfer_fee_evidence(
            &rpc,
            &SelectedMintProofRequestV1 {
                run_id: "run",
                cutoff_slot: 20,
                provider: &provider,
                evidence_tier: EvidenceTier::AuditGrade,
                binding: &binding,
                operation_program_evidence: &evidence,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            audit,
            SelectedMintTransferFeeResolutionV1::FullHistoryRequired {
                mint: mint.clone(),
                reason: SelectedMintFullHistoryReasonV1::AuditGradeRequiresFullHistory,
            }
        );

        let mut incomplete = evidence;
        incomplete.classified_relevant_operation_count = 0;
        let decision = resolve_selected_mint_transfer_fee_evidence(
            &rpc,
            &SelectedMintProofRequestV1 {
                run_id: "run",
                cutoff_slot: 20,
                provider: &provider,
                evidence_tier: EvidenceTier::DecisionGrade,
                binding: &binding,
                operation_program_evidence: &incomplete,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            decision,
            SelectedMintTransferFeeResolutionV1::FullHistoryRequired {
                mint,
                reason: SelectedMintFullHistoryReasonV1::OperationProgramEvidenceIncomplete,
            }
        );
        assert!(rpc.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn token_2022_or_tampered_post_cutoff_account_evidence_fails_closed() {
        let mint = pubkey(71).to_string();
        let binding = selected_binding(mint.clone(), 72, SelectedMintRoleV1::Token0);
        let evidence = operation_evidence(mint.clone(), TokenProgramKindV1::LegacyToken);
        let provider = provider();
        let token_2022_rpc = finalized_mint_rpc(
            &mint,
            finalized_mint_response(
                &mint,
                20,
                21,
                TOKEN_2022_PROGRAM_ID,
                initialized_legacy_mint_data(6),
            ),
        );
        let request = SelectedMintProofRequestV1 {
            run_id: "run",
            cutoff_slot: 20,
            provider: &provider,
            evidence_tier: EvidenceTier::DecisionGrade,
            binding: &binding,
            operation_program_evidence: &evidence,
        };
        assert_eq!(
            resolve_selected_mint_transfer_fee_evidence(&token_2022_rpc, &request)
                .await
                .unwrap(),
            SelectedMintTransferFeeResolutionV1::FullHistoryRequired {
                mint: mint.clone(),
                reason: SelectedMintFullHistoryReasonV1::Token2022AccountOwner,
            }
        );

        let mut tampered = finalized_mint_response(
            &mint,
            20,
            21,
            LEGACY_TOKEN_PROGRAM_ID,
            initialized_legacy_mint_data(6),
        );
        tampered.value.account_data_sha256 = rpc_hash('0');
        let tampered_rpc = finalized_mint_rpc(&mint, tampered);
        assert!(
            resolve_selected_mint_transfer_fee_evidence(&tampered_rpc, &request)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn full_history_reconstructs_pending_overwrite_and_legacy_zero_fee() {
        let directory = TestDirectory::new();
        let token2022 = Pubkey::from_str(TOKEN_2022_PROGRAM_ID).unwrap();
        let legacy = Pubkey::from_str(LEGACY_TOKEN_PROGRAM_ID).unwrap();
        let mint2022 = pubkey(1);
        let mint_legacy = pubkey(2);
        let lp_mint = pubkey(3);
        let authority = pubkey(4);

        let init_fee = fee_instruction::initialize_transfer_fee_config(
            &token2022,
            &mint2022,
            Some(&authority),
            Some(&authority),
            25,
            100,
        )
        .unwrap();
        let init_2022 =
            token_instruction::initialize_mint2(&token2022, &mint2022, &authority, None, 9)
                .unwrap();
        let set_50 =
            fee_instruction::set_transfer_fee(&token2022, &mint2022, &authority, &[], 50, 200)
                .unwrap();
        let set_failed =
            fee_instruction::set_transfer_fee(&token2022, &mint2022, &authority, &[], 99, 999)
                .unwrap();
        let set_60 =
            fee_instruction::set_transfer_fee(&token2022, &mint2022, &authority, &[], 75, 300)
                .unwrap();
        let init_legacy =
            token_instruction::initialize_mint2(&legacy, &mint_legacy, &authority, None, 6)
                .unwrap();
        let init_lp =
            token_instruction::initialize_mint2(&legacy, &lp_mint, &authority, None, 6).unwrap();

        let rows_2022 = vec![
            signature_row(4, 60, true),
            signature_row(3, 55, false),
            signature_row(2, 50, true),
            signature_row(1, 10, true),
        ];
        let rows_legacy = vec![signature_row(5, 20, true)];
        let rows_lp = vec![signature_row(6, 25, true)];
        let mut pages = BTreeMap::new();
        add_pages(&mut pages, &mint2022.to_string(), rows_2022.clone(), 'c');
        add_pages(
            &mut pages,
            &mint_legacy.to_string(),
            rows_legacy.clone(),
            'e',
        );
        add_pages(&mut pages, &lp_mint.to_string(), rows_lp.clone(), '1');
        let mut blocks = BTreeMap::new();
        add_block(
            &mut blocks,
            10,
            archived_transaction(rows_2022[3].signature.clone(), true, &[init_fee, init_2022]),
        );
        add_block(
            &mut blocks,
            50,
            archived_transaction(rows_2022[2].signature.clone(), true, &[set_50]),
        );
        add_block(
            &mut blocks,
            55,
            archived_transaction(rows_2022[1].signature.clone(), false, &[set_failed]),
        );
        add_block(
            &mut blocks,
            60,
            archived_transaction(rows_2022[0].signature.clone(), true, &[set_60]),
        );
        add_block(
            &mut blocks,
            20,
            archived_transaction(rows_legacy[0].signature.clone(), true, &[init_legacy]),
        );
        add_block(
            &mut blocks,
            25,
            archived_transaction(rows_lp[0].signature.clone(), true, &[init_lp]),
        );
        let calls = Arc::new(Mutex::new(RpcCalls::default()));
        let rpc = FakeRpc {
            epoch: epoch_proof(),
            pages: Arc::new(pages),
            blocks: Arc::new(blocks),
            calls: calls.clone(),
        };
        let provider = provider();
        let market = MarketMintBindingV1 {
            pair_address: pubkey(9).to_string(),
            token0: mint2022.to_string(),
            token1: mint_legacy.to_string(),
            lp_mint: lp_mint.to_string(),
        };
        let request = TransferFeeExtractionRequestV1 {
            run_id: "run-fixture",
            cutoff_slot: 80,
            provider: &provider,
            markets: &[market],
            output_dir: &directory.0,
            signature_page_limit: 1_000,
        };
        let output = extract_transfer_fee_provenance(&rpc, &request)
            .await
            .unwrap();
        assert!(output.artifact.blockers.is_empty());
        assert!(output.artifact.market_coverage[0].ready);
        assert_eq!(output.artifact.ready_mint_count(), 3);
        let registry = output.artifact.registry.as_ref().unwrap();
        let schedule = registry
            .mints
            .iter()
            .find(|schedule| schedule.mint == mint2022.to_string())
            .unwrap();
        assert_eq!(schedule.epochs.len(), 2);
        assert_eq!(schedule.epochs[0].transfer_fee_basis_points, 25);
        assert_eq!(schedule.epochs[1].effective_epoch, 2);
        assert_eq!(schedule.epochs[1].transfer_fee_basis_points, 75);
        assert_eq!(schedule.epochs[1].maximum_fee.as_str(), "300");
        let legacy_schedule = registry
            .mints
            .iter()
            .find(|schedule| schedule.mint == mint_legacy.to_string())
            .unwrap();
        assert_eq!(legacy_schedule.epochs.len(), 1);
        assert_eq!(legacy_schedule.epochs[0].transfer_fee_basis_points, 0);
        assert_eq!(legacy_schedule.epochs[0].maximum_fee.as_str(), "0");
        let history = output
            .artifact
            .mint_histories
            .iter()
            .find(|history| history.mint == mint2022.to_string())
            .unwrap();
        assert_eq!(history.failed_signatures_through_cutoff, 1);
        assert_eq!(history.signatures_through_cutoff, 4);
        assert_eq!(history.decimals, Some(9));
        assert_eq!(history.transfer_fee_config_ever_exists, Some(true));
        assert_eq!(
            history.initialized_token_2022_extensions,
            Some(vec![Token2022MintExtensionV1::TransferFeeConfig])
        );
        assert!(valid_sha256(&history.decoded_mint_history_sha256));
        let projections = output
            .artifact
            .leverage_mint_eligibility_projections()
            .unwrap();
        assert!(
            !projections
                .iter()
                .find(|row| row.mint == mint2022.to_string())
                .unwrap()
                .native_leverage_eligible
        );
        assert!(
            projections
                .iter()
                .find(|row| row.mint == mint_legacy.to_string())
                .unwrap()
                .native_leverage_eligible
        );
        assert_eq!(calls.lock().unwrap().pages.len(), 6);

        let calls_before_resume = {
            let calls = calls.lock().unwrap();
            (calls.epoch, calls.pages.len(), calls.blocks.len())
        };
        let resumed = extract_transfer_fee_provenance(&rpc, &request)
            .await
            .unwrap();
        assert_eq!(resumed.artifact, output.artifact);
        let calls_after_resume = {
            let calls = calls.lock().unwrap();
            (calls.epoch, calls.pages.len(), calls.blocks.len())
        };
        assert_eq!(calls_after_resume, calls_before_resume);
    }

    #[tokio::test]
    async fn fee_free_token_2022_with_allowed_extension_is_leverage_eligible() {
        let directory = TestDirectory::new();
        let token2022 = Pubkey::from_str(TOKEN_2022_PROGRAM_ID).unwrap();
        let mint = pubkey(61);
        let authority = pubkey(62);
        let pointer =
            metadata_pointer_instruction::initialize(&token2022, &mint, None, Some(mint)).unwrap();
        let init =
            token_instruction::initialize_mint2(&token2022, &mint, &authority, None, 9).unwrap();
        let row = signature_row(61, 10, true);
        let mut pages = BTreeMap::new();
        add_pages(&mut pages, &mint.to_string(), vec![row.clone()], '2');
        let mut blocks = BTreeMap::new();
        add_block(
            &mut blocks,
            10,
            archived_transaction(row.signature, true, &[pointer, init]),
        );
        let rpc = FakeRpc {
            epoch: epoch_proof(),
            pages: Arc::new(pages),
            blocks: Arc::new(blocks),
            calls: Arc::new(Mutex::new(RpcCalls::default())),
        };
        let provider = provider();
        let market = MarketMintBindingV1 {
            pair_address: pubkey(63).to_string(),
            token0: mint.to_string(),
            token1: mint.to_string(),
            lp_mint: mint.to_string(),
        };
        let output = extract_transfer_fee_provenance(
            &rpc,
            &TransferFeeExtractionRequestV1 {
                run_id: "allowed-token-2022",
                cutoff_slot: 20,
                provider: &provider,
                markets: &[market],
                output_dir: &directory.0,
                signature_page_limit: DEFAULT_MINT_SIGNATURE_PAGE_LIMIT,
            },
        )
        .await
        .unwrap();
        let history = &output.artifact.mint_histories[0];
        assert_eq!(history.token_program, Some(TokenProgramKindV1::Token2022));
        assert_eq!(history.decimals, Some(9));
        assert_eq!(
            history.initialized_token_2022_extensions,
            Some(vec![Token2022MintExtensionV1::MetadataPointer])
        );
        assert_eq!(history.transfer_fee_config_ever_exists, Some(false));
        let projection = output
            .artifact
            .leverage_mint_eligibility_projections()
            .unwrap()
            .remove(0);
        assert!(projection.dusk_native_guards_pass);
        assert!(projection.native_leverage_eligible);
    }

    #[tokio::test]
    async fn unknown_successful_token_2022_mint_instruction_fails_closed() {
        let directory = TestDirectory::new();
        let token2022 = Pubkey::from_str(TOKEN_2022_PROGRAM_ID).unwrap();
        let mint = pubkey(71);
        let authority = pubkey(72);
        let init =
            token_instruction::initialize_mint2(&token2022, &mint, &authority, None, 9).unwrap();
        let unknown = Instruction {
            program_id: token2022,
            accounts: vec![AccountMeta::new(mint, false)],
            data: vec![u8::MAX],
        };
        let rows = vec![signature_row(72, 20, true), signature_row(71, 10, true)];
        let mut pages = BTreeMap::new();
        add_pages(&mut pages, &mint.to_string(), rows.clone(), '4');
        let mut blocks = BTreeMap::new();
        add_block(
            &mut blocks,
            10,
            archived_transaction(rows[1].signature.clone(), true, &[init]),
        );
        add_block(
            &mut blocks,
            20,
            archived_transaction(rows[0].signature.clone(), true, &[unknown]),
        );
        let rpc = FakeRpc {
            epoch: epoch_proof(),
            pages: Arc::new(pages),
            blocks: Arc::new(blocks),
            calls: Arc::new(Mutex::new(RpcCalls::default())),
        };
        let provider = provider();
        let market = MarketMintBindingV1 {
            pair_address: pubkey(73).to_string(),
            token0: mint.to_string(),
            token1: mint.to_string(),
            lp_mint: mint.to_string(),
        };
        let output = extract_transfer_fee_provenance(
            &rpc,
            &TransferFeeExtractionRequestV1 {
                run_id: "unknown-extension",
                cutoff_slot: 20,
                provider: &provider,
                markets: &[market],
                output_dir: &directory.0,
                signature_page_limit: DEFAULT_MINT_SIGNATURE_PAGE_LIMIT,
            },
        )
        .await
        .unwrap();
        assert!(output.artifact.registry.is_none());
        let history = &output.artifact.mint_histories[0];
        assert_eq!(history.token_program, Some(TokenProgramKindV1::Token2022));
        assert_eq!(history.decimals, Some(9));
        assert_eq!(history.initialized_token_2022_extensions, None);
        assert_eq!(history.transfer_fee_config_ever_exists, None);
        assert!(history.blockers.iter().any(|blocker| {
            blocker.code == TransferFeeHistoryBlockerCodeV1::UnknownMintExtension
        }));
        let projection = output
            .artifact
            .leverage_mint_eligibility_projections()
            .unwrap()
            .remove(0);
        assert!(!projection.dusk_native_guards_pass);
        assert!(!projection.native_leverage_eligible);
    }

    #[tokio::test]
    async fn transfer_hook_is_a_global_and_market_grain_blocker() {
        let directory = TestDirectory::new();
        let token2022 = Pubkey::from_str(TOKEN_2022_PROGRAM_ID).unwrap();
        let mint = pubkey(11);
        let authority = pubkey(12);
        let hook_program = pubkey(13);
        let hook =
            hook_instruction::initialize(&token2022, &mint, Some(authority), Some(hook_program))
                .unwrap();
        let init =
            token_instruction::initialize_mint2(&token2022, &mint, &authority, None, 9).unwrap();
        let row = signature_row(11, 10, true);
        let mut pages = BTreeMap::new();
        add_pages(&mut pages, &mint.to_string(), vec![row.clone()], '7');
        let mut blocks = BTreeMap::new();
        add_block(
            &mut blocks,
            10,
            archived_transaction(row.signature, true, &[hook, init]),
        );
        let rpc = FakeRpc {
            epoch: epoch_proof(),
            pages: Arc::new(pages),
            blocks: Arc::new(blocks),
            calls: Arc::new(Mutex::new(RpcCalls::default())),
        };
        let provider = provider();
        let market = MarketMintBindingV1 {
            pair_address: pubkey(14).to_string(),
            token0: mint.to_string(),
            token1: mint.to_string(),
            lp_mint: mint.to_string(),
        };
        let output = extract_transfer_fee_provenance(
            &rpc,
            &TransferFeeExtractionRequestV1 {
                run_id: "hook-run",
                cutoff_slot: 20,
                provider: &provider,
                markets: &[market],
                output_dir: &directory.0,
                signature_page_limit: 1_000,
            },
        )
        .await
        .unwrap();
        assert!(output.registry_path.is_none());
        assert!(output.artifact.registry.is_none());
        assert!(!output.artifact.market_coverage[0].ready);
        assert_eq!(output.artifact.ready_mint_count(), 0);
        assert!(output.artifact.blockers.iter().any(|blocker| {
            blocker.code == TransferFeeHistoryBlockerCodeV1::UnsupportedTransferHook
        }));
    }

    #[tokio::test]
    async fn unavailable_full_block_is_persisted_as_an_explicit_gap() {
        let directory = TestDirectory::new();
        let mint = pubkey(21);
        let row = signature_row(21, 10, true);
        let mut pages = BTreeMap::new();
        add_pages(&mut pages, &mint.to_string(), vec![row], '8');
        let blocks = BTreeMap::from([(
            10,
            RpcResponseProofV1 {
                raw_response_sha256: rpc_hash('9'),
                value: None,
            },
        )]);
        let rpc = FakeRpc {
            epoch: epoch_proof(),
            pages: Arc::new(pages),
            blocks: Arc::new(blocks),
            calls: Arc::new(Mutex::new(RpcCalls::default())),
        };
        let provider = provider();
        let market = MarketMintBindingV1 {
            pair_address: pubkey(22).to_string(),
            token0: mint.to_string(),
            token1: mint.to_string(),
            lp_mint: mint.to_string(),
        };
        let output = extract_transfer_fee_provenance(
            &rpc,
            &TransferFeeExtractionRequestV1 {
                run_id: "gap-run",
                cutoff_slot: 20,
                provider: &provider,
                markets: &[market],
                output_dir: &directory.0,
                signature_page_limit: 1_000,
            },
        )
        .await
        .unwrap();
        assert!(output.artifact.registry.is_none());
        assert_eq!(output.artifact.mint_histories[0].block_slots_expected, 1);
        assert_eq!(output.artifact.mint_histories[0].block_slots_fetched, 0);
        assert!(output.artifact.blockers.iter().any(|blocker| {
            blocker.code == TransferFeeHistoryBlockerCodeV1::ArchivedBlockUnavailable
        }));
    }

    #[tokio::test]
    async fn shared_block_cache_retains_exact_crawled_signatures_and_full_inventory_proof() {
        let directory = TestDirectory::new();
        let slot = 2_345_678;
        let target_success = archived_transaction(signature(31), true, &[]);
        let target_failure = archived_transaction(signature(32), false, &[]);
        let unrelated = archived_transaction(signature(33), true, &[]);
        let full_block = block_with_transactions(
            slot,
            vec![unrelated, target_failure.clone(), target_success.clone()],
        );
        let full_inventory_sha256 = full_block.full_transaction_inventory_sha256.clone();
        let raw_response_sha256 = rpc_hash('d');
        let calls = Arc::new(Mutex::new(RpcCalls::default()));
        let rpc = FakeRpc {
            epoch: epoch_proof(),
            pages: Arc::new(BTreeMap::new()),
            blocks: Arc::new(BTreeMap::from([(
                slot,
                RpcResponseProofV1 {
                    raw_response_sha256: raw_response_sha256.clone(),
                    value: Some(full_block),
                },
            )])),
            calls: calls.clone(),
        };
        let provider = provider();
        let request = TransferFeeExtractionRequestV1 {
            run_id: "filtered-block-run",
            cutoff_slot: slot,
            provider: &provider,
            markets: &[],
            output_dir: &directory.0,
            signature_page_limit: DEFAULT_MINT_SIGNATURE_PAGE_LIMIT,
        };
        let mint = pubkey(31).to_string();
        let mints = BTreeSet::from([mint]);
        let targets = BTreeSet::from([
            target_failure.signature.clone(),
            target_success.signature.clone(),
        ]);
        let shard = slot / 1_000_000;
        let path = directory
            .0
            .join("blocks")
            .join(shard.to_string())
            .join(format!("{slot}.json.zst"));
        let legacy_path = directory.0.join("blocks").join(format!("{slot}.json"));
        let artifact = load_or_fetch_block(
            &rpc,
            &request,
            &path,
            &legacy_path,
            slot,
            &mints,
            &targets,
            1,
        )
        .await
        .unwrap();
        artifact.validate().unwrap();
        let retained = artifact.block.as_ref().unwrap();
        assert_eq!(retained.full_transaction_count, 3);
        assert_eq!(
            retained.full_transaction_inventory_sha256,
            full_inventory_sha256
        );
        assert_eq!(artifact.raw_response_sha256, raw_response_sha256);
        assert_eq!(retained.transactions.len(), 2);
        assert_eq!(retained.transactions[0].transaction_index, 1);
        assert!(!retained.transactions[0].succeeded);
        assert_eq!(retained.transactions[1].transaction_index, 2);
        assert_eq!(&fs::read(&path).unwrap()[..4], &[0x28, 0xb5, 0x2f, 0xfd]);
        assert_eq!(
            read_json_zstd::<MintHistoryBlockArtifactV1>(&path).unwrap(),
            artifact
        );
        assert_eq!(calls.lock().unwrap().blocks, vec![slot]);

        let mut tampered = artifact.clone();
        tampered.block.as_mut().unwrap().transactions.pop();
        tampered = tampered.seal().unwrap();
        assert!(tampered.validate().is_err());
    }

    #[tokio::test]
    async fn legacy_full_block_migrates_to_filtered_zstd_before_plain_file_is_removed() {
        let directory = TestDirectory::new();
        let slot = 1_234_567;
        let target = archived_transaction(signature(41), true, &[]);
        let unrelated = archived_transaction(signature(42), false, &[]);
        let full_block = block_with_transactions(slot, vec![unrelated, target.clone()]);
        let full_inventory_sha256 = full_block.full_transaction_inventory_sha256.clone();
        let provider = provider();
        let mint = pubkey(41).to_string();
        let mints = BTreeSet::from([mint.clone()]);
        let targets = BTreeSet::from([target.signature.clone()]);
        let legacy_path = directory.0.join("blocks").join(format!("{slot}.json"));
        let mut legacy = LegacyMintHistoryBlockArtifactV1 {
            schema_version: TRANSFER_FEE_HISTORY_SCHEMA_VERSION,
            run_id: "legacy-block-run".to_owned(),
            cutoff_slot: slot,
            provider_fingerprint_sha256: provider.secret_fingerprint_sha256.clone(),
            slot,
            commitment: "finalized".to_owned(),
            encoding: "json".to_owned(),
            transaction_details: "full".to_owned(),
            max_supported_transaction_version: 0,
            mints: vec![mint],
            raw_response_sha256: rpc_hash('e'),
            block: Some(full_block),
            artifact_sha256: String::new(),
        };
        legacy.artifact_sha256 = hash_serialized(LEGACY_BLOCK_HASH_DOMAIN, &legacy).unwrap();
        legacy.validate().unwrap();
        write_json_atomic(&legacy_path, &legacy).unwrap();
        let path = directory
            .0
            .join("blocks")
            .join((slot / 1_000_000).to_string())
            .join(format!("{slot}.json.zst"));
        let calls = Arc::new(Mutex::new(RpcCalls::default()));
        let rpc = FakeRpc {
            epoch: epoch_proof(),
            pages: Arc::new(BTreeMap::new()),
            blocks: Arc::new(BTreeMap::new()),
            calls: calls.clone(),
        };
        let request = TransferFeeExtractionRequestV1 {
            run_id: "legacy-block-run",
            cutoff_slot: slot,
            provider: &provider,
            markets: &[],
            output_dir: &directory.0,
            signature_page_limit: DEFAULT_MINT_SIGNATURE_PAGE_LIMIT,
        };
        let migrated = load_or_fetch_block(
            &rpc,
            &request,
            &path,
            &legacy_path,
            slot,
            &mints,
            &targets,
            1,
        )
        .await
        .unwrap();
        assert!(path.exists());
        assert!(!legacy_path.exists());
        assert!(calls.lock().unwrap().blocks.is_empty());
        let retained = migrated.block.unwrap();
        assert_eq!(retained.full_transaction_count, 2);
        assert_eq!(
            retained.full_transaction_inventory_sha256,
            full_inventory_sha256
        );
        assert_eq!(retained.transactions.len(), 1);
        assert_eq!(retained.transactions[0].signature, target.signature);
    }

    #[tokio::test]
    async fn fetched_block_missing_an_independently_crawled_signature_hard_fails() {
        let directory = TestDirectory::new();
        let slot = 99;
        let expected_signature = signature(51);
        let full_block =
            block_with_transactions(slot, vec![archived_transaction(signature(52), true, &[])]);
        let rpc = FakeRpc {
            epoch: epoch_proof(),
            pages: Arc::new(BTreeMap::new()),
            blocks: Arc::new(BTreeMap::from([(
                slot,
                RpcResponseProofV1 {
                    raw_response_sha256: rpc_hash('f'),
                    value: Some(full_block),
                },
            )])),
            calls: Arc::new(Mutex::new(RpcCalls::default())),
        };
        let provider = provider();
        let request = TransferFeeExtractionRequestV1 {
            run_id: "missing-target-run",
            cutoff_slot: slot,
            provider: &provider,
            markets: &[],
            output_dir: &directory.0,
            signature_page_limit: DEFAULT_MINT_SIGNATURE_PAGE_LIMIT,
        };
        let path = directory.0.join("blocks/0/99.json.zst");
        let legacy_path = directory.0.join("blocks/99.json");
        let result = load_or_fetch_block(
            &rpc,
            &request,
            &path,
            &legacy_path,
            slot,
            &BTreeSet::from([pubkey(51).to_string()]),
            &BTreeSet::from([expected_signature]),
            1,
        )
        .await;
        assert!(result.is_err());
        assert!(!path.exists());
    }
}
