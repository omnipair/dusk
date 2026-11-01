//! Exact-cutoff terminal account reconciliation for the native V1 replay.
//!
//! This module never performs an RPC read. It accepts only a snapshot already
//! validated by [`crate::terminal`], derives the complete account set from the
//! replayed native checkpoints, decodes the version-pinned V1 account layouts,
//! and compares every replay-backed raw unit. Wallet-level position contents
//! remain in the gitignored snapshot/run artifacts; the persisted result below
//! contains only aggregate counts and cryptographic commitments.

use std::{collections::BTreeMap, str::FromStr};

use anchor_lang::{AccountDeserialize, Discriminator, Space};
use omnipair::{
    constants::{
        COLLATERAL_VAULT_SEED_PREFIX, FUTARCHY_AUTHORITY_SEED_PREFIX, PAIR_SEED_PREFIX,
        POSITION_SEED_PREFIX, RESERVE_VAULT_SEED_PREFIX, VERSION,
    },
    state::{FutarchyAuthority, Pair, RateModel, UserPosition},
};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};
use solana_pubkey::Pubkey;
use spl_token_2022::{
    extension::{
        transfer_fee::TransferFeeAmount, BaseStateWithExtensions, ExtensionType,
        StateWithExtensions,
    },
    state::{Account as TokenAccount, AccountState},
};
use thiserror::Error;

use crate::{
    terminal::{
        compute_terminal_binding_sha256, TerminalAccountSnapshotV1, TerminalProviderIdentityV1,
        TerminalSnapshotBindingV1, TerminalSnapshotValidationError, ValidatedTerminalAccountV1,
        ValidatedTerminalSnapshotV1,
    },
    transfer_fee_extract::{LEGACY_TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID},
    v1_engine::{
        V1AuthorityCheckpointV1, V1NativeCheckpointV1, V1PairCheckpointV1, V1PositionCheckpointV1,
        V1ProgramRevisionV1, V1RateModelCheckpointV1, V1_NATIVE_ENGINE_ID,
        V1_NATIVE_ENGINE_SCHEMA_VERSION,
    },
    v1_reconciliation::canonical_terminal_checkpoint_sha256,
};

pub const V1_TERMINAL_RECONCILIATION_SCHEMA_VERSION: u16 = 1;
pub const TERMINAL_BINDING_JSON: &str = "terminal-binding.json";
pub const TERMINAL_SNAPSHOT_JSON: &str = "terminal-snapshot.json";
pub const V1_TERMINAL_RECONCILIATION_JSON: &str = "v1-terminal-reconciliation.json";

const ARTIFACT_HASH_DOMAIN: &[u8] = b"omnipair:v1-vs-dusk:v1-terminal-reconciliation:v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct V1TerminalMarketReconciliationV1 {
    pub market: String,
    pub required_account_count: u64,
    pub decoded_position_count: u64,
    pub raw_unit_comparison_count: u64,
    pub terminal_checkpoint_sha256: String,
    pub all_raw_units_equal: bool,
}

/// Sanitized proof that the replayed V1 terminal state equals independently
/// sourced exact-cutoff accounts. It deliberately contains no account bytes,
/// position addresses, owners, raw URLs, or provider credentials.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct V1TerminalReconciliationArtifactV1 {
    pub schema_version: u16,
    pub run_id: String,
    pub cutoff_slot: u64,
    pub program_id: String,
    pub provider: TerminalProviderIdentityV1,
    pub binding_sha256: String,
    pub source_raw_response_sha256: String,
    pub account_set_sha256: String,
    pub market_count: u64,
    pub expected_account_count: u64,
    pub observed_account_count: u64,
    pub state_account_count: u64,
    pub token_vault_account_count: u64,
    pub decoded_position_count: u64,
    pub raw_unit_comparison_count: u64,
    pub raw_unit_mismatch_count: u64,
    pub terminal_state_verified: bool,
    pub markets: Vec<V1TerminalMarketReconciliationV1>,
    pub artifact_sha256: String,
}

