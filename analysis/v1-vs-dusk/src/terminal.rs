//! Exact-cutoff terminal account snapshots for factual replay reconciliation.
//!
//! Standard Solana `minContextSlot` is a lower bound, not a historical read.
//! Import therefore requires provider-declared exact-slot semantics and an RPC
//! response context equal to the immutable cutoff. The validator also takes an
//! external expected binding so a self-consistent artifact from another run,
//! program, cutoff, provider, or account list cannot be substituted.

use std::{collections::BTreeSet, str::FromStr};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use solana_pubkey::Pubkey;
use thiserror::Error;

use crate::canonical::sha256_hex;

pub const TERMINAL_SNAPSHOT_SCHEMA_VERSION: u16 = 1;

const BINDING_HASH_DOMAIN: &[u8] = b"omnipair:v1-vs-dusk:terminal-binding:v1";
const ACCOUNT_SET_HASH_DOMAIN: &[u8] = b"omnipair:v1-vs-dusk:terminal-account-set:v1";

/// Sanitized provider identity. Raw RPC URLs and credentials must never enter
/// the artifact; callers should hash a normalized, secret-free endpoint ID.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalProviderIdentityV1 {
    pub schema_version: u16,
    pub provider_id: String,
    pub provider_identity_sha256: String,
    pub endpoint_fingerprint_sha256: String,
}

/// Immutable expectation supplied independently of the imported response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalSnapshotBindingV1 {
    pub schema_version: u16,
    pub run_id: String,
    pub cutoff_slot: u64,
    pub program_id: String,
    /// Must be unique, canonical pubkeys in ascending textual order.
    pub requested_accounts: Vec<String>,
    pub provider: TerminalProviderIdentityV1,
}

/// Modes are explicit so artifacts created with standard lower-bound RPC or a
/// current bank can be diagnosed and rejected rather than silently accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalReadSemanticsV1 {
    ExactHistoricalSlot,
    MinimumContextSlot,
    CurrentState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalSnapshotSourceEvidenceV1 {
    pub schema_version: u16,
    pub read_semantics: TerminalReadSemanticsV1,
    pub provider_request_id: String,
    /// SHA-256 of the complete raw provider response retained out of band.
    pub raw_response_sha256: String,
}

