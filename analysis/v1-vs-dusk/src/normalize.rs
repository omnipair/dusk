//! Decoder-independent canonical operations, assertions, and config epochs.
//!
//! Custody amounts are derived from exact SPL-token instructions whose verified
//! CPI ancestry belongs to one state-changing V1 operation. Transaction-wide
//! balance deltas remain an optional reconciliation check; routed outer calls
//! and sibling V1 operations never make an otherwise exact operation ambiguous.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use thiserror::Error;

use crate::{
    canonical::{
        deterministic_event_id, sha256_hex, CanonicalEventV1, ConfigScope, ConfigVersionV1,
        EventPayloadV1, FlashloanPayloadV1, LiquidationPayloadV1, LiquidityAction,
        LiquidityPayloadV1, LpPositionPayloadV1, PairDeltaPayloadV1, PairUpdatePayloadV1,
        PositionPayloadV1, ProtocolFeeClaimPayloadV1, SignedTokenAmount, SwapPayloadV1,
        TokenAmount,
    },
    decode::{
        DecodedInstructionClass, DecodedInstructionV1, DecodedTransactionV1, TokenAccountBalanceV1,
        TransactionExecutionProvenanceV1,
    },
    replay::{CanonicalOperationV1, OperationPayloadV1, REPLAY_SCHEMA_VERSION},
    token_cpi::{
        aggregate_exact_operation_token_deltas_v1, ExactOperationTokenDeltasV1,
        ExpectedTokenSupplyChangeV1, ExpectedTokenTransferV1, ParsedTokenCpiV1,
        TokenSupplyChangeKindV1, TokenSupplyChangeVariantV1, TokenTransferVariantV1,
        TOKEN_CPI_SCHEMA_VERSION,
    },
};