impl V1TerminalReconciliationArtifactV1 {
    fn seal(mut self) -> Result<Self, V1TerminalReconciliationError> {
        self.artifact_sha256.clear();
        self.artifact_sha256 = hash_artifact(&self)?;
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), V1TerminalReconciliationError> {
        let account_partition_complete = self
            .state_account_count
            .checked_add(self.token_vault_account_count)
            == Some(self.expected_account_count);
        let market_positions = self.markets.iter().try_fold(0_u64, |total, market| {
            total.checked_add(market.decoded_position_count)
        });
        let market_comparisons = self.markets.iter().try_fold(0_u64, |total, market| {
            total.checked_add(market.raw_unit_comparison_count)
        });
        if self.schema_version != V1_TERMINAL_RECONCILIATION_SCHEMA_VERSION {
            return Err(V1TerminalReconciliationError::UnsupportedSchema(
                self.schema_version,
            ));
        }
        if self.run_id.trim().is_empty()
            || Pubkey::from_str(&self.program_id).ok() != Some(omnipair::ID)
            || self.provider.schema_version != crate::terminal::TERMINAL_SNAPSHOT_SCHEMA_VERSION
            || !valid_sanitized_provider_id(&self.provider.provider_id)
            || !valid_sha256(&self.provider.provider_identity_sha256)
            || !valid_sha256(&self.provider.endpoint_fingerprint_sha256)
            || !valid_sha256(&self.binding_sha256)
            || !valid_sha256(&self.source_raw_response_sha256)
            || !valid_sha256(&self.account_set_sha256)
            || !valid_sha256(&self.artifact_sha256)
            || self.market_count == 0
            || self.expected_account_count == 0
            || self.raw_unit_comparison_count == 0
            || self.market_count != self.markets.len() as u64
            || self.expected_account_count != self.observed_account_count
            || !account_partition_complete
            || Some(self.decoded_position_count) != market_positions
            || Some(self.raw_unit_comparison_count) != market_comparisons
            || self.raw_unit_mismatch_count != 0
            || !self.terminal_state_verified
            || self.markets.iter().any(|market| {
                !market.all_raw_units_equal
                    || Pubkey::from_str(&market.market).is_err()
                    || market.required_account_count < 7
                    || market.raw_unit_comparison_count == 0
                    || !valid_sha256(&market.terminal_checkpoint_sha256)
            })
            || self
                .markets
                .windows(2)
                .any(|markets| markets[0].market >= markets[1].market)
        {
            return Err(V1TerminalReconciliationError::InvalidArtifact);
        }
        let mut unsigned = self.clone();
        unsigned.artifact_sha256.clear();
        if self.artifact_sha256 != hash_artifact(&unsigned)? {
            return Err(V1TerminalReconciliationError::ArtifactHashMismatch);
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum V1TerminalReconciliationError {
    #[error(transparent)]
    Snapshot(#[from] TerminalSnapshotValidationError),
    #[error("unsupported terminal reconciliation schema version {0}")]
    UnsupportedSchema(u16),
    #[error("terminal reconciliation artifact is internally inconsistent")]
    InvalidArtifact,
    #[error("terminal reconciliation artifact checksum mismatch")]
    ArtifactHashMismatch,
    #[error("terminal binding does not match replay run, cutoff, or pinned V1 program")]
    ReplayBindingMismatch,
    #[error("terminal binding account list does not exactly equal replay-required accounts")]
    RequestedAccountSetMismatch,
    #[error("validated terminal snapshot does not match the independently supplied binding")]
    ValidatedSnapshotBindingMismatch,
    #[error("native replay produced no terminal markets")]
    EmptyReplay,
    #[error("terminal checkpoint identity is invalid for market {market}: {detail}")]
    InvalidCheckpoint {
        market: String,
        detail: &'static str,
    },
    #[error("terminal account address has conflicting semantic roles")]
    AccountRoleCollision,
    #[error("terminal snapshot is missing replay-required account role {role}")]
    MissingRequiredAccount { role: &'static str },
    #[error("invalid {role} account envelope: {detail}")]
    InvalidAccountEnvelope {
        role: &'static str,
        detail: &'static str,
    },
    #[error("failed to decode exact V1 {role} account layout")]
    AccountDecode { role: &'static str },
    #[error("unsupported token vault state for {role}: {detail}")]
    UnsupportedTokenVault {
        role: &'static str,
        detail: &'static str,
    },
    #[error("raw-unit mismatch for market {market}, role {role}, field {field}")]
    RawUnitMismatch {
        market: String,
        role: &'static str,
        field: String,
    },
    #[error("terminal reconciliation serialization or count overflow: {0}")]
    Serialization(String),
}

/// Validates a sealed snapshot and reconciles it against replay terminal state.
pub fn reconcile_terminal_snapshot(
    expected_run_id: &str,
    expected_cutoff_slot: u64,
    binding: &TerminalSnapshotBindingV1,
    snapshot: &TerminalAccountSnapshotV1,
    checkpoints: &BTreeMap<Pubkey, V1NativeCheckpointV1>,
) -> Result<V1TerminalReconciliationArtifactV1, V1TerminalReconciliationError> {
    let validated = snapshot.validate_for_import(binding)?;
    reconcile_validated_terminal_snapshot(
        expected_run_id,
        expected_cutoff_slot,
        binding,
        &validated,
        checkpoints,
    )
}

/// Reconciles an already validated exact-cutoff snapshot. The independent
/// binding is still required so a validated object cannot be substituted from
/// another run or account universe.
pub fn reconcile_validated_terminal_snapshot(
    expected_run_id: &str,
    expected_cutoff_slot: u64,
    binding: &TerminalSnapshotBindingV1,
    snapshot: &ValidatedTerminalSnapshotV1,
    checkpoints: &BTreeMap<Pubkey, V1NativeCheckpointV1>,
) -> Result<V1TerminalReconciliationArtifactV1, V1TerminalReconciliationError> {
    validate_replay_binding(expected_run_id, expected_cutoff_slot, binding, snapshot)?;
    let expected_accounts = required_terminal_accounts(&binding.program_id, checkpoints)?;
    if binding.requested_accounts != expected_accounts {
        return Err(V1TerminalReconciliationError::RequestedAccountSetMismatch);
    }
    let observed_accounts: Vec<_> = snapshot
        .accounts
        .iter()
        .map(|account| account.address.clone())
        .collect();
    if observed_accounts != expected_accounts {
        return Err(V1TerminalReconciliationError::RequestedAccountSetMismatch);
    }

    let program_id = Pubkey::from_str(&binding.program_id)
        .map_err(|_| V1TerminalReconciliationError::ReplayBindingMismatch)?;
    let accounts: BTreeMap<_, _> = snapshot
        .accounts
        .iter()
        .map(|account| {
            Pubkey::from_str(&account.address)
                .map(|address| (address, account))
                .map_err(|_| V1TerminalReconciliationError::ValidatedSnapshotBindingMismatch)
        })
        .collect::<Result<_, _>>()?;

    let authority_key =
        Pubkey::find_program_address(&[FUTARCHY_AUTHORITY_SEED_PREFIX], &program_id).0;
    let authority_account = required_account(&accounts, &authority_key, "futarchy_authority")?;
    let authority: FutarchyAuthority =
        decode_anchor_account(authority_account, &program_id, "futarchy_authority")?;
    let actual_authority = authority_checkpoint(&authority);

    let mut market_results = Vec::with_capacity(checkpoints.len());
    let mut total_positions = 0_u64;
    let mut total_comparisons = 0_u64;
    let mut unique_state_accounts = BTreeMap::<Pubkey, ()>::new();
    let mut unique_token_accounts = BTreeMap::<Pubkey, ()>::new();

    for (market_key, checkpoint) in checkpoints {
        let market = market_key.to_string();
        let pair_account = required_account(&accounts, market_key, "pair")?;
        let pair: Pair = decode_anchor_account(pair_account, &program_id, "pair")?;
        let actual_pair = pair_checkpoint(&pair);
        let mut comparisons = compare_exact(&checkpoint.pair, &actual_pair, &market, "pair")?;
        unique_state_accounts.insert(*market_key, ());

        let rate_key = checkpoint.rate_model.key;
        let rate_account = required_account(&accounts, &rate_key, "rate_model")?;
        let rate: RateModel = decode_anchor_account(rate_account, &program_id, "rate_model")?;
        let actual_rate = rate_model_checkpoint(rate_key, &rate);
        comparisons = comparisons
            .checked_add(compare_exact(
                &checkpoint.rate_model,
                &actual_rate,
                &market,
                "rate_model",
            )?)
            .ok_or_else(|| {
                V1TerminalReconciliationError::Serialization("rate comparison count".to_owned())
            })?;
        unique_state_accounts.insert(rate_key, ());

        comparisons = comparisons
            .checked_add(compare_exact(
                &checkpoint.authority,
                &actual_authority,
                &market,
                "futarchy_authority",
            )?)
            .ok_or_else(|| {
                V1TerminalReconciliationError::Serialization(
                    "authority comparison count".to_owned(),
                )
            })?;
        unique_state_accounts.insert(authority_key, ());

        for (position_key, expected_position) in &checkpoint.positions {
            let account = required_account(&accounts, position_key, "user_position")?;
            let position: UserPosition =
                decode_anchor_account(account, &program_id, "user_position")?;
            comparisons = comparisons
                .checked_add(compare_exact(
                    expected_position,
                    &position_checkpoint(&position),
                    &market,
                    "user_position",
                )?)
                .ok_or_else(|| {
                    V1TerminalReconciliationError::Serialization(
                        "position comparison count".to_owned(),
                    )
                })?;
            unique_state_accounts.insert(*position_key, ());
        }

        let vaults = market_vaults(&program_id, checkpoint);
        for (vault_key, mint, expected_amount, role) in vaults {
            let account = required_account(&accounts, &vault_key, role)?;
            let decoded = decode_token_vault(account, role)?;
            compare_token_value(&market, role, "mint", mint, decoded.mint)?;
            compare_token_value(
                &market,
                role,
                "authority",
                checkpoint.pair_key,
                decoded.authority,
            )?;
            if expected_amount != decoded.amount {
                return Err(V1TerminalReconciliationError::RawUnitMismatch {
                    market: market.clone(),
                    role,
                    field: "amount".to_owned(),
                });
            }
            comparisons = comparisons.checked_add(3).ok_or_else(|| {
                V1TerminalReconciliationError::Serialization("vault comparison count".to_owned())
            })?;
            unique_token_accounts.insert(vault_key, ());
        }

        total_positions = total_positions
            .checked_add(checkpoint.positions.len() as u64)
            .ok_or_else(|| {
                V1TerminalReconciliationError::Serialization("position count".to_owned())
            })?;
        total_comparisons = total_comparisons.checked_add(comparisons).ok_or_else(|| {
            V1TerminalReconciliationError::Serialization("total comparison count".to_owned())
        })?;
        market_results.push(V1TerminalMarketReconciliationV1 {
            market,
            required_account_count: 7_u64
                .checked_add(checkpoint.positions.len() as u64)
                .ok_or_else(|| {
                    V1TerminalReconciliationError::Serialization(
                        "required market account count".to_owned(),
                    )
                })?,
            decoded_position_count: checkpoint.positions.len() as u64,
            raw_unit_comparison_count: comparisons,
            terminal_checkpoint_sha256: canonical_terminal_checkpoint_sha256(checkpoint).map_err(
                |error| {
                    V1TerminalReconciliationError::Serialization(format!(
                        "checkpoint commitment: {error}"
                    ))
                },
            )?,
            all_raw_units_equal: true,
        });
    }
    market_results.sort_by(|left, right| left.market.cmp(&right.market));

    V1TerminalReconciliationArtifactV1 {
        schema_version: V1_TERMINAL_RECONCILIATION_SCHEMA_VERSION,
        run_id: snapshot.run_id.clone(),
        cutoff_slot: snapshot.cutoff_slot,
        program_id: snapshot.program_id.clone(),
        provider: snapshot.provider.clone(),
        binding_sha256: snapshot.binding_sha256.clone(),
        source_raw_response_sha256: snapshot.source_raw_response_sha256.clone(),
        account_set_sha256: snapshot.account_set_sha256.clone(),
        market_count: checkpoints.len() as u64,
        expected_account_count: expected_accounts.len() as u64,
        observed_account_count: snapshot.accounts.len() as u64,
        state_account_count: unique_state_accounts.len() as u64,
        token_vault_account_count: unique_token_accounts.len() as u64,
        decoded_position_count: total_positions,
        raw_unit_comparison_count: total_comparisons,
        raw_unit_mismatch_count: 0,
        terminal_state_verified: true,
        markets: market_results,
        artifact_sha256: String::new(),
    }
    .seal()
}

/// Derives the only account list that can prove terminal equality for the
/// supplied replay checkpoints. The textual sort matches the terminal snapshot
/// contract exactly.
pub fn required_terminal_accounts(
    program_id: &str,
    checkpoints: &BTreeMap<Pubkey, V1NativeCheckpointV1>,
) -> Result<Vec<String>, V1TerminalReconciliationError> {
    let program_id = Pubkey::from_str(program_id)
        .map_err(|_| V1TerminalReconciliationError::ReplayBindingMismatch)?;
    if program_id != omnipair::ID {
        return Err(V1TerminalReconciliationError::ReplayBindingMismatch);
    }
    if checkpoints.is_empty() {
        return Err(V1TerminalReconciliationError::EmptyReplay);
    }

    let mut roles = BTreeMap::<Pubkey, &'static str>::new();
    let authority = Pubkey::find_program_address(&[FUTARCHY_AUTHORITY_SEED_PREFIX], &program_id);
    insert_role(&mut roles, authority.0, "futarchy_authority")?;

    for (market_key, checkpoint) in checkpoints {
        validate_checkpoint_identity(*market_key, checkpoint, &program_id, authority)?;
        insert_role(&mut roles, *market_key, "pair")?;
        insert_role(&mut roles, checkpoint.rate_model.key, "rate_model")?;
        for position_key in checkpoint.positions.keys() {
            insert_role(&mut roles, *position_key, "user_position")?;
        }
        for (vault, _, _, role) in market_vaults(&program_id, checkpoint) {
            insert_role(&mut roles, vault, role)?;
        }
    }
    let mut accounts: Vec<_> = roles.keys().map(ToString::to_string).collect();
    accounts.sort();
    Ok(accounts)
}

fn validate_replay_binding(
    expected_run_id: &str,
    expected_cutoff_slot: u64,
    binding: &TerminalSnapshotBindingV1,
    snapshot: &ValidatedTerminalSnapshotV1,
) -> Result<(), V1TerminalReconciliationError> {
    let expected_binding_sha256 = compute_terminal_binding_sha256(binding)?;
    if expected_run_id.trim().is_empty()
        || binding.run_id != expected_run_id
        || binding.cutoff_slot != expected_cutoff_slot
        || binding.run_id != snapshot.run_id
        || binding.cutoff_slot != snapshot.cutoff_slot
        || binding.cutoff_slot != snapshot.response_context_slot
        || binding.program_id != snapshot.program_id
        || binding.provider != snapshot.provider
        || expected_binding_sha256 != snapshot.binding_sha256
        || Pubkey::from_str(&binding.program_id).ok() != Some(omnipair::ID)
    {
        return Err(V1TerminalReconciliationError::ValidatedSnapshotBindingMismatch);
    }
    Ok(())
}

fn validate_checkpoint_identity(
    market_key: Pubkey,
    checkpoint: &V1NativeCheckpointV1,
    program_id: &Pubkey,
    authority: (Pubkey, u8),
) -> Result<(), V1TerminalReconciliationError> {
    let market = market_key.to_string();
    if checkpoint.schema_version != V1_NATIVE_ENGINE_SCHEMA_VERSION
        || checkpoint.engine_id != V1_NATIVE_ENGINE_ID
        || checkpoint.program_revision != V1ProgramRevisionV1::OmnipairV1
        || checkpoint.pair_key != market_key
        || checkpoint.pair.version != VERSION
        || checkpoint.authority.version != FutarchyAuthority::CURRENT_VERSION
        || checkpoint.pair.rate_model != checkpoint.rate_model.key
    {
        return Err(V1TerminalReconciliationError::InvalidCheckpoint {
            market,
            detail: "version_or_account_identity",
        });
    }
    let derived_pair = Pubkey::find_program_address(
        &[
            PAIR_SEED_PREFIX,
            checkpoint.pair.token0.as_ref(),
            checkpoint.pair.token1.as_ref(),
            checkpoint.pair.params_hash.as_ref(),
        ],
        program_id,
    );
    if derived_pair != (market_key, checkpoint.pair.bump) {
        return Err(V1TerminalReconciliationError::InvalidCheckpoint {
            market,
            detail: "pair_pda",
        });
    }
    if authority.1 != checkpoint.authority.bump {
        return Err(V1TerminalReconciliationError::InvalidCheckpoint {
            market,
            detail: "authority_pda",
        });
    }
    for (position_key, position) in &checkpoint.positions {
        let derived = Pubkey::find_program_address(
            &[
                POSITION_SEED_PREFIX,
                market_key.as_ref(),
                position.owner.as_ref(),
            ],
            program_id,
        );
        if position.pair != market_key || derived != (*position_key, position.bump) {
            return Err(V1TerminalReconciliationError::InvalidCheckpoint {
                market,
                detail: "position_pda",
            });
        }
    }
    let vaults = [
        (
            RESERVE_VAULT_SEED_PREFIX,
            checkpoint.pair.token0,
            checkpoint.pair.reserve0_vault_bump,
        ),
        (
            RESERVE_VAULT_SEED_PREFIX,
            checkpoint.pair.token1,
            checkpoint.pair.reserve1_vault_bump,
        ),
        (
            COLLATERAL_VAULT_SEED_PREFIX,
            checkpoint.pair.token0,
            checkpoint.pair.collateral0_vault_bump,
        ),
        (
            COLLATERAL_VAULT_SEED_PREFIX,
            checkpoint.pair.token1,
            checkpoint.pair.collateral1_vault_bump,
        ),
    ];
    for (prefix, mint, bump) in vaults {
        if Pubkey::find_program_address(&[prefix, market_key.as_ref(), mint.as_ref()], program_id).1
            != bump
        {
            return Err(V1TerminalReconciliationError::InvalidCheckpoint {
                market,
                detail: "vault_pda",
            });
        }
    }
    Ok(())
}

fn insert_role(
    roles: &mut BTreeMap<Pubkey, &'static str>,
    address: Pubkey,
    role: &'static str,
) -> Result<(), V1TerminalReconciliationError> {
    if roles
        .insert(address, role)
        .is_some_and(|existing| existing != role)
    {
        return Err(V1TerminalReconciliationError::AccountRoleCollision);
    }
    Ok(())
}

fn market_vaults(
    program_id: &Pubkey,
    checkpoint: &V1NativeCheckpointV1,
) -> [(Pubkey, Pubkey, u64, &'static str); 4] {
    let pair = checkpoint.pair_key;
    [
        (
            Pubkey::find_program_address(
                &[
                    RESERVE_VAULT_SEED_PREFIX,
                    pair.as_ref(),
                    checkpoint.pair.token0.as_ref(),
                ],
                program_id,
            )
            .0,
            checkpoint.pair.token0,
            checkpoint.custody.reserve_vault0,
            "reserve_vault0",
        ),
        (
            Pubkey::find_program_address(
                &[
                    RESERVE_VAULT_SEED_PREFIX,
                    pair.as_ref(),
                    checkpoint.pair.token1.as_ref(),
                ],
                program_id,
            )
            .0,
            checkpoint.pair.token1,
            checkpoint.custody.reserve_vault1,
            "reserve_vault1",
        ),
        (
            Pubkey::find_program_address(
                &[
                    COLLATERAL_VAULT_SEED_PREFIX,
                    pair.as_ref(),
                    checkpoint.pair.token0.as_ref(),
                ],
                program_id,
            )
            .0,
            checkpoint.pair.token0,
            checkpoint.custody.collateral_vault0,
            "collateral_vault0",
        ),
        (
            Pubkey::find_program_address(
                &[
                    COLLATERAL_VAULT_SEED_PREFIX,
                    pair.as_ref(),
                    checkpoint.pair.token1.as_ref(),
                ],
                program_id,
            )
            .0,
            checkpoint.pair.token1,
            checkpoint.custody.collateral_vault1,
            "collateral_vault1",
        ),
    ]
}

fn required_account<'a>(
    accounts: &'a BTreeMap<Pubkey, &'a ValidatedTerminalAccountV1>,
    key: &Pubkey,
    role: &'static str,
) -> Result<&'a ValidatedTerminalAccountV1, V1TerminalReconciliationError> {
    accounts
        .get(key)
        .copied()
        .ok_or(V1TerminalReconciliationError::MissingRequiredAccount { role })
}

fn decode_anchor_account<T>(
    account: &ValidatedTerminalAccountV1,
    program_id: &Pubkey,
    role: &'static str,
) -> Result<T, V1TerminalReconciliationError>
where
    T: AccountDeserialize + Discriminator + Space,
{
    if account.owner != program_id.to_string() {
        return Err(V1TerminalReconciliationError::InvalidAccountEnvelope {
            role,
            detail: "wrong_owner_program",
        });
    }
    if account.executable || account.lamports == 0 {
        return Err(V1TerminalReconciliationError::InvalidAccountEnvelope {
            role,
            detail: "executable_or_zero_lamports",
        });
    }
    let expected_len = T::DISCRIMINATOR.len().saturating_add(T::INIT_SPACE);
    if account.data.len() != expected_len {
        return Err(V1TerminalReconciliationError::InvalidAccountEnvelope {
            role,
            detail: "wrong_account_data_length",
        });
    }
    let mut data = account.data.as_slice();
    T::try_deserialize(&mut data).map_err(|_| V1TerminalReconciliationError::AccountDecode { role })
}

struct DecodedTokenVault {
    mint: Pubkey,
    authority: Pubkey,
    amount: u64,
}

fn decode_token_vault(
    account: &ValidatedTerminalAccountV1,
    role: &'static str,
) -> Result<DecodedTokenVault, V1TerminalReconciliationError> {
    if account.executable || account.lamports == 0 {
        return Err(V1TerminalReconciliationError::InvalidAccountEnvelope {
            role,
            detail: "executable_or_zero_lamports",
        });
    }
    let is_legacy = account.owner == LEGACY_TOKEN_PROGRAM_ID;
    let is_token_2022 = account.owner == TOKEN_2022_PROGRAM_ID;
    if !is_legacy && !is_token_2022 {
        return Err(V1TerminalReconciliationError::InvalidAccountEnvelope {
            role,
            detail: "unsupported_token_program",
        });
    }
    if is_legacy && account.data.len() != 165 {
        return Err(V1TerminalReconciliationError::InvalidAccountEnvelope {
            role,
            detail: "legacy_token_account_length",
        });
    }
    let state = StateWithExtensions::<TokenAccount>::unpack(&account.data)
        .map_err(|_| V1TerminalReconciliationError::AccountDecode { role })?;
    if state.base.state != AccountState::Initialized {
        return Err(V1TerminalReconciliationError::UnsupportedTokenVault {
            role,
            detail: "uninitialized_or_frozen",
        });
    }
    if state.base.delegate.is_some()
        || state.base.delegated_amount != 0
        || state.base.close_authority.is_some()
    {
        return Err(V1TerminalReconciliationError::UnsupportedTokenVault {
            role,
            detail: "delegate_or_close_authority",
        });
    }
    let extension_types = state
        .get_extension_types()
        .map_err(|_| V1TerminalReconciliationError::AccountDecode { role })?;
    if extension_types.iter().any(|extension| {
        !matches!(
            extension,
            ExtensionType::TransferFeeAmount | ExtensionType::ImmutableOwner
        )
    }) {
        return Err(V1TerminalReconciliationError::UnsupportedTokenVault {
            role,
            detail: "unmodeled_token_2022_account_extension",
        });
    }
    if extension_types.contains(&ExtensionType::TransferFeeAmount) {
        let transfer_fee = state
            .get_extension::<TransferFeeAmount>()
            .map_err(|_| V1TerminalReconciliationError::AccountDecode { role })?;
        if u64::from(transfer_fee.withheld_amount) != 0 {
            return Err(V1TerminalReconciliationError::UnsupportedTokenVault {
                role,
                detail: "nonzero_withheld_transfer_fee",
            });
        }
    }
    Ok(DecodedTokenVault {
        mint: Pubkey::new_from_array(state.base.mint.to_bytes()),
        authority: Pubkey::new_from_array(state.base.owner.to_bytes()),
        amount: state.base.amount,
    })
}

fn pair_checkpoint(pair: &Pair) -> V1PairCheckpointV1 {
    V1PairCheckpointV1 {
        token0: pair.token0,
        token1: pair.token1,
        lp_mint: pair.lp_mint,
        rate_model: pair.rate_model,
        swap_fee_bps: pair.swap_fee_bps,
        half_life: pair.half_life,
        fixed_cf_bps: pair.fixed_cf_bps,
        reserve0: pair.reserve0,
        reserve1: pair.reserve1,
        cash_reserve0: pair.cash_reserve0,
        cash_reserve1: pair.cash_reserve1,
        symmetric_price0_ema_nad: pair.last_price0_ema.symmetric,
        directional_price0_ema_nad: pair.last_price0_ema.directional,
        symmetric_price1_ema_nad: pair.last_price1_ema.symmetric,
        directional_price1_ema_nad: pair.last_price1_ema.directional,
        last_update_slot: pair.last_update,
        last_rate0_nad: pair.last_rate0,
        last_rate1_nad: pair.last_rate1,
        total_debt0: pair.total_debt0,
        total_debt1: pair.total_debt1,
        total_debt0_shares: pair.total_debt0_shares,
        total_debt1_shares: pair.total_debt1_shares,
        total_supply: pair.total_supply,
        total_collateral0: pair.total_collateral0,
        total_collateral1: pair.total_collateral1,
        token0_decimals: pair.token0_decimals,
        token1_decimals: pair.token1_decimals,
        params_hash: pair.params_hash,
        version: pair.version,
        bump: pair.bump,
        reserve0_vault_bump: pair.vault_bumps.reserve0,
        reserve1_vault_bump: pair.vault_bumps.reserve1,
        collateral0_vault_bump: pair.vault_bumps.collateral0,
        collateral1_vault_bump: pair.vault_bumps.collateral1,
        reduce_only: pair.reduce_only,
    }
}

fn rate_model_checkpoint(key: Pubkey, rate_model: &RateModel) -> V1RateModelCheckpointV1 {
    V1RateModelCheckpointV1 {
        key,
        exp_rate: rate_model.exp_rate,
        target_util_start_nad: rate_model.target_util_start,
        target_util_end_nad: rate_model.target_util_end,
        half_life_ms: rate_model.half_life_ms,
        min_rate_nad: rate_model.min_rate,
        max_rate_nad: rate_model.max_rate,
        initial_rate_nad: rate_model.initial_rate,
    }
}

fn authority_checkpoint(authority: &FutarchyAuthority) -> V1AuthorityCheckpointV1 {
    V1AuthorityCheckpointV1 {
        version: authority.version,
        authority: authority.authority,
        swap_bps: authority.revenue_share.swap_bps,
        interest_bps: authority.revenue_share.interest_bps,
        futarchy_treasury: authority.recipients.futarchy_treasury,
        buybacks_vault: authority.recipients.buybacks_vault,
        team_treasury: authority.recipients.team_treasury,
        futarchy_treasury_bps: authority.revenue_distribution.futarchy_treasury_bps,
        buybacks_vault_bps: authority.revenue_distribution.buybacks_vault_bps,
        team_treasury_bps: authority.revenue_distribution.team_treasury_bps,
        global_reduce_only: authority.global_reduce_only,
        bump: authority.bump,
    }
}

fn position_checkpoint(position: &UserPosition) -> V1PositionCheckpointV1 {
    V1PositionCheckpointV1 {
        owner: position.owner,
        pair: position.pair,
        collateral0_liquidation_cf_bps: position.collateral0_liquidation_cf_bps,
        collateral1_liquidation_cf_bps: position.collateral1_liquidation_cf_bps,
        collateral0: position.collateral0,
        collateral1: position.collateral1,
        debt0_shares: position.debt0_shares,
        debt1_shares: position.debt1_shares,
        bump: position.bump,
    }
}

fn compare_token_value(
    market: &str,
    role: &'static str,
    field: &'static str,
    expected: Pubkey,
    actual: Pubkey,
) -> Result<(), V1TerminalReconciliationError> {
    if expected != actual {
        return Err(V1TerminalReconciliationError::RawUnitMismatch {
            market: market.to_owned(),
            role,
            field: field.to_owned(),
        });
    }
    Ok(())
}

fn compare_exact<T: Serialize>(
    expected: &T,
    actual: &T,
    market: &str,
    role: &'static str,
) -> Result<u64, V1TerminalReconciliationError> {
    let expected = serde_json::to_value(expected).map_err(|error| {
        V1TerminalReconciliationError::Serialization(format!("expected comparison value: {error}"))
    })?;
    let actual = serde_json::to_value(actual).map_err(|error| {
        V1TerminalReconciliationError::Serialization(format!("actual comparison value: {error}"))
    })?;
    let mut comparisons = 0_u64;
    compare_json(&expected, &actual, "", &mut comparisons).map_err(|field| {
        V1TerminalReconciliationError::RawUnitMismatch {
            market: market.to_owned(),
            role,
            field,
        }
    })?;
    Ok(comparisons)
}

fn compare_json(
    expected: &JsonValue,
    actual: &JsonValue,
    path: &str,
    comparisons: &mut u64,
) -> Result<(), String> {
    match (expected, actual) {
        (JsonValue::Object(expected), JsonValue::Object(actual)) => {
            if expected.len() != actual.len() {
                return Err(path_or_root(path));
            }
            for (key, expected_value) in expected {
                let next = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                let actual_value = actual.get(key).ok_or_else(|| next.clone())?;
                compare_json(expected_value, actual_value, &next, comparisons)?;
            }
            Ok(())
        }
        (JsonValue::Array(expected), JsonValue::Array(actual)) => {
            if expected.len() != actual.len() {
                return Err(path_or_root(path));
            }
            for (index, (expected_value, actual_value)) in expected.iter().zip(actual).enumerate() {
                compare_json(
                    expected_value,
                    actual_value,
                    &format!("{path}[{index}]"),
                    comparisons,
                )?;
            }
            Ok(())
        }
        _ => {
            *comparisons = comparisons.saturating_add(1);
            if expected == actual {
                Ok(())
            } else {
                Err(path_or_root(path))
            }
        }
    }
}

fn path_or_root(path: &str) -> String {
    if path.is_empty() {
        "<root>".to_owned()
    } else {
        path.to_owned()
    }
}

fn hash_artifact<T: Serialize>(artifact: &T) -> Result<String, V1TerminalReconciliationError> {
    let bytes = serde_json::to_vec(artifact).map_err(|error| {
        V1TerminalReconciliationError::Serialization(format!("artifact commitment: {error}"))
    })?;
    let mut digest = Sha256::new();
    digest.update((ARTIFACT_HASH_DOMAIN.len() as u64).to_be_bytes());
    digest.update(ARTIFACT_HASH_DOMAIN);
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
    Ok(hex::encode(digest.finalize()))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn valid_sanitized_provider_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use anchor_lang::AccountSerialize;
    use omnipair::state::{
        LastPriceEMA, RevenueDistribution, RevenueRecipients, RevenueShare, VaultBumps,
    };

    use super::*;
    use crate::{
        canonical::sha256_hex,
        terminal::{
            TerminalAccountDataV1, TerminalAccountV1, TerminalReadSemanticsV1,
            TerminalSnapshotSourceEvidenceV1, TERMINAL_SNAPSHOT_SCHEMA_VERSION,
        },
        v1_engine::{V1CustodyCheckpointV1, V1RevenueCheckpointV1},
    };

    struct Fixture {
        binding: TerminalSnapshotBindingV1,
        snapshot: TerminalAccountSnapshotV1,
        checkpoints: BTreeMap<Pubkey, V1NativeCheckpointV1>,
        reserve0_vault: Pubkey,
    }

    #[test]
    fn exact_snapshot_reconciles_all_native_accounts_and_raw_units() {
        let fixture = fixture();
        let artifact = reconcile_terminal_snapshot(
            "fixture-run",
            500,
            &fixture.binding,
            &fixture.snapshot,
            &fixture.checkpoints,
        )
        .unwrap();
        assert!(artifact.terminal_state_verified);
        assert_eq!(artifact.market_count, 1);
        assert_eq!(artifact.expected_account_count, 8);
        assert_eq!(artifact.state_account_count, 4);
        assert_eq!(artifact.token_vault_account_count, 4);
        assert_eq!(artifact.decoded_position_count, 1);
        assert!(artifact.raw_unit_comparison_count > 40);
        artifact.validate().unwrap();
    }

    #[test]
    fn missing_or_extra_binding_accounts_fail_closed() {
        let mut missing = fixture();
        missing.binding.requested_accounts.remove(0);
        missing.snapshot.binding = missing.binding.clone();
        missing.snapshot.accounts.remove(0);
        missing.snapshot.binding_sha256.clear();
        missing.snapshot.account_set_sha256.clear();
        missing.snapshot = missing.snapshot.seal().unwrap();
        assert!(matches!(
            reconcile_terminal_snapshot(
                "fixture-run",
                500,
                &missing.binding,
                &missing.snapshot,
                &missing.checkpoints
            ),
            Err(V1TerminalReconciliationError::RequestedAccountSetMismatch)
        ));

        let mut extra = fixture();
        let extra_key = Pubkey::new_from_array([99; 32]);
        extra.binding.requested_accounts.push(extra_key.to_string());
        extra.binding.requested_accounts.sort();
        extra.snapshot.binding = extra.binding.clone();
        extra
            .snapshot
            .accounts
            .push(terminal_account(extra_key, omnipair::ID, vec![0; 8]));
        extra
            .snapshot
            .accounts
            .sort_by(|left, right| left.address.cmp(&right.address));
        extra.snapshot.binding_sha256.clear();
        extra.snapshot.account_set_sha256.clear();
        extra.snapshot = extra.snapshot.seal().unwrap();
        assert!(matches!(
            reconcile_terminal_snapshot(
                "fixture-run",
                500,
                &extra.binding,
                &extra.snapshot,
                &extra.checkpoints
            ),
            Err(V1TerminalReconciliationError::RequestedAccountSetMismatch)
        ));
    }

    #[test]
    fn valid_snapshot_cannot_cross_run_or_cutoff_boundaries() {
        let fixture = fixture();
        assert!(matches!(
            reconcile_terminal_snapshot(
                "different-run",
                500,
                &fixture.binding,
                &fixture.snapshot,
                &fixture.checkpoints
            ),
            Err(V1TerminalReconciliationError::ValidatedSnapshotBindingMismatch)
        ));
        assert!(matches!(
            reconcile_terminal_snapshot(
                "fixture-run",
                501,
                &fixture.binding,
                &fixture.snapshot,
                &fixture.checkpoints
            ),
            Err(V1TerminalReconciliationError::ValidatedSnapshotBindingMismatch)
        ));
    }

    #[test]
    fn one_raw_vault_unit_mismatch_blocks_terminal_equality() {
        let mut fixture = fixture();
        let account = fixture
            .snapshot
            .accounts
            .iter_mut()
            .find(|account| account.address == fixture.reserve0_vault.to_string())
            .unwrap();
        let mut data = match &account.data {
            TerminalAccountDataV1::Bytes { bytes, .. } => bytes.clone(),
            TerminalAccountDataV1::Base64 { .. } => unreachable!(),
        };
        data[64..72].copy_from_slice(&101_u64.to_le_bytes());
        account.data = terminal_data(data);
        fixture.snapshot.binding_sha256.clear();
        fixture.snapshot.account_set_sha256.clear();
        fixture.snapshot = fixture.snapshot.seal().unwrap();
        assert!(matches!(
            reconcile_terminal_snapshot(
                "fixture-run",
                500,
                &fixture.binding,
                &fixture.snapshot,
                &fixture.checkpoints
            ),
            Err(V1TerminalReconciliationError::RawUnitMismatch { field, .. })
                if field == "amount"
        ));
    }

    #[test]
    fn wrong_program_owner_and_unmodeled_vault_state_are_rejected() {
        let mut wrong_owner = fixture();
        let pair = wrong_owner.checkpoints.keys().next().unwrap().to_string();
        wrong_owner
            .snapshot
            .accounts
            .iter_mut()
            .find(|account| account.address == pair)
            .unwrap()
            .owner = Pubkey::new_from_array([98; 32]).to_string();
        wrong_owner.snapshot.binding_sha256.clear();
        wrong_owner.snapshot.account_set_sha256.clear();
        wrong_owner.snapshot = wrong_owner.snapshot.seal().unwrap();
        assert!(matches!(
            reconcile_terminal_snapshot(
                "fixture-run",
                500,
                &wrong_owner.binding,
                &wrong_owner.snapshot,
                &wrong_owner.checkpoints
            ),
            Err(V1TerminalReconciliationError::InvalidAccountEnvelope { .. })
        ));

        let mut frozen = fixture();
        let account = frozen
            .snapshot
            .accounts
            .iter_mut()
            .find(|account| account.address == frozen.reserve0_vault.to_string())
            .unwrap();
        let mut data = match &account.data {
            TerminalAccountDataV1::Bytes { bytes, .. } => bytes.clone(),
            TerminalAccountDataV1::Base64 { .. } => unreachable!(),
        };
        data[108] = 2;
        account.data = terminal_data(data);
        frozen.snapshot.binding_sha256.clear();
        frozen.snapshot.account_set_sha256.clear();
        frozen.snapshot = frozen.snapshot.seal().unwrap();
        assert!(matches!(
            reconcile_terminal_snapshot(
                "fixture-run",
                500,
                &frozen.binding,
                &frozen.snapshot,
                &frozen.checkpoints
            ),
            Err(V1TerminalReconciliationError::UnsupportedTokenVault { .. })
        ));
    }

    fn fixture() -> Fixture {
        let program_id = omnipair::ID;
        let token0 = Pubkey::new_from_array([1; 32]);
        let token1 = Pubkey::new_from_array([2; 32]);
        let lp_mint = Pubkey::new_from_array([3; 32]);
        let params_hash = [4; 32];
        let (pair_key, pair_bump) = Pubkey::find_program_address(
            &[
                PAIR_SEED_PREFIX,
                token0.as_ref(),
                token1.as_ref(),
                params_hash.as_ref(),
            ],
            &program_id,
        );
        let rate_key = Pubkey::new_from_array([5; 32]);
        let owner = Pubkey::new_from_array([6; 32]);
        let (position_key, position_bump) = Pubkey::find_program_address(
            &[POSITION_SEED_PREFIX, pair_key.as_ref(), owner.as_ref()],
            &program_id,
        );
        let (authority_key, authority_bump) =
            Pubkey::find_program_address(&[FUTARCHY_AUTHORITY_SEED_PREFIX], &program_id);
        let vault = |prefix: &'static [u8], mint: Pubkey| {
            Pubkey::find_program_address(&[prefix, pair_key.as_ref(), mint.as_ref()], &program_id)
        };
        let reserve0 = vault(RESERVE_VAULT_SEED_PREFIX, token0);
        let reserve1 = vault(RESERVE_VAULT_SEED_PREFIX, token1);
        let collateral0 = vault(COLLATERAL_VAULT_SEED_PREFIX, token0);
        let collateral1 = vault(COLLATERAL_VAULT_SEED_PREFIX, token1);

        let checkpoint = V1NativeCheckpointV1 {
            schema_version: V1_NATIVE_ENGINE_SCHEMA_VERSION,
            engine_id: V1_NATIVE_ENGINE_ID.to_owned(),
            program_revision: V1ProgramRevisionV1::OmnipairV1,
            pair_key,
            pair: V1PairCheckpointV1 {
                token0,
                token1,
                lp_mint,
                rate_model: rate_key,
                swap_fee_bps: 30,
                half_life: 1_000,
                fixed_cf_bps: Some(8_000),
                reserve0: 1_000,
                reserve1: 2_000,
                cash_reserve0: 900,
                cash_reserve1: 1_800,
                symmetric_price0_ema_nad: 2_000_000_000,
                directional_price0_ema_nad: 2_000_000_000,
                symmetric_price1_ema_nad: 500_000_000,
                directional_price1_ema_nad: 500_000_000,
                last_update_slot: 500,
                last_rate0_nad: 10,
                last_rate1_nad: 11,
                total_debt0: 100,
                total_debt1: 200,
                total_debt0_shares: 1_000_000,
                total_debt1_shares: 2_000_000,
                total_supply: 1_400,
                total_collateral0: 12,
                total_collateral1: 13,
                token0_decimals: 6,
                token1_decimals: 9,
                params_hash,
                version: VERSION,
                bump: pair_bump,
                reserve0_vault_bump: reserve0.1,
                reserve1_vault_bump: reserve1.1,
                collateral0_vault_bump: collateral0.1,
                collateral1_vault_bump: collateral1.1,
                reduce_only: false,
            },
            rate_model: V1RateModelCheckpointV1 {
                key: rate_key,
                exp_rate: 1,
                target_util_start_nad: 700_000_000,
                target_util_end_nad: 900_000_000,
                half_life_ms: 1_000,
                min_rate_nad: 10,
                max_rate_nad: 100,
                initial_rate_nad: 20,
            },
            authority: V1AuthorityCheckpointV1 {
                version: FutarchyAuthority::CURRENT_VERSION,
                authority: Pubkey::new_from_array([7; 32]),
                swap_bps: 2_000,
                interest_bps: 1_000,
                futarchy_treasury: Pubkey::new_from_array([8; 32]),
                buybacks_vault: Pubkey::new_from_array([9; 32]),
                team_treasury: Pubkey::new_from_array([10; 32]),
                futarchy_treasury_bps: 4_000,
                buybacks_vault_bps: 3_000,
                team_treasury_bps: 3_000,
                global_reduce_only: false,
                bump: authority_bump,
            },
            positions: BTreeMap::from([(
                position_key,
                V1PositionCheckpointV1 {
                    owner,
                    pair: pair_key,
                    collateral0_liquidation_cf_bps: 8_000,
                    collateral1_liquidation_cf_bps: 7_500,
                    collateral0: 12,
                    collateral1: 13,
                    debt0_shares: 1_000_000,
                    debt1_shares: 2_000_000,
                    bump: position_bump,
                },
            )]),
            custody: V1CustodyCheckpointV1 {
                reserve_vault0: 100,
                reserve_vault1: 200,
                collateral_vault0: 12,
                collateral_vault1: 13,
            },
            revenue: V1RevenueCheckpointV1 {
                lp_swap_fee0: 1,
                lp_swap_fee1: 2,
                protocol_swap_fee0: 3,
                protocol_swap_fee1: 4,
                lp_interest0: 5,
                lp_interest1: 6,
                protocol_interest0: 7,
                protocol_interest1: 8,
                flashloan_fee0: 9,
                flashloan_fee1: 10,
                protocol_claim_realized0: 11,
                protocol_claim_realized1: 12,
            },
        };
        let checkpoints = BTreeMap::from([(pair_key, checkpoint.clone())]);
        let requested_accounts =
            required_terminal_accounts(&program_id.to_string(), &checkpoints).unwrap();
        let binding = TerminalSnapshotBindingV1 {
            schema_version: TERMINAL_SNAPSHOT_SCHEMA_VERSION,
            run_id: "fixture-run".to_owned(),
            cutoff_slot: 500,
            program_id: program_id.to_string(),
            requested_accounts,
            provider: TerminalProviderIdentityV1 {
                schema_version: TERMINAL_SNAPSHOT_SCHEMA_VERSION,
                provider_id: "fixture-archive".to_owned(),
                provider_identity_sha256: "1".repeat(64),
                endpoint_fingerprint_sha256: "2".repeat(64),
            },
        };

        let pair = Pair {
            token0,
            token1,
            lp_mint,
            rate_model: rate_key,
            swap_fee_bps: checkpoint.pair.swap_fee_bps,
            half_life: checkpoint.pair.half_life,
            fixed_cf_bps: checkpoint.pair.fixed_cf_bps,
            reserve0: checkpoint.pair.reserve0,
            reserve1: checkpoint.pair.reserve1,
            cash_reserve0: checkpoint.pair.cash_reserve0,
            cash_reserve1: checkpoint.pair.cash_reserve1,
            last_price0_ema: LastPriceEMA {
                symmetric: checkpoint.pair.symmetric_price0_ema_nad,
                directional: checkpoint.pair.directional_price0_ema_nad,
            },
            last_price1_ema: LastPriceEMA {
                symmetric: checkpoint.pair.symmetric_price1_ema_nad,
                directional: checkpoint.pair.directional_price1_ema_nad,
            },
            last_update: checkpoint.pair.last_update_slot,
            last_rate0: checkpoint.pair.last_rate0_nad,
            last_rate1: checkpoint.pair.last_rate1_nad,
            total_debt0: checkpoint.pair.total_debt0,
            total_debt1: checkpoint.pair.total_debt1,
            total_debt0_shares: checkpoint.pair.total_debt0_shares,
            total_debt1_shares: checkpoint.pair.total_debt1_shares,
            total_supply: checkpoint.pair.total_supply,
            total_collateral0: checkpoint.pair.total_collateral0,
            total_collateral1: checkpoint.pair.total_collateral1,
            token0_decimals: checkpoint.pair.token0_decimals,
            token1_decimals: checkpoint.pair.token1_decimals,
            params_hash,
            version: VERSION,
            bump: pair_bump,
            vault_bumps: VaultBumps {
                reserve0: reserve0.1,
                reserve1: reserve1.1,
                collateral0: collateral0.1,
                collateral1: collateral1.1,
            },
            reduce_only: false,
        };
        let rate = RateModel {
            exp_rate: checkpoint.rate_model.exp_rate,
            target_util_start: checkpoint.rate_model.target_util_start_nad,
            target_util_end: checkpoint.rate_model.target_util_end_nad,
            half_life_ms: checkpoint.rate_model.half_life_ms,
            min_rate: checkpoint.rate_model.min_rate_nad,
            max_rate: checkpoint.rate_model.max_rate_nad,
            initial_rate: checkpoint.rate_model.initial_rate_nad,
        };
        let authority = FutarchyAuthority {
            version: checkpoint.authority.version,
            authority: checkpoint.authority.authority,
            recipients: RevenueRecipients {
                futarchy_treasury: checkpoint.authority.futarchy_treasury,
                buybacks_vault: checkpoint.authority.buybacks_vault,
                team_treasury: checkpoint.authority.team_treasury,
            },
            revenue_share: RevenueShare {
                swap_bps: checkpoint.authority.swap_bps,
                interest_bps: checkpoint.authority.interest_bps,
            },
            revenue_distribution: RevenueDistribution {
                futarchy_treasury_bps: checkpoint.authority.futarchy_treasury_bps,
                buybacks_vault_bps: checkpoint.authority.buybacks_vault_bps,
                team_treasury_bps: checkpoint.authority.team_treasury_bps,
            },
            global_reduce_only: checkpoint.authority.global_reduce_only,
            bump: authority_bump,
        };
        let expected_position = checkpoint.positions.get(&position_key).unwrap();
        let position = UserPosition {
            owner: expected_position.owner,
            pair: expected_position.pair,
            collateral0_liquidation_cf_bps: expected_position.collateral0_liquidation_cf_bps,
            collateral1_liquidation_cf_bps: expected_position.collateral1_liquidation_cf_bps,
            collateral0: expected_position.collateral0,
            collateral1: expected_position.collateral1,
            debt0_shares: expected_position.debt0_shares,
            debt1_shares: expected_position.debt1_shares,
            bump: expected_position.bump,
        };
        let mut by_address = BTreeMap::from([
            (
                pair_key,
                terminal_account(pair_key, program_id, anchor_data(&pair)),
            ),
            (
                rate_key,
                terminal_account(rate_key, program_id, anchor_data(&rate)),
            ),
            (
                authority_key,
                terminal_account(authority_key, program_id, anchor_data(&authority)),
            ),
            (
                position_key,
                terminal_account(position_key, program_id, anchor_data(&position)),
            ),
            (
                reserve0.0,
                terminal_account(
                    reserve0.0,
                    Pubkey::from_str(LEGACY_TOKEN_PROGRAM_ID).unwrap(),
                    token_account_data(token0, pair_key, checkpoint.custody.reserve_vault0),
                ),
            ),
            (
                reserve1.0,
                terminal_account(
                    reserve1.0,
                    Pubkey::from_str(LEGACY_TOKEN_PROGRAM_ID).unwrap(),
                    token_account_data(token1, pair_key, checkpoint.custody.reserve_vault1),
                ),
            ),
            (
                collateral0.0,
                terminal_account(
                    collateral0.0,
                    Pubkey::from_str(LEGACY_TOKEN_PROGRAM_ID).unwrap(),
                    token_account_data(token0, pair_key, checkpoint.custody.collateral_vault0),
                ),
            ),
            (
                collateral1.0,
                terminal_account(
                    collateral1.0,
                    Pubkey::from_str(LEGACY_TOKEN_PROGRAM_ID).unwrap(),
                    token_account_data(token1, pair_key, checkpoint.custody.collateral_vault1),
                ),
            ),
        ]);
        let accounts = binding
            .requested_accounts
            .iter()
            .map(|address| {
                by_address
                    .remove(&Pubkey::from_str(address).unwrap())
                    .unwrap()
            })
            .collect();
        let snapshot = TerminalAccountSnapshotV1 {
            schema_version: TERMINAL_SNAPSHOT_SCHEMA_VERSION,
            binding: binding.clone(),
            binding_sha256: String::new(),
            response_context_slot: 500,
            source: TerminalSnapshotSourceEvidenceV1 {
                schema_version: TERMINAL_SNAPSHOT_SCHEMA_VERSION,
                read_semantics: TerminalReadSemanticsV1::ExactHistoricalSlot,
                provider_request_id: "fixture-request".to_owned(),
                raw_response_sha256: "3".repeat(64),
            },
            accounts,
            account_set_sha256: String::new(),
        }
        .seal()
        .unwrap();
        Fixture {
            binding,
            snapshot,
            checkpoints,
            reserve0_vault: reserve0.0,
        }
    }

    fn anchor_data<T: AccountSerialize + Space + Discriminator>(value: &T) -> Vec<u8> {
        let mut bytes = Vec::new();
        value.try_serialize(&mut bytes).unwrap();
        bytes.resize(T::DISCRIMINATOR.len() + T::INIT_SPACE, 0);
        bytes
    }

    fn token_account_data(mint: Pubkey, authority: Pubkey, amount: u64) -> Vec<u8> {
        let mut bytes = vec![0_u8; 165];
        bytes[..32].copy_from_slice(mint.as_ref());
        bytes[32..64].copy_from_slice(authority.as_ref());
        bytes[64..72].copy_from_slice(&amount.to_le_bytes());
        bytes[108] = 1;
        bytes
    }

    fn terminal_account(address: Pubkey, owner: Pubkey, bytes: Vec<u8>) -> TerminalAccountV1 {
        TerminalAccountV1 {
            schema_version: TERMINAL_SNAPSHOT_SCHEMA_VERSION,
            address: address.to_string(),
            owner: owner.to_string(),
            lamports: "1000000".to_owned(),
            executable: false,
            rent_epoch: "0".to_owned(),
            data: terminal_data(bytes),
        }
    }

    fn terminal_data(bytes: Vec<u8>) -> TerminalAccountDataV1 {
        TerminalAccountDataV1::Bytes {
            byte_length: bytes.len() as u64,
            data_sha256: sha256_hex(&bytes),
            bytes,
        }
    }
}