/// Account bytes may be carried as canonical RFC 4648 base64 or directly as a
/// JSON byte array. In either case the digest is over decoded raw bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "encoding", rename_all = "snake_case", deny_unknown_fields)]
pub enum TerminalAccountDataV1 {
    Base64 {
        data_base64: String,
        byte_length: u64,
        data_sha256: String,
    },
    Bytes {
        bytes: Vec<u8>,
        byte_length: u64,
        data_sha256: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalAccountV1 {
    pub schema_version: u16,
    pub address: String,
    pub owner: String,
    /// Canonical unsigned base-10 strings avoid JSON `u64` precision loss.
    pub lamports: String,
    pub executable: bool,
    pub rent_epoch: String,
    pub data: TerminalAccountDataV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalAccountSnapshotV1 {
    pub schema_version: u16,
    pub binding: TerminalSnapshotBindingV1,
    pub binding_sha256: String,
    /// Must equal `binding.cutoff_slot`; a later slot is not accepted.
    pub response_context_slot: u64,
    pub source: TerminalSnapshotSourceEvidenceV1,
    /// Must have the same addresses and order as `binding.requested_accounts`.
    pub accounts: Vec<TerminalAccountV1>,
    /// Domain-separated digest of the binding, source evidence, exact response
    /// slot, and every normalized account field and raw-data digest.
    pub account_set_sha256: String,
}

/// Import-ready account data returned only after every binding and integrity
/// check succeeds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedTerminalAccountV1 {
    pub address: String,
    pub owner: String,
    pub lamports: u64,
    pub executable: bool,
    pub rent_epoch: u64,
    pub data: Vec<u8>,
    pub data_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedTerminalSnapshotV1 {
    pub run_id: String,
    pub cutoff_slot: u64,
    pub program_id: String,
    pub provider: TerminalProviderIdentityV1,
    pub response_context_slot: u64,
    pub binding_sha256: String,
    pub source_raw_response_sha256: String,
    pub account_set_sha256: String,
    pub accounts: Vec<ValidatedTerminalAccountV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum TerminalSnapshotValidationError {
    #[error("unsupported {record} schema version {actual}")]
    UnsupportedSchema { record: &'static str, actual: u16 },
    #[error("required field {0} is empty")]
    EmptyField(&'static str),
    #[error("terminal provider id must be a sanitized ASCII identifier")]
    InvalidProviderId,
    #[error("{field} is not a canonical Solana pubkey: {value}")]
    InvalidPubkey { field: &'static str, value: String },
    #[error("{field} is not a canonical lowercase SHA-256 digest")]
    InvalidSha256 { field: &'static str },
    #[error("terminal snapshot requires at least one requested account")]
    EmptyAccountList,
    #[error("duplicate requested account {0}")]
    DuplicateRequestedAccount(String),
    #[error("requested accounts are not in canonical ascending order")]
    NonCanonicalRequestedAccountOrder,
    #[error("snapshot binding does not equal the independently supplied expectation")]
    BindingMismatch,
    #[error("snapshot binding digest does not match its contents")]
    BindingHashMismatch,
    #[error("read semantics {actual:?} cannot prove an exact historical state")]
    NonExactReadSemantics { actual: TerminalReadSemanticsV1 },
    #[error("response context slot {actual} does not equal cutoff {expected}")]
    WrongResponseContextSlot { expected: u64, actual: u64 },
    #[error("duplicate terminal account {0}")]
    DuplicateAccount(String),
    #[error("terminal response is missing requested account {0}")]
    MissingAccount(String),
    #[error("terminal response contains unrequested account {0}")]
    UnexpectedAccount(String),
    #[error("terminal accounts do not follow the requested account order")]
    AccountOrderMismatch,
    #[error("invalid canonical u64 string in {field} for account {address}")]
    InvalidU64String {
        field: &'static str,
        address: String,
    },
    #[error("invalid canonical base64 account data for {address}")]
    InvalidBase64 { address: String },
    #[error("raw data length mismatch for {address}: declared {declared}, actual {actual}")]
    DataLengthMismatch {
        address: String,
        declared: u64,
        actual: u64,
    },
    #[error("raw data SHA-256 mismatch for account {0}")]
    DataHashMismatch(String),
    #[error("terminal account-set digest does not match its contents")]
    AccountSetHashMismatch,
}

impl TerminalAccountSnapshotV1 {
    /// Computes both digests after validating all unsigned source material.
    /// This is intended for an extractor before writing an immutable artifact.
    pub fn seal(mut self) -> Result<Self, TerminalSnapshotValidationError> {
        let accounts = validate_snapshot_payload(&self)?;
        self.binding_sha256 = compute_terminal_binding_sha256(&self.binding)?;
        self.account_set_sha256 = compute_account_set_sha256(&self, &accounts);
        Ok(self)
    }

    /// Validates against an independently retained binding and returns decoded,
    /// typed account values suitable for a replay importer.
    pub fn validate_for_import(
        &self,
        expected: &TerminalSnapshotBindingV1,
    ) -> Result<ValidatedTerminalSnapshotV1, TerminalSnapshotValidationError> {
        validate_binding(expected)?;
        let accounts = validate_snapshot_payload(self)?;
        if &self.binding != expected {
            return Err(TerminalSnapshotValidationError::BindingMismatch);
        }
        let binding_sha256 = compute_terminal_binding_sha256(&self.binding)?;
        if self.binding_sha256 != binding_sha256 {
            return Err(TerminalSnapshotValidationError::BindingHashMismatch);
        }
        let account_set_sha256 = compute_account_set_sha256(self, &accounts);
        if self.account_set_sha256 != account_set_sha256 {
            return Err(TerminalSnapshotValidationError::AccountSetHashMismatch);
        }
        Ok(ValidatedTerminalSnapshotV1 {
            run_id: self.binding.run_id.clone(),
            cutoff_slot: self.binding.cutoff_slot,
            program_id: self.binding.program_id.clone(),
            provider: self.binding.provider.clone(),
            response_context_slot: self.response_context_slot,
            binding_sha256,
            source_raw_response_sha256: self.source.raw_response_sha256.clone(),
            account_set_sha256,
            accounts,
        })
    }
}

pub fn compute_terminal_binding_sha256(
    binding: &TerminalSnapshotBindingV1,
) -> Result<String, TerminalSnapshotValidationError> {
    validate_binding(binding)?;
    let mut digest = Sha256::new();
    digest_field(&mut digest, BINDING_HASH_DOMAIN);
    digest_u16(&mut digest, binding.schema_version);
    digest_field(&mut digest, binding.run_id.as_bytes());
    digest_u64(&mut digest, binding.cutoff_slot);
    digest_field(&mut digest, binding.program_id.as_bytes());
    digest_u64(&mut digest, binding.requested_accounts.len() as u64);
    for address in &binding.requested_accounts {
        digest_field(&mut digest, address.as_bytes());
    }
    digest_u16(&mut digest, binding.provider.schema_version);
    digest_field(&mut digest, binding.provider.provider_id.as_bytes());
    digest_field(
        &mut digest,
        binding.provider.provider_identity_sha256.as_bytes(),
    );
    digest_field(
        &mut digest,
        binding.provider.endpoint_fingerprint_sha256.as_bytes(),
    );
    Ok(hex::encode(digest.finalize()))
}

pub fn compute_terminal_account_set_sha256(
    snapshot: &TerminalAccountSnapshotV1,
) -> Result<String, TerminalSnapshotValidationError> {
    let accounts = validate_snapshot_payload(snapshot)?;
    if snapshot.binding_sha256 != compute_terminal_binding_sha256(&snapshot.binding)? {
        return Err(TerminalSnapshotValidationError::BindingHashMismatch);
    }
    Ok(compute_account_set_sha256(snapshot, &accounts))
}

fn validate_snapshot_payload(
    snapshot: &TerminalAccountSnapshotV1,
) -> Result<Vec<ValidatedTerminalAccountV1>, TerminalSnapshotValidationError> {
    validate_schema("terminal snapshot", snapshot.schema_version)?;
    validate_binding(&snapshot.binding)?;
    validate_schema("terminal source evidence", snapshot.source.schema_version)?;
    if snapshot.source.read_semantics != TerminalReadSemanticsV1::ExactHistoricalSlot {
        return Err(TerminalSnapshotValidationError::NonExactReadSemantics {
            actual: snapshot.source.read_semantics,
        });
    }
    if snapshot.source.provider_request_id.trim().is_empty() {
        return Err(TerminalSnapshotValidationError::EmptyField(
            "source.provider_request_id",
        ));
    }
    validate_sha256(
        "source.raw_response_sha256",
        &snapshot.source.raw_response_sha256,
    )?;
    if snapshot.response_context_slot != snapshot.binding.cutoff_slot {
        return Err(TerminalSnapshotValidationError::WrongResponseContextSlot {
            expected: snapshot.binding.cutoff_slot,
            actual: snapshot.response_context_slot,
        });
    }
    validate_accounts(&snapshot.binding, &snapshot.accounts)
}

fn validate_binding(
    binding: &TerminalSnapshotBindingV1,
) -> Result<(), TerminalSnapshotValidationError> {
    validate_schema("terminal binding", binding.schema_version)?;
    if binding.run_id.trim().is_empty() {
        return Err(TerminalSnapshotValidationError::EmptyField("run_id"));
    }
    validate_pubkey("program_id", &binding.program_id)?;
    validate_schema("terminal provider", binding.provider.schema_version)?;
    if !valid_sanitized_provider_id(&binding.provider.provider_id) {
        return Err(TerminalSnapshotValidationError::InvalidProviderId);
    }
    validate_sha256(
        "provider.provider_identity_sha256",
        &binding.provider.provider_identity_sha256,
    )?;
    validate_sha256(
        "provider.endpoint_fingerprint_sha256",
        &binding.provider.endpoint_fingerprint_sha256,
    )?;
    if binding.requested_accounts.is_empty() {
        return Err(TerminalSnapshotValidationError::EmptyAccountList);
    }
    let mut unique = BTreeSet::new();
    for address in &binding.requested_accounts {
        validate_pubkey("requested_accounts", address)?;
        if !unique.insert(address.as_str()) {
            return Err(TerminalSnapshotValidationError::DuplicateRequestedAccount(
                address.clone(),
            ));
        }
    }
    if binding
        .requested_accounts
        .windows(2)
        .any(|pair| pair[0] >= pair[1])
    {
        return Err(TerminalSnapshotValidationError::NonCanonicalRequestedAccountOrder);
    }
    Ok(())
}

fn validate_accounts(
    binding: &TerminalSnapshotBindingV1,
    accounts: &[TerminalAccountV1],
) -> Result<Vec<ValidatedTerminalAccountV1>, TerminalSnapshotValidationError> {
    let mut unique = BTreeSet::new();
    for account in accounts {
        validate_schema("terminal account", account.schema_version)?;
        validate_pubkey("account.address", &account.address)?;
        if !unique.insert(account.address.as_str()) {
            return Err(TerminalSnapshotValidationError::DuplicateAccount(
                account.address.clone(),
            ));
        }
    }
    let requested: BTreeSet<_> = binding
        .requested_accounts
        .iter()
        .map(String::as_str)
        .collect();
    if let Some(missing) = requested.difference(&unique).next() {
        return Err(TerminalSnapshotValidationError::MissingAccount(
            (*missing).to_owned(),
        ));
    }
    if let Some(unexpected) = unique.difference(&requested).next() {
        return Err(TerminalSnapshotValidationError::UnexpectedAccount(
            (*unexpected).to_owned(),
        ));
    }
    if accounts
        .iter()
        .map(|account| account.address.as_str())
        .ne(binding.requested_accounts.iter().map(String::as_str))
    {
        return Err(TerminalSnapshotValidationError::AccountOrderMismatch);
    }

    accounts.iter().map(validate_account).collect()
}

fn validate_account(
    account: &TerminalAccountV1,
) -> Result<ValidatedTerminalAccountV1, TerminalSnapshotValidationError> {
    validate_pubkey("account.owner", &account.owner)?;
    let lamports = parse_canonical_u64("lamports", &account.address, &account.lamports)?;
    let rent_epoch = parse_canonical_u64("rent_epoch", &account.address, &account.rent_epoch)?;
    let (data, declared_length, data_sha256) = match &account.data {
        TerminalAccountDataV1::Base64 {
            data_base64,
            byte_length,
            data_sha256,
        } => (
            decode_standard_base64(data_base64).map_err(|()| {
                TerminalSnapshotValidationError::InvalidBase64 {
                    address: account.address.clone(),
                }
            })?,
            *byte_length,
            data_sha256,
        ),
        TerminalAccountDataV1::Bytes {
            bytes,
            byte_length,
            data_sha256,
        } => (bytes.clone(), *byte_length, data_sha256),
    };
    validate_sha256("account.data.data_sha256", data_sha256)?;
    let actual_length = u64::try_from(data.len()).map_err(|_| {
        TerminalSnapshotValidationError::DataLengthMismatch {
            address: account.address.clone(),
            declared: declared_length,
            actual: u64::MAX,
        }
    })?;
    if declared_length != actual_length {
        return Err(TerminalSnapshotValidationError::DataLengthMismatch {
            address: account.address.clone(),
            declared: declared_length,
            actual: actual_length,
        });
    }
    if *data_sha256 != sha256_hex(&data) {
        return Err(TerminalSnapshotValidationError::DataHashMismatch(
            account.address.clone(),
        ));
    }
    Ok(ValidatedTerminalAccountV1 {
        address: account.address.clone(),
        owner: account.owner.clone(),
        lamports,
        executable: account.executable,
        rent_epoch,
        data,
        data_sha256: data_sha256.clone(),
    })
}

fn compute_account_set_sha256(
    snapshot: &TerminalAccountSnapshotV1,
    accounts: &[ValidatedTerminalAccountV1],
) -> String {
    let mut digest = Sha256::new();
    digest_field(&mut digest, ACCOUNT_SET_HASH_DOMAIN);
    digest_u16(&mut digest, snapshot.schema_version);
    digest_field(&mut digest, snapshot.binding_sha256.as_bytes());
    digest_u16(&mut digest, snapshot.source.schema_version);
    digest_u8(
        &mut digest,
        match snapshot.source.read_semantics {
            TerminalReadSemanticsV1::ExactHistoricalSlot => 0,
            TerminalReadSemanticsV1::MinimumContextSlot => 1,
            TerminalReadSemanticsV1::CurrentState => 2,
        },
    );
    digest_field(&mut digest, snapshot.source.provider_request_id.as_bytes());
    digest_field(&mut digest, snapshot.source.raw_response_sha256.as_bytes());
    digest_u64(&mut digest, snapshot.response_context_slot);
    digest_u64(&mut digest, accounts.len() as u64);
    for account in accounts {
        digest_field(&mut digest, account.address.as_bytes());
        digest_field(&mut digest, account.owner.as_bytes());
        digest_u64(&mut digest, account.lamports);
        digest_u8(&mut digest, u8::from(account.executable));
        digest_u64(&mut digest, account.rent_epoch);
        digest_u64(&mut digest, account.data.len() as u64);
        digest_field(&mut digest, account.data_sha256.as_bytes());
    }
    hex::encode(digest.finalize())
}

fn validate_schema(
    record: &'static str,
    actual: u16,
) -> Result<(), TerminalSnapshotValidationError> {
    if actual != TERMINAL_SNAPSHOT_SCHEMA_VERSION {
        return Err(TerminalSnapshotValidationError::UnsupportedSchema { record, actual });
    }
    Ok(())
}

fn validate_pubkey(
    field: &'static str,
    value: &str,
) -> Result<(), TerminalSnapshotValidationError> {
    let parsed =
        Pubkey::from_str(value).map_err(|_| TerminalSnapshotValidationError::InvalidPubkey {
            field,
            value: value.to_owned(),
        })?;
    if parsed.to_string() != value {
        return Err(TerminalSnapshotValidationError::InvalidPubkey {
            field,
            value: value.to_owned(),
        });
    }
    Ok(())
}

fn validate_sha256(
    field: &'static str,
    value: &str,
) -> Result<(), TerminalSnapshotValidationError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(TerminalSnapshotValidationError::InvalidSha256 { field });
    }
    Ok(())
}

fn valid_sanitized_provider_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn parse_canonical_u64(
    field: &'static str,
    address: &str,
    value: &str,
) -> Result<u64, TerminalSnapshotValidationError> {
    if value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return Err(TerminalSnapshotValidationError::InvalidU64String {
            field,
            address: address.to_owned(),
        });
    }
    value
        .parse()
        .map_err(|_| TerminalSnapshotValidationError::InvalidU64String {
            field,
            address: address.to_owned(),
        })
}

fn digest_field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
}

fn digest_u64(digest: &mut Sha256, value: u64) {
    digest.update(value.to_be_bytes());
}

fn digest_u16(digest: &mut Sha256, value: u16) {
    digest.update(value.to_be_bytes());
}

fn digest_u8(digest: &mut Sha256, value: u8) {
    digest.update([value]);
}

fn decode_standard_base64(input: &str) -> Result<Vec<u8>, ()> {
    if input.is_empty() {
        return Ok(Vec::new());
    }
    let bytes = input.as_bytes();
    if bytes.len() % 4 != 0 {
        return Err(());
    }
    let mut output = Vec::with_capacity((bytes.len() / 4).saturating_mul(3));
    for (chunk_index, chunk) in bytes.chunks_exact(4).enumerate() {
        let is_last = chunk_index + 1 == bytes.len() / 4;
        let a = base64_value(chunk[0]).ok_or(())?;
        let b = base64_value(chunk[1]).ok_or(())?;
        let c_padding = chunk[2] == b'=';
        let d_padding = chunk[3] == b'=';
        if c_padding && !d_padding {
            return Err(());
        }
        if (c_padding || d_padding) && !is_last {
            return Err(());
        }
        let c = if c_padding {
            0
        } else {
            base64_value(chunk[2]).ok_or(())?
        };
        let d = if d_padding {
            0
        } else {
            base64_value(chunk[3]).ok_or(())?
        };
        output.push((a << 2) | (b >> 4));
        if c_padding {
            if b & 0x0f != 0 {
                return Err(());
            }
            continue;
        }
        output.push((b << 4) | (c >> 2));
        if d_padding {
            if c & 0x03 != 0 {
                return Err(());
            }
            continue;
        }
        output.push((c << 6) | d);
    }
    Ok(output)
}

fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pubkey(seed: u8) -> String {
        Pubkey::new_from_array([seed; 32]).to_string()
    }

    fn fixture() -> (TerminalSnapshotBindingV1, TerminalAccountSnapshotV1) {
        let owner = pubkey(9);
        let mut requested_accounts = vec![pubkey(1), pubkey(2)];
        requested_accounts.sort();
        let binding = TerminalSnapshotBindingV1 {
            schema_version: TERMINAL_SNAPSHOT_SCHEMA_VERSION,
            run_id: "run-fixture".to_owned(),
            cutoff_slot: 42,
            program_id: pubkey(7),
            requested_accounts: requested_accounts.clone(),
            provider: TerminalProviderIdentityV1 {
                schema_version: TERMINAL_SNAPSHOT_SCHEMA_VERSION,
                provider_id: "archive-fixture".to_owned(),
                provider_identity_sha256: "11".repeat(32),
                endpoint_fingerprint_sha256: "22".repeat(32),
            },
        };
        let mut accounts = vec![
            TerminalAccountV1 {
                schema_version: TERMINAL_SNAPSHOT_SCHEMA_VERSION,
                address: requested_accounts[0].clone(),
                owner: owner.clone(),
                lamports: "123".to_owned(),
                executable: false,
                rent_epoch: "18446744073709551615".to_owned(),
                data: TerminalAccountDataV1::Base64 {
                    data_base64: "AQID".to_owned(),
                    byte_length: 3,
                    data_sha256: sha256_hex(&[1, 2, 3]),
                },
            },
            TerminalAccountV1 {
                schema_version: TERMINAL_SNAPSHOT_SCHEMA_VERSION,
                address: requested_accounts[1].clone(),
                owner,
                lamports: "0".to_owned(),
                executable: true,
                rent_epoch: "9".to_owned(),
                data: TerminalAccountDataV1::Bytes {
                    bytes: vec![4, 5],
                    byte_length: 2,
                    data_sha256: sha256_hex(&[4, 5]),
                },
            },
        ];
        accounts.sort_by(|left, right| left.address.cmp(&right.address));
        let snapshot = TerminalAccountSnapshotV1 {
            schema_version: TERMINAL_SNAPSHOT_SCHEMA_VERSION,
            binding: binding.clone(),
            binding_sha256: String::new(),
            response_context_slot: binding.cutoff_slot,
            source: TerminalSnapshotSourceEvidenceV1 {
                schema_version: TERMINAL_SNAPSHOT_SCHEMA_VERSION,
                read_semantics: TerminalReadSemanticsV1::ExactHistoricalSlot,
                provider_request_id: "request-42".to_owned(),
                raw_response_sha256: "33".repeat(32),
            },
            accounts,
            account_set_sha256: String::new(),
        }
        .seal()
        .unwrap();
        (binding, snapshot)
    }

    #[test]
    fn exact_cutoff_fixture_is_import_ready() {
        let (binding, snapshot) = fixture();
        let validated = snapshot.validate_for_import(&binding).unwrap();
        assert_eq!(validated.cutoff_slot, 42);
        assert_eq!(validated.response_context_slot, 42);
        assert_eq!(validated.accounts.len(), 2);
        assert_eq!(validated.accounts[0].data, vec![1, 2, 3]);
        assert_eq!(validated.accounts[0].rent_epoch, u64::MAX);
        assert_eq!(validated.accounts[1].data, vec![4, 5]);
        assert_eq!(snapshot.binding_sha256.len(), 64);
        assert_eq!(snapshot.account_set_sha256.len(), 64);

        let json = serde_json::to_value(&snapshot).unwrap();
        assert!(json["accounts"][0]["lamports"].is_string());
        assert!(json["accounts"][0]["rent_epoch"].is_string());
    }

    #[test]
    fn wrong_slot_minimum_context_and_current_state_are_rejected() {
        let (binding, snapshot) = fixture();

        let mut wrong_slot = snapshot.clone();
        wrong_slot.response_context_slot += 1;
        assert_eq!(
            wrong_slot.validate_for_import(&binding).unwrap_err(),
            TerminalSnapshotValidationError::WrongResponseContextSlot {
                expected: 42,
                actual: 43,
            }
        );

        for semantics in [
            TerminalReadSemanticsV1::MinimumContextSlot,
            TerminalReadSemanticsV1::CurrentState,
        ] {
            let mut substituted = snapshot.clone();
            substituted.source.read_semantics = semantics;
            assert_eq!(
                substituted.validate_for_import(&binding).unwrap_err(),
                TerminalSnapshotValidationError::NonExactReadSemantics { actual: semantics }
            );
        }
    }

    #[test]
    fn missing_duplicate_unexpected_and_reordered_accounts_are_rejected() {
        let (binding, snapshot) = fixture();

        let mut missing = snapshot.clone();
        let removed = missing.accounts.pop().unwrap();
        assert_eq!(
            missing.validate_for_import(&binding).unwrap_err(),
            TerminalSnapshotValidationError::MissingAccount(removed.address)
        );

        let mut duplicate = snapshot.clone();
        duplicate.accounts[1] = duplicate.accounts[0].clone();
        assert!(matches!(
            duplicate.validate_for_import(&binding),
            Err(TerminalSnapshotValidationError::DuplicateAccount(_))
        ));

        let mut unexpected = snapshot.clone();
        let mut extra = unexpected.accounts[1].clone();
        extra.address = pubkey(8);
        unexpected.accounts.push(extra);
        assert!(matches!(
            unexpected.validate_for_import(&binding),
            Err(TerminalSnapshotValidationError::UnexpectedAccount(_))
        ));

        let mut reordered = snapshot;
        reordered.accounts.swap(0, 1);
        assert_eq!(
            reordered.validate_for_import(&binding).unwrap_err(),
            TerminalSnapshotValidationError::AccountOrderMismatch
        );
    }

    #[test]
    fn external_binding_rejects_cross_run_cutoff_program_list_and_provider_substitution() {
        let (binding, snapshot) = fixture();
        let mut alternatives = Vec::new();

        let mut changed = binding.clone();
        changed.run_id = "other-run".to_owned();
        alternatives.push(changed);
        let mut changed = binding.clone();
        changed.cutoff_slot += 1;
        alternatives.push(changed);
        let mut changed = binding.clone();
        changed.program_id = pubkey(6);
        alternatives.push(changed);
        let mut changed = binding.clone();
        changed.requested_accounts[0] = pubkey(3);
        changed.requested_accounts.sort();
        alternatives.push(changed);
        let mut changed = binding;
        changed.provider.provider_id = "other-provider".to_owned();
        alternatives.push(changed);

        for expected in alternatives {
            assert_eq!(
                snapshot.validate_for_import(&expected).unwrap_err(),
                TerminalSnapshotValidationError::BindingMismatch
            );
        }
    }

    #[test]
    fn property_style_field_tampering_breaks_data_or_account_set_hash() {
        let (binding, snapshot) = fixture();
        let mut variants = Vec::new();

        let mut changed = snapshot.clone();
        changed.accounts[0].owner = pubkey(5);
        variants.push(changed);
        let mut changed = snapshot.clone();
        changed.accounts[0].lamports = "124".to_owned();
        variants.push(changed);
        let mut changed = snapshot.clone();
        changed.accounts[0].executable = true;
        variants.push(changed);
        let mut changed = snapshot.clone();
        changed.accounts[0].rent_epoch = "8".to_owned();
        variants.push(changed);

        for changed in variants {
            assert_eq!(
                changed.validate_for_import(&binding).unwrap_err(),
                TerminalSnapshotValidationError::AccountSetHashMismatch
            );
        }

        let mut changed_data = snapshot;
        if let TerminalAccountDataV1::Base64 { data_base64, .. } =
            &mut changed_data.accounts[0].data
        {
            *data_base64 = "AQIE".to_owned();
        }
        assert!(matches!(
            changed_data.validate_for_import(&binding),
            Err(TerminalSnapshotValidationError::DataHashMismatch(_))
        ));
    }

    #[test]
    fn malformed_base64_lengths_digests_and_integer_strings_are_rejected() {
        let (binding, snapshot) = fixture();

        let mut malformed = snapshot.clone();
        if let TerminalAccountDataV1::Base64 { data_base64, .. } = &mut malformed.accounts[0].data {
            *data_base64 = "AR==".to_owned();
        }
        assert!(matches!(
            malformed.validate_for_import(&binding),
            Err(TerminalSnapshotValidationError::InvalidBase64 { .. })
        ));

        let mut wrong_length = snapshot.clone();
        if let TerminalAccountDataV1::Bytes { byte_length, .. } = &mut wrong_length.accounts[1].data
        {
            *byte_length = 3;
        }
        assert!(matches!(
            wrong_length.validate_for_import(&binding),
            Err(TerminalSnapshotValidationError::DataLengthMismatch { .. })
        ));

        let mut noncanonical_integer = snapshot.clone();
        noncanonical_integer.accounts[0].lamports = "0123".to_owned();
        assert!(matches!(
            noncanonical_integer.validate_for_import(&binding),
            Err(TerminalSnapshotValidationError::InvalidU64String {
                field: "lamports",
                ..
            })
        ));

        let mut bad_digest = snapshot;
        bad_digest.source.raw_response_sha256 = "AA".repeat(32);
        assert_eq!(
            bad_digest.validate_for_import(&binding).unwrap_err(),
            TerminalSnapshotValidationError::InvalidSha256 {
                field: "source.raw_response_sha256"
            }
        );
    }

    #[test]
    fn duplicate_or_noncanonical_requested_lists_are_rejected() {
        let (binding, snapshot) = fixture();

        let mut duplicate = binding.clone();
        duplicate.requested_accounts[1] = duplicate.requested_accounts[0].clone();
        assert!(matches!(
            snapshot.validate_for_import(&duplicate),
            Err(TerminalSnapshotValidationError::DuplicateRequestedAccount(
                _
            ))
        ));

        let mut reversed = binding;
        reversed.requested_accounts.reverse();
        assert_eq!(
            snapshot.validate_for_import(&reversed).unwrap_err(),
            TerminalSnapshotValidationError::NonCanonicalRequestedAccountOrder
        );
    }

    #[test]
    fn provider_identity_cannot_embed_an_rpc_url_or_credentials() {
        let (mut binding, mut snapshot) = fixture();
        binding.provider.provider_id = "https://archive.example/?key=secret".to_owned();
        snapshot.binding = binding.clone();
        assert_eq!(
            snapshot.validate_for_import(&binding),
            Err(TerminalSnapshotValidationError::InvalidProviderId)
        );
    }

    #[test]
    fn unknown_snapshot_fields_are_schema_drift() {
        let (_, snapshot) = fixture();
        let mut value = serde_json::to_value(snapshot).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("current_slot".to_owned(), serde_json::json!(999));
        assert!(serde_json::from_value::<TerminalAccountSnapshotV1>(value).is_err());
    }
}