pub const NORMALIZE_SCHEMA_VERSION: u16 = 2;
const GLOBAL_ENTITY: &str = "__global__";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NormalizationGapCode {
    MalformedDecodedPayload,
    MissingRequiredAssertion,
    MissingTokenBalance,
    AmbiguousTransactionCustodyDelta,
    UnsupportedStateChangingInstruction,
    InconsistentMarketIdentity,
    UnverifiedCpiProvenance,
    InstructionScopedTokenEvidence,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizationGapV1 {
    pub schema_version: u16,
    pub signature: String,
    pub order: crate::canonical::ChainOrder,
    pub instruction_kind: String,
    pub code: NormalizationGapCode,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedHistoryV1 {
    pub schema_version: u16,
    pub operations: Vec<CanonicalOperationV1>,
    pub events: Vec<CanonicalEventV1>,
    pub configurations: Vec<ConfigVersionV1>,
    pub gaps: Vec<NormalizationGapV1>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MarketIdentity {
    token0: String,
    token1: String,
    token0_decimals: u8,
    token1_decimals: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LegacySwapFeeEvidence {
    lp_fee: TokenAmount,
    protocol_fee: TokenAmount,
}

#[derive(Clone, Copy)]
enum DeltaDirection {
    Credit,
    Debit,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum NormalizeError {
    #[error("decoded transaction or instruction order is duplicated")]
    DuplicateOrder,
    #[error("decoded payload is malformed")]
    MalformedPayload,
    #[error("a required emitted assertion is missing")]
    MissingAssertion,
    #[error("token-balance evidence is missing")]
    MissingTokenBalance,
    #[error("transaction-level custody delta is ambiguous")]
    AmbiguousCustodyDelta,
    #[error("state-changing instruction is unsupported")]
    UnsupportedInstruction,
    #[error("market identity conflicts with its initialization assertion")]
    InconsistentMarketIdentity,
    #[error("finalized RPC does not prove the instruction's CPI frame")]
    UnverifiedCpiProvenance,
    #[error("instruction-scoped SPL-token evidence is missing, conflicting, or incomplete")]
    InstructionScopedTokenEvidence,
    #[error("integer amount exceeds its canonical domain")]
    AmountOverflow,
}

impl NormalizeError {
    fn gap_code(&self) -> NormalizationGapCode {
        match self {
            Self::MalformedPayload | Self::AmountOverflow => {
                NormalizationGapCode::MalformedDecodedPayload
            }
            Self::MissingAssertion => NormalizationGapCode::MissingRequiredAssertion,
            Self::MissingTokenBalance => NormalizationGapCode::MissingTokenBalance,
            Self::AmbiguousCustodyDelta => NormalizationGapCode::AmbiguousTransactionCustodyDelta,
            Self::UnsupportedInstruction => {
                NormalizationGapCode::UnsupportedStateChangingInstruction
            }
            Self::InconsistentMarketIdentity => NormalizationGapCode::InconsistentMarketIdentity,
            Self::UnverifiedCpiProvenance => NormalizationGapCode::UnverifiedCpiProvenance,
            Self::InstructionScopedTokenEvidence => {
                NormalizationGapCode::InstructionScopedTokenEvidence
            }
            Self::DuplicateOrder => NormalizationGapCode::MalformedDecodedPayload,
        }
    }
}

pub fn normalize_decoded_history(
    transactions: &[DecodedTransactionV1],
    cluster: &str,
    program_id: &str,
) -> Result<NormalizedHistoryV1, NormalizeError> {
    let mut transactions = transactions.to_vec();
    transactions.sort_by_key(|transaction| (transaction.slot, transaction.transaction_index));
    let mut state = NormalizationStateV1::new(cluster, program_id);
    let mut operations = Vec::new();
    let mut events = Vec::new();
    let mut gaps = Vec::new();
    for transaction in transactions {
        let batch = state.push_transaction(transaction)?;
        operations.extend(batch.operations);
        events.extend(batch.events);
        gaps.extend(batch.gaps);
    }
    let mut configurations = state.finish_configurations();
    operations.sort_by_key(|operation| operation.order);
    events.sort_by_key(|event| event.order);
    configurations.sort_by_key(|config| config.valid_from);
    gaps.sort_by_key(|item| item.order);
    Ok(NormalizedHistoryV1 {
        schema_version: NORMALIZE_SCHEMA_VERSION,
        operations,
        events,
        configurations,
        gaps,
    })
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedTransactionBatchV1 {
    pub operations: Vec<CanonicalOperationV1>,
    pub events: Vec<CanonicalEventV1>,
    pub gaps: Vec<NormalizationGapV1>,
}

/// Stateful normalizer used by extraction to keep full history streaming.
pub struct NormalizationStateV1 {
    cluster: String,
    program_id: String,
    markets: BTreeMap<String, MarketIdentity>,
    config_open: BTreeMap<(ConfigScope, String), usize>,
    configurations: Vec<ConfigVersionV1>,
    last_transaction: Option<(u64, u32)>,
    last_instruction_order: Option<crate::canonical::ChainOrder>,
}

impl NormalizationStateV1 {
    pub fn new(cluster: &str, program_id: &str) -> Self {
        Self {
            cluster: cluster.to_owned(),
            program_id: program_id.to_owned(),
            markets: BTreeMap::new(),
            config_open: BTreeMap::new(),
            configurations: Vec::new(),
            last_transaction: None,
            last_instruction_order: None,
        }
    }

    pub fn push_transaction(
        &mut self,
        transaction: DecodedTransactionV1,
    ) -> Result<NormalizedTransactionBatchV1, NormalizeError> {
        self.push_transaction_inner(transaction, None)
    }

    pub fn push_transaction_with_provenance(
        &mut self,
        transaction: DecodedTransactionV1,
        provenance: &TransactionExecutionProvenanceV1,
    ) -> Result<NormalizedTransactionBatchV1, NormalizeError> {
        if provenance.signature != transaction.signature
            || provenance.slot != transaction.slot
            || provenance.transaction_index != transaction.transaction_index
            || provenance.decoder_version_id != transaction.decoder_version_id
        {
            return Err(NormalizeError::UnverifiedCpiProvenance);
        }
        self.push_transaction_inner(transaction, Some(provenance))
    }

    fn push_transaction_inner(
        &mut self,
        transaction: DecodedTransactionV1,
        provenance: Option<&TransactionExecutionProvenanceV1>,
    ) -> Result<NormalizedTransactionBatchV1, NormalizeError> {
        let transaction_order = (transaction.slot, transaction.transaction_index);
        if self
            .last_transaction
            .is_some_and(|previous| previous >= transaction_order)
        {
            return Err(NormalizeError::DuplicateOrder);
        }
        self.last_transaction = Some(transaction_order);
        let mut instructions = transaction.instructions.clone();
        instructions.sort_by_key(|instruction| instruction.order);
        for instruction in &instructions {
            if self
                .last_instruction_order
                .is_some_and(|previous| previous >= instruction.order)
            {
                return Err(NormalizeError::DuplicateOrder);
            }
            self.last_instruction_order = Some(instruction.order);
        }
        let account_use = state_changing_account_use(&instructions);
        let mut batch = NormalizedTransactionBatchV1::default();
        let mut assertions_by_parent =
            BTreeMap::<crate::canonical::ChainOrder, Vec<&DecodedInstructionV1>>::new();
        for assertion in instructions
            .iter()
            .filter(|instruction| instruction.class == DecodedInstructionClass::EmittedAssertion)
        {
            match provenance.and_then(|frames| frames.verified_parent(assertion.order)) {
                Some(parent) => assertions_by_parent
                    .entry(parent)
                    .or_default()
                    .push(assertion),
                None => batch.gaps.push(gap(
                    assertion,
                    NormalizationGapCode::UnverifiedCpiProvenance,
                )),
            }
        }

        for instruction in instructions
            .iter()
            .filter(|instruction| instruction.class == DecodedInstructionClass::StateChanging)
        {
            let exact_token_deltas = match provenance
                .map(|proof| exact_operation_token_deltas(&transaction, proof, instruction))
                .transpose()
            {
                Ok(value) => value,
                Err(error) => {
                    batch.gaps.push(gap(instruction, error.gap_code()));
                    continue;
                }
            };
            let decoded_assertions = assertions_by_parent
                .remove(&instruction.order)
                .unwrap_or_default();
            let legacy_swap_fees = match decoded_assertions
                .iter()
                .find(|assertion| assertion.kind == "swap_event_legacy_v0")
                .map(|assertion| {
                    legacy_swap_fee_evidence(
                        instruction,
                        assertion,
                        exact_token_deltas
                            .as_ref()
                            .ok_or(NormalizeError::InstructionScopedTokenEvidence)?,
                    )
                })
                .transpose()
            {
                Ok(value) => value,
                Err(error) => {
                    batch.gaps.push(gap(instruction, error.gap_code()));
                    continue;
                }
            };
            let canonical_assertions = decoded_assertions
                .iter()
                .map(|assertion| {
                    let execution_path = provenance
                        .and_then(|frames| frames.verified_execution_path(assertion.order))
                        .ok_or(NormalizeError::UnverifiedCpiProvenance)?;
                    canonical_event(
                        assertion,
                        execution_path.to_vec(),
                        &self.cluster,
                        &self.program_id,
                        legacy_swap_fees.as_ref(),
                    )
                })
                .collect::<Result<Vec<_>, _>>();
            let canonical_assertions = match canonical_assertions {
                Ok(assertions) => assertions,
                Err(error) => {
                    batch.gaps.push(gap(instruction, error.gap_code()));
                    continue;
                }
            };
            let pair = match operation_pair(instruction, &canonical_assertions) {
                Ok(pair) => pair,
                Err(error) => {
                    batch.gaps.push(gap(instruction, error.gap_code()));
                    continue;
                }
            };
            let actor = operation_actor(instruction, &canonical_assertions);
            let payload = build_operation_payload(
                instruction,
                &transaction,
                &canonical_assertions,
                &account_use,
                exact_token_deltas.as_ref(),
                &mut self.markets,
            );
            let payload = match payload {
                Ok(payload) => payload,
                Err(error) => {
                    if std::env::var_os("V1_VS_DUSK_TRACE_TOKEN_CPI").is_some() {
                        eprintln!(
                            "operation payload normalization failed signature={} order={:?}: {error}",
                            instruction.signature, instruction.order
                        );
                    }
                    batch.gaps.push(gap(instruction, error.gap_code()));
                    continue;
                }
            };
            let operation = CanonicalOperationV1 {
                schema_version: REPLAY_SCHEMA_VERSION,
                operation_id: deterministic_event_id(
                    &self.cluster,
                    &self.program_id,
                    &instruction.signature,
                    instruction.order,
                ),
                signature: instruction.signature.clone(),
                pair: pair.clone(),
                actor,
                order: instruction.order,
                block_time_unix_seconds: instruction.block_time_unix_seconds,
                decoder_version_id: instruction.decoder_version_id.clone(),
                payload,
                assertions: canonical_assertions,
            };
            if operation.validate().is_err() {
                batch.gaps.push(gap(
                    instruction,
                    NormalizationGapCode::MalformedDecodedPayload,
                ));
                continue;
            }
            // Canonical assertions are emitted only when their state-changing
            // operation is itself replayable. This prevents an orphan event
            // from looking like a factual transition after normalization
            // recorded a gap for the same instruction.
            batch.events.extend(operation.assertions.iter().cloned());
            for (scope, entity, fields) in configs_from_operation(instruction, &operation) {
                let key = (scope.clone(), entity.clone());
                let merged_fields = self
                    .config_open
                    .get(&key)
                    .map(|open_index| self.configurations[*open_index].fields.clone())
                    .map_or(fields.clone(), |mut current| {
                        current.extend(fields);
                        current
                    });
                if let Some(open_index) = self.config_open.insert(key, self.configurations.len()) {
                    self.configurations[open_index].valid_to_exclusive = Some(operation.order);
                }
                self.configurations.push(ConfigVersionV1 {
                    config_id: sha256_hex(
                        format!("{}:{scope:?}:{entity}", operation.operation_id).as_bytes(),
                    ),
                    scope,
                    entity_address: entity,
                    valid_from: operation.order,
                    valid_to_exclusive: None,
                    source_signature: operation.signature.clone(),
                    decoder_version_id: operation.decoder_version_id.clone(),
                    fields: merged_fields,
                });
            }
            batch.operations.push(operation);
        }
        // A proven assertion whose direct parent is not a decoded
        // state-changing target instruction is still orphaned evidence.
        for assertions in assertions_by_parent.into_values() {
            for assertion in assertions {
                batch.gaps.push(gap(
                    assertion,
                    NormalizationGapCode::UnverifiedCpiProvenance,
                ));
            }
        }
        batch.operations.sort_by_key(|operation| operation.order);
        batch.events.sort_by_key(|event| event.order);
        batch.gaps.sort_by_key(|item| item.order);
        Ok(batch)
    }

    pub fn finish_configurations(self) -> Vec<ConfigVersionV1> {
        self.configurations
    }
}

fn gap(instruction: &DecodedInstructionV1, code: NormalizationGapCode) -> NormalizationGapV1 {
    NormalizationGapV1 {
        schema_version: NORMALIZE_SCHEMA_VERSION,
        signature: instruction.signature.clone(),
        order: instruction.order,
        instruction_kind: instruction.kind.clone(),
        code,
    }
}

fn state_changing_account_use(instructions: &[DecodedInstructionV1]) -> BTreeMap<String, u32> {
    let mut counts = BTreeMap::new();
    for account in instructions
        .iter()
        .filter(|instruction| instruction.class == DecodedInstructionClass::StateChanging)
        .flat_map(|instruction| instruction.accounts.iter())
    {
        *counts.entry(account.clone()).or_default() += 1;
    }
    counts
}

fn exact_operation_token_deltas(
    transaction: &DecodedTransactionV1,
    provenance: &TransactionExecutionProvenanceV1,
    instruction: &DecodedInstructionV1,
) -> Result<ExactOperationTokenDeltasV1, NormalizeError> {
    let mut expected_transfers = Vec::new();
    let mut expected_supply_changes = Vec::new();
    for parsed in &provenance.token_cpis {
        if nearest_state_changing_target_ancestor(provenance, parsed.order())?
            != Some(instruction.order)
        {
            continue;
        }
        match parsed {
            ParsedTokenCpiV1::Transfer { evidence } => {
                let source = optional_token_balance_for_account(
                    transaction,
                    &evidence.source_token_account,
                )?;
                let destination = optional_token_balance_for_account(
                    transaction,
                    &evidence.destination_token_account,
                )?;
                let (mint, decimals) = match evidence.variant {
                    TokenTransferVariantV1::LegacyTransfer => {
                        if evidence.mint.is_some() || evidence.decimals.is_some() {
                            return Err(NormalizeError::InstructionScopedTokenEvidence);
                        }
                        let identity = source
                            .or(destination)
                            .ok_or(NormalizeError::MissingTokenBalance)?;
                        for available in [source, destination].into_iter().flatten() {
                            cross_check_token_identity(
                                available,
                                evidence.token_program,
                                &identity.mint,
                                identity.decimals,
                            )?;
                        }
                        (identity.mint.clone(), identity.decimals)
                    }
                    TokenTransferVariantV1::LegacyTransferChecked
                    | TokenTransferVariantV1::Token2022TransferChecked
                    | TokenTransferVariantV1::Token2022TransferCheckedWithFee => {
                        let mint = evidence
                            .mint
                            .as_deref()
                            .ok_or(NormalizeError::InstructionScopedTokenEvidence)?;
                        let decimals = evidence
                            .decimals
                            .ok_or(NormalizeError::InstructionScopedTokenEvidence)?;
                        for available in [source, destination].into_iter().flatten() {
                            cross_check_token_identity(
                                available,
                                evidence.token_program,
                                mint,
                                decimals,
                            )?;
                        }
                        (mint.to_owned(), decimals)
                    }
                };
                let resolved_fee = match evidence.variant {
                    TokenTransferVariantV1::LegacyTransfer
                    | TokenTransferVariantV1::LegacyTransferChecked => Some(TokenAmount::zero()),
                    TokenTransferVariantV1::Token2022TransferCheckedWithFee => evidence
                        .explicit_fee
                        .clone()
                        .ok_or(NormalizeError::InstructionScopedTokenEvidence)
                        .map(Some)?,
                    // A plain Token-2022 checked transfer does not put its
                    // assessed fee on the wire. The selected panel is Legacy;
                    // any future Token-2022 row remains a typed gap until the
                    // historical schedule is supplied to normalization.
                    TokenTransferVariantV1::Token2022TransferChecked => {
                        return Err(NormalizeError::InstructionScopedTokenEvidence)
                    }
                };
                expected_transfers.push(ExpectedTokenTransferV1 {
                    schema_version: TOKEN_CPI_SCHEMA_VERSION,
                    transfer_order: evidence.order,
                    token_program: evidence.token_program,
                    source_token_account: evidence.source_token_account.clone(),
                    destination_token_account: evidence.destination_token_account.clone(),
                    mint,
                    authority: evidence.authority.clone(),
                    expected_gross_amount: Some(evidence.gross_amount.clone()),
                    expected_decimals: Some(decimals),
                    resolved_fee,
                });
            }
            ParsedTokenCpiV1::SupplyChange { evidence } => {
                let token_account =
                    optional_token_balance_for_account(transaction, &evidence.token_account)?;
                let checked = matches!(
                    evidence.variant,
                    TokenSupplyChangeVariantV1::LegacyMintToChecked
                        | TokenSupplyChangeVariantV1::LegacyBurnChecked
                        | TokenSupplyChangeVariantV1::Token2022MintToChecked
                        | TokenSupplyChangeVariantV1::Token2022BurnChecked
                );
                let decimals = match (checked, evidence.decimals) {
                    (true, Some(decimals)) => decimals,
                    (false, None) => {
                        token_account
                            .ok_or(NormalizeError::MissingTokenBalance)?
                            .decimals
                    }
                    _ => return Err(NormalizeError::InstructionScopedTokenEvidence),
                };
                if let Some(available) = token_account {
                    cross_check_token_identity(
                        available,
                        evidence.token_program,
                        &evidence.mint,
                        decimals,
                    )?;
                }
                expected_supply_changes.push(ExpectedTokenSupplyChangeV1 {
                    schema_version: TOKEN_CPI_SCHEMA_VERSION,
                    supply_change_order: evidence.order,
                    token_program: evidence.token_program,
                    kind: evidence.kind,
                    token_account: evidence.token_account.clone(),
                    mint: evidence.mint.clone(),
                    authority: evidence.authority.clone(),
                    expected_amount: Some(evidence.amount.clone()),
                    expected_decimals: Some(decimals),
                });
            }
            ParsedTokenCpiV1::RecognizedNonTransfer { .. } => {}
        }
    }
    aggregate_exact_operation_token_deltas_v1(
        provenance,
        instruction.order,
        &provenance.token_cpis,
        &expected_transfers,
        &expected_supply_changes,
    )
    .map_err(|error| {
        if std::env::var_os("V1_VS_DUSK_TRACE_TOKEN_CPI").is_some() {
            eprintln!(
                "token CPI normalization failed signature={} order={:?}: {error}",
                transaction.signature, instruction.order
            );
        }
        NormalizeError::InstructionScopedTokenEvidence
    })
}

fn nearest_state_changing_target_ancestor(
    provenance: &TransactionExecutionProvenanceV1,
    child: crate::canonical::ChainOrder,
) -> Result<Option<crate::canonical::ChainOrder>, NormalizeError> {
    let mut current = child;
    let mut visited = BTreeSet::new();
    loop {
        if !visited.insert(current) {
            return Err(NormalizeError::UnverifiedCpiProvenance);
        }
        let Some(parent) = provenance.verified_parent(current) else {
            return Ok(None);
        };
        let frame = provenance
            .frames
            .iter()
            .find(|frame| frame.order == parent)
            .ok_or(NormalizeError::UnverifiedCpiProvenance)?;
        if frame.is_target_program
            && frame.decoded_target_class == Some(DecodedInstructionClass::StateChanging)
        {
            return Ok(Some(parent));
        }
        current = parent;
    }
}

fn optional_token_balance_for_account<'a>(
    transaction: &'a DecodedTransactionV1,
    account: &str,
) -> Result<Option<&'a TokenAccountBalanceV1>, NormalizeError> {
    let mut matches = transaction
        .token_balances
        .iter()
        .filter(|balance| balance.account == account);
    let value = matches.next();
    if matches.next().is_some() {
        return Err(NormalizeError::InstructionScopedTokenEvidence);
    }
    Ok(value)
}

fn cross_check_token_identity(
    balance: &TokenAccountBalanceV1,
    token_program: crate::transfer_fee::TokenProgramKindV1,
    mint: &str,
    decimals: u8,
) -> Result<(), NormalizeError> {
    if balance.token_program != token_program
        || balance.mint != mint
        || balance.decimals != decimals
    {
        return Err(NormalizeError::InstructionScopedTokenEvidence);
    }
    Ok(())
}

fn scoped_delta(
    evidence: &ExactOperationTokenDeltasV1,
    account: &str,
    direction: DeltaDirection,
) -> Result<TokenAmount, NormalizeError> {
    let signed = evidence
        .account_deltas
        .iter()
        .find(|row| row.token_account == account)
        .map(|row| row.signed_amount.as_str())
        .unwrap_or("0")
        .parse::<i128>()
        .map_err(|_| NormalizeError::AmountOverflow)?;
    let magnitude = match direction {
        DeltaDirection::Credit if signed >= 0 => signed as u128,
        DeltaDirection::Debit if signed <= 0 => signed.unsigned_abs(),
        DeltaDirection::Credit | DeltaDirection::Debit => {
            return Err(NormalizeError::InstructionScopedTokenEvidence)
        }
    };
    token(magnitude)
}

fn require_zero_token_effects(
    evidence: Option<&ExactOperationTokenDeltasV1>,
) -> Result<(), NormalizeError> {
    let evidence = evidence.ok_or(NormalizeError::InstructionScopedTokenEvidence)?;
    if !evidence.matched_transfers.is_empty()
        || !evidence.matched_supply_changes.is_empty()
        || evidence
            .account_deltas
            .iter()
            .any(|delta| delta.signed_amount.as_str() != "0")
    {
        return Err(NormalizeError::InstructionScopedTokenEvidence);
    }
    Ok(())
}

fn custody_delta(
    exact: Option<&ExactOperationTokenDeltasV1>,
    transaction: &DecodedTransactionV1,
    account: &str,
    direction: DeltaDirection,
    account_use: &BTreeMap<String, u32>,
) -> Result<TokenAmount, NormalizeError> {
    let Some(evidence) = exact else {
        return account_delta(transaction, account, direction, account_use);
    };
    let scoped = scoped_delta(evidence, account, direction)?;
    if account_use.get(account).copied().unwrap_or_default() == 1
        && !transaction
            .non_target_outer_touched_accounts
            .iter()
            .any(|touched| touched == account)
    {
        let whole_transaction = transaction
            .token_balances
            .iter()
            .find(|balance| balance.account == account)
            .ok_or(NormalizeError::MissingTokenBalance)
            .and_then(|balance| delta(balance, direction))?;
        if whole_transaction != scoped {
            return Err(NormalizeError::InstructionScopedTokenEvidence);
        }
    }
    Ok(scoped)
}

fn operation_pair(
    instruction: &DecodedInstructionV1,
    assertions: &[CanonicalEventV1],
) -> Result<String, NormalizeError> {
    if let Some(pair) = assertions.iter().find_map(|event| event.pair.clone()) {
        return Ok(pair);
    }
    let index = match instruction.kind.as_str() {
        "initialize" => Some(3),
        "claim_protocol_fees" => Some(1),
        "set_pair_reduce_only" => Some(2),
        "init_futarchy_authority"
        | "set_global_reduce_only"
        | "update_futarchy_authority"
        | "update_protocol_revenue"
        | "update_revenue_recipients" => None,
        _ => Some(0),
    };
    index.map_or_else(
        || Ok(GLOBAL_ENTITY.to_owned()),
        |index| {
            instruction
                .accounts
                .get(index)
                .cloned()
                .ok_or(NormalizeError::MalformedPayload)
        },
    )
}

fn operation_actor(
    instruction: &DecodedInstructionV1,
    assertions: &[CanonicalEventV1],
) -> Option<String> {
    assertions
        .iter()
        .find_map(|event| event.actor.clone())
        .or_else(|| {
            let index = match instruction.kind.as_str() {
                "initialize" => 0,
                "claim_protocol_fees" => 0,
                "add_liquidity" | "remove_liquidity" => 11,
                "swap" => 9,
                "add_collateral" | "remove_collateral" => 7,
                "borrow" | "repay" => 7,
                "liquidate" => 9,
                "flashloan" => 10,
                _ => 0,
            };
            instruction.accounts.get(index).cloned()
        })
}

fn build_operation_payload(
    instruction: &DecodedInstructionV1,
    transaction: &DecodedTransactionV1,
    assertions: &[CanonicalEventV1],
    account_use: &BTreeMap<String, u32>,
    exact_token_deltas: Option<&ExactOperationTokenDeltasV1>,
    markets: &mut BTreeMap<String, MarketIdentity>,
) -> Result<OperationPayloadV1, NormalizeError> {
    let body = variant_body(&instruction.data)?;
    match instruction.kind.as_str() {
        "initialize" => {
            let created = assertions
                .iter()
                .find_map(|event| match &event.payload {
                    EventPayloadV1::Config(fields)
                        if fields.get("event_kind").map(String::as_str) == Some("pair_created") =>
                    {
                        Some(fields)
                    }
                    _ => None,
                })
                .ok_or(NormalizeError::MissingAssertion)?;
            let pair = assertions
                .iter()
                .find_map(|event| event.pair.clone())
                .ok_or(NormalizeError::MissingAssertion)?;
            let token0 = required_config(created, "token0")?.to_owned();
            let token1 = required_config(created, "token1")?.to_owned();
            // Historical revisions accepted a fresh signer rate-model account,
            // while newer code derives a PDA. In both revisions the exact
            // instruction account must equal the key emitted by
            // PairCreatedEvent; preserve that common, revision-independent
            // binding before the replay adapter selects an address policy.
            if account(instruction, 5)? != required_config(created, "rate_model")? {
                return Err(NormalizeError::InconsistentMarketIdentity);
            }
            let token0_decimals = parse_u8(required_config(created, "token0_decimals")?)?;
            let token1_decimals = parse_u8(required_config(created, "token1_decimals")?)?;
            let identity = MarketIdentity {
                token0: token0.clone(),
                token1: token1.clone(),
                token0_decimals,
                token1_decimals,
            };
            if markets.insert(pair.clone(), identity.clone()).is_some() {
                return Err(NormalizeError::InconsistentMarketIdentity);
            }
            let mint = required_liquidity(assertions, LiquidityAction::Mint)?;
            if let Some(exact) = exact_token_deltas {
                require_supply_change(
                    exact,
                    TokenSupplyChangeKindV1::Mint,
                    required_config(created, "lp_mint")?,
                    None,
                    &pair,
                    &mint.liquidity,
                )?;
            }
            let mut initialization_fields = created.clone();
            // PairCreatedEvent proves post-initialize values, while the decoded
            // instruction retains inputs that the event does not emit (most
            // importantly `initial_rate_bps`). Keep both namespaces so a
            // native replay can reconstruct initialization without defaults.
            flatten_json("instruction", body, &mut initialization_fields);
            // `flatten_json` intentionally omits JSON nulls for ordinary
            // slowly-changing config updates. For initialize, however, null
            // is the exact on-chain encoding of an absent Option and differs
            // from a decoder omission. Preserve every optional rate/collateral
            // argument explicitly so revision-pinned replay can resolve the
            // program default without guessing.
            preserve_initialize_optional_args(body, &mut initialization_fields);
            Ok(OperationPayloadV1::InitializeMarket {
                token0,
                token1,
                token0_decimals,
                token1_decimals,
                gross_amount0: amount_field(body, &["args", "amount0_in"])?,
                gross_amount1: amount_field(body, &["args", "amount1_in"])?,
                vault_credit0: custody_delta(
                    exact_token_deltas,
                    transaction,
                    account(instruction, 9)?,
                    DeltaDirection::Credit,
                    account_use,
                )?,
                vault_credit1: custody_delta(
                    exact_token_deltas,
                    transaction,
                    account(instruction, 10)?,
                    DeltaDirection::Credit,
                    account_use,
                )?,
                minted_lp: mint.liquidity.clone(),
                min_liquidity_out: amount_field(body, &["args", "min_liquidity_out"])?,
                fields: initialization_fields,
            })
        }
        "add_liquidity" => {
            let mint = required_liquidity(assertions, LiquidityAction::Mint)?;
            if let Some(exact) = exact_token_deltas {
                require_supply_change(
                    exact,
                    TokenSupplyChangeKindV1::Mint,
                    account(instruction, 9)?,
                    Some(account(instruction, 10)?),
                    account(instruction, 0)?,
                    &mint.liquidity,
                )?;
            }
            Ok(OperationPayloadV1::AddLiquidity {
                gross_amount0: amount_field(body, &["args", "amount0_in"])?,
                gross_amount1: amount_field(body, &["args", "amount1_in"])?,
                vault_credit0: custody_delta(
                    exact_token_deltas,
                    transaction,
                    account(instruction, 3)?,
                    DeltaDirection::Credit,
                    account_use,
                )?,
                vault_credit1: custody_delta(
                    exact_token_deltas,
                    transaction,
                    account(instruction, 4)?,
                    DeltaDirection::Credit,
                    account_use,
                )?,
                minted_lp: mint.liquidity.clone(),
                min_liquidity_out: amount_field(body, &["args", "min_liquidity_out"])?,
            })
        }
        "remove_liquidity" => {
            let burn = required_liquidity(assertions, LiquidityAction::Burn)?;
            if let Some(exact) = exact_token_deltas {
                require_supply_change(
                    exact,
                    TokenSupplyChangeKindV1::Burn,
                    account(instruction, 9)?,
                    Some(account(instruction, 10)?),
                    account(instruction, 11)?,
                    &burn.liquidity,
                )?;
            }
            Ok(OperationPayloadV1::RemoveLiquidity {
                requested_lp: amount_field(body, &["args", "liquidity_in"])?,
                burned_lp: burn.liquidity.clone(),
                vault_debit0: custody_delta(
                    exact_token_deltas,
                    transaction,
                    account(instruction, 3)?,
                    DeltaDirection::Debit,
                    account_use,
                )?,
                vault_debit1: custody_delta(
                    exact_token_deltas,
                    transaction,
                    account(instruction, 4)?,
                    DeltaDirection::Debit,
                    account_use,
                )?,
                recipient_credit0: custody_delta(
                    exact_token_deltas,
                    transaction,
                    account(instruction, 5)?,
                    DeltaDirection::Credit,
                    account_use,
                )?,
                recipient_credit1: custody_delta(
                    exact_token_deltas,
                    transaction,
                    account(instruction, 6)?,
                    DeltaDirection::Credit,
                    account_use,
                )?,
                min_amount0_out: amount_field(body, &["args", "min_amount0_out"])?,
                min_amount1_out: amount_field(body, &["args", "min_amount1_out"])?,
            })
        }
        "swap" => {
            let swap = assertions
                .iter()
                .find_map(|event| match &event.payload {
                    EventPayloadV1::Swap(payload) => Some(payload),
                    _ => None,
                })
                .ok_or(NormalizeError::MissingAssertion)?;
            Ok(OperationPayloadV1::SwapExactIn {
                token0_in: swap.token0_in,
                gross_amount_in: amount_field(body, &["args", "amount_in"])?,
                vault_credit_in: custody_delta(
                    exact_token_deltas,
                    transaction,
                    account(instruction, 3)?,
                    DeltaDirection::Credit,
                    account_use,
                )?,
                min_amount_out: amount_field(body, &["args", "min_amount_out"])?,
                vault_debit_out: custody_delta(
                    exact_token_deltas,
                    transaction,
                    account(instruction, 4)?,
                    DeltaDirection::Debit,
                    account_use,
                )?,
                recipient_credit_out: custody_delta(
                    exact_token_deltas,
                    transaction,
                    account(instruction, 6)?,
                    DeltaDirection::Credit,
                    account_use,
                )?,
            })
        }
        "add_collateral" | "remove_collateral" => {
            let delta = required_pair_delta(assertions, true)?;
            let depositing = instruction.kind == "add_collateral";
            let amount0 = parse_signed(&delta.amount0)?;
            let amount1 = parse_signed(&delta.amount1)?;
            // Historical deployments permitted a successful `remove all`
            // against an already-empty position. The instruction emitted an
            // exact zero delta and performed no SPL custody transition. Keep
            // the ordered operation as a clock-only replay step instead of
            // inventing a collateral side or transfer amount.
            if amount0 == 0 && amount1 == 0 {
                require_zero_token_effects(exact_token_deltas)?;
                return Ok(OperationPayloadV1::ClockOnly);
            }
            let absolute_delta = if amount0 != 0 && amount1 == 0 {
                amount0.unsigned_abs()
            } else if amount1 != 0 && amount0 == 0 {
                amount1.unsigned_abs()
            } else {
                return Err(NormalizeError::MalformedPayload);
            };
            Ok(OperationPayloadV1::CollateralDelta {
                position: account(instruction, if depositing { 3 } else { 1 })?.to_owned(),
                amount0: delta.amount0.clone(),
                amount1: delta.amount1.clone(),
                requested_amount: amount_field(body, &["args", "amount"])?,
                gross_transfer_amount: token(absolute_delta)?,
                vault_amount: custody_delta(
                    exact_token_deltas,
                    transaction,
                    account(instruction, 4)?,
                    if depositing {
                        DeltaDirection::Credit
                    } else {
                        DeltaDirection::Debit
                    },
                    account_use,
                )?,
                counterparty_amount: custody_delta(
                    exact_token_deltas,
                    transaction,
                    account(instruction, 5)?,
                    if depositing {
                        DeltaDirection::Debit
                    } else {
                        DeltaDirection::Credit
                    },
                    account_use,
                )?,
            })
        }
        "borrow" | "repay" => {
            let delta = required_pair_delta(assertions, false)?;
            let amount0 = parse_signed(&delta.amount0)?;
            let amount1 = parse_signed(&delta.amount1)?;
            let (debt_token0, absolute) = if amount0 != 0 && amount1 == 0 {
                (true, amount0.unsigned_abs())
            } else if amount1 != 0 && amount0 == 0 {
                (false, amount1.unsigned_abs())
            } else {
                return Err(NormalizeError::MalformedPayload);
            };
            let amount = token(absolute)?;
            if instruction.kind == "borrow" {
                // Bind the event-derived actual debt increase to the routed
                // reserve-vault debit CPI before `gross_amount` is allowed to
                // stand for both pieces of evidence downstream.
                let reserve_vault_debit = custody_delta(
                    exact_token_deltas,
                    transaction,
                    account(instruction, 4)?,
                    DeltaDirection::Debit,
                    account_use,
                )?;
                if reserve_vault_debit != amount {
                    return Err(NormalizeError::InstructionScopedTokenEvidence);
                }
                Ok(OperationPayloadV1::Borrow {
                    position: account(instruction, 1)?.to_owned(),
                    debt_token0,
                    requested_amount: amount_field(body, &["args", "amount"])?,
                    gross_amount: amount,
                    recipient_credit: custody_delta(
                        exact_token_deltas,
                        transaction,
                        account(instruction, 5)?,
                        DeltaDirection::Credit,
                        account_use,
                    )?,
                })
            } else {
                Ok(OperationPayloadV1::Repay {
                    position: account(instruction, 1)?.to_owned(),
                    debt_token0,
                    requested_amount: amount_field(body, &["args", "amount"])?,
                    gross_amount: amount,
                    vault_credit: custody_delta(
                        exact_token_deltas,
                        transaction,
                        account(instruction, 4)?,
                        DeltaDirection::Credit,
                        account_use,
                    )?,
                })
            }
        }
        "liquidate" => {
            let liquidation = assertions
                .iter()
                .find_map(|event| match &event.payload {
                    EventPayloadV1::Liquidation(payload) => Some(payload),
                    _ => None,
                })
                .ok_or(NormalizeError::MissingAssertion)?;
            let debt0 = parse_amount(&liquidation.debt0_liquidated)?;
            let debt1 = parse_amount(&liquidation.debt1_liquidated)?;
            let (debt_token0, requested_repay) = match (debt0 > 0, debt1 > 0) {
                (true, false) => (true, liquidation.debt0_liquidated.clone()),
                (false, true) => (false, liquidation.debt1_liquidated.clone()),
                (false, false) => {
                    // An older V1 liquidation path could succeed after its
                    // clock/rate update while liquidating nothing. Its event
                    // is authoritative only when every economic field is
                    // zero, k is unchanged, and instruction-scoped SPL
                    // evidence proves no custody effect.
                    let economic_fields_are_zero = [
                        &liquidation.collateral0_liquidated,
                        &liquidation.collateral1_liquidated,
                        &liquidation.liquidation_bonus,
                        &liquidation.shortfall,
                    ]
                    .into_iter()
                    .try_fold(true, |all_zero, value| {
                        Ok::<_, NormalizeError>(all_zero && parse_amount(value)? == 0)
                    })?;
                    if !economic_fields_are_zero || liquidation.k_before != liquidation.k_after {
                        return Err(NormalizeError::MalformedPayload);
                    }
                    require_zero_token_effects(exact_token_deltas)?;
                    return Ok(OperationPayloadV1::ClockOnly);
                }
                (true, true) => return Err(NormalizeError::MalformedPayload),
            };
            Ok(OperationPayloadV1::Liquidate {
                position: liquidation.position.clone(),
                debt_token0,
                requested_repay,
                collateral_vault_debit: custody_delta(
                    exact_token_deltas,
                    transaction,
                    account(instruction, 4)?,
                    DeltaDirection::Debit,
                    account_use,
                )?,
                reserve_vault_credit: custody_delta(
                    exact_token_deltas,
                    transaction,
                    account(instruction, 7)?,
                    DeltaDirection::Credit,
                    account_use,
                )?,
                liquidator_recipient_credit: custody_delta(
                    exact_token_deltas,
                    transaction,
                    account(instruction, 5)?,
                    DeltaDirection::Credit,
                    account_use,
                )?,
            })
        }
        "flashloan" => {
            let flashloan = assertions
                .iter()
                .find_map(|event| match &event.payload {
                    EventPayloadV1::Flashloan(payload) => Some(payload),
                    _ => None,
                })
                .ok_or(NormalizeError::MissingAssertion)?;
            Ok(OperationPayloadV1::Flashloan {
                amount0: flashloan.amount0.clone(),
                amount1: flashloan.amount1.clone(),
                fee0: flashloan.fee0.clone(),
                fee1: flashloan.fee1.clone(),
                net_vault_credit0: custody_delta(
                    exact_token_deltas,
                    transaction,
                    account(instruction, 3)?,
                    DeltaDirection::Credit,
                    account_use,
                )?,
                net_vault_credit1: custody_delta(
                    exact_token_deltas,
                    transaction,
                    account(instruction, 4)?,
                    DeltaDirection::Credit,
                    account_use,
                )?,
            })
        }
        "claim_protocol_fees" => {
            let exact = exact_token_deltas.ok_or(NormalizeError::InstructionScopedTokenEvidence)?;
            let amount0 = scoped_delta(exact, account(instruction, 4)?, DeltaDirection::Debit)?;
            let amount1 = scoped_delta(exact, account(instruction, 5)?, DeltaDirection::Debit)?;
            if let Some(claim) = assertions.iter().find_map(|event| match &event.payload {
                EventPayloadV1::ProtocolFeeClaim(payload) => Some(payload),
                _ => None,
            }) {
                let asserted0 = sum_amounts(&[
                    &claim.futarchy_treasury_amount0,
                    &claim.buybacks_vault_amount0,
                    &claim.team_treasury_amount0,
                ])?;
                let asserted1 = sum_amounts(&[
                    &claim.futarchy_treasury_amount1,
                    &claim.buybacks_vault_amount1,
                    &claim.team_treasury_amount1,
                ])?;
                if asserted0 != amount0 || asserted1 != amount1 {
                    return Err(NormalizeError::InstructionScopedTokenEvidence);
                }
            }
            Ok(OperationPayloadV1::ClaimProtocolRevenue { amount0, amount1 })
        }
        "update_pair" => {
            if !assertions
                .iter()
                .any(|event| matches!(&event.payload, EventPayloadV1::PairUpdate(_)))
            {
                return Err(NormalizeError::MissingAssertion);
            }
            Ok(OperationPayloadV1::ClockOnly)
        }
        "init_futarchy_authority"
        | "set_global_reduce_only"
        | "set_pair_reduce_only"
        | "update_futarchy_authority"
        | "update_protocol_revenue"
        | "update_revenue_recipients" => {
            let mut fields = BTreeMap::new();
            fields.insert("instruction_kind".to_owned(), instruction.kind.clone());
            flatten_json("", body, &mut fields);
            Ok(OperationPayloadV1::UpdateConfig { fields })
        }
        _ => Err(NormalizeError::UnsupportedInstruction),
    }
}

fn legacy_swap_fee_evidence(
    operation: &DecodedInstructionV1,
    assertion: &DecodedInstructionV1,
    evidence: &ExactOperationTokenDeltasV1,
) -> Result<LegacySwapFeeEvidence, NormalizeError> {
    if operation.kind != "swap" || assertion.kind != "swap_event_legacy_v0" {
        return Err(NormalizeError::InstructionScopedTokenEvidence);
    }
    let body = variant_body(&assertion.data)?;
    let amount_in = parse_amount(&amount_map(body, "amount_in")?)?;
    let amount_out = parse_amount(&amount_map(body, "amount_out")?)?;
    let amount_after_fee = parse_amount(&amount_map(body, "amount_in_after_fee")?)?;
    let total_fee = amount_in
        .checked_sub(amount_after_fee)
        .ok_or(NormalizeError::MalformedPayload)?;

    let input_vault = account(operation, 3)?;
    let output_vault = account(operation, 4)?;
    let user_input = account(operation, 5)?;
    let user_output = account(operation, 6)?;
    let input_mint = account(operation, 7)?;
    let output_mint = account(operation, 8)?;
    let pair_authority = account(operation, 0)?;
    let protocol_destination = account(operation, 9)?;

    let mut input_transfer = None;
    let mut output_transfer = None;
    let mut protocol_transfer = None;
    for transfer in &evidence.matched_transfers {
        if transfer.source_token_account == user_input
            && transfer.destination_token_account == input_vault
            && transfer.mint == input_mint
        {
            if input_transfer.replace(transfer).is_some() {
                return Err(NormalizeError::InstructionScopedTokenEvidence);
            }
        } else if transfer.source_token_account == output_vault
            && transfer.destination_token_account == user_output
            && transfer.mint == output_mint
            && transfer.authority == pair_authority
        {
            if output_transfer.replace(transfer).is_some() {
                return Err(NormalizeError::InstructionScopedTokenEvidence);
            }
        } else if transfer.source_token_account == input_vault
            && transfer.destination_token_account == protocol_destination
            && transfer.mint == input_mint
            && transfer.authority == pair_authority
        {
            if protocol_transfer.replace(transfer).is_some() {
                return Err(NormalizeError::InstructionScopedTokenEvidence);
            }
        } else {
            return Err(NormalizeError::InstructionScopedTokenEvidence);
        }
    }
    let input_transfer = input_transfer.ok_or(NormalizeError::InstructionScopedTokenEvidence)?;
    let output_transfer = output_transfer.ok_or(NormalizeError::InstructionScopedTokenEvidence)?;
    if parse_amount(&input_transfer.gross_amount)? != amount_in
        || parse_amount(&input_transfer.destination_credit)? != amount_in
        || parse_amount(&output_transfer.gross_amount)? != amount_out
        || parse_amount(&output_transfer.destination_credit)? != amount_out
    {
        return Err(NormalizeError::InstructionScopedTokenEvidence);
    }
    let protocol_fee = protocol_transfer
        .map(|transfer| parse_amount(&transfer.gross_amount))
        .transpose()?
        .unwrap_or_default();
    let lp_fee = total_fee
        .checked_sub(protocol_fee)
        .ok_or(NormalizeError::InstructionScopedTokenEvidence)?;
    Ok(LegacySwapFeeEvidence {
        lp_fee: token(lp_fee)?,
        protocol_fee: token(protocol_fee)?,
    })
}

fn require_supply_change(
    evidence: &ExactOperationTokenDeltasV1,
    kind: TokenSupplyChangeKindV1,
    mint: &str,
    token_account: Option<&str>,
    authority: &str,
    amount: &TokenAmount,
) -> Result<(), NormalizeError> {
    let mut matches = evidence.matched_supply_changes.iter().filter(|change| {
        change.kind == kind
            && change.mint == mint
            && change.authority == authority
            && token_account.is_none_or(|account| change.token_account == account)
    });
    let value = matches
        .next()
        .ok_or(NormalizeError::InstructionScopedTokenEvidence)?;
    if matches.next().is_some() || &value.amount != amount {
        return Err(NormalizeError::InstructionScopedTokenEvidence);
    }
    Ok(())
}

fn canonical_event(
    instruction: &DecodedInstructionV1,
    execution_path: Vec<u32>,
    cluster: &str,
    program_id: &str,
    legacy_swap_fees: Option<&LegacySwapFeeEvidence>,
) -> Result<CanonicalEventV1, NormalizeError> {
    let body = variant_body(&instruction.data)?;
    let metadata = body.get("metadata").and_then(JsonValue::as_object);
    let pair = metadata
        .and_then(|value| value.get("pair"))
        .and_then(JsonValue::as_str)
        .map(str::to_owned);
    let actor = metadata
        .and_then(|value| value.get("signer"))
        .and_then(JsonValue::as_str)
        .map(str::to_owned);
    let payload = event_payload(&instruction.kind, body, legacy_swap_fees)?;
    Ok(CanonicalEventV1 {
        event_id: deterministic_event_id(
            cluster,
            program_id,
            &instruction.signature,
            instruction.order,
        ),
        cluster: cluster.to_owned(),
        program_id: program_id.to_owned(),
        pair,
        actor,
        signature: instruction.signature.clone(),
        order: instruction.order,
        execution_path,
        block_time_unix_seconds: instruction.block_time_unix_seconds,
        decoder_version_id: instruction.decoder_version_id.clone(),
        raw_payload_sha256: instruction.raw_data_sha256.clone(),
        db_source: None,
        payload,
    })
}

fn event_payload(
    kind: &str,
    body: &serde_json::Map<String, JsonValue>,
    legacy_swap_fees: Option<&LegacySwapFeeEvidence>,
) -> Result<EventPayloadV1, NormalizeError> {
    Ok(match kind {
        "swap_event" => EventPayloadV1::Swap(SwapPayloadV1 {
            token0_in: bool_field(body, "is_token0_in")?,
            amount_in: amount_map(body, "amount_in")?,
            amount_out: amount_map(body, "amount_out")?,
            amount_in_after_fee: amount_map(body, "amount_in_after_fee")?,
            lp_fee: amount_map(body, "lp_fee")?,
            protocol_fee: amount_map(body, "protocol_fee")?,
            reserve0_after: amount_map(body, "reserve0")?,
            reserve1_after: amount_map(body, "reserve1")?,
        }),
        "swap_event_legacy_v0" => {
            let fees = legacy_swap_fees.ok_or(NormalizeError::InstructionScopedTokenEvidence)?;
            EventPayloadV1::Swap(SwapPayloadV1 {
                token0_in: bool_field(body, "is_token0_in")?,
                amount_in: amount_map(body, "amount_in")?,
                amount_out: amount_map(body, "amount_out")?,
                amount_in_after_fee: amount_map(body, "amount_in_after_fee")?,
                lp_fee: fees.lp_fee.clone(),
                protocol_fee: fees.protocol_fee.clone(),
                reserve0_after: amount_map(body, "reserve0")?,
                reserve1_after: amount_map(body, "reserve1")?,
            })
        }
        "adjust_liquidity_event" | "mint_event" | "burn_event" => {
            EventPayloadV1::Liquidity(LiquidityPayloadV1 {
                action: if kind == "burn_event" {
                    LiquidityAction::Burn
                } else {
                    LiquidityAction::Mint
                },
                amount0: amount_map(body, "amount0")?,
                amount1: amount_map(body, "amount1")?,
                liquidity: amount_map(body, "liquidity")?,
            })
        }
        "user_liquidity_position_updated_event"
        | "user_liquidity_position_updated_event_legacy_v0" => {
            EventPayloadV1::LpPosition(LpPositionPayloadV1 {
                lp_amount: amount_map(body, "lp_amount")?,
                amount0: amount_map(body, "token0_amount")?,
                amount1: amount_map(body, "token1_amount")?,
                cash_reserve0: body.get("cash_reserve0").map(amount_value).transpose()?,
                cash_reserve1: body.get("cash_reserve1").map(amount_value).transpose()?,
            })
        }
        "user_position_updated_event_legacy_v0" => {
            let mut fields = BTreeMap::new();
            fields.insert(
                "event_kind".to_owned(),
                "user_position_updated_legacy_v0".to_owned(),
            );
            flatten_json("", body, &mut fields);
            EventPayloadV1::Config(fields)
        }
        "adjust_collateral_event" => EventPayloadV1::CollateralDelta(PairDeltaPayloadV1 {
            amount0: signed_map(body, "amount0")?,
            amount1: signed_map(body, "amount1")?,
        }),
        "adjust_debt_event" => EventPayloadV1::DebtDelta(PairDeltaPayloadV1 {
            amount0: signed_map(body, "amount0")?,
            amount1: signed_map(body, "amount1")?,
        }),
        "user_position_updated_event" => EventPayloadV1::Position(PositionPayloadV1 {
            position: string_map(body, "position")?,
            collateral0: amount_map(body, "collateral0")?,
            collateral1: amount_map(body, "collateral1")?,
            debt0_shares: amount_map(body, "debt0_shares")?,
            debt1_shares: amount_map(body, "debt1_shares")?,
            collateral0_max_cf_bps: u16_map(body, "collateral0_max_cf_bps")?,
            collateral1_max_cf_bps: u16_map(body, "collateral1_max_cf_bps")?,
            collateral0_liquidation_cf_bps: u16_map(body, "collateral0_liquidation_cf_bps")?,
            collateral1_liquidation_cf_bps: u16_map(body, "collateral1_liquidation_cf_bps")?,
        }),
        "user_position_liquidated_event" => EventPayloadV1::Liquidation(LiquidationPayloadV1 {
            position: string_map(body, "position")?,
            liquidator: string_map(body, "liquidator")?,
            collateral0_liquidated: amount_map(body, "collateral0_liquidated")?,
            collateral1_liquidated: amount_map(body, "collateral1_liquidated")?,
            debt0_liquidated: amount_map(body, "debt0_liquidated")?,
            debt1_liquidated: amount_map(body, "debt1_liquidated")?,
            collateral_price_nad: amount_map(body, "collateral_price")?,
            shortfall: amount_map(body, "shortfall")?,
            liquidation_bonus: amount_map(body, "liquidation_bonus_applied")?,
            k_before: amount_map(body, "k0")?,
            k_after: amount_map(body, "k1")?,
        }),
        "update_pair_event" => EventPayloadV1::PairUpdate(PairUpdatePayloadV1 {
            price0_ema_nad: amount_map(body, "price0_ema")?,
            price1_ema_nad: amount_map(body, "price1_ema")?,
            rate0_nad: amount_map(body, "rate0")?,
            rate1_nad: amount_map(body, "rate1")?,
            accrued_interest0: amount_map(body, "accrued_interest0")?,
            accrued_interest1: amount_map(body, "accrued_interest1")?,
            lp_interest0: amount_map(body, "lp_interest0")?,
            lp_interest1: amount_map(body, "lp_interest1")?,
            protocol_interest0: amount_map(body, "protocol_interest0")?,
            protocol_interest1: amount_map(body, "protocol_interest1")?,
            cash_reserve0: amount_map(body, "cash_reserve0")?,
            cash_reserve1: amount_map(body, "cash_reserve1")?,
            reserve0_after_interest: amount_map(body, "reserve0_after_interest")?,
            reserve1_after_interest: amount_map(body, "reserve1_after_interest")?,
        }),
        "flashloan_event" => EventPayloadV1::Flashloan(FlashloanPayloadV1 {
            amount0: amount_map(body, "amount0")?,
            amount1: amount_map(body, "amount1")?,
            fee0: amount_map(body, "fee0")?,
            fee1: amount_map(body, "fee1")?,
            receiver: string_map(body, "receiver")?,
        }),
        "claim_protocol_fees_event" => {
            EventPayloadV1::ProtocolFeeClaim(ProtocolFeeClaimPayloadV1 {
                token0: string_map(body, "token0")?,
                token1: string_map(body, "token1")?,
                futarchy_treasury_amount0: amount_map(body, "futarchy_treasury_amount0")?,
                futarchy_treasury_amount1: amount_map(body, "futarchy_treasury_amount1")?,
                buybacks_vault_amount0: amount_map(body, "buybacks_vault_amount0")?,
                buybacks_vault_amount1: amount_map(body, "buybacks_vault_amount1")?,
                team_treasury_amount0: amount_map(body, "team_treasury_amount0")?,
                team_treasury_amount1: amount_map(body, "team_treasury_amount1")?,
            })
        }
        "pair_created_event" | "user_position_created_event" => {
            let mut fields = BTreeMap::new();
            fields.insert(
                "event_kind".to_owned(),
                kind.trim_end_matches("_event").to_owned(),
            );
            flatten_json("", body, &mut fields);
            EventPayloadV1::Config(fields)
        }
        _ => return Err(NormalizeError::UnsupportedInstruction),
    })
}

fn configs_from_operation(
    instruction: &DecodedInstructionV1,
    operation: &CanonicalOperationV1,
) -> Vec<(ConfigScope, String, BTreeMap<String, String>)> {
    let fields = match &operation.payload {
        OperationPayloadV1::InitializeMarket { fields, .. }
        | OperationPayloadV1::UpdateConfig { fields } => fields.clone(),
        _ => return Vec::new(),
    };
    match instruction.kind.as_str() {
        "initialize" => {
            let mut configs = vec![
                (ConfigScope::Pair, operation.pair.clone(), fields.clone()),
                (
                    ConfigScope::ReduceOnly,
                    operation.pair.clone(),
                    BTreeMap::from([
                        ("reduce_only".to_owned(), "false".to_owned()),
                        ("source".to_owned(), "pair_initializer_default".to_owned()),
                    ]),
                ),
            ];
            if let Some(rate_model) = fields.get("rate_model") {
                let mut rate_fields = select_config_fields(
                    &fields,
                    &[
                        ("target_util_start_bps", "target_util_start_bps"),
                        ("target_util_end_bps", "target_util_end_bps"),
                        ("rate_half_life_ms", "rate_half_life_ms"),
                        ("min_rate_bps", "min_rate_bps"),
                        ("max_rate_bps", "max_rate_bps"),
                        ("instruction.args.initial_rate_bps", "initial_rate_bps"),
                    ],
                );
                let explicit_initial_rate = fields
                    .get("instruction.args.initial_rate_bps")
                    .filter(|value| value.as_str() != "null")
                    .cloned();
                rate_fields.insert(
                    "initial_rate_bps".to_owned(),
                    explicit_initial_rate.clone().unwrap_or_else(|| {
                        omnipair::constants::DEFAULT_INITIAL_RATE_BPS.to_string()
                    }),
                );
                rate_fields.insert(
                    "initial_rate_source".to_owned(),
                    if explicit_initial_rate.is_some() {
                        "instruction_argument"
                    } else {
                        "program_revision_default"
                    }
                    .to_owned(),
                );
                configs.push((ConfigScope::RateModel, rate_model.clone(), rate_fields));
            }
            configs
        }
        "init_futarchy_authority" => vec![
            (
                ConfigScope::ProtocolRevenue,
                GLOBAL_ENTITY.to_owned(),
                select_config_fields(
                    &fields,
                    &[
                        ("args.authority", "authority"),
                        ("args.swap_bps", "swap_bps"),
                        ("args.interest_bps", "interest_bps"),
                        ("args.futarchy_treasury_bps", "futarchy_treasury_bps"),
                        ("args.buybacks_vault_bps", "buybacks_vault_bps"),
                        ("args.team_treasury_bps", "team_treasury_bps"),
                    ],
                ),
            ),
            (
                ConfigScope::RevenueRecipients,
                GLOBAL_ENTITY.to_owned(),
                select_config_fields(
                    &fields,
                    &[
                        ("args.futarchy_treasury", "futarchy_treasury"),
                        ("args.buybacks_vault", "buybacks_vault"),
                        ("args.team_treasury", "team_treasury"),
                    ],
                ),
            ),
            (
                ConfigScope::ReduceOnly,
                GLOBAL_ENTITY.to_owned(),
                BTreeMap::from([
                    ("reduce_only".to_owned(), "false".to_owned()),
                    (
                        "source".to_owned(),
                        "futarchy_initializer_default".to_owned(),
                    ),
                ]),
            ),
        ],
        "update_futarchy_authority" => vec![(
            ConfigScope::ProtocolRevenue,
            GLOBAL_ENTITY.to_owned(),
            select_config_fields(&fields, &[("args.new_authority", "authority")]),
        )],
        "update_protocol_revenue" => vec![(
            ConfigScope::ProtocolRevenue,
            GLOBAL_ENTITY.to_owned(),
            select_config_fields(
                &fields,
                &[
                    ("args.swap_bps", "swap_bps"),
                    ("args.interest_bps", "interest_bps"),
                    (
                        "args.revenue_distribution.futarchy_treasury_bps",
                        "futarchy_treasury_bps",
                    ),
                    (
                        "args.revenue_distribution.buybacks_vault_bps",
                        "buybacks_vault_bps",
                    ),
                    (
                        "args.revenue_distribution.team_treasury_bps",
                        "team_treasury_bps",
                    ),
                ],
            ),
        )],
        "update_revenue_recipients" => vec![(
            ConfigScope::RevenueRecipients,
            GLOBAL_ENTITY.to_owned(),
            select_config_fields(
                &fields,
                &[
                    ("args.futarchy_treasury", "futarchy_treasury"),
                    ("args.buybacks_vault", "buybacks_vault"),
                    ("args.team_treasury", "team_treasury"),
                ],
            ),
        )],
        "set_global_reduce_only" | "set_pair_reduce_only" => vec![(
            ConfigScope::ReduceOnly,
            if instruction.kind == "set_global_reduce_only" {
                GLOBAL_ENTITY.to_owned()
            } else {
                operation.pair.clone()
            },
            select_config_fields(&fields, &[("args.reduce_only", "reduce_only")]),
        )],
        _ => Vec::new(),
    }
}

fn select_config_fields(
    fields: &BTreeMap<String, String>,
    mappings: &[(&str, &str)],
) -> BTreeMap<String, String> {
    mappings
        .iter()
        .filter_map(|(source, destination)| {
            fields
                .get(*source)
                .map(|value| ((*destination).to_owned(), value.clone()))
        })
        .collect()
}

fn account<'a>(
    instruction: &'a DecodedInstructionV1,
    index: usize,
) -> Result<&'a str, NormalizeError> {
    instruction
        .accounts
        .get(index)
        .map(String::as_str)
        .ok_or(NormalizeError::MalformedPayload)
}

fn account_delta(
    transaction: &DecodedTransactionV1,
    account: &str,
    direction: DeltaDirection,
    account_use: &BTreeMap<String, u32>,
) -> Result<TokenAmount, NormalizeError> {
    if account_use.get(account).copied().unwrap_or_default() != 1
        || transaction
            .non_target_outer_touched_accounts
            .iter()
            .any(|touched| touched == account)
    {
        return Err(NormalizeError::AmbiguousCustodyDelta);
    }
    let balance = transaction
        .token_balances
        .iter()
        .find(|balance| balance.account == account)
        .ok_or(NormalizeError::MissingTokenBalance)?;
    delta(balance, direction)
}

fn delta(
    balance: &TokenAccountBalanceV1,
    direction: DeltaDirection,
) -> Result<TokenAmount, NormalizeError> {
    let pre = parse_amount(&balance.pre_amount)?;
    let post = parse_amount(&balance.post_amount)?;
    let amount = match direction {
        DeltaDirection::Credit => post
            .checked_sub(pre)
            .ok_or(NormalizeError::MalformedPayload)?,
        DeltaDirection::Debit => pre
            .checked_sub(post)
            .ok_or(NormalizeError::MalformedPayload)?,
    };
    token(amount)
}

fn required_liquidity(
    assertions: &[CanonicalEventV1],
    action: LiquidityAction,
) -> Result<&LiquidityPayloadV1, NormalizeError> {
    assertions
        .iter()
        .find_map(|event| match &event.payload {
            EventPayloadV1::Liquidity(payload) if payload.action == action => Some(payload),
            _ => None,
        })
        .ok_or(NormalizeError::MissingAssertion)
}

fn required_pair_delta(
    assertions: &[CanonicalEventV1],
    collateral: bool,
) -> Result<&PairDeltaPayloadV1, NormalizeError> {
    assertions
        .iter()
        .find_map(|event| match (&event.payload, collateral) {
            (EventPayloadV1::CollateralDelta(payload), true)
            | (EventPayloadV1::DebtDelta(payload), false) => Some(payload),
            _ => None,
        })
        .ok_or(NormalizeError::MissingAssertion)
}

fn variant_body(value: &JsonValue) -> Result<&serde_json::Map<String, JsonValue>, NormalizeError> {
    let outer = value.as_object().ok_or(NormalizeError::MalformedPayload)?;
    if outer.len() != 1 {
        return Err(NormalizeError::MalformedPayload);
    }
    outer
        .values()
        .next()
        .and_then(JsonValue::as_object)
        .ok_or(NormalizeError::MalformedPayload)
}

fn value_at<'a>(
    body: &'a serde_json::Map<String, JsonValue>,
    path: &[&str],
) -> Result<&'a JsonValue, NormalizeError> {
    let mut value = body.get(path[0]).ok_or(NormalizeError::MalformedPayload)?;
    for key in &path[1..] {
        value = value.get(*key).ok_or(NormalizeError::MalformedPayload)?;
    }
    Ok(value)
}

