//! Exact SPL Token CPI evidence scoped by verified Solana frame ancestry.
//!
//! This module deliberately does not infer transfers from transaction-wide
//! balance deltas. It decodes the raw compiled instruction, binds every role
//! to the resolved account-key order, and only aggregates transfers or supply
//! changes owned by one exact V1 operation.

use std::{
    collections::{BTreeMap, BTreeSet},
    str::FromStr,
};

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use solana_pubkey::Pubkey;
use spl_token_2022::{
    extension::transfer_fee::instruction::TransferFeeInstruction, instruction::TokenInstruction,
};
use thiserror::Error;

use crate::{
    canonical::{sha256_hex, ChainOrder, SignedTokenAmount, TokenAmount},
    decode::{
        CpiFrameProvenanceV1, DecodedInstructionClass, InstructionExecutionFrameV1,
        TransactionExecutionProvenanceV1, DECODE_SCHEMA_VERSION,
    },
    transfer_fee::TokenProgramKindV1,
};

pub const TOKEN_CPI_SCHEMA_VERSION: u16 = 2;
pub const SPL_TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
pub const SPL_TOKEN_2022_PROGRAM_ID: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";
pub const ASSOCIATED_TOKEN_PROGRAM_ID: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";
const NATIVE_MINT_ID: &str = "So11111111111111111111111111111111111111112";

const LEGACY_TRANSFER_DISCRIMINANT: u8 = 3;
const MINT_TO_DISCRIMINANT: u8 = 7;
const BURN_DISCRIMINANT: u8 = 8;
const TRANSFER_CHECKED_DISCRIMINANT: u8 = 12;
const MINT_TO_CHECKED_DISCRIMINANT: u8 = 14;
const BURN_CHECKED_DISCRIMINANT: u8 = 15;
const TRANSFER_FEE_EXTENSION_DISCRIMINANT: u8 = 26;
const TRANSFER_CHECKED_WITH_FEE_DISCRIMINANT: u8 = 1;
const LEGACY_MAX_DISCRIMINANT: u8 = 24;
const TOKEN_2022_MAX_DISCRIMINANT: u8 = 42;
const LEGACY_TRANSFER_WIRE_LEN: usize = 9;
const TRANSFER_CHECKED_WIRE_LEN: usize = 10;
const SUPPLY_CHANGE_WIRE_LEN: usize = 9;
const SUPPLY_CHANGE_CHECKED_WIRE_LEN: usize = 10;
const TRANSFER_CHECKED_WITH_FEE_WIRE_LEN: usize = 19;

/// The only SPL transfer encodings accepted as exact transfer evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenTransferVariantV1 {
    LegacyTransfer,
    LegacyTransferChecked,
    Token2022TransferChecked,
    Token2022TransferCheckedWithFee,
}

/// Raw, instruction-local transfer evidence. `mint` and `decimals` are absent
/// for the legacy unchecked transfer because those values do not exist on its
/// wire. `explicit_fee` exists only for `TransferCheckedWithFee`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenTransferCpiV1 {
    pub schema_version: u16,
    pub order: ChainOrder,
    pub token_program: TokenProgramKindV1,
    pub variant: TokenTransferVariantV1,
    pub source_token_account: String,
    pub destination_token_account: String,
    pub mint: Option<String>,
    pub authority: String,
    /// Preserves the complete compiled account order, including multisig,
    /// transfer-hook, or other remaining accounts.
    pub ordered_accounts: Vec<String>,
    pub gross_amount: TokenAmount,
    pub decimals: Option<u8>,
    pub explicit_fee: Option<TokenAmount>,
    /// SHA-256 of the decoded instruction bytes, not of the base58 text.
    pub raw_data_sha256: String,
}

/// The exact SPL instruction used to change a mint's supply. The token
/// program is part of the variant so evidence cannot be reinterpreted under a
/// different program after it has been persisted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenSupplyChangeVariantV1 {
    LegacyMintTo,
    LegacyMintToChecked,
    LegacyBurn,
    LegacyBurnChecked,
    Token2022MintTo,
    Token2022MintToChecked,
    Token2022Burn,
    Token2022BurnChecked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenSupplyChangeKindV1 {
    Mint,
    Burn,
}

/// Raw, instruction-local mint/burn evidence. `token_account` is the credited
/// destination for mint instructions and the debited source for burns.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenSupplyChangeCpiV1 {
    pub schema_version: u16,
    pub order: ChainOrder,
    pub token_program: TokenProgramKindV1,
    pub variant: TokenSupplyChangeVariantV1,
    pub kind: TokenSupplyChangeKindV1,
    pub token_account: String,
    pub mint: String,
    pub authority: String,
    /// Preserves multisig signers and all other remaining compiled accounts.
    pub ordered_accounts: Vec<String>,
    pub amount: TokenAmount,
    pub decimals: Option<u8>,
    /// SHA-256 of the decoded instruction bytes, not of the base58 text.
    pub raw_data_sha256: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenAccountAdjustmentReasonV1 {
    SyncNative,
    NativeAccountInitialization,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertifiedZeroNativeInitializationV1 {
    pub order: ChainOrder,
    pub associated_token_parent_order: ChainOrder,
    pub raw_data_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TokenAccountAdjustmentSourceV1 {
    /// A successful canonical Associated Token Account creation initializes a
    /// newly created native account at zero spendable token amount. The direct
    /// ATA parent is retained and rechecked against frame ancestry.
    AssociatedTokenAccountCreation { parent_order: ChainOrder },
    /// The adjustment is the sole unresolved custody effect on this account in
    /// the transaction. Raw RPC pre/post balances and every other exact token
    /// effect form a closed arithmetic identity.
    RpcTokenBalanceResidual {
        transaction_pre_amount: TokenAmount,
        transaction_post_amount: TokenAmount,
        other_exact_delta: SignedTokenAmount,
        certified_zero_external_cpis: Vec<CertifiedZeroNativeInitializationV1>,
    },
}

/// Exact non-transfer adjustment to one token account. This is deliberately
/// separate from ordinary zero-effect instructions: bare `SyncNative` and
/// native-account initialization remain blocking until the decoder supplies
/// one of the typed, locally verifiable proofs above.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExactTokenAccountAdjustmentV1 {
    pub reason: TokenAccountAdjustmentReasonV1,
    pub source: TokenAccountAdjustmentSourceV1,
    pub token_account: String,
    pub mint: String,
    pub decimals: u8,
    pub signed_amount: SignedTokenAmount,
}

/// A strictly decoded instruction whose custody effect cannot be determined
/// from its wire alone. This type is never persisted in canonical history; it
/// must be resolved to an `ExactTokenAccountAdjustmentV1` first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeferredTokenAccountAdjustmentCpiV1 {
    pub order: ChainOrder,
    pub token_program: TokenProgramKindV1,
    pub reason: TokenAccountAdjustmentReasonV1,
    pub token_account: String,
    pub mint: String,
    pub top_level_discriminant: u8,
    pub ordered_accounts: Vec<String>,
    pub raw_data_sha256: String,
}

/// A canonically decoded SPL instruction that is not a transfer or mint/burn.
/// Most variants have a proven zero custody-amount delta. The two explicitly
/// typed native-account cases may instead carry an exact decoder-certified
/// account adjustment; all other amount-moving/state-dependent variants remain
/// blocking parse errors.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecognizedNonTransferTokenCpiV1 {
    pub schema_version: u16,
    pub order: ChainOrder,
    pub token_program: TokenProgramKindV1,
    pub top_level_discriminant: u8,
    pub extension_discriminant: Option<u8>,
    pub ordered_accounts: Vec<String>,
    pub raw_data_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact_account_adjustment: Option<ExactTokenAccountAdjustmentV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "classification", rename_all = "snake_case")]
pub enum ParsedTokenCpiV1 {
    Transfer {
        evidence: TokenTransferCpiV1,
    },
    SupplyChange {
        evidence: TokenSupplyChangeCpiV1,
    },
    RecognizedNonTransfer {
        evidence: RecognizedNonTransferTokenCpiV1,
    },
}

impl ParsedTokenCpiV1 {
    pub fn order(&self) -> ChainOrder {
        match self {
            Self::Transfer { evidence } => evidence.order,
            Self::SupplyChange { evidence } => evidence.order,
            Self::RecognizedNonTransfer { evidence } => evidence.order,
        }
    }

    pub fn token_program(&self) -> TokenProgramKindV1 {
        match self {
            Self::Transfer { evidence } => evidence.token_program,
            Self::SupplyChange { evidence } => evidence.token_program,
            Self::RecognizedNonTransfer { evidence } => evidence.token_program,
        }
    }

    pub fn schema_version(&self) -> u16 {
        match self {
            Self::Transfer { evidence } => evidence.schema_version,
            Self::SupplyChange { evidence } => evidence.schema_version,
            Self::RecognizedNonTransfer { evidence } => evidence.schema_version,
        }
    }

    pub fn raw_data_sha256(&self) -> &str {
        match self {
            Self::Transfer { evidence } => &evidence.raw_data_sha256,
            Self::SupplyChange { evidence } => &evidence.raw_data_sha256,
            Self::RecognizedNonTransfer { evidence } => &evidence.raw_data_sha256,
        }
    }
}

/// Independent facts expected for one transfer call. A checked instruction
/// supplies its mint on-wire; an unchecked legacy instruction instead binds
/// the mint from this independently certified record. Likewise, a Token-2022
/// `TransferChecked` requires `resolved_fee` (including an exact zero) because
/// its assessed fee is not encoded in the instruction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedTokenTransferV1 {
    pub schema_version: u16,
    pub transfer_order: ChainOrder,
    pub token_program: TokenProgramKindV1,
    pub source_token_account: String,
    pub destination_token_account: String,
    pub mint: String,
    pub authority: String,
    pub expected_gross_amount: Option<TokenAmount>,
    pub expected_decimals: Option<u8>,
    pub resolved_fee: Option<TokenAmount>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchedTokenTransferV1 {
    pub schema_version: u16,
    pub transfer_order: ChainOrder,
    pub token_program: TokenProgramKindV1,
    pub variant: TokenTransferVariantV1,
    pub source_token_account: String,
    pub destination_token_account: String,
    pub mint: String,
    pub authority: String,
    pub gross_amount: TokenAmount,
    pub decimals: Option<u8>,
    pub transfer_fee: TokenAmount,
    pub destination_credit: TokenAmount,
    pub raw_data_sha256: String,
}

/// Independent expected facts for one supply-changing CPI. Account effects
/// are not applied until every supplied field agrees with the decoded wire.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedTokenSupplyChangeV1 {
    pub schema_version: u16,
    pub supply_change_order: ChainOrder,
    pub token_program: TokenProgramKindV1,
    pub kind: TokenSupplyChangeKindV1,
    pub token_account: String,
    pub mint: String,
    pub authority: String,
    pub expected_amount: Option<TokenAmount>,
    pub expected_decimals: Option<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchedTokenSupplyChangeV1 {
    pub schema_version: u16,
    pub supply_change_order: ChainOrder,
    pub token_program: TokenProgramKindV1,
    pub variant: TokenSupplyChangeVariantV1,
    pub kind: TokenSupplyChangeKindV1,
    pub token_account: String,
    pub mint: String,
    pub authority: String,
    pub amount: TokenAmount,
    pub decimals: Option<u8>,
    pub raw_data_sha256: String,
}