fn amount_field(
    body: &serde_json::Map<String, JsonValue>,
    path: &[&str],
) -> Result<TokenAmount, NormalizeError> {
    amount_value(value_at(body, path)?)
}

fn amount_map(
    body: &serde_json::Map<String, JsonValue>,
    key: &str,
) -> Result<TokenAmount, NormalizeError> {
    amount_value(body.get(key).ok_or(NormalizeError::MalformedPayload)?)
}

fn amount_value(value: &JsonValue) -> Result<TokenAmount, NormalizeError> {
    value
        .as_str()
        .ok_or(NormalizeError::MalformedPayload)?
        .parse::<TokenAmount>()
        .map_err(|_| NormalizeError::AmountOverflow)
}

fn signed_map(
    body: &serde_json::Map<String, JsonValue>,
    key: &str,
) -> Result<SignedTokenAmount, NormalizeError> {
    body.get(key)
        .and_then(JsonValue::as_str)
        .ok_or(NormalizeError::MalformedPayload)?
        .parse::<SignedTokenAmount>()
        .map_err(|_| NormalizeError::AmountOverflow)
}

fn string_map(
    body: &serde_json::Map<String, JsonValue>,
    key: &str,
) -> Result<String, NormalizeError> {
    body.get(key)
        .and_then(JsonValue::as_str)
        .map(str::to_owned)
        .ok_or(NormalizeError::MalformedPayload)
}

fn bool_field(
    body: &serde_json::Map<String, JsonValue>,
    key: &str,
) -> Result<bool, NormalizeError> {
    body.get(key)
        .and_then(JsonValue::as_bool)
        .ok_or(NormalizeError::MalformedPayload)
}

fn u16_map(body: &serde_json::Map<String, JsonValue>, key: &str) -> Result<u16, NormalizeError> {
    parse_u16(
        body.get(key)
            .and_then(JsonValue::as_str)
            .ok_or(NormalizeError::MalformedPayload)?,
    )
}

fn parse_u8(value: &str) -> Result<u8, NormalizeError> {
    value.parse().map_err(|_| NormalizeError::AmountOverflow)
}

fn parse_u16(value: &str) -> Result<u16, NormalizeError> {
    value.parse().map_err(|_| NormalizeError::AmountOverflow)
}

fn parse_amount(value: &TokenAmount) -> Result<u128, NormalizeError> {
    value
        .as_str()
        .parse()
        .map_err(|_| NormalizeError::AmountOverflow)
}

fn parse_signed(value: &SignedTokenAmount) -> Result<i128, NormalizeError> {
    value
        .as_str()
        .parse()
        .map_err(|_| NormalizeError::AmountOverflow)
}

fn token(value: u128) -> Result<TokenAmount, NormalizeError> {
    TokenAmount::new(value.to_string()).map_err(|_| NormalizeError::AmountOverflow)
}