/// Signed delta of the SPL account's spendable/base token amount. A
/// Token-2022 withheld fee is recorded on the corresponding matched transfer
/// and is deliberately not counted as destination spendable credit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExactTokenAccountDeltaV1 {
    pub schema_version: u16,
    pub token_account: String,
    pub token_program: TokenProgramKindV1,
    pub mint: String,
    pub signed_amount: SignedTokenAmount,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExactOperationTokenDeltasV1 {
    pub schema_version: u16,
    pub target_operation: ChainOrder,
    pub matched_transfers: Vec<MatchedTokenTransferV1>,
    pub matched_supply_changes: Vec<MatchedTokenSupplyChangeV1>,
    pub account_deltas: Vec<ExactTokenAccountDeltaV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum TokenCpiError {
    #[error("instruction order must identify an instruction, not an emitted event")]
    InvalidInstructionOrder,
    #[error("raw instruction is not a compiled instruction object")]
    InvalidCompiledInstruction,
    #[error("compiled instruction field `{0}` is missing or malformed")]
    InvalidCompiledField(&'static str),
    #[error("compiled account index {0} is outside the resolved account-key vector")]
    AccountIndexOutOfBounds(u64),
    #[error("compiled account index {0} exceeds Solana's u8 index domain")]
    AccountIndexOverflow(u64),
    #[error("account key is not a canonical Solana pubkey: {0}")]
    InvalidAccountKey(String),
    #[error("instruction is not addressed to an accepted SPL Token program: {0}")]
    UnsupportedProgram(String),
    #[error("instruction data is not canonical base58")]
    InvalidBase58,
    #[error("instruction data is empty")]
    EmptyInstructionData,
    #[error("unknown instruction discriminant {discriminant} for {program:?}")]
    UnknownInstruction {
        program: TokenProgramKindV1,
        discriminant: u8,
    },
    #[error("malformed instruction discriminant {discriminant} for {program:?}")]
    MalformedInstruction {
        program: TokenProgramKindV1,
        discriminant: u8,
    },
    #[error("non-canonical instruction encoding for discriminant {discriminant} in {program:?}")]
    NonCanonicalInstructionEncoding {
        program: TokenProgramKindV1,
        discriminant: u8,
    },
    #[error("unsupported value-moving instruction discriminant {discriminant} in {program:?}")]
    UnsupportedValueMovement {
        program: TokenProgramKindV1,
        discriminant: u8,
    },
    #[error(
        "instruction discriminant {discriminant} in {program:?} needs external account-state proof"
    )]
    RequiresExternalCustodyState {
        program: TokenProgramKindV1,
        discriminant: u8,
    },
    #[error("exact account-adjustment proof is invalid at {0:?}")]
    InvalidAccountAdjustment(ChainOrder),
    #[error("unsupported Token-2022 extension prefix {0}")]
    UnsupportedToken2022Extension(u8),
    #[error("transfer instruction has {actual} accounts; at least {minimum} are required")]
    TooFewTransferAccounts { minimum: usize, actual: usize },
    #[error("supply-change instruction has {actual} accounts; at least {minimum} are required")]
    TooFewSupplyChangeAccounts { minimum: usize, actual: usize },
    #[error("record schema {actual} does not match expected schema {expected}")]
    SchemaVersion { expected: u16, actual: u16 },
    #[error("frame ancestry is incomplete")]
    IncompleteFrameAncestry,
    #[error("frame ancestry is malformed at {0:?}")]
    MalformedFrameAncestry(ChainOrder),
    #[error("duplicate frame order {0:?}")]
    DuplicateFrame(ChainOrder),
    #[error("duplicate parsed token instruction order {0:?}")]
    DuplicateParsedInstruction(ChainOrder),
    #[error("duplicate expected transfer order {0:?}")]
    DuplicateExpectedTransfer(ChainOrder),
    #[error("duplicate expected supply-change order {0:?}")]
    DuplicateExpectedSupplyChange(ChainOrder),
    #[error("transfer and supply-change expectations collide at {0:?}")]
    ConflictingExpectedInstructionKind(ChainOrder),
    #[error("target operation frame is missing or is not a state-changing V1 instruction")]
    InvalidTargetOperation,
    #[error("parsed token instruction has no verified frame at {0:?}")]
    ParsedInstructionMissingFrame(ChainOrder),
    #[error("parsed token program conflicts with its verified frame at {0:?}")]
    ParsedProgramConflict(ChainOrder),
    #[error("token instruction in the target subtree was not parsed at {0:?}")]
    MissingParsedTokenInstruction(ChainOrder),
    #[error("observed transfer has no exact expectation at {0:?}")]
    UnmatchedObservedTransfer(ChainOrder),
    #[error("expected transfer has no observed transfer in target subtree at {0:?}")]
    UnmatchedExpectedTransfer(ChainOrder),
    #[error("expected transfer is outside the target operation subtree at {0:?}")]
    ExpectedTransferOutsideSubtree(ChainOrder),
    #[error("observed supply change has no exact expectation at {0:?}")]
    UnmatchedObservedSupplyChange(ChainOrder),
    #[error("expected supply change has no observed instruction in target subtree at {0:?}")]
    UnmatchedExpectedSupplyChange(ChainOrder),
    #[error("expected supply change is outside the target operation subtree at {0:?}")]
    ExpectedSupplyChangeOutsideSubtree(ChainOrder),
    #[error("transfer program conflicts with expectation at {0:?}")]
    ConflictingProgram(ChainOrder),
    #[error("transfer account roles conflict with expectation at {0:?}")]
    ConflictingAccounts(ChainOrder),
    #[error("transfer mint conflicts with expectation at {0:?}")]
    ConflictingMint(ChainOrder),
    #[error("transfer authority conflicts with expectation at {0:?}")]
    ConflictingAuthority(ChainOrder),
    #[error("transfer amount conflicts with expectation at {0:?}")]
    ConflictingAmount(ChainOrder),
    #[error("transfer decimals conflict with expectation at {0:?}")]
    ConflictingDecimals(ChainOrder),
    #[error("supply-change direction conflicts with expectation at {0:?}")]
    ConflictingSupplyChangeKind(ChainOrder),
    #[error("transfer fee evidence is missing at {0:?}")]
    MissingFeeEvidence(ChainOrder),
    #[error("transfer fee conflicts with expectation at {0:?}")]
    ConflictingFee(ChainOrder),
    #[error("transfer fee exceeds gross amount at {0:?}")]
    FeeExceedsGrossAmount(ChainOrder),
    #[error("token account {account} is bound to conflicting mint or program identities")]
    ConflictingAccountIdentity { account: String },
    #[error("token amount does not fit the SPL u64 wire domain")]
    InvalidTokenAmount,
    #[error("signed account-delta accumulation overflowed")]
    DeltaOverflow,
    #[error("raw-data SHA-256 is malformed")]
    InvalidRawDataHash,
}