fn sum_amounts(values: &[&TokenAmount]) -> Result<TokenAmount, NormalizeError> {
    let total = values.iter().try_fold(0_u128, |total, value| {
        total
            .checked_add(parse_amount(value)?)
            .ok_or(NormalizeError::AmountOverflow)
    })?;
    token(total)
}

fn required_config<'a>(
    fields: &'a BTreeMap<String, String>,
    key: &str,
) -> Result<&'a str, NormalizeError> {
    fields
        .get(key)
        .map(String::as_str)
        .ok_or(NormalizeError::MalformedPayload)
}

fn flatten_json(
    prefix: &str,
    value: &serde_json::Map<String, JsonValue>,
    output: &mut BTreeMap<String, String>,
) {
    for (key, value) in value {
        if key == "metadata" {
            continue;
        }
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        if let Some(object) = value.as_object() {
            flatten_json(&path, object, output);
        } else if let Some(text) = value.as_str() {
            output.insert(path, text.to_owned());
        } else if let Some(boolean) = value.as_bool() {
            output.insert(path, boolean.to_string());
        } else if !value.is_null() {
            output.insert(path, value.to_string());
        }
    }
}

fn preserve_initialize_optional_args(
    body: &serde_json::Map<String, JsonValue>,
    output: &mut BTreeMap<String, String>,
) {
    if let Some(args) = body.get("args").and_then(JsonValue::as_object) {
        for key in [
            "fixed_cf_bps",
            "target_util_start_bps",
            "target_util_end_bps",
            "rate_half_life_ms",
            "min_rate_bps",
            "max_rate_bps",
            "initial_rate_bps",
        ] {
            if let Some(value) = args.get(key) {
                output.insert(format!("instruction.args.{key}"), value.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        canonical::ChainOrder,
        decode::{
            CpiFrameProvenanceV1, DecodedInstructionClass, InstructionExecutionFrameV1,
            DECODE_SCHEMA_VERSION,
        },
        token_cpi::{TokenSupplyChangeCpiV1, TokenTransferCpiV1, SPL_TOKEN_PROGRAM_ID},
        transfer_fee::TokenProgramKindV1,
    };

    fn amount(value: u128) -> TokenAmount {
        TokenAmount::new(value.to_string()).unwrap()
    }

    #[test]
    fn initialize_preserves_absent_options_as_explicit_nulls() {
        let body = serde_json::json!({
            "args": {
                "fixed_cf_bps": null,
                "target_util_start_bps": 3_000,
                "target_util_end_bps": null,
                "rate_half_life_ms": null,
                "min_rate_bps": null,
                "max_rate_bps": 1_000,
                "initial_rate_bps": null
            }
        });
        let mut fields = BTreeMap::new();
        preserve_initialize_optional_args(body.as_object().unwrap(), &mut fields);
        assert_eq!(fields["instruction.args.fixed_cf_bps"], "null");
        assert_eq!(fields["instruction.args.initial_rate_bps"], "null");
        assert_eq!(fields["instruction.args.target_util_start_bps"], "3000");
        assert_eq!(fields["instruction.args.max_rate_bps"], "1000");
    }

    #[test]
    fn pair_created_pubkeys_reach_canonical_metadata_and_config_as_base58() {
        let signer = solana_pubkey::Pubkey::new_from_array([51; 32]).to_string();
        let pair = solana_pubkey::Pubkey::new_from_array([52; 32]).to_string();
        let token0 = solana_pubkey::Pubkey::new_from_array([53; 32]).to_string();
        let token1 = solana_pubkey::Pubkey::new_from_array([54; 32]).to_string();
        let lp_mint = solana_pubkey::Pubkey::new_from_array([55; 32]).to_string();
        let rate_model = solana_pubkey::Pubkey::new_from_array([56; 32]).to_string();
        let instruction = DecodedInstructionV1 {
            schema_version: crate::decode::DECODE_SCHEMA_VERSION,
            decoder_version_id: "decoder".to_owned(),
            signature: "signature".to_owned(),
            order: ChainOrder {
                slot: 10,
                transaction_index: 0,
                outer_instruction_index: 0,
                inner_execution_index: Some(0),
                event_sequence: 0,
            },
            block_time_unix_seconds: 100,
            class: DecodedInstructionClass::EmittedAssertion,
            kind: "pair_created_event".to_owned(),
            data: serde_json::json!({
                "PairCreatedEvent": {
                    "token0": token0,
                    "token1": token1,
                    "lp_mint": lp_mint,
                    "rate_model": rate_model,
                    "params_hash": [0, 1, 2, 3],
                    "metadata": {
                        "signer": signer,
                        "pair": pair,
                        "slot": "10",
                    }
                }
            }),
            accounts: Vec::new(),
            raw_data_sha256: "payload-hash".to_owned(),
            stack_height: Some(2),
        };
        let event =
            canonical_event(&instruction, vec![0, 0], "mainnet-beta", "program", None).unwrap();
        assert_eq!(event.pair.as_deref(), Some(pair.as_str()));
        assert_eq!(event.actor.as_deref(), Some(signer.as_str()));
        let EventPayloadV1::Config(fields) = event.payload else {
            panic!("pair-created assertion must normalize as config evidence");
        };
        assert_eq!(fields["token0"], token0);
        assert_eq!(fields["token1"], token1);
        assert_eq!(fields["lp_mint"], lp_mint);
        assert_eq!(fields["rate_model"], rate_model);
        assert_eq!(fields["params_hash"], "[0,1,2,3]");
    }

    #[test]
    fn transaction_balance_delta_preserves_transfer_fee() {
        let balance = TokenAccountBalanceV1 {
            account_index: 0,
            account: "vault".to_owned(),
            mint: "mint".to_owned(),
            token_program: crate::transfer_fee::TokenProgramKindV1::LegacyToken,
            owner: None,
            decimals: 6,
            pre_amount: amount(1_000),
            post_amount: amount(1_990),
        };
        assert_eq!(
            delta(&balance, DeltaDirection::Credit).unwrap(),
            amount(990)
        );
    }

    #[test]
    fn same_account_in_two_state_transitions_is_not_silently_allocated() {
        let order = ChainOrder {
            slot: 1,
            transaction_index: 0,
            outer_instruction_index: 0,
            inner_execution_index: None,
            event_sequence: 0,
        };
        let instruction = DecodedInstructionV1 {
            schema_version: 1,
            decoder_version_id: "decoder".to_owned(),
            signature: "sig".to_owned(),
            order,
            block_time_unix_seconds: 1,
            class: DecodedInstructionClass::StateChanging,
            kind: "swap".to_owned(),
            data: serde_json::json!({"Swap": {"args": {"amount_in": "100", "min_amount_out": "1"}}}),
            accounts: vec!["vault".to_owned()],
            raw_data_sha256: "hash".to_owned(),
            stack_height: None,
        };
        let counts = state_changing_account_use(&[instruction.clone(), instruction]);
        let transaction = DecodedTransactionV1 {
            schema_version: 1,
            decoder_version_id: "decoder".to_owned(),
            signature: "sig".to_owned(),
            rpc_payload_sha256: "rpc".to_owned(),
            slot: 1,
            transaction_index: 0,
            block_time_unix_seconds: 1,
            program_instruction_count: 2,
            state_changing_instruction_count: 2,
            emitted_assertion_count: 0,
            unknown_program_instruction_hashes: Vec::new(),
            non_target_outer_touched_accounts: Vec::new(),
            token_balances: vec![TokenAccountBalanceV1 {
                account_index: 0,
                account: "vault".to_owned(),
                mint: "mint".to_owned(),
                token_program: crate::transfer_fee::TokenProgramKindV1::LegacyToken,
                owner: None,
                decimals: 6,
                pre_amount: amount(0),
                post_amount: amount(100),
            }],
            instructions: Vec::new(),
        };
        assert_eq!(
            account_delta(&transaction, "vault", DeltaDirection::Credit, &counts),
            Err(NormalizeError::AmbiguousCustodyDelta)
        );
    }

    fn exact_test_order(inner_execution_index: Option<u32>) -> ChainOrder {
        ChainOrder {
            slot: 99,
            transaction_index: 3,
            outer_instruction_index: 0,
            inner_execution_index,
            event_sequence: 0,
        }
    }

    fn exact_test_frame(
        order: ChainOrder,
        program_id: String,
        is_target_program: bool,
        stack_height: u32,
        parent_order: Option<ChainOrder>,
        execution_path: Vec<u32>,
    ) -> InstructionExecutionFrameV1 {
        InstructionExecutionFrameV1 {
            schema_version: DECODE_SCHEMA_VERSION,
            order,
            program_id,
            is_target_program,
            stack_height: Some(stack_height),
            parent_order,
            execution_path,
            provenance: CpiFrameProvenanceV1::Complete,
            decoded_target_class: is_target_program
                .then_some(DecodedInstructionClass::StateChanging),
        }
    }

    fn exact_test_instruction(order: ChainOrder) -> DecodedInstructionV1 {
        DecodedInstructionV1 {
            schema_version: DECODE_SCHEMA_VERSION,
            decoder_version_id: "decoder".to_owned(),
            signature: "signature".to_owned(),
            order,
            block_time_unix_seconds: 1_700_000_000,
            class: DecodedInstructionClass::StateChanging,
            kind: "swap".to_owned(),
            data: serde_json::json!({"Swap": {"args": {"amount_in": "100"}}}),
            accounts: Vec::new(),
            raw_data_sha256: "aa".repeat(32),
            stack_height: Some(if order.inner_execution_index.is_some() {
                2
            } else {
                1
            }),
        }
    }

    fn exact_test_transaction(token_balances: Vec<TokenAccountBalanceV1>) -> DecodedTransactionV1 {
        DecodedTransactionV1 {
            schema_version: DECODE_SCHEMA_VERSION,
            decoder_version_id: "decoder".to_owned(),
            signature: "signature".to_owned(),
            rpc_payload_sha256: "bb".repeat(32),
            slot: 99,
            transaction_index: 3,
            block_time_unix_seconds: 1_700_000_000,
            program_instruction_count: 1,
            state_changing_instruction_count: 1,
            emitted_assertion_count: 0,
            unknown_program_instruction_hashes: Vec::new(),
            non_target_outer_touched_accounts: Vec::new(),
            token_balances,
            instructions: Vec::new(),
        }
    }

    #[test]
    fn routed_checked_swap_accepts_closed_temporary_destination() {
        let router_order = exact_test_order(None);
        let target_order = exact_test_order(Some(0));
        let transfer_order = exact_test_order(Some(1));
        let source = solana_pubkey::Pubkey::new_from_array([61; 32]).to_string();
        let temporary_destination = solana_pubkey::Pubkey::new_from_array([62; 32]).to_string();
        let mint = solana_pubkey::Pubkey::new_from_array([63; 32]).to_string();
        let authority = solana_pubkey::Pubkey::new_from_array([64; 32]).to_string();
        let parsed = ParsedTokenCpiV1::Transfer {
            evidence: TokenTransferCpiV1 {
                schema_version: TOKEN_CPI_SCHEMA_VERSION,
                order: transfer_order,
                token_program: TokenProgramKindV1::LegacyToken,
                variant: TokenTransferVariantV1::LegacyTransferChecked,
                source_token_account: source.clone(),
                destination_token_account: temporary_destination.clone(),
                mint: Some(mint.clone()),
                authority: authority.clone(),
                ordered_accounts: vec![
                    source.clone(),
                    mint.clone(),
                    temporary_destination.clone(),
                    authority,
                ],
                gross_amount: amount(100),
                decimals: Some(6),
                explicit_fee: None,
                raw_data_sha256: "cc".repeat(32),
            },
        };
        let provenance = TransactionExecutionProvenanceV1 {
            schema_version: DECODE_SCHEMA_VERSION,
            decoder_version_id: "decoder".to_owned(),
            signature: "signature".to_owned(),
            slot: 99,
            transaction_index: 3,
            frames: vec![
                exact_test_frame(
                    router_order,
                    solana_pubkey::Pubkey::new_from_array([65; 32]).to_string(),
                    false,
                    1,
                    None,
                    vec![0],
                ),
                exact_test_frame(
                    target_order,
                    solana_pubkey::Pubkey::new_from_array([66; 32]).to_string(),
                    true,
                    2,
                    Some(router_order),
                    vec![0, 0],
                ),
                exact_test_frame(
                    transfer_order,
                    SPL_TOKEN_PROGRAM_ID.to_owned(),
                    false,
                    3,
                    Some(target_order),
                    vec![0, 0, 0],
                ),
            ],
            token_cpis: vec![parsed],
        };
        let transaction = exact_test_transaction(vec![TokenAccountBalanceV1 {
            account_index: 0,
            account: source.clone(),
            mint,
            token_program: TokenProgramKindV1::LegacyToken,
            owner: None,
            decimals: 6,
            pre_amount: amount(1_000),
            post_amount: amount(900),
        }]);

        let exact = exact_operation_token_deltas(
            &transaction,
            &provenance,
            &exact_test_instruction(target_order),
        )
        .unwrap();
        assert_eq!(exact.matched_transfers.len(), 1);
        assert_eq!(exact.matched_transfers[0].gross_amount, amount(100));
        assert_eq!(
            exact
                .account_deltas
                .iter()
                .find(|row| row.token_account == temporary_destination)
                .unwrap()
                .signed_amount,
            SignedTokenAmount::new("100").unwrap()
        );
    }

    #[test]
    fn checked_supply_change_accepts_absent_closed_token_account() {
        let target_order = exact_test_order(None);
        let mint_order = exact_test_order(Some(0));
        let token_account = solana_pubkey::Pubkey::new_from_array([71; 32]).to_string();
        let mint = solana_pubkey::Pubkey::new_from_array([72; 32]).to_string();
        let authority = solana_pubkey::Pubkey::new_from_array([73; 32]).to_string();
        let parsed = ParsedTokenCpiV1::SupplyChange {
            evidence: TokenSupplyChangeCpiV1 {
                schema_version: TOKEN_CPI_SCHEMA_VERSION,
                order: mint_order,
                token_program: TokenProgramKindV1::LegacyToken,
                variant: TokenSupplyChangeVariantV1::LegacyMintToChecked,
                kind: TokenSupplyChangeKindV1::Mint,
                token_account: token_account.clone(),
                mint: mint.clone(),
                authority: authority.clone(),
                ordered_accounts: vec![mint, token_account.clone(), authority],
                amount: amount(50),
                decimals: Some(6),
                raw_data_sha256: "dd".repeat(32),
            },
        };
        let provenance = TransactionExecutionProvenanceV1 {
            schema_version: DECODE_SCHEMA_VERSION,
            decoder_version_id: "decoder".to_owned(),
            signature: "signature".to_owned(),
            slot: 99,
            transaction_index: 3,
            frames: vec![
                exact_test_frame(
                    target_order,
                    solana_pubkey::Pubkey::new_from_array([74; 32]).to_string(),
                    true,
                    1,
                    None,
                    vec![0],
                ),
                exact_test_frame(
                    mint_order,
                    SPL_TOKEN_PROGRAM_ID.to_owned(),
                    false,
                    2,
                    Some(target_order),
                    vec![0, 0],
                ),
            ],
            token_cpis: vec![parsed],
        };

        let exact = exact_operation_token_deltas(
            &exact_test_transaction(Vec::new()),
            &provenance,
            &exact_test_instruction(target_order),
        )
        .unwrap();
        assert_eq!(exact.matched_supply_changes.len(), 1);
        assert_eq!(exact.account_deltas.len(), 1);
        assert_eq!(exact.account_deltas[0].token_account, token_account);
        assert_eq!(
            exact.account_deltas[0].signed_amount,
            SignedTokenAmount::new("50").unwrap()
        );
    }

    #[test]
    fn unchecked_transfer_without_any_balance_identity_fails_closed() {
        let target_order = exact_test_order(None);
        let transfer_order = exact_test_order(Some(0));
        let source = solana_pubkey::Pubkey::new_from_array([81; 32]).to_string();
        let destination = solana_pubkey::Pubkey::new_from_array([82; 32]).to_string();
        let authority = solana_pubkey::Pubkey::new_from_array([83; 32]).to_string();
        let parsed = ParsedTokenCpiV1::Transfer {
            evidence: TokenTransferCpiV1 {
                schema_version: TOKEN_CPI_SCHEMA_VERSION,
                order: transfer_order,
                token_program: TokenProgramKindV1::LegacyToken,
                variant: TokenTransferVariantV1::LegacyTransfer,
                source_token_account: source.clone(),
                destination_token_account: destination.clone(),
                mint: None,
                authority: authority.clone(),
                ordered_accounts: vec![source, destination, authority],
                gross_amount: amount(1),
                decimals: None,
                explicit_fee: None,
                raw_data_sha256: "ee".repeat(32),
            },
        };
        let provenance = TransactionExecutionProvenanceV1 {
            schema_version: DECODE_SCHEMA_VERSION,
            decoder_version_id: "decoder".to_owned(),
            signature: "signature".to_owned(),
            slot: 99,
            transaction_index: 3,
            frames: vec![
                exact_test_frame(
                    target_order,
                    solana_pubkey::Pubkey::new_from_array([84; 32]).to_string(),
                    true,
                    1,
                    None,
                    vec![0],
                ),
                exact_test_frame(
                    transfer_order,
                    SPL_TOKEN_PROGRAM_ID.to_owned(),
                    false,
                    2,
                    Some(target_order),
                    vec![0, 0],
                ),
            ],
            token_cpis: vec![parsed],
        };
        assert_eq!(
            exact_operation_token_deltas(
                &exact_test_transaction(Vec::new()),
                &provenance,
                &exact_test_instruction(target_order),
            ),
            Err(NormalizeError::MissingTokenBalance)
        );
    }

    fn exact_zero_effects(order: ChainOrder) -> ExactOperationTokenDeltasV1 {
        ExactOperationTokenDeltasV1 {
            schema_version: TOKEN_CPI_SCHEMA_VERSION,
            target_operation: order,
            matched_transfers: Vec::new(),
            matched_supply_changes: Vec::new(),
            account_deltas: Vec::new(),
        }
    }

    fn assertion(order: ChainOrder, payload: EventPayloadV1) -> CanonicalEventV1 {
        CanonicalEventV1::new(
            "mainnet-beta".to_owned(),
            "program".to_owned(),
            Some("pair".to_owned()),
            Some("actor".to_owned()),
            "signature".to_owned(),
            ChainOrder {
                inner_execution_index: Some(0),
                ..order
            },
            vec![order.outer_instruction_index, 0],
            1_700_000_000,
            "decoder".to_owned(),
            b"assertion",
            None,
            payload,
        )
    }

    #[test]
    fn historical_empty_remove_collateral_is_clock_only() {
        let order = exact_test_order(None);
        let mut instruction = exact_test_instruction(order);
        instruction.kind = "remove_collateral".to_owned();
        instruction.data = serde_json::json!({
            "RemoveCollateral": {"args": {"amount": u64::MAX.to_string()}}
        });
        let assertions = vec![assertion(
            order,
            EventPayloadV1::CollateralDelta(PairDeltaPayloadV1 {
                amount0: SignedTokenAmount::new("0").unwrap(),
                amount1: SignedTokenAmount::new("0").unwrap(),
            }),
        )];
        let payload = build_operation_payload(
            &instruction,
            &exact_test_transaction(Vec::new()),
            &assertions,
            &BTreeMap::new(),
            Some(&exact_zero_effects(order)),
            &mut BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(payload, OperationPayloadV1::ClockOnly);
    }

    #[test]
    fn historical_zero_liquidation_is_clock_only_but_nonzero_shortfall_fails() {
        let order = exact_test_order(None);
        let mut instruction = exact_test_instruction(order);
        instruction.kind = "liquidate".to_owned();
        instruction.data = serde_json::json!({"Liquidate": {}});
        let liquidation = LiquidationPayloadV1 {
            position: "position".to_owned(),
            liquidator: "liquidator".to_owned(),
            collateral0_liquidated: amount(0),
            collateral1_liquidated: amount(0),
            debt0_liquidated: amount(0),
            debt1_liquidated: amount(0),
            collateral_price_nad: amount(1),
            shortfall: amount(0),
            liquidation_bonus: amount(0),
            k_before: amount(7),
            k_after: amount(7),
        };
        let payload = build_operation_payload(
            &instruction,
            &exact_test_transaction(Vec::new()),
            &[assertion(
                order,
                EventPayloadV1::Liquidation(liquidation.clone()),
            )],
            &BTreeMap::new(),
            Some(&exact_zero_effects(order)),
            &mut BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(payload, OperationPayloadV1::ClockOnly);

        let mut invalid = liquidation;
        invalid.shortfall = amount(1);
        assert_eq!(
            build_operation_payload(
                &instruction,
                &exact_test_transaction(Vec::new()),
                &[assertion(order, EventPayloadV1::Liquidation(invalid))],
                &BTreeMap::new(),
                Some(&exact_zero_effects(order)),
                &mut BTreeMap::new(),
            ),
            Err(NormalizeError::MalformedPayload)
        );
    }
}