/// Parse one raw compiled SPL instruction against the exact resolved account
/// key vector (`static`, then loaded writable, then loaded readonly).
pub fn parse_compiled_token_instruction_v1(
    order: ChainOrder,
    raw_instruction: &JsonValue,
    account_keys: &[String],
) -> Result<ParsedTokenCpiV1, TokenCpiError> {
    if order.event_sequence != 0 {
        return Err(TokenCpiError::InvalidInstructionOrder);
    }
    let raw = raw_instruction
        .as_object()
        .ok_or(TokenCpiError::InvalidCompiledInstruction)?;
    let program_index = compiled_index(raw.get("programIdIndex"), "programIdIndex")?;
    let program_id = resolve_account_key(program_index, account_keys)?;
    let token_program = token_program(program_id)?;
    let raw_accounts = raw
        .get("accounts")
        .and_then(JsonValue::as_array)
        .ok_or(TokenCpiError::InvalidCompiledField("accounts"))?;
    let ordered_accounts = raw_accounts
        .iter()
        .map(|value| {
            compiled_index(Some(value), "accounts")
                .and_then(|index| resolve_account_key(index, account_keys).map(str::to_owned))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let encoded_data = raw
        .get("data")
        .and_then(JsonValue::as_str)
        .ok_or(TokenCpiError::InvalidCompiledField("data"))?;
    let data = bs58::decode(encoded_data)
        .into_vec()
        .map_err(|_| TokenCpiError::InvalidBase58)?;
    if data.is_empty() {
        return Err(TokenCpiError::EmptyInstructionData);
    }
    if bs58::encode(&data).into_string() != encoded_data {
        return Err(TokenCpiError::InvalidBase58);
    }
    let raw_data_sha256 = sha256_hex(&data);
    match token_program {
        TokenProgramKindV1::LegacyToken => {
            parse_legacy_instruction(order, ordered_accounts, &data, raw_data_sha256)
        }
        TokenProgramKindV1::Token2022 => {
            parse_token_2022_instruction(order, ordered_accounts, &data, raw_data_sha256)
        }
    }
}

/// Decode only the two native-account instructions that require transaction
/// state to determine their custody effect. Callers must not persist the
/// returned candidate; use `resolve_deferred_token_account_adjustment_v1`
/// after constructing an exact proof.
pub fn parse_deferred_token_account_adjustment_v1(
    order: ChainOrder,
    raw_instruction: &JsonValue,
    account_keys: &[String],
) -> Result<DeferredTokenAccountAdjustmentCpiV1, TokenCpiError> {
    if order.event_sequence != 0 {
        return Err(TokenCpiError::InvalidInstructionOrder);
    }
    let raw = raw_instruction
        .as_object()
        .ok_or(TokenCpiError::InvalidCompiledInstruction)?;
    let program_index = compiled_index(raw.get("programIdIndex"), "programIdIndex")?;
    let program_id = resolve_account_key(program_index, account_keys)?;
    let token_program = token_program(program_id)?;
    let raw_accounts = raw
        .get("accounts")
        .and_then(JsonValue::as_array)
        .ok_or(TokenCpiError::InvalidCompiledField("accounts"))?;
    let ordered_accounts = raw_accounts
        .iter()
        .map(|value| {
            compiled_index(Some(value), "accounts")
                .and_then(|index| resolve_account_key(index, account_keys).map(str::to_owned))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let encoded_data = raw
        .get("data")
        .and_then(JsonValue::as_str)
        .ok_or(TokenCpiError::InvalidCompiledField("data"))?;
    let data = bs58::decode(encoded_data)
        .into_vec()
        .map_err(|_| TokenCpiError::InvalidBase58)?;
    if data.is_empty() {
        return Err(TokenCpiError::EmptyInstructionData);
    }
    if bs58::encode(&data).into_string() != encoded_data {
        return Err(TokenCpiError::InvalidBase58);
    }
    let discriminant = data[0];
    let decoded =
        TokenInstruction::unpack(&data).map_err(|_| malformed(token_program, discriminant))?;
    if decoded.pack() != data {
        return Err(TokenCpiError::NonCanonicalInstructionEncoding {
            program: token_program,
            discriminant,
        });
    }
    if token_program != TokenProgramKindV1::LegacyToken {
        return Err(TokenCpiError::RequiresExternalCustodyState {
            program: token_program,
            discriminant,
        });
    }
    let (reason, token_account, mint) = match decoded {
        TokenInstruction::SyncNative if ordered_accounts.len() == 1 => (
            TokenAccountAdjustmentReasonV1::SyncNative,
            ordered_accounts[0].clone(),
            NATIVE_MINT_ID.to_owned(),
        ),
        TokenInstruction::InitializeAccount
        | TokenInstruction::InitializeAccount2 { .. }
        | TokenInstruction::InitializeAccount3 { .. }
            if ordered_accounts.len() >= 2 && ordered_accounts[1].as_str() == NATIVE_MINT_ID =>
        {
            (
                TokenAccountAdjustmentReasonV1::NativeAccountInitialization,
                ordered_accounts[0].clone(),
                ordered_accounts[1].clone(),
            )
        }
        _ => {
            return Err(TokenCpiError::RequiresExternalCustodyState {
                program: token_program,
                discriminant,
            })
        }
    };
    Ok(DeferredTokenAccountAdjustmentCpiV1 {
        order,
        token_program,
        reason,
        token_account,
        mint,
        top_level_discriminant: discriminant,
        ordered_accounts,
        raw_data_sha256: sha256_hex(&data),
    })
}

/// Bind a deferred native-account instruction to exact local state evidence.
/// The returned record is accepted by aggregation only if its arithmetic and
/// frame proof remain valid.
pub fn resolve_deferred_token_account_adjustment_v1(
    candidate: DeferredTokenAccountAdjustmentCpiV1,
    adjustment: ExactTokenAccountAdjustmentV1,
) -> Result<ParsedTokenCpiV1, TokenCpiError> {
    if candidate.reason != adjustment.reason
        || candidate.token_account != adjustment.token_account
        || candidate.mint != adjustment.mint
    {
        return Err(TokenCpiError::InvalidAccountAdjustment(candidate.order));
    }
    let evidence = RecognizedNonTransferTokenCpiV1 {
        schema_version: TOKEN_CPI_SCHEMA_VERSION,
        order: candidate.order,
        token_program: candidate.token_program,
        top_level_discriminant: candidate.top_level_discriminant,
        extension_discriminant: None,
        ordered_accounts: candidate.ordered_accounts,
        raw_data_sha256: candidate.raw_data_sha256,
        exact_account_adjustment: Some(adjustment),
    };
    validate_recognized_non_transfer(&evidence)?;
    Ok(ParsedTokenCpiV1::RecognizedNonTransfer { evidence })
}

fn parse_legacy_instruction(
    order: ChainOrder,
    accounts: Vec<String>,
    data: &[u8],
    raw_data_sha256: String,
) -> Result<ParsedTokenCpiV1, TokenCpiError> {
    let discriminant = data[0];
    match discriminant {
        LEGACY_TRANSFER_DISCRIMINANT => {
            require_exact_len(
                data,
                LEGACY_TRANSFER_WIRE_LEN,
                TokenProgramKindV1::LegacyToken,
                discriminant,
            )?;
            #[allow(deprecated)]
            let amount = match TokenInstruction::unpack(data)
                .map_err(|_| malformed(TokenProgramKindV1::LegacyToken, discriminant))?
            {
                TokenInstruction::Transfer { amount } => amount,
                _ => return Err(malformed(TokenProgramKindV1::LegacyToken, discriminant)),
            };
            transfer_result(
                order,
                TokenProgramKindV1::LegacyToken,
                TokenTransferVariantV1::LegacyTransfer,
                accounts,
                amount,
                None,
                None,
                raw_data_sha256,
            )
        }
        TRANSFER_CHECKED_DISCRIMINANT => {
            require_exact_len(
                data,
                TRANSFER_CHECKED_WIRE_LEN,
                TokenProgramKindV1::LegacyToken,
                discriminant,
            )?;
            let (amount, decimals) = match TokenInstruction::unpack(data)
                .map_err(|_| malformed(TokenProgramKindV1::LegacyToken, discriminant))?
            {
                TokenInstruction::TransferChecked { amount, decimals } => (amount, decimals),
                _ => return Err(malformed(TokenProgramKindV1::LegacyToken, discriminant)),
            };
            transfer_result(
                order,
                TokenProgramKindV1::LegacyToken,
                TokenTransferVariantV1::LegacyTransferChecked,
                accounts,
                amount,
                Some(decimals),
                None,
                raw_data_sha256,
            )
        }
        MINT_TO_DISCRIMINANT
        | BURN_DISCRIMINANT
        | MINT_TO_CHECKED_DISCRIMINANT
        | BURN_CHECKED_DISCRIMINANT => parse_supply_change_instruction(
            order,
            TokenProgramKindV1::LegacyToken,
            accounts,
            data,
            raw_data_sha256,
        ),
        0..=LEGACY_MAX_DISCRIMINANT => recognized_base_instruction(
            order,
            TokenProgramKindV1::LegacyToken,
            accounts,
            data,
            raw_data_sha256,
        ),
        _ => Err(TokenCpiError::UnknownInstruction {
            program: TokenProgramKindV1::LegacyToken,
            discriminant,
        }),
    }
}

fn parse_token_2022_instruction(
    order: ChainOrder,
    accounts: Vec<String>,
    data: &[u8],
    raw_data_sha256: String,
) -> Result<ParsedTokenCpiV1, TokenCpiError> {
    let discriminant = data[0];
    match discriminant {
        LEGACY_TRANSFER_DISCRIMINANT => Err(TokenCpiError::UnsupportedValueMovement {
            program: TokenProgramKindV1::Token2022,
            discriminant,
        }),
        TRANSFER_CHECKED_DISCRIMINANT => {
            require_exact_len(
                data,
                TRANSFER_CHECKED_WIRE_LEN,
                TokenProgramKindV1::Token2022,
                discriminant,
            )?;
            let (amount, decimals) = match TokenInstruction::unpack(data)
                .map_err(|_| malformed(TokenProgramKindV1::Token2022, discriminant))?
            {
                TokenInstruction::TransferChecked { amount, decimals } => (amount, decimals),
                _ => return Err(malformed(TokenProgramKindV1::Token2022, discriminant)),
            };
            transfer_result(
                order,
                TokenProgramKindV1::Token2022,
                TokenTransferVariantV1::Token2022TransferChecked,
                accounts,
                amount,
                Some(decimals),
                None,
                raw_data_sha256,
            )
        }
        MINT_TO_DISCRIMINANT
        | BURN_DISCRIMINANT
        | MINT_TO_CHECKED_DISCRIMINANT
        | BURN_CHECKED_DISCRIMINANT => parse_supply_change_instruction(
            order,
            TokenProgramKindV1::Token2022,
            accounts,
            data,
            raw_data_sha256,
        ),
        TRANSFER_FEE_EXTENSION_DISCRIMINANT => {
            parse_transfer_fee_extension(order, accounts, data, raw_data_sha256)
        }
        27 | 28 | 30 | 33 | 34 | 36 | 37 | 39 | 40 | 41 | 42 => {
            Err(TokenCpiError::UnsupportedToken2022Extension(discriminant))
        }
        0..=TOKEN_2022_MAX_DISCRIMINANT => recognized_base_instruction(
            order,
            TokenProgramKindV1::Token2022,
            accounts,
            data,
            raw_data_sha256,
        ),
        _ => Err(TokenCpiError::UnknownInstruction {
            program: TokenProgramKindV1::Token2022,
            discriminant,
        }),
    }
}

fn parse_supply_change_instruction(
    order: ChainOrder,
    token_program: TokenProgramKindV1,
    accounts: Vec<String>,
    data: &[u8],
    raw_data_sha256: String,
) -> Result<ParsedTokenCpiV1, TokenCpiError> {
    let discriminant = data[0];
    let checked = matches!(
        discriminant,
        MINT_TO_CHECKED_DISCRIMINANT | BURN_CHECKED_DISCRIMINANT
    );
    require_exact_len(
        data,
        if checked {
            SUPPLY_CHANGE_CHECKED_WIRE_LEN
        } else {
            SUPPLY_CHANGE_WIRE_LEN
        },
        token_program,
        discriminant,
    )?;
    let decoded =
        TokenInstruction::unpack(data).map_err(|_| malformed(token_program, discriminant))?;
    let (kind, amount, decimals) = match (discriminant, decoded) {
        (MINT_TO_DISCRIMINANT, TokenInstruction::MintTo { amount }) => {
            (TokenSupplyChangeKindV1::Mint, amount, None)
        }
        (BURN_DISCRIMINANT, TokenInstruction::Burn { amount }) => {
            (TokenSupplyChangeKindV1::Burn, amount, None)
        }
        (MINT_TO_CHECKED_DISCRIMINANT, TokenInstruction::MintToChecked { amount, decimals }) => {
            (TokenSupplyChangeKindV1::Mint, amount, Some(decimals))
        }
        (BURN_CHECKED_DISCRIMINANT, TokenInstruction::BurnChecked { amount, decimals }) => {
            (TokenSupplyChangeKindV1::Burn, amount, Some(decimals))
        }
        _ => return Err(malformed(token_program, discriminant)),
    };
    if accounts.len() < 3 {
        return Err(TokenCpiError::TooFewSupplyChangeAccounts {
            minimum: 3,
            actual: accounts.len(),
        });
    }
    let (token_account, mint, authority) = match kind {
        TokenSupplyChangeKindV1::Mint => (
            accounts[1].clone(),
            accounts[0].clone(),
            accounts[2].clone(),
        ),
        TokenSupplyChangeKindV1::Burn => (
            accounts[0].clone(),
            accounts[1].clone(),
            accounts[2].clone(),
        ),
    };
    let variant = match (token_program, discriminant) {
        (TokenProgramKindV1::LegacyToken, MINT_TO_DISCRIMINANT) => {
            TokenSupplyChangeVariantV1::LegacyMintTo
        }
        (TokenProgramKindV1::LegacyToken, MINT_TO_CHECKED_DISCRIMINANT) => {
            TokenSupplyChangeVariantV1::LegacyMintToChecked
        }
        (TokenProgramKindV1::LegacyToken, BURN_DISCRIMINANT) => {
            TokenSupplyChangeVariantV1::LegacyBurn
        }
        (TokenProgramKindV1::LegacyToken, BURN_CHECKED_DISCRIMINANT) => {
            TokenSupplyChangeVariantV1::LegacyBurnChecked
        }
        (TokenProgramKindV1::Token2022, MINT_TO_DISCRIMINANT) => {
            TokenSupplyChangeVariantV1::Token2022MintTo
        }
        (TokenProgramKindV1::Token2022, MINT_TO_CHECKED_DISCRIMINANT) => {
            TokenSupplyChangeVariantV1::Token2022MintToChecked
        }
        (TokenProgramKindV1::Token2022, BURN_DISCRIMINANT) => {
            TokenSupplyChangeVariantV1::Token2022Burn
        }
        (TokenProgramKindV1::Token2022, BURN_CHECKED_DISCRIMINANT) => {
            TokenSupplyChangeVariantV1::Token2022BurnChecked
        }
        _ => return Err(malformed(token_program, discriminant)),
    };
    Ok(ParsedTokenCpiV1::SupplyChange {
        evidence: TokenSupplyChangeCpiV1 {
            schema_version: TOKEN_CPI_SCHEMA_VERSION,
            order,
            token_program,
            variant,
            kind,
            token_account,
            mint,
            authority,
            ordered_accounts: accounts,
            amount: token_amount(amount),
            decimals,
            raw_data_sha256,
        },
    })
}

fn parse_transfer_fee_extension(
    order: ChainOrder,
    accounts: Vec<String>,
    data: &[u8],
    raw_data_sha256: String,
) -> Result<ParsedTokenCpiV1, TokenCpiError> {
    let Some(&extension_discriminant) = data.get(1) else {
        return Err(malformed(
            TokenProgramKindV1::Token2022,
            TRANSFER_FEE_EXTENSION_DISCRIMINANT,
        ));
    };
    if extension_discriminant > 5 {
        return Err(TokenCpiError::UnknownInstruction {
            program: TokenProgramKindV1::Token2022,
            discriminant: extension_discriminant,
        });
    }
    if (2..=4).contains(&extension_discriminant) {
        return Err(TokenCpiError::UnsupportedValueMovement {
            program: TokenProgramKindV1::Token2022,
            discriminant: extension_discriminant,
        });
    }
    let decoded = TransferFeeInstruction::unpack(&data[1..]).map_err(|_| {
        malformed(
            TokenProgramKindV1::Token2022,
            TRANSFER_FEE_EXTENSION_DISCRIMINANT,
        )
    })?;
    let mut canonical = vec![TRANSFER_FEE_EXTENSION_DISCRIMINANT];
    decoded.pack(&mut canonical);
    if canonical != data {
        return Err(TokenCpiError::NonCanonicalInstructionEncoding {
            program: TokenProgramKindV1::Token2022,
            discriminant: TRANSFER_FEE_EXTENSION_DISCRIMINANT,
        });
    }
    match decoded {
        TransferFeeInstruction::TransferCheckedWithFee {
            amount,
            decimals,
            fee,
        } => {
            if extension_discriminant != TRANSFER_CHECKED_WITH_FEE_DISCRIMINANT {
                return Err(malformed(
                    TokenProgramKindV1::Token2022,
                    TRANSFER_FEE_EXTENSION_DISCRIMINANT,
                ));
            }
            require_exact_len(
                data,
                TRANSFER_CHECKED_WITH_FEE_WIRE_LEN,
                TokenProgramKindV1::Token2022,
                TRANSFER_FEE_EXTENSION_DISCRIMINANT,
            )?;
            transfer_result(
                order,
                TokenProgramKindV1::Token2022,
                TokenTransferVariantV1::Token2022TransferCheckedWithFee,
                accounts,
                amount,
                Some(decimals),
                Some(fee),
                raw_data_sha256,
            )
        }
        TransferFeeInstruction::InitializeTransferFeeConfig { .. }
        | TransferFeeInstruction::SetTransferFee { .. } => {
            Ok(ParsedTokenCpiV1::RecognizedNonTransfer {
                evidence: RecognizedNonTransferTokenCpiV1 {
                    schema_version: TOKEN_CPI_SCHEMA_VERSION,
                    order,
                    token_program: TokenProgramKindV1::Token2022,
                    top_level_discriminant: TRANSFER_FEE_EXTENSION_DISCRIMINANT,
                    extension_discriminant: Some(extension_discriminant),
                    ordered_accounts: accounts,
                    raw_data_sha256,
                    exact_account_adjustment: None,
                },
            })
        }
        TransferFeeInstruction::WithdrawWithheldTokensFromMint
        | TransferFeeInstruction::WithdrawWithheldTokensFromAccounts { .. }
        | TransferFeeInstruction::HarvestWithheldTokensToMint => {
            Err(TokenCpiError::UnsupportedValueMovement {
                program: TokenProgramKindV1::Token2022,
                discriminant: extension_discriminant,
            })
        }
    }
}

fn recognized_base_instruction(
    order: ChainOrder,
    token_program: TokenProgramKindV1,
    accounts: Vec<String>,
    data: &[u8],
    raw_data_sha256: String,
) -> Result<ParsedTokenCpiV1, TokenCpiError> {
    let discriminant = data[0];
    let decoded =
        TokenInstruction::unpack(data).map_err(|_| malformed(token_program, discriminant))?;
    if decoded.pack() != data {
        return Err(TokenCpiError::NonCanonicalInstructionEncoding {
            program: token_program,
            discriminant,
        });
    }
    // These variants have a direct custody-amount effect even though they are
    // not one of the transfer encodings extracted by this module.
    if matches!(discriminant, 7 | 8 | 14 | 15 | 17) {
        return Err(TokenCpiError::UnsupportedValueMovement {
            program: token_program,
            discriminant,
        });
    }
    // A successful ordinary-mint account initialization starts with zero token
    // amount. Native-mint initialization instead derives amount from lamports
    // and therefore requires external state. CloseAccount is likewise
    // state-dependent and remains blocking.
    if matches!(discriminant, 1 | 16 | 18)
        && accounts.get(1).map(String::as_str) == Some(NATIVE_MINT_ID)
    {
        return Err(TokenCpiError::RequiresExternalCustodyState {
            program: token_program,
            discriminant,
        });
    }
    if discriminant == 9 {
        return Err(TokenCpiError::RequiresExternalCustodyState {
            program: token_program,
            discriminant,
        });
    }
    Ok(ParsedTokenCpiV1::RecognizedNonTransfer {
        evidence: RecognizedNonTransferTokenCpiV1 {
            schema_version: TOKEN_CPI_SCHEMA_VERSION,
            order,
            token_program,
            top_level_discriminant: discriminant,
            extension_discriminant: None,
            ordered_accounts: accounts,
            raw_data_sha256,
            exact_account_adjustment: None,
        },
    })
}

#[allow(clippy::too_many_arguments)]
fn transfer_result(
    order: ChainOrder,
    token_program: TokenProgramKindV1,
    variant: TokenTransferVariantV1,
    accounts: Vec<String>,
    amount: u64,
    decimals: Option<u8>,
    explicit_fee: Option<u64>,
    raw_data_sha256: String,
) -> Result<ParsedTokenCpiV1, TokenCpiError> {
    let checked = !matches!(variant, TokenTransferVariantV1::LegacyTransfer);
    let minimum = if checked { 4 } else { 3 };
    if accounts.len() < minimum {
        return Err(TokenCpiError::TooFewTransferAccounts {
            minimum,
            actual: accounts.len(),
        });
    }
    let (source, mint, destination, authority) = if checked {
        (
            accounts[0].clone(),
            Some(accounts[1].clone()),
            accounts[2].clone(),
            accounts[3].clone(),
        )
    } else {
        (
            accounts[0].clone(),
            None,
            accounts[1].clone(),
            accounts[2].clone(),
        )
    };
    Ok(ParsedTokenCpiV1::Transfer {
        evidence: TokenTransferCpiV1 {
            schema_version: TOKEN_CPI_SCHEMA_VERSION,
            order,
            token_program,
            variant,
            source_token_account: source,
            destination_token_account: destination,
            mint,
            authority,
            ordered_accounts: accounts,
            gross_amount: token_amount(amount),
            decimals,
            explicit_fee: explicit_fee.map(token_amount),
            raw_data_sha256,
        },
    })
}

fn require_exact_len(
    data: &[u8],
    expected: usize,
    program: TokenProgramKindV1,
    discriminant: u8,
) -> Result<(), TokenCpiError> {
    if data.len() == expected {
        Ok(())
    } else {
        Err(TokenCpiError::MalformedInstruction {
            program,
            discriminant,
        })
    }
}

fn malformed(program: TokenProgramKindV1, discriminant: u8) -> TokenCpiError {
    TokenCpiError::MalformedInstruction {
        program,
        discriminant,
    }
}

fn compiled_index(value: Option<&JsonValue>, field: &'static str) -> Result<u64, TokenCpiError> {
    let index = value
        .and_then(JsonValue::as_u64)
        .ok_or(TokenCpiError::InvalidCompiledField(field))?;
    if u8::try_from(index).is_err() {
        return Err(TokenCpiError::AccountIndexOverflow(index));
    }
    Ok(index)
}

fn resolve_account_key(index: u64, account_keys: &[String]) -> Result<&str, TokenCpiError> {
    let index_usize =
        usize::try_from(index).map_err(|_| TokenCpiError::AccountIndexOutOfBounds(index))?;
    let key = account_keys
        .get(index_usize)
        .ok_or(TokenCpiError::AccountIndexOutOfBounds(index))?;
    validate_pubkey(key)?;
    Ok(key)
}

fn validate_pubkey(key: &str) -> Result<(), TokenCpiError> {
    let parsed =
        Pubkey::from_str(key).map_err(|_| TokenCpiError::InvalidAccountKey(key.to_owned()))?;
    if parsed.to_string() != key {
        return Err(TokenCpiError::InvalidAccountKey(key.to_owned()));
    }
    Ok(())
}

fn token_program(program_id: &str) -> Result<TokenProgramKindV1, TokenCpiError> {
    match program_id {
        SPL_TOKEN_PROGRAM_ID => Ok(TokenProgramKindV1::LegacyToken),
        SPL_TOKEN_2022_PROGRAM_ID => Ok(TokenProgramKindV1::Token2022),
        _ => Err(TokenCpiError::UnsupportedProgram(program_id.to_owned())),
    }
}

fn token_amount(value: u64) -> TokenAmount {
    TokenAmount::new(value.to_string()).expect("u64 formatting is a canonical token amount")
}

/// Aggregate exact spendable token-account deltas for one V1 operation. Every
/// token instruction in the proven subtree must be parsed, every supported
/// transfer or supply change must have one order-bound expectation, and every
/// expectation must match one observed instruction before account effects are
/// applied.
pub fn aggregate_exact_operation_token_deltas_v1(
    provenance: &TransactionExecutionProvenanceV1,
    target_operation: ChainOrder,
    parsed_instructions: &[ParsedTokenCpiV1],
    expected_transfers: &[ExpectedTokenTransferV1],
    expected_supply_changes: &[ExpectedTokenSupplyChangeV1],
) -> Result<ExactOperationTokenDeltasV1, TokenCpiError> {
    let frames = verified_frame_map(provenance)?;
    let target_frame = frames
        .get(&target_operation)
        .ok_or(TokenCpiError::InvalidTargetOperation)?;
    if !target_frame.is_target_program
        || target_frame.decoded_target_class != Some(DecodedInstructionClass::StateChanging)
    {
        return Err(TokenCpiError::InvalidTargetOperation);
    }

    let mut parsed_by_order = BTreeMap::new();
    for parsed in parsed_instructions {
        let order = parsed.order();
        if parsed_by_order.insert(order, parsed).is_some() {
            return Err(TokenCpiError::DuplicateParsedInstruction(order));
        }
        let frame = frames
            .get(&order)
            .ok_or(TokenCpiError::ParsedInstructionMissingFrame(order))?;
        if frame.program_id != token_program_id(parsed.token_program()) {
            return Err(TokenCpiError::ParsedProgramConflict(order));
        }
        validate_parsed_instruction(parsed)?;
    }

    let mut expected_transfers_by_order = BTreeMap::new();
    for expected in expected_transfers {
        validate_expected_transfer(expected)?;
        if expected_transfers_by_order
            .insert(expected.transfer_order, expected)
            .is_some()
        {
            return Err(TokenCpiError::DuplicateExpectedTransfer(
                expected.transfer_order,
            ));
        }
        if !is_owned_by_target_operation(expected.transfer_order, target_operation, &frames)? {
            return Err(TokenCpiError::ExpectedTransferOutsideSubtree(
                expected.transfer_order,
            ));
        }
    }
    let mut expected_supply_changes_by_order = BTreeMap::new();
    for expected in expected_supply_changes {
        validate_expected_supply_change(expected)?;
        if expected_transfers_by_order.contains_key(&expected.supply_change_order) {
            return Err(TokenCpiError::ConflictingExpectedInstructionKind(
                expected.supply_change_order,
            ));
        }
        if expected_supply_changes_by_order
            .insert(expected.supply_change_order, expected)
            .is_some()
        {
            return Err(TokenCpiError::DuplicateExpectedSupplyChange(
                expected.supply_change_order,
            ));
        }
        if !is_owned_by_target_operation(expected.supply_change_order, target_operation, &frames)? {
            return Err(TokenCpiError::ExpectedSupplyChangeOutsideSubtree(
                expected.supply_change_order,
            ));
        }
    }

    let mut subtree_token_orders = Vec::new();
    for frame in provenance.frames.iter().filter(|frame| {
        frame.program_id == SPL_TOKEN_PROGRAM_ID || frame.program_id == SPL_TOKEN_2022_PROGRAM_ID
    }) {
        if is_owned_by_target_operation(frame.order, target_operation, &frames)? {
            subtree_token_orders.push(frame.order);
        }
    }
    subtree_token_orders.sort_unstable();

    let mut matched_transfers = Vec::new();
    let mut matched_supply_changes = Vec::new();
    let mut account_accumulators = BTreeMap::<String, AccountAccumulator>::new();
    let mut matched_transfer_expectations = BTreeSet::new();
    let mut matched_supply_change_expectations = BTreeSet::new();
    for order in subtree_token_orders {
        let parsed = parsed_by_order
            .get(&order)
            .ok_or(TokenCpiError::MissingParsedTokenInstruction(order))?;
        match parsed {
            ParsedTokenCpiV1::Transfer { evidence } => {
                let expected = expected_transfers_by_order
                    .get(&order)
                    .ok_or(TokenCpiError::UnmatchedObservedTransfer(order))?;
                let transfer = match_transfer(evidence, expected)?;
                apply_delta(
                    &mut account_accumulators,
                    &transfer.source_token_account,
                    transfer.token_program,
                    &transfer.mint,
                    -i128::from(parse_u64(&transfer.gross_amount)?),
                )?;
                apply_delta(
                    &mut account_accumulators,
                    &transfer.destination_token_account,
                    transfer.token_program,
                    &transfer.mint,
                    i128::from(parse_u64(&transfer.destination_credit)?),
                )?;
                matched_transfer_expectations.insert(order);
                matched_transfers.push(transfer);
            }
            ParsedTokenCpiV1::SupplyChange { evidence } => {
                let expected = expected_supply_changes_by_order
                    .get(&order)
                    .ok_or(TokenCpiError::UnmatchedObservedSupplyChange(order))?;
                let supply_change = match_supply_change(evidence, expected)?;
                let magnitude = i128::from(parse_u64(&supply_change.amount)?);
                let signed_delta = match supply_change.kind {
                    TokenSupplyChangeKindV1::Mint => magnitude,
                    TokenSupplyChangeKindV1::Burn => -magnitude,
                };
                apply_delta(
                    &mut account_accumulators,
                    &supply_change.token_account,
                    supply_change.token_program,
                    &supply_change.mint,
                    signed_delta,
                )?;
                matched_supply_change_expectations.insert(order);
                matched_supply_changes.push(supply_change);
            }
            ParsedTokenCpiV1::RecognizedNonTransfer { evidence } => {
                if let Some(adjustment) = &evidence.exact_account_adjustment {
                    validate_adjustment_frame_proof(evidence, target_operation, &frames)?;
                    let signed_delta = adjustment
                        .signed_amount
                        .as_str()
                        .parse::<i128>()
                        .map_err(|_| TokenCpiError::InvalidAccountAdjustment(order))?;
                    apply_delta(
                        &mut account_accumulators,
                        &adjustment.token_account,
                        evidence.token_program,
                        &adjustment.mint,
                        signed_delta,
                    )?;
                }
            }
        }
    }

    for order in expected_transfers_by_order.keys() {
        if !matched_transfer_expectations.contains(order) {
            return Err(TokenCpiError::UnmatchedExpectedTransfer(*order));
        }
    }
    for order in expected_supply_changes_by_order.keys() {
        if !matched_supply_change_expectations.contains(order) {
            return Err(TokenCpiError::UnmatchedExpectedSupplyChange(*order));
        }
    }

    let account_deltas = account_accumulators
        .into_iter()
        .map(|(token_account, accumulator)| {
            Ok(ExactTokenAccountDeltaV1 {
                schema_version: TOKEN_CPI_SCHEMA_VERSION,
                token_account,
                token_program: accumulator.token_program,
                mint: accumulator.mint,
                signed_amount: SignedTokenAmount::new(accumulator.amount.to_string())
                    .map_err(|_| TokenCpiError::DeltaOverflow)?,
            })
        })
        .collect::<Result<Vec<_>, TokenCpiError>>()?;

    Ok(ExactOperationTokenDeltasV1 {
        schema_version: TOKEN_CPI_SCHEMA_VERSION,
        target_operation,
        matched_transfers,
        matched_supply_changes,
        account_deltas,
    })
}

fn verified_frame_map(
    provenance: &TransactionExecutionProvenanceV1,
) -> Result<BTreeMap<ChainOrder, &InstructionExecutionFrameV1>, TokenCpiError> {
    if provenance.schema_version != DECODE_SCHEMA_VERSION {
        return Err(TokenCpiError::SchemaVersion {
            expected: DECODE_SCHEMA_VERSION,
            actual: provenance.schema_version,
        });
    }
    if !provenance.is_complete() {
        return Err(TokenCpiError::IncompleteFrameAncestry);
    }
    let mut frames = BTreeMap::new();
    let mut paths = BTreeSet::new();
    for frame in &provenance.frames {
        if frame.schema_version != DECODE_SCHEMA_VERSION {
            return Err(TokenCpiError::SchemaVersion {
                expected: DECODE_SCHEMA_VERSION,
                actual: frame.schema_version,
            });
        }
        if frame.order.slot != provenance.slot
            || frame.order.transaction_index != provenance.transaction_index
            || frame.order.event_sequence != 0
            || !paths.insert(frame.execution_path.clone())
            || frames.insert(frame.order, frame).is_some()
        {
            return Err(TokenCpiError::DuplicateFrame(frame.order));
        }
    }
    for frame in frames.values() {
        validate_frame(frame, &frames)?;
    }
    Ok(frames)
}

fn validate_frame(
    frame: &InstructionExecutionFrameV1,
    frames: &BTreeMap<ChainOrder, &InstructionExecutionFrameV1>,
) -> Result<(), TokenCpiError> {
    if frame.provenance != CpiFrameProvenanceV1::Complete {
        return Err(TokenCpiError::IncompleteFrameAncestry);
    }
    match (
        frame.order.inner_execution_index,
        frame.stack_height,
        frame.parent_order,
    ) {
        (None, Some(1), None) if frame.execution_path == [frame.order.outer_instruction_index] => {
            Ok(())
        }
        (Some(_), Some(height), Some(parent_order)) if height >= 2 => {
            let parent = frames
                .get(&parent_order)
                .ok_or(TokenCpiError::MalformedFrameAncestry(frame.order))?;
            let parent_height = parent
                .stack_height
                .ok_or(TokenCpiError::MalformedFrameAncestry(frame.order))?;
            let path_is_child = frame.execution_path.len() == parent.execution_path.len() + 1
                && frame.execution_path.starts_with(&parent.execution_path);
            if parent_height.checked_add(1) != Some(height)
                || !path_is_child
                || parent.order.slot != frame.order.slot
                || parent.order.transaction_index != frame.order.transaction_index
                || parent.order.outer_instruction_index != frame.order.outer_instruction_index
                || parent.order >= frame.order
            {
                return Err(TokenCpiError::MalformedFrameAncestry(frame.order));
            }
            Ok(())
        }
        _ => Err(TokenCpiError::MalformedFrameAncestry(frame.order)),
    }
}

/// A token CPI belongs to the nearest verified state-changing V1 ancestor.
/// This is deliberately stricter than ordinary subtree membership: an outer
/// flashloan must not absorb transfers owned by a nested swap or liquidity
/// operation.
fn is_owned_by_target_operation(
    candidate: ChainOrder,
    target: ChainOrder,
    frames: &BTreeMap<ChainOrder, &InstructionExecutionFrameV1>,
) -> Result<bool, TokenCpiError> {
    let mut current = candidate;
    let mut visited = BTreeSet::new();
    loop {
        if !visited.insert(current) {
            return Err(TokenCpiError::MalformedFrameAncestry(candidate));
        }
        let Some(frame) = frames.get(&current) else {
            return Err(TokenCpiError::MalformedFrameAncestry(candidate));
        };
        if frame.is_target_program
            && frame.decoded_target_class == Some(DecodedInstructionClass::StateChanging)
        {
            return Ok(current == target);
        }
        let Some(parent) = frame.parent_order else {
            return Ok(false);
        };
        current = parent;
    }
}

fn validate_adjustment_frame_proof(
    evidence: &RecognizedNonTransferTokenCpiV1,
    target: ChainOrder,
    frames: &BTreeMap<ChainOrder, &InstructionExecutionFrameV1>,
) -> Result<(), TokenCpiError> {
    let Some(adjustment) = &evidence.exact_account_adjustment else {
        return Ok(());
    };
    let invalid = || TokenCpiError::InvalidAccountAdjustment(evidence.order);
    let evidence_frame = frames.get(&evidence.order).ok_or_else(invalid)?;
    match &adjustment.source {
        TokenAccountAdjustmentSourceV1::AssociatedTokenAccountCreation { parent_order } => {
            let parent = frames.get(parent_order).ok_or_else(invalid)?;
            if evidence_frame.parent_order != Some(*parent_order)
                || parent.program_id != ASSOCIATED_TOKEN_PROGRAM_ID
            {
                return Err(invalid());
            }
        }
        TokenAccountAdjustmentSourceV1::RpcTokenBalanceResidual {
            certified_zero_external_cpis,
            ..
        } => {
            for certified in certified_zero_external_cpis {
                let frame = frames.get(&certified.order).ok_or_else(invalid)?;
                let parent = frames
                    .get(&certified.associated_token_parent_order)
                    .ok_or_else(invalid)?;
                if frame.program_id != SPL_TOKEN_PROGRAM_ID
                    || frame.parent_order != Some(certified.associated_token_parent_order)
                    || parent.program_id != ASSOCIATED_TOKEN_PROGRAM_ID
                    || is_owned_by_target_operation(certified.order, target, frames)?
                {
                    return Err(invalid());
                }
            }
        }
    }
    Ok(())
}

fn validate_parsed_instruction(parsed: &ParsedTokenCpiV1) -> Result<(), TokenCpiError> {
    let (schema_version, hash) = match parsed {
        ParsedTokenCpiV1::Transfer { evidence } => {
            validate_transfer_evidence(evidence)?;
            (evidence.schema_version, evidence.raw_data_sha256.as_str())
        }
        ParsedTokenCpiV1::SupplyChange { evidence } => {
            validate_supply_change_evidence(evidence)?;
            (evidence.schema_version, evidence.raw_data_sha256.as_str())
        }
        ParsedTokenCpiV1::RecognizedNonTransfer { evidence } => {
            validate_recognized_non_transfer(evidence)?;
            (evidence.schema_version, evidence.raw_data_sha256.as_str())
        }
    };
    if schema_version != TOKEN_CPI_SCHEMA_VERSION {
        return Err(TokenCpiError::SchemaVersion {
            expected: TOKEN_CPI_SCHEMA_VERSION,
            actual: schema_version,
        });
    }
    if !valid_sha256(hash) {
        return Err(TokenCpiError::InvalidRawDataHash);
    }
    Ok(())
}

fn validate_supply_change_evidence(evidence: &TokenSupplyChangeCpiV1) -> Result<(), TokenCpiError> {
    if evidence.schema_version != TOKEN_CPI_SCHEMA_VERSION {
        return Err(TokenCpiError::SchemaVersion {
            expected: TOKEN_CPI_SCHEMA_VERSION,
            actual: evidence.schema_version,
        });
    }
    if evidence.order.event_sequence != 0 {
        return Err(TokenCpiError::InvalidInstructionOrder);
    }
    for key in [
        evidence.token_account.as_str(),
        evidence.mint.as_str(),
        evidence.authority.as_str(),
    ] {
        validate_pubkey(key)?;
    }
    for account in &evidence.ordered_accounts {
        validate_pubkey(account)?;
    }
    parse_u64(&evidence.amount)?;
    let expected_program = supply_change_variant_program(evidence.variant);
    if evidence.token_program != expected_program
        || evidence.kind != supply_change_variant_kind(evidence.variant)
    {
        return Err(TokenCpiError::ConflictingProgram(evidence.order));
    }
    let checked = supply_change_variant_is_checked(evidence.variant);
    if evidence.ordered_accounts.len() < 3
        || checked != evidence.decimals.is_some()
        || evidence.ordered_accounts[2] != evidence.authority
    {
        return Err(TokenCpiError::ConflictingAccounts(evidence.order));
    }
    let roles_match = match evidence.kind {
        TokenSupplyChangeKindV1::Mint => {
            evidence.ordered_accounts[0] == evidence.mint
                && evidence.ordered_accounts[1] == evidence.token_account
        }
        TokenSupplyChangeKindV1::Burn => {
            evidence.ordered_accounts[0] == evidence.token_account
                && evidence.ordered_accounts[1] == evidence.mint
        }
    };
    if !roles_match {
        return Err(TokenCpiError::ConflictingAccounts(evidence.order));
    }
    Ok(())
}

fn supply_change_variant_program(variant: TokenSupplyChangeVariantV1) -> TokenProgramKindV1 {
    match variant {
        TokenSupplyChangeVariantV1::LegacyMintTo
        | TokenSupplyChangeVariantV1::LegacyMintToChecked
        | TokenSupplyChangeVariantV1::LegacyBurn
        | TokenSupplyChangeVariantV1::LegacyBurnChecked => TokenProgramKindV1::LegacyToken,
        TokenSupplyChangeVariantV1::Token2022MintTo
        | TokenSupplyChangeVariantV1::Token2022MintToChecked
        | TokenSupplyChangeVariantV1::Token2022Burn
        | TokenSupplyChangeVariantV1::Token2022BurnChecked => TokenProgramKindV1::Token2022,
    }
}

fn supply_change_variant_kind(variant: TokenSupplyChangeVariantV1) -> TokenSupplyChangeKindV1 {
    match variant {
        TokenSupplyChangeVariantV1::LegacyMintTo
        | TokenSupplyChangeVariantV1::LegacyMintToChecked
        | TokenSupplyChangeVariantV1::Token2022MintTo
        | TokenSupplyChangeVariantV1::Token2022MintToChecked => TokenSupplyChangeKindV1::Mint,
        TokenSupplyChangeVariantV1::LegacyBurn
        | TokenSupplyChangeVariantV1::LegacyBurnChecked
        | TokenSupplyChangeVariantV1::Token2022Burn
        | TokenSupplyChangeVariantV1::Token2022BurnChecked => TokenSupplyChangeKindV1::Burn,
    }
}

fn supply_change_variant_is_checked(variant: TokenSupplyChangeVariantV1) -> bool {
    matches!(
        variant,
        TokenSupplyChangeVariantV1::LegacyMintToChecked
            | TokenSupplyChangeVariantV1::LegacyBurnChecked
            | TokenSupplyChangeVariantV1::Token2022MintToChecked
            | TokenSupplyChangeVariantV1::Token2022BurnChecked
    )
}

fn validate_transfer_evidence(evidence: &TokenTransferCpiV1) -> Result<(), TokenCpiError> {
    if evidence.schema_version != TOKEN_CPI_SCHEMA_VERSION {
        return Err(TokenCpiError::SchemaVersion {
            expected: TOKEN_CPI_SCHEMA_VERSION,
            actual: evidence.schema_version,
        });
    }
    if evidence.order.event_sequence != 0 {
        return Err(TokenCpiError::InvalidInstructionOrder);
    }
    validate_pubkey(&evidence.source_token_account)?;
    validate_pubkey(&evidence.destination_token_account)?;
    validate_pubkey(&evidence.authority)?;
    if let Some(mint) = &evidence.mint {
        validate_pubkey(mint)?;
    }
    for account in &evidence.ordered_accounts {
        validate_pubkey(account)?;
    }
    parse_u64(&evidence.gross_amount)?;
    if let Some(fee) = &evidence.explicit_fee {
        parse_u64(fee)?;
    }
    let valid_variant = matches!(
        (evidence.token_program, evidence.variant),
        (
            TokenProgramKindV1::LegacyToken,
            TokenTransferVariantV1::LegacyTransfer | TokenTransferVariantV1::LegacyTransferChecked
        ) | (
            TokenProgramKindV1::Token2022,
            TokenTransferVariantV1::Token2022TransferChecked
                | TokenTransferVariantV1::Token2022TransferCheckedWithFee
        )
    );
    if !valid_variant {
        return Err(TokenCpiError::ConflictingProgram(evidence.order));
    }
    let roles_match = match evidence.variant {
        TokenTransferVariantV1::LegacyTransfer => {
            evidence.ordered_accounts.len() >= 3
                && evidence.ordered_accounts[0] == evidence.source_token_account
                && evidence.ordered_accounts[1] == evidence.destination_token_account
                && evidence.ordered_accounts[2] == evidence.authority
                && evidence.mint.is_none()
                && evidence.decimals.is_none()
                && evidence.explicit_fee.is_none()
        }
        TokenTransferVariantV1::LegacyTransferChecked
        | TokenTransferVariantV1::Token2022TransferChecked => {
            evidence.ordered_accounts.len() >= 4
                && evidence.ordered_accounts[0] == evidence.source_token_account
                && evidence.mint.as_ref() == evidence.ordered_accounts.get(1)
                && evidence.ordered_accounts[2] == evidence.destination_token_account
                && evidence.ordered_accounts[3] == evidence.authority
                && evidence.decimals.is_some()
                && evidence.explicit_fee.is_none()
        }
        TokenTransferVariantV1::Token2022TransferCheckedWithFee => {
            evidence.ordered_accounts.len() >= 4
                && evidence.ordered_accounts[0] == evidence.source_token_account
                && evidence.mint.as_ref() == evidence.ordered_accounts.get(1)
                && evidence.ordered_accounts[2] == evidence.destination_token_account
                && evidence.ordered_accounts[3] == evidence.authority
                && evidence.decimals.is_some()
                && evidence.explicit_fee.is_some()
        }
    };
    if !roles_match {
        return Err(TokenCpiError::ConflictingAccounts(evidence.order));
    }
    Ok(())
}

fn validate_recognized_non_transfer(
    evidence: &RecognizedNonTransferTokenCpiV1,
) -> Result<(), TokenCpiError> {
    if evidence.schema_version != TOKEN_CPI_SCHEMA_VERSION {
        return Err(TokenCpiError::SchemaVersion {
            expected: TOKEN_CPI_SCHEMA_VERSION,
            actual: evidence.schema_version,
        });
    }
    if evidence.order.event_sequence != 0 {
        return Err(TokenCpiError::InvalidInstructionOrder);
    }
    for account in &evidence.ordered_accounts {
        validate_pubkey(account)?;
    }
    let discriminant = evidence.top_level_discriminant;
    if let Some(adjustment) = &evidence.exact_account_adjustment {
        validate_exact_account_adjustment(evidence, adjustment)?;
        return Ok(());
    }
    let ordinary_account_initialization = matches!(discriminant, 1 | 16 | 18)
        && evidence.ordered_accounts.len() >= 2
        && evidence.ordered_accounts[1].as_str() != NATIVE_MINT_ID;
    let direct_zero_delta = !matches!(
        discriminant,
        1 | 3 | 7 | 8 | 9 | 12 | 14 | 15 | 16 | 17 | 18
    ) || ordinary_account_initialization;
    let valid = match evidence.token_program {
        TokenProgramKindV1::LegacyToken => {
            discriminant <= LEGACY_MAX_DISCRIMINANT
                && evidence.extension_discriminant.is_none()
                && direct_zero_delta
        }
        TokenProgramKindV1::Token2022 if discriminant == TRANSFER_FEE_EXTENSION_DISCRIMINANT => {
            matches!(evidence.extension_discriminant, Some(0 | 5))
        }
        TokenProgramKindV1::Token2022 => {
            discriminant <= TOKEN_2022_MAX_DISCRIMINANT
                && evidence.extension_discriminant.is_none()
                && direct_zero_delta
                && !matches!(
                    discriminant,
                    27 | 28 | 30 | 33 | 34 | 36 | 37 | 39 | 40 | 41 | 42
                )
        }
    };
    if !valid {
        return Err(TokenCpiError::UnsupportedValueMovement {
            program: evidence.token_program,
            discriminant,
        });
    }
    Ok(())
}

fn validate_exact_account_adjustment(
    evidence: &RecognizedNonTransferTokenCpiV1,
    adjustment: &ExactTokenAccountAdjustmentV1,
) -> Result<(), TokenCpiError> {
    let invalid = || TokenCpiError::InvalidAccountAdjustment(evidence.order);
    if evidence.token_program != TokenProgramKindV1::LegacyToken
        || evidence.extension_discriminant.is_some()
        || adjustment.mint != NATIVE_MINT_ID
        || adjustment.decimals != 9
        || evidence.ordered_accounts.first() != Some(&adjustment.token_account)
    {
        return Err(invalid());
    }
    validate_pubkey(&adjustment.token_account)?;
    validate_pubkey(&adjustment.mint)?;
    let signed_amount = adjustment
        .signed_amount
        .as_str()
        .parse::<i128>()
        .map_err(|_| invalid())?;
    match adjustment.reason {
        TokenAccountAdjustmentReasonV1::SyncNative => {
            if evidence.top_level_discriminant != 17
                || evidence.ordered_accounts.len() != 1
                || signed_amount < 0
            {
                return Err(invalid());
            }
        }
        TokenAccountAdjustmentReasonV1::NativeAccountInitialization => {
            if !matches!(evidence.top_level_discriminant, 1 | 16 | 18)
                || evidence.ordered_accounts.len() < 2
                || evidence.ordered_accounts[1] != adjustment.mint
                || signed_amount != 0
            {
                return Err(invalid());
            }
        }
    }
    match &adjustment.source {
        TokenAccountAdjustmentSourceV1::AssociatedTokenAccountCreation { parent_order } => {
            if adjustment.reason != TokenAccountAdjustmentReasonV1::NativeAccountInitialization
                || signed_amount != 0
                || parent_order.slot != evidence.order.slot
                || parent_order.transaction_index != evidence.order.transaction_index
                || parent_order.outer_instruction_index != evidence.order.outer_instruction_index
                || *parent_order >= evidence.order
            {
                return Err(invalid());
            }
        }
        TokenAccountAdjustmentSourceV1::RpcTokenBalanceResidual {
            transaction_pre_amount,
            transaction_post_amount,
            other_exact_delta,
            certified_zero_external_cpis,
        } => {
            let pre = i128::from(parse_u64(transaction_pre_amount)?);
            let post = i128::from(parse_u64(transaction_post_amount)?);
            let other = other_exact_delta
                .as_str()
                .parse::<i128>()
                .map_err(|_| invalid())?;
            let net = post.checked_sub(pre).ok_or_else(invalid)?;
            let reconstructed = other.checked_add(signed_amount).ok_or_else(invalid)?;
            if net != reconstructed {
                return Err(invalid());
            }
            let mut orders = BTreeSet::new();
            for certified in certified_zero_external_cpis {
                if certified.order.slot != evidence.order.slot
                    || certified.order.transaction_index != evidence.order.transaction_index
                    || certified.order == evidence.order
                    || certified.associated_token_parent_order.slot != evidence.order.slot
                    || certified.associated_token_parent_order.transaction_index
                        != evidence.order.transaction_index
                    || certified
                        .associated_token_parent_order
                        .outer_instruction_index
                        != certified.order.outer_instruction_index
                    || certified.associated_token_parent_order >= certified.order
                    || !orders.insert(certified.order)
                    || !valid_sha256(&certified.raw_data_sha256)
                {
                    return Err(invalid());
                }
            }
        }
    }
    Ok(())
}

fn validate_expected_transfer(expected: &ExpectedTokenTransferV1) -> Result<(), TokenCpiError> {
    if expected.schema_version != TOKEN_CPI_SCHEMA_VERSION {
        return Err(TokenCpiError::SchemaVersion {
            expected: TOKEN_CPI_SCHEMA_VERSION,
            actual: expected.schema_version,
        });
    }
    if expected.transfer_order.event_sequence != 0 {
        return Err(TokenCpiError::InvalidInstructionOrder);
    }
    for key in [
        expected.source_token_account.as_str(),
        expected.destination_token_account.as_str(),
        expected.mint.as_str(),
        expected.authority.as_str(),
    ] {
        validate_pubkey(key)?;
    }
    if let Some(amount) = &expected.expected_gross_amount {
        parse_u64(amount)?;
    }
    if let Some(fee) = &expected.resolved_fee {
        parse_u64(fee)?;
    }
    Ok(())
}

fn validate_expected_supply_change(
    expected: &ExpectedTokenSupplyChangeV1,
) -> Result<(), TokenCpiError> {
    if expected.schema_version != TOKEN_CPI_SCHEMA_VERSION {
        return Err(TokenCpiError::SchemaVersion {
            expected: TOKEN_CPI_SCHEMA_VERSION,
            actual: expected.schema_version,
        });
    }
    if expected.supply_change_order.event_sequence != 0 {
        return Err(TokenCpiError::InvalidInstructionOrder);
    }
    for key in [
        expected.token_account.as_str(),
        expected.mint.as_str(),
        expected.authority.as_str(),
    ] {
        validate_pubkey(key)?;
    }
    if let Some(amount) = &expected.expected_amount {
        parse_u64(amount)?;
    }
    Ok(())
}

fn match_transfer(
    evidence: &TokenTransferCpiV1,
    expected: &ExpectedTokenTransferV1,
) -> Result<MatchedTokenTransferV1, TokenCpiError> {
    let order = evidence.order;
    if evidence.token_program != expected.token_program {
        return Err(TokenCpiError::ConflictingProgram(order));
    }
    if evidence.source_token_account != expected.source_token_account
        || evidence.destination_token_account != expected.destination_token_account
    {
        return Err(TokenCpiError::ConflictingAccounts(order));
    }
    if evidence
        .mint
        .as_deref()
        .is_some_and(|mint| mint != expected.mint)
    {
        return Err(TokenCpiError::ConflictingMint(order));
    }
    if evidence.authority != expected.authority {
        return Err(TokenCpiError::ConflictingAuthority(order));
    }
    if expected
        .expected_gross_amount
        .as_ref()
        .is_some_and(|amount| amount != &evidence.gross_amount)
    {
        return Err(TokenCpiError::ConflictingAmount(order));
    }
    if expected
        .expected_decimals
        .is_some_and(|decimals| evidence.decimals.is_some_and(|actual| actual != decimals))
    {
        return Err(TokenCpiError::ConflictingDecimals(order));
    }
    let gross = parse_u64(&evidence.gross_amount)?;
    let expected_fee = expected.resolved_fee.as_ref().map(parse_u64).transpose()?;
    let explicit_fee = evidence.explicit_fee.as_ref().map(parse_u64).transpose()?;
    let fee = match evidence.variant {
        TokenTransferVariantV1::LegacyTransfer | TokenTransferVariantV1::LegacyTransferChecked => {
            if expected_fee.is_some_and(|fee| fee != 0) {
                return Err(TokenCpiError::ConflictingFee(order));
            }
            0
        }
        TokenTransferVariantV1::Token2022TransferChecked => {
            expected_fee.ok_or(TokenCpiError::MissingFeeEvidence(order))?
        }
        TokenTransferVariantV1::Token2022TransferCheckedWithFee => {
            let explicit_fee = explicit_fee.ok_or(TokenCpiError::MissingFeeEvidence(order))?;
            if expected_fee.is_some_and(|expected| expected != explicit_fee) {
                return Err(TokenCpiError::ConflictingFee(order));
            }
            explicit_fee
        }
    };
    let destination_credit = gross
        .checked_sub(fee)
        .ok_or(TokenCpiError::FeeExceedsGrossAmount(order))?;
    Ok(MatchedTokenTransferV1 {
        schema_version: TOKEN_CPI_SCHEMA_VERSION,
        transfer_order: order,
        token_program: evidence.token_program,
        variant: evidence.variant,
        source_token_account: evidence.source_token_account.clone(),
        destination_token_account: evidence.destination_token_account.clone(),
        mint: expected.mint.clone(),
        authority: evidence.authority.clone(),
        gross_amount: evidence.gross_amount.clone(),
        decimals: evidence.decimals,
        transfer_fee: token_amount(fee),
        destination_credit: token_amount(destination_credit),
        raw_data_sha256: evidence.raw_data_sha256.clone(),
    })
}

fn match_supply_change(
    evidence: &TokenSupplyChangeCpiV1,
    expected: &ExpectedTokenSupplyChangeV1,
) -> Result<MatchedTokenSupplyChangeV1, TokenCpiError> {
    let order = evidence.order;
    if evidence.token_program != expected.token_program {
        return Err(TokenCpiError::ConflictingProgram(order));
    }
    if evidence.kind != expected.kind {
        return Err(TokenCpiError::ConflictingSupplyChangeKind(order));
    }
    if evidence.token_account != expected.token_account {
        return Err(TokenCpiError::ConflictingAccounts(order));
    }
    if evidence.mint != expected.mint {
        return Err(TokenCpiError::ConflictingMint(order));
    }
    if evidence.authority != expected.authority {
        return Err(TokenCpiError::ConflictingAuthority(order));
    }
    if expected
        .expected_amount
        .as_ref()
        .is_some_and(|amount| amount != &evidence.amount)
    {
        return Err(TokenCpiError::ConflictingAmount(order));
    }
    // Unchecked MintTo/Burn instructions do not carry decimals on the wire.
    // Their independently resolved token-account identity may still provide
    // an expected decimal value, so only compare when the instruction itself
    // supplies one. Checked variants are required to carry `Some(decimals)`
    // by `validate_supply_change_evidence` above.
    if expected
        .expected_decimals
        .is_some_and(|decimals| evidence.decimals.is_some_and(|actual| actual != decimals))
    {
        return Err(TokenCpiError::ConflictingDecimals(order));
    }
    Ok(MatchedTokenSupplyChangeV1 {
        schema_version: TOKEN_CPI_SCHEMA_VERSION,
        supply_change_order: order,
        token_program: evidence.token_program,
        variant: evidence.variant,
        kind: evidence.kind,
        token_account: evidence.token_account.clone(),
        mint: evidence.mint.clone(),
        authority: evidence.authority.clone(),
        amount: evidence.amount.clone(),
        decimals: evidence.decimals,
        raw_data_sha256: evidence.raw_data_sha256.clone(),
    })
}

#[derive(Clone, Debug)]
struct AccountAccumulator {
    token_program: TokenProgramKindV1,
    mint: String,
    amount: i128,
}

fn apply_delta(
    accounts: &mut BTreeMap<String, AccountAccumulator>,
    account: &str,
    token_program: TokenProgramKindV1,
    mint: &str,
    delta: i128,
) -> Result<(), TokenCpiError> {
    match accounts.get_mut(account) {
        Some(existing) => {
            if existing.token_program != token_program || existing.mint != mint {
                return Err(TokenCpiError::ConflictingAccountIdentity {
                    account: account.to_owned(),
                });
            }
            existing.amount = existing
                .amount
                .checked_add(delta)
                .ok_or(TokenCpiError::DeltaOverflow)?;
        }
        None => {
            accounts.insert(
                account.to_owned(),
                AccountAccumulator {
                    token_program,
                    mint: mint.to_owned(),
                    amount: delta,
                },
            );
        }
    }
    Ok(())
}

fn parse_u64(value: &TokenAmount) -> Result<u64, TokenCpiError> {
    value
        .as_str()
        .parse::<u64>()
        .map_err(|_| TokenCpiError::InvalidTokenAmount)
}

fn token_program_id(program: TokenProgramKindV1) -> &'static str {
    match program {
        TokenProgramKindV1::LegacyToken => SPL_TOKEN_PROGRAM_ID,
        TokenProgramKindV1::Token2022 => SPL_TOKEN_2022_PROGRAM_ID,
    }
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const V1_PROGRAM_ID: &str = "omnixgS8fnqHfCcTGKWj6JtKjzpJZ1Y5y9pyFkQDkYE";
    const ROUTER_ID: &str = "11111111111111111111111111111111";
    const USER: &str = "4xp5LiAtVsmrBX8Fi5W3ai7HnL5BFCsExnCQWnPzDRwY";
    const INPUT_MINT: &str = "iAVYNrstVncUihbYbDmLbt7r9oPNckDoUuwtw7hWoaG";
    const INPUT_VAULT: &str = "FRayk6TUMcjx3df5YLvxvBJ5cp9ZYq9oDXvaEav8jP9H";
    const AUTHORITY: &str = "8tF4uYMBXqGhCUGRZL3AmPqRzbX8JJ1TpYnY3uJKN4kt";
    const SECOND_USER: &str = "8eq82Lam4MEh1EE7QT7N1fgMiKvdcwR1PnkW4ETen2kN";

    fn order(outer: u32, inner: Option<u32>) -> ChainOrder {
        ChainOrder {
            slot: 398_300_935,
            transaction_index: 271,
            outer_instruction_index: outer,
            inner_execution_index: inner,
            event_sequence: 0,
        }
    }

    fn slot_398300935_keys() -> Vec<String> {
        [
            AUTHORITY,
            "F4yeoHwXtEF1n4Bo1j6qzuaU7XtR7tMn7LQ2CAwK7Jfr",
            "9AZvDGdfy46mT1C5dnq7jnhDP5WBbENruBLu7pTXabY2",
            INPUT_VAULT,
            "FHZ8exdTeSqyE1dP97P3Cw6VwSq5u7BXmzkJdwUiG2sN",
            USER,
            SECOND_USER,
            "DUVgVvoDWs5teeqHn3fVPPEYUYr8pzUXJLkdA1aTAc2T",
            "ComputeBudget111111111111111111111111111111",
            V1_PROGRAM_ID,
            "2SMS1Y4EAyL2dQLpXD6VJCrNbQJ2eQ2pN3qYcX1vim3E",
            INPUT_MINT,
            "AKfDVR3ytkqm86aEY178aRmWwusqpt43HwNfJDf4odJu",
            SPL_TOKEN_PROGRAM_ID,
            SPL_TOKEN_2022_PROGRAM_ID,
            "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL",
            ROUTER_ID,
            "FWdP9yTogKbuXvEqQNNHYw2TYm38MbinAZ2iTHeZWX8H",
            "L2TExMFKdjpN9kozasaurPirfHy9P8sbXoAN1qA3S95",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }

    #[test]
    fn golden_slot_398300935_legacy_swap_cpis_are_exact() {
        let keys = slot_398300935_keys();
        let fixtures = [
            (
                json!({"programIdIndex":13,"accounts":[5,11,3,0],"data":"iCvq63Tcet7oT","stackHeight":2}),
                5,
                "98908581",
                "70f3fd2aff28d6009c96592ad4572b6b48425209a60997eab36aaf7014072ddf",
                USER,
                INPUT_VAULT,
            ),
            (
                json!({"programIdIndex":13,"accounts":[3,11,7,1],"data":"i3SYzK9wj2t6q","stackHeight":2}),
                6,
                "24728",
                "8d9d98212e5dcd4ea0d02ba7b7376f8aff1f072fa406fe3e4ba7b9f4226f256e",
                INPUT_VAULT,
                "DUVgVvoDWs5teeqHn3fVPPEYUYr8pzUXJLkdA1aTAc2T",
            ),
            (
                json!({"programIdIndex":13,"accounts":[4,12,6,1],"data":"iXs5rSg21iRFs","stackHeight":2}),
                7,
                "49209278",
                "4ca4850ead49d2f291ebfa909f8c3d905ef1df0db42d3808cdd601f7431439f0",
                "FHZ8exdTeSqyE1dP97P3Cw6VwSq5u7BXmzkJdwUiG2sN",
                SECOND_USER,
            ),
        ];
        for (raw, inner, amount, hash, source, destination) in fixtures {
            let ParsedTokenCpiV1::Transfer { evidence } =
                parse_compiled_token_instruction_v1(order(2, Some(inner)), &raw, &keys).unwrap()
            else {
                panic!("golden CPI was not classified as a transfer");
            };
            assert_eq!(
                evidence.variant,
                TokenTransferVariantV1::LegacyTransferChecked
            );
            assert_eq!(evidence.gross_amount.as_str(), amount);
            assert_eq!(evidence.decimals, Some(6));
            assert_eq!(
                evidence.mint.as_deref(),
                Some(if inner == 7 { &keys[12] } else { INPUT_MINT })
            );
            assert_eq!(evidence.source_token_account, source);
            assert_eq!(evidence.destination_token_account, destination);
            assert_eq!(evidence.raw_data_sha256, hash);
        }

        let non_transfer = json!({
            "programIdIndex": 13,
            "accounts": [11],
            "data": "84eT",
            "stackHeight": 3
        });
        assert!(matches!(
            parse_compiled_token_instruction_v1(order(2, Some(1)), &non_transfer, &keys).unwrap(),
            ParsedTokenCpiV1::RecognizedNonTransfer { .. }
        ));
    }

    #[test]
    fn token_2022_checked_with_fee_is_exact_and_trailing_bytes_fail() {
        let mut keys = slot_398300935_keys();
        keys[13] = SPL_TOKEN_2022_PROGRAM_ID.to_owned();
        let mut data = vec![26, 1];
        data.extend_from_slice(&1_000_u64.to_le_bytes());
        data.push(6);
        data.extend_from_slice(&7_u64.to_le_bytes());
        let raw = json!({
            "programIdIndex": 13,
            "accounts": [5, 11, 3, 0],
            "data": bs58::encode(&data).into_string()
        });
        let ParsedTokenCpiV1::Transfer { evidence } =
            parse_compiled_token_instruction_v1(order(2, Some(0)), &raw, &keys).unwrap()
        else {
            panic!("expected transfer");
        };
        assert_eq!(
            evidence.variant,
            TokenTransferVariantV1::Token2022TransferCheckedWithFee
        );
        assert_eq!(evidence.explicit_fee.as_ref().unwrap().as_str(), "7");

        data.push(0);
        let malformed = json!({
            "programIdIndex": 13,
            "accounts": [5, 11, 3, 0],
            "data": bs58::encode(&data).into_string()
        });
        assert!(matches!(
            parse_compiled_token_instruction_v1(order(2, Some(0)), &malformed, &keys),
            Err(TokenCpiError::NonCanonicalInstructionEncoding { .. })
        ));
    }

    #[test]
    fn legacy_unchecked_and_token_2022_checked_wire_variants_are_supported() {
        let keys = slot_398300935_keys();
        let mut legacy_data = vec![LEGACY_TRANSFER_DISCRIMINANT];
        legacy_data.extend_from_slice(&42_u64.to_le_bytes());
        let legacy = json!({
            "programIdIndex": 13,
            "accounts": [5, 3, 0],
            "data": bs58::encode(legacy_data).into_string()
        });
        let ParsedTokenCpiV1::Transfer { evidence } =
            parse_compiled_token_instruction_v1(order(2, Some(0)), &legacy, &keys).unwrap()
        else {
            panic!("expected legacy transfer");
        };
        assert_eq!(evidence.variant, TokenTransferVariantV1::LegacyTransfer);
        assert_eq!(evidence.source_token_account, USER);
        assert_eq!(evidence.destination_token_account, INPUT_VAULT);
        assert_eq!(evidence.authority, AUTHORITY);
        assert_eq!(evidence.gross_amount.as_str(), "42");
        assert_eq!(evidence.mint, None);

        let mut token_2022_keys = keys;
        token_2022_keys[13] = SPL_TOKEN_2022_PROGRAM_ID.to_owned();
        let mut checked_data = vec![TRANSFER_CHECKED_DISCRIMINANT];
        checked_data.extend_from_slice(&43_u64.to_le_bytes());
        checked_data.push(6);
        let checked = json!({
            "programIdIndex": 13,
            "accounts": [5, 11, 3, 0],
            "data": bs58::encode(checked_data).into_string()
        });
        let ParsedTokenCpiV1::Transfer { evidence } =
            parse_compiled_token_instruction_v1(order(2, Some(1)), &checked, &token_2022_keys)
                .unwrap()
        else {
            panic!("expected Token-2022 checked transfer");
        };
        assert_eq!(
            evidence.variant,
            TokenTransferVariantV1::Token2022TransferChecked
        );
        assert_eq!(evidence.gross_amount.as_str(), "43");
        assert_eq!(evidence.explicit_fee, None);

        let mut unsupported_data = vec![LEGACY_TRANSFER_DISCRIMINANT];
        unsupported_data.extend_from_slice(&1_u64.to_le_bytes());
        let unsupported = json!({
            "programIdIndex": 13,
            "accounts": [5, 3, 0],
            "data": bs58::encode(unsupported_data).into_string()
        });
        assert!(matches!(
            parse_compiled_token_instruction_v1(order(2, Some(2)), &unsupported, &token_2022_keys,),
            Err(TokenCpiError::UnsupportedValueMovement { .. })
        ));
    }

    #[test]
    fn legacy_and_token_2022_supply_change_wires_are_exact() {
        let fixtures = [
            (
                vec![7, 0xe8, 0x03, 0, 0, 0, 0, 0, 0],
                TokenSupplyChangeKindV1::Mint,
                "1000",
                None,
                "880d23678da97ca0892561a1e2dcb220ceec64192e7eb12abbfd93d0d1b6ca81",
            ),
            (
                vec![14, 0xf4, 0x01, 0, 0, 0, 0, 0, 0, 6],
                TokenSupplyChangeKindV1::Mint,
                "500",
                Some(6),
                "589ef66cd59895d7e2d502c8045fe688cb7f00557be0ba1c7f4c78bf9bc87f36",
            ),
            (
                vec![8, 0x58, 0x02, 0, 0, 0, 0, 0, 0],
                TokenSupplyChangeKindV1::Burn,
                "600",
                None,
                "7ffb927253685cde1314e364432dbc3500bfe780be69917001b1e965e09d25ea",
            ),
            (
                vec![15, 0x90, 0x01, 0, 0, 0, 0, 0, 0, 6],
                TokenSupplyChangeKindV1::Burn,
                "400",
                Some(6),
                "e55a0bd9bb16e552540eeaa42132e87ca3d86eb46d60c5c78541593f7abeec93",
            ),
        ];
        for token_program in [
            TokenProgramKindV1::LegacyToken,
            TokenProgramKindV1::Token2022,
        ] {
            for (index, (data, kind, amount, decimals, hash)) in fixtures.iter().enumerate() {
                let mut keys = slot_398300935_keys();
                keys[13] = token_program_id(token_program).to_owned();
                let accounts = match kind {
                    TokenSupplyChangeKindV1::Mint => vec![11, 5, 0],
                    TokenSupplyChangeKindV1::Burn => vec![5, 11, 0],
                };
                let raw = json!({
                    "programIdIndex": 13,
                    "accounts": accounts,
                    "data": bs58::encode(data).into_string(),
                });
                let ParsedTokenCpiV1::SupplyChange { evidence } =
                    parse_compiled_token_instruction_v1(
                        order(2, Some(u32::try_from(index).unwrap())),
                        &raw,
                        &keys,
                    )
                    .unwrap()
                else {
                    panic!("expected exact supply-change evidence");
                };
                assert_eq!(evidence.token_program, token_program);
                assert_eq!(evidence.kind, *kind);
                assert_eq!(evidence.amount.as_str(), *amount);
                assert_eq!(evidence.decimals, *decimals);
                assert_eq!(evidence.mint, INPUT_MINT);
                assert_eq!(evidence.token_account, USER);
                assert_eq!(evidence.authority, AUTHORITY);
                assert_eq!(evidence.raw_data_sha256, *hash);
                assert_eq!(
                    evidence.variant,
                    match (token_program, index) {
                        (TokenProgramKindV1::LegacyToken, 0) =>
                            TokenSupplyChangeVariantV1::LegacyMintTo,
                        (TokenProgramKindV1::LegacyToken, 1) =>
                            TokenSupplyChangeVariantV1::LegacyMintToChecked,
                        (TokenProgramKindV1::LegacyToken, 2) =>
                            TokenSupplyChangeVariantV1::LegacyBurn,
                        (TokenProgramKindV1::LegacyToken, 3) =>
                            TokenSupplyChangeVariantV1::LegacyBurnChecked,
                        (TokenProgramKindV1::Token2022, 0) =>
                            TokenSupplyChangeVariantV1::Token2022MintTo,
                        (TokenProgramKindV1::Token2022, 1) =>
                            TokenSupplyChangeVariantV1::Token2022MintToChecked,
                        (TokenProgramKindV1::Token2022, 2) =>
                            TokenSupplyChangeVariantV1::Token2022Burn,
                        (TokenProgramKindV1::Token2022, 3) =>
                            TokenSupplyChangeVariantV1::Token2022BurnChecked,
                        _ => unreachable!(),
                    }
                );
            }
        }
    }

    #[test]
    fn malformed_unknown_and_non_transfer_are_distinct() {
        let keys = slot_398300935_keys();
        let malformed = json!({"programIdIndex":13,"accounts":[5,3,0],"data":bs58::encode([3_u8]).into_string()});
        assert!(matches!(
            parse_compiled_token_instruction_v1(order(2, Some(0)), &malformed, &keys),
            Err(TokenCpiError::MalformedInstruction { .. })
        ));
        let unknown =
            json!({"programIdIndex":13,"accounts":[],"data":bs58::encode([255_u8]).into_string()});
        assert!(matches!(
            parse_compiled_token_instruction_v1(order(2, Some(0)), &unknown, &keys),
            Err(TokenCpiError::UnknownInstruction { .. })
        ));
        let revoke =
            json!({"programIdIndex":13,"accounts":[5,0],"data":bs58::encode([5_u8]).into_string()});
        assert!(matches!(
            parse_compiled_token_instruction_v1(order(2, Some(0)), &revoke, &keys).unwrap(),
            ParsedTokenCpiV1::RecognizedNonTransfer { .. }
        ));
        let sync_native =
            json!({"programIdIndex":13,"accounts":[5],"data":bs58::encode([17_u8]).into_string()});
        assert!(matches!(
            parse_compiled_token_instruction_v1(order(2, Some(0)), &sync_native, &keys),
            Err(TokenCpiError::UnsupportedValueMovement { .. })
        ));
        let initialize_account = json!({"programIdIndex":13,"accounts":[5,11,0,16],"data":bs58::encode([1_u8]).into_string()});
        assert!(matches!(
            parse_compiled_token_instruction_v1(order(2, Some(0)), &initialize_account, &keys,)
                .unwrap(),
            ParsedTokenCpiV1::RecognizedNonTransfer { .. }
        ));
        let mut native_keys = keys.clone();
        native_keys[11] = NATIVE_MINT_ID.to_owned();
        assert!(matches!(
            parse_compiled_token_instruction_v1(
                order(2, Some(0)),
                &initialize_account,
                &native_keys,
            ),
            Err(TokenCpiError::RequiresExternalCustodyState { .. })
        ));
    }

    #[test]
    fn native_adjustments_require_typed_exact_proof_and_closed_arithmetic() {
        let mut keys = slot_398300935_keys();
        keys[11] = NATIVE_MINT_ID.to_owned();
        let sync = json!({
            "programIdIndex": 13,
            "accounts": [5],
            "data": bs58::encode([17_u8]).into_string(),
        });
        assert!(matches!(
            parse_compiled_token_instruction_v1(order(2, Some(0)), &sync, &keys),
            Err(TokenCpiError::UnsupportedValueMovement { .. })
        ));
        let candidate =
            parse_deferred_token_account_adjustment_v1(order(2, Some(0)), &sync, &keys).unwrap();
        let exact = ExactTokenAccountAdjustmentV1 {
            reason: TokenAccountAdjustmentReasonV1::SyncNative,
            source: TokenAccountAdjustmentSourceV1::RpcTokenBalanceResidual {
                transaction_pre_amount: token_amount(0),
                transaction_post_amount: token_amount(0),
                other_exact_delta: SignedTokenAmount::new("-42").unwrap(),
                certified_zero_external_cpis: Vec::new(),
            },
            token_account: USER.to_owned(),
            mint: NATIVE_MINT_ID.to_owned(),
            decimals: 9,
            signed_amount: SignedTokenAmount::new("42").unwrap(),
        };
        let parsed =
            resolve_deferred_token_account_adjustment_v1(candidate.clone(), exact.clone()).unwrap();
        let ParsedTokenCpiV1::RecognizedNonTransfer { evidence } = parsed else {
            panic!("resolved SyncNative must remain explicitly non-transfer");
        };
        assert_eq!(evidence.exact_account_adjustment, Some(exact.clone()));

        let mut wrong = exact;
        wrong.signed_amount = SignedTokenAmount::new("41").unwrap();
        assert!(matches!(
            resolve_deferred_token_account_adjustment_v1(candidate, wrong),
            Err(TokenCpiError::InvalidAccountAdjustment(_))
        ));

        let mut initialize_data = vec![18_u8];
        initialize_data.extend_from_slice(Pubkey::from_str(AUTHORITY).unwrap().as_ref());
        let initialize_native = json!({
            "programIdIndex": 13,
            "accounts": [5, 11],
            "data": bs58::encode(initialize_data).into_string(),
        });
        assert!(matches!(
            parse_compiled_token_instruction_v1(order(2, Some(1)), &initialize_native, &keys,),
            Err(TokenCpiError::RequiresExternalCustodyState { .. })
        ));
        let initialize_candidate = parse_deferred_token_account_adjustment_v1(
            order(2, Some(1)),
            &initialize_native,
            &keys,
        )
        .unwrap();
        let initialization = ExactTokenAccountAdjustmentV1 {
            reason: TokenAccountAdjustmentReasonV1::NativeAccountInitialization,
            source: TokenAccountAdjustmentSourceV1::RpcTokenBalanceResidual {
                transaction_pre_amount: token_amount(0),
                transaction_post_amount: token_amount(100),
                other_exact_delta: SignedTokenAmount::new("100").unwrap(),
                certified_zero_external_cpis: Vec::new(),
            },
            token_account: USER.to_owned(),
            mint: NATIVE_MINT_ID.to_owned(),
            decimals: 9,
            signed_amount: SignedTokenAmount::zero(),
        };
        assert!(
            resolve_deferred_token_account_adjustment_v1(initialize_candidate, initialization,)
                .is_ok()
        );

        let close = json!({
            "programIdIndex": 13,
            "accounts": [5, 3, 0],
            "data": bs58::encode([9_u8]).into_string(),
        });
        assert!(matches!(
            parse_compiled_token_instruction_v1(order(2, Some(2)), &close, &keys),
            Err(TokenCpiError::RequiresExternalCustodyState { .. })
        ));
        assert!(
            parse_deferred_token_account_adjustment_v1(order(2, Some(2)), &close, &keys,).is_err()
        );
    }

    fn frame(
        order: ChainOrder,
        program_id: &str,
        is_target_program: bool,
        height: u32,
        parent_order: Option<ChainOrder>,
        path: Vec<u32>,
    ) -> InstructionExecutionFrameV1 {
        InstructionExecutionFrameV1 {
            schema_version: DECODE_SCHEMA_VERSION,
            order,
            program_id: program_id.to_owned(),
            is_target_program,
            stack_height: Some(height),
            parent_order,
            execution_path: path,
            provenance: CpiFrameProvenanceV1::Complete,
            decoded_target_class: is_target_program
                .then_some(DecodedInstructionClass::StateChanging),
        }
    }

    fn provenance(frames: Vec<InstructionExecutionFrameV1>) -> TransactionExecutionProvenanceV1 {
        TransactionExecutionProvenanceV1 {
            schema_version: DECODE_SCHEMA_VERSION,
            decoder_version_id: "test".to_owned(),
            signature: "test-signature".to_owned(),
            slot: 398_300_935,
            transaction_index: 271,
            frames,
            token_cpis: Vec::new(),
        }
    }

    fn parsed_transfer(
        transfer_order: ChainOrder,
        source: &str,
        destination: &str,
        amount: u64,
    ) -> ParsedTokenCpiV1 {
        ParsedTokenCpiV1::Transfer {
            evidence: TokenTransferCpiV1 {
                schema_version: TOKEN_CPI_SCHEMA_VERSION,
                order: transfer_order,
                token_program: TokenProgramKindV1::LegacyToken,
                variant: TokenTransferVariantV1::LegacyTransferChecked,
                source_token_account: source.to_owned(),
                destination_token_account: destination.to_owned(),
                mint: Some(INPUT_MINT.to_owned()),
                authority: AUTHORITY.to_owned(),
                ordered_accounts: vec![
                    source.to_owned(),
                    INPUT_MINT.to_owned(),
                    destination.to_owned(),
                    AUTHORITY.to_owned(),
                ],
                gross_amount: token_amount(amount),
                decimals: Some(6),
                explicit_fee: None,
                raw_data_sha256: "00".repeat(32),
            },
        }
    }

    fn expectation(
        transfer_order: ChainOrder,
        source: &str,
        destination: &str,
        amount: u64,
    ) -> ExpectedTokenTransferV1 {
        ExpectedTokenTransferV1 {
            schema_version: TOKEN_CPI_SCHEMA_VERSION,
            transfer_order,
            token_program: TokenProgramKindV1::LegacyToken,
            source_token_account: source.to_owned(),
            destination_token_account: destination.to_owned(),
            mint: INPUT_MINT.to_owned(),
            authority: AUTHORITY.to_owned(),
            expected_gross_amount: Some(token_amount(amount)),
            expected_decimals: Some(6),
            resolved_fee: None,
        }
    }

    fn parsed_supply_change(
        supply_change_order: ChainOrder,
        kind: TokenSupplyChangeKindV1,
        amount: u64,
    ) -> ParsedTokenCpiV1 {
        let (variant, ordered_accounts) = match kind {
            TokenSupplyChangeKindV1::Mint => (
                TokenSupplyChangeVariantV1::LegacyMintToChecked,
                vec![INPUT_MINT.to_owned(), USER.to_owned(), AUTHORITY.to_owned()],
            ),
            TokenSupplyChangeKindV1::Burn => (
                TokenSupplyChangeVariantV1::LegacyBurnChecked,
                vec![USER.to_owned(), INPUT_MINT.to_owned(), AUTHORITY.to_owned()],
            ),
        };
        ParsedTokenCpiV1::SupplyChange {
            evidence: TokenSupplyChangeCpiV1 {
                schema_version: TOKEN_CPI_SCHEMA_VERSION,
                order: supply_change_order,
                token_program: TokenProgramKindV1::LegacyToken,
                variant,
                kind,
                token_account: USER.to_owned(),
                mint: INPUT_MINT.to_owned(),
                authority: AUTHORITY.to_owned(),
                ordered_accounts,
                amount: token_amount(amount),
                decimals: Some(6),
                raw_data_sha256: "22".repeat(32),
            },
        }
    }

    fn expected_supply_change(
        supply_change_order: ChainOrder,
        kind: TokenSupplyChangeKindV1,
        amount: u64,
    ) -> ExpectedTokenSupplyChangeV1 {
        ExpectedTokenSupplyChangeV1 {
            schema_version: TOKEN_CPI_SCHEMA_VERSION,
            supply_change_order,
            token_program: TokenProgramKindV1::LegacyToken,
            kind,
            token_account: USER.to_owned(),
            mint: INPUT_MINT.to_owned(),
            authority: AUTHORITY.to_owned(),
            expected_amount: Some(token_amount(amount)),
            expected_decimals: Some(6),
        }
    }

    #[test]
    fn add_and_remove_liquidity_supply_changes_are_scoped_and_signed() {
        let add = order(0, None);
        let mint = order(0, Some(0));
        let remove = order(1, None);
        let burn = order(1, Some(0));
        let provenance = provenance(vec![
            frame(add, V1_PROGRAM_ID, true, 1, None, vec![0]),
            frame(mint, SPL_TOKEN_PROGRAM_ID, false, 2, Some(add), vec![0, 0]),
            frame(remove, V1_PROGRAM_ID, true, 1, None, vec![1]),
            frame(
                burn,
                SPL_TOKEN_PROGRAM_ID,
                false,
                2,
                Some(remove),
                vec![1, 0],
            ),
        ]);
        let parsed = vec![
            parsed_supply_change(mint, TokenSupplyChangeKindV1::Mint, 1_000),
            parsed_supply_change(burn, TokenSupplyChangeKindV1::Burn, 400),
        ];

        let minted = aggregate_exact_operation_token_deltas_v1(
            &provenance,
            add,
            &parsed,
            &[],
            &[expected_supply_change(
                mint,
                TokenSupplyChangeKindV1::Mint,
                1_000,
            )],
        )
        .unwrap();
        assert_eq!(minted.matched_supply_changes.len(), 1);
        assert_eq!(minted.matched_supply_changes[0].amount.as_str(), "1000");
        assert_eq!(minted.account_deltas.len(), 1);
        assert_eq!(minted.account_deltas[0].signed_amount.as_str(), "1000");

        let burned = aggregate_exact_operation_token_deltas_v1(
            &provenance,
            remove,
            &parsed,
            &[],
            &[expected_supply_change(
                burn,
                TokenSupplyChangeKindV1::Burn,
                400,
            )],
        )
        .unwrap();
        assert_eq!(burned.matched_supply_changes.len(), 1);
        assert_eq!(burned.matched_supply_changes[0].amount.as_str(), "400");
        assert_eq!(burned.account_deltas.len(), 1);
        assert_eq!(burned.account_deltas[0].signed_amount.as_str(), "-400");
    }

    #[test]
    fn unchecked_supply_change_accepts_independently_resolved_decimals() {
        let target = order(0, None);
        let mint = order(0, Some(0));
        let provenance = provenance(vec![
            frame(target, V1_PROGRAM_ID, true, 1, None, vec![0]),
            frame(
                mint,
                SPL_TOKEN_PROGRAM_ID,
                false,
                2,
                Some(target),
                vec![0, 0],
            ),
        ]);
        let mut parsed = parsed_supply_change(mint, TokenSupplyChangeKindV1::Mint, 1_000);
        let ParsedTokenCpiV1::SupplyChange { evidence } = &mut parsed else {
            unreachable!("fixture is a supply change")
        };
        evidence.variant = TokenSupplyChangeVariantV1::LegacyMintTo;
        evidence.decimals = None;

        let result = aggregate_exact_operation_token_deltas_v1(
            &provenance,
            target,
            &[parsed],
            &[],
            &[expected_supply_change(
                mint,
                TokenSupplyChangeKindV1::Mint,
                1_000,
            )],
        )
        .unwrap();
        assert_eq!(result.matched_supply_changes.len(), 1);
        assert_eq!(result.matched_supply_changes[0].decimals, None);
        assert_eq!(result.account_deltas[0].signed_amount.as_str(), "1000");
    }

    #[test]
    fn supply_change_amount_mismatch_fails_before_delta_application() {
        let target = order(0, None);
        let mint = order(0, Some(0));
        let provenance = provenance(vec![
            frame(target, V1_PROGRAM_ID, true, 1, None, vec![0]),
            frame(
                mint,
                SPL_TOKEN_PROGRAM_ID,
                false,
                2,
                Some(target),
                vec![0, 0],
            ),
        ]);
        assert_eq!(
            aggregate_exact_operation_token_deltas_v1(
                &provenance,
                target,
                &[parsed_supply_change(
                    mint,
                    TokenSupplyChangeKindV1::Mint,
                    1_000,
                )],
                &[],
                &[expected_supply_change(
                    mint,
                    TokenSupplyChangeKindV1::Mint,
                    999,
                )],
            ),
            Err(TokenCpiError::ConflictingAmount(mint))
        );
    }

    #[test]
    fn multiple_v1_operations_may_share_accounts_without_transaction_wide_ambiguity() {
        let first = order(0, None);
        let first_transfer = order(0, Some(0));
        let second = order(1, None);
        let second_transfer = order(1, Some(0));
        let provenance = provenance(vec![
            frame(first, V1_PROGRAM_ID, true, 1, None, vec![0]),
            frame(
                first_transfer,
                SPL_TOKEN_PROGRAM_ID,
                false,
                2,
                Some(first),
                vec![0, 0],
            ),
            frame(second, V1_PROGRAM_ID, true, 1, None, vec![1]),
            frame(
                second_transfer,
                SPL_TOKEN_PROGRAM_ID,
                false,
                2,
                Some(second),
                vec![1, 0],
            ),
        ]);
        let parsed = vec![
            parsed_transfer(first_transfer, USER, INPUT_VAULT, 100),
            parsed_transfer(second_transfer, USER, INPUT_VAULT, 200),
        ];
        let result = aggregate_exact_operation_token_deltas_v1(
            &provenance,
            first,
            &parsed,
            &[expectation(first_transfer, USER, INPUT_VAULT, 100)],
            &[],
        )
        .unwrap();
        assert_eq!(result.matched_transfers.len(), 1);
        assert_eq!(result.matched_transfers[0].gross_amount.as_str(), "100");
        assert_eq!(
            result
                .account_deltas
                .iter()
                .find(|row| row.token_account == USER)
                .unwrap()
                .signed_amount
                .as_str(),
            "-100"
        );
    }

    #[test]
    fn routed_outer_touches_are_excluded_by_verified_subtree() {
        let router = order(0, None);
        let target = order(0, Some(0));
        let target_transfer = order(0, Some(1));
        let harmless_token_call = order(0, Some(2));
        let sibling_transfer = order(0, Some(3));
        let provenance = provenance(vec![
            frame(router, ROUTER_ID, false, 1, None, vec![0]),
            frame(target, V1_PROGRAM_ID, true, 2, Some(router), vec![0, 0]),
            frame(
                target_transfer,
                SPL_TOKEN_PROGRAM_ID,
                false,
                3,
                Some(target),
                vec![0, 0, 0],
            ),
            frame(
                harmless_token_call,
                SPL_TOKEN_PROGRAM_ID,
                false,
                3,
                Some(target),
                vec![0, 0, 1],
            ),
            frame(
                sibling_transfer,
                SPL_TOKEN_PROGRAM_ID,
                false,
                2,
                Some(router),
                vec![0, 1],
            ),
        ]);
        let parsed = vec![
            parsed_transfer(target_transfer, USER, INPUT_VAULT, 100),
            ParsedTokenCpiV1::RecognizedNonTransfer {
                evidence: RecognizedNonTransferTokenCpiV1 {
                    schema_version: TOKEN_CPI_SCHEMA_VERSION,
                    order: harmless_token_call,
                    token_program: TokenProgramKindV1::LegacyToken,
                    top_level_discriminant: 5,
                    extension_discriminant: None,
                    ordered_accounts: vec![USER.to_owned(), AUTHORITY.to_owned()],
                    raw_data_sha256: "11".repeat(32),
                    exact_account_adjustment: None,
                },
            },
            parsed_transfer(sibling_transfer, USER, SECOND_USER, 999),
        ];
        let result = aggregate_exact_operation_token_deltas_v1(
            &provenance,
            target,
            &parsed,
            &[expectation(target_transfer, USER, INPUT_VAULT, 100)],
            &[],
        )
        .unwrap();
        assert_eq!(result.matched_transfers.len(), 1);
        assert!(result
            .account_deltas
            .iter()
            .all(|row| row.token_account != SECOND_USER));
    }

    #[test]
    fn nested_state_changing_v1_child_owns_its_token_cpis() {
        let outer = order(0, None);
        let child = order(0, Some(0));
        let child_transfer = order(0, Some(1));
        let child_mint = order(0, Some(2));
        let outer_transfer = order(0, Some(3));
        let provenance = provenance(vec![
            frame(outer, V1_PROGRAM_ID, true, 1, None, vec![0]),
            frame(child, V1_PROGRAM_ID, true, 2, Some(outer), vec![0, 0]),
            frame(
                child_transfer,
                SPL_TOKEN_PROGRAM_ID,
                false,
                3,
                Some(child),
                vec![0, 0, 0],
            ),
            frame(
                child_mint,
                SPL_TOKEN_PROGRAM_ID,
                false,
                3,
                Some(child),
                vec![0, 0, 1],
            ),
            frame(
                outer_transfer,
                SPL_TOKEN_PROGRAM_ID,
                false,
                2,
                Some(outer),
                vec![0, 1],
            ),
        ]);
        let parsed = vec![
            parsed_transfer(child_transfer, USER, INPUT_VAULT, 1_000),
            parsed_supply_change(child_mint, TokenSupplyChangeKindV1::Mint, 10),
            parsed_transfer(outer_transfer, SECOND_USER, INPUT_VAULT, 50),
        ];

        let outer_result = aggregate_exact_operation_token_deltas_v1(
            &provenance,
            outer,
            &parsed,
            &[expectation(outer_transfer, SECOND_USER, INPUT_VAULT, 50)],
            &[],
        )
        .unwrap();
        assert_eq!(outer_result.matched_transfers.len(), 1);
        assert_eq!(
            outer_result.matched_transfers[0].gross_amount.as_str(),
            "50"
        );
        assert!(outer_result.matched_supply_changes.is_empty());

        let child_result = aggregate_exact_operation_token_deltas_v1(
            &provenance,
            child,
            &parsed,
            &[expectation(child_transfer, USER, INPUT_VAULT, 1_000)],
            &[expected_supply_change(
                child_mint,
                TokenSupplyChangeKindV1::Mint,
                10,
            )],
        )
        .unwrap();
        assert_eq!(child_result.matched_transfers.len(), 1);
        assert_eq!(child_result.matched_supply_changes.len(), 1);

        assert_eq!(
            aggregate_exact_operation_token_deltas_v1(
                &provenance,
                outer,
                &parsed,
                &[expectation(child_transfer, USER, INPUT_VAULT, 1_000,)],
                &[],
            ),
            Err(TokenCpiError::ExpectedTransferOutsideSubtree(
                child_transfer
            ))
        );
        assert_eq!(
            aggregate_exact_operation_token_deltas_v1(
                &provenance,
                outer,
                &parsed,
                &[],
                &[expected_supply_change(
                    child_mint,
                    TokenSupplyChangeKindV1::Mint,
                    10,
                )],
            ),
            Err(TokenCpiError::ExpectedSupplyChangeOutsideSubtree(
                child_mint
            ))
        );
    }

    #[test]
    fn token_2022_checked_requires_external_fee_evidence() {
        let target = order(0, None);
        let transfer_order = order(0, Some(0));
        let provenance = provenance(vec![
            frame(target, V1_PROGRAM_ID, true, 1, None, vec![0]),
            frame(
                transfer_order,
                SPL_TOKEN_2022_PROGRAM_ID,
                false,
                2,
                Some(target),
                vec![0, 0],
            ),
        ]);
        let mut parsed = parsed_transfer(transfer_order, USER, INPUT_VAULT, 100);
        let ParsedTokenCpiV1::Transfer { evidence } = &mut parsed else {
            unreachable!();
        };
        evidence.token_program = TokenProgramKindV1::Token2022;
        evidence.variant = TokenTransferVariantV1::Token2022TransferChecked;
        let mut expected = expectation(transfer_order, USER, INPUT_VAULT, 100);
        expected.token_program = TokenProgramKindV1::Token2022;
        assert_eq!(
            aggregate_exact_operation_token_deltas_v1(
                &provenance,
                target,
                &[parsed.clone()],
                &[expected.clone()],
                &[],
            ),
            Err(TokenCpiError::MissingFeeEvidence(transfer_order))
        );
        expected.resolved_fee = Some(token_amount(3));
        let result = aggregate_exact_operation_token_deltas_v1(
            &provenance,
            target,
            &[parsed],
            &[expected],
            &[],
        )
        .unwrap();
        assert_eq!(
            result.matched_transfers[0].destination_credit.as_str(),
            "97"
        );
    }

    #[test]
    fn helper_fails_closed_on_missing_or_conflicting_expectation() {
        let target = order(0, None);
        let transfer_order = order(0, Some(0));
        let provenance = provenance(vec![
            frame(target, V1_PROGRAM_ID, true, 1, None, vec![0]),
            frame(
                transfer_order,
                SPL_TOKEN_PROGRAM_ID,
                false,
                2,
                Some(target),
                vec![0, 0],
            ),
        ]);
        let parsed = parsed_transfer(transfer_order, USER, INPUT_VAULT, 100);
        let expected = expectation(transfer_order, USER, INPUT_VAULT, 100);
        assert_eq!(
            aggregate_exact_operation_token_deltas_v1(
                &provenance,
                target,
                &[],
                &[expected.clone()],
                &[],
            ),
            Err(TokenCpiError::MissingParsedTokenInstruction(transfer_order))
        );
        assert_eq!(
            aggregate_exact_operation_token_deltas_v1(
                &provenance,
                target,
                &[parsed.clone()],
                &[],
                &[],
            ),
            Err(TokenCpiError::UnmatchedObservedTransfer(transfer_order))
        );
        assert_eq!(
            aggregate_exact_operation_token_deltas_v1(
                &provenance,
                target,
                &[parsed.clone(), parsed.clone()],
                &[expected.clone()],
                &[],
            ),
            Err(TokenCpiError::DuplicateParsedInstruction(transfer_order))
        );
        assert_eq!(
            aggregate_exact_operation_token_deltas_v1(
                &provenance,
                target,
                &[parsed.clone()],
                &[expected.clone(), expected.clone()],
                &[],
            ),
            Err(TokenCpiError::DuplicateExpectedTransfer(transfer_order))
        );
        let mut program_conflict = expected.clone();
        program_conflict.token_program = TokenProgramKindV1::Token2022;
        assert_eq!(
            aggregate_exact_operation_token_deltas_v1(
                &provenance,
                target,
                &[parsed.clone()],
                &[program_conflict],
                &[],
            ),
            Err(TokenCpiError::ConflictingProgram(transfer_order))
        );
        let mut expected = expected;
        expected.mint = "AKfDVR3ytkqm86aEY178aRmWwusqpt43HwNfJDf4odJu".to_owned();
        assert_eq!(
            aggregate_exact_operation_token_deltas_v1(
                &provenance,
                target,
                &[parsed],
                &[expected],
                &[],
            ),
            Err(TokenCpiError::ConflictingMint(transfer_order))
        );

        let target_expectation = expectation(target, USER, INPUT_VAULT, 100);
        assert_eq!(
            aggregate_exact_operation_token_deltas_v1(
                &provenance,
                target,
                &[parsed_transfer(transfer_order, USER, INPUT_VAULT, 100)],
                &[
                    expectation(transfer_order, USER, INPUT_VAULT, 100),
                    target_expectation,
                ],
                &[],
            ),
            Err(TokenCpiError::UnmatchedExpectedTransfer(target))
        );
    }
}
