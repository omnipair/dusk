//! Finalized archival Solana JSON-RPC transport.

use std::{future::Future, pin::Pin, str::FromStr, time::Duration};

use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use solana_pubkey::Pubkey;

use crate::canonical::sha256_hex;
use crate::extract::{
    canonical_transaction_payload_sha256, finalized_get_block_signatures_request,
    finalized_get_transaction_request, full_payload_transaction_inventory_sha256,
    independent_get_transaction_request_sha256, normalize_independent_get_transaction_meta,
    ordered_signature_block_request_sha256, ordered_signature_inventory_sha256,
    ArchivalRpcTransport, ArchivedBlock, ArchivedBlockProofBasisV1, ArchivedTransaction,
    ArchivedTransactionProofBasisV1, GetBlockRequest, GetTransactionRequest,
    IndependentGetTransactionNormalizationV1, IndependentGetTransactionProofV1, RpcFailureKind,
    INDEPENDENT_GET_TRANSACTION_PROOF_SCHEMA_VERSION,
};
use crate::transfer_fee::EpochScheduleV1;

/// A parsed RPC value bound to the exact response-body bytes received from the
/// configured provider. The body itself is deliberately not logged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RpcResponseProofV1<T> {
    pub raw_response_sha256: String,
    pub value: T,
}

/// A proof binds both the canonical JSON-RPC request and the exact raw response
/// bytes. It is used when request parameters are part of the evidence claim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundRpcResponseProofV1<T> {
    pub canonical_request_sha256: String,
    pub raw_response_sha256: String,
    pub value: T,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalizedAccountInfoV1 {
    pub context_slot: u64,
    pub owner: String,
    pub lamports: u64,
    pub executable: bool,
    pub rent_epoch: u64,
    pub data: Vec<u8>,
    pub account_data_sha256: String,
}

pub const FINALIZED_ACCOUNT_INFO_REQUEST_ID: u64 = 4;

#[derive(Clone)]
pub struct HttpArchivalRpc {
    client: Client,
    endpoint: String,
    maximum_attempts: u32,
}

impl HttpArchivalRpc {
    pub fn new(endpoint: String, maximum_attempts: u32) -> Result<Self, RpcFailureKind> {
        if endpoint.is_empty() || maximum_attempts == 0 {
            return Err(RpcFailureKind::InvalidResponse);
        }
        let client = Client::builder()
            .timeout(Duration::from_secs(45))
            .build()
            .map_err(|_| RpcFailureKind::Transport)?;
        Ok(Self {
            client,
            endpoint,
            maximum_attempts,
        })
    }

    pub async fn finalized_slot(&self) -> Result<u64, RpcFailureKind> {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getSlot",
            "params": [{"commitment": "finalized"}]
        });
        let value = self.post_with_retry(&request).await?;
        value
            .get("result")
            .and_then(JsonValue::as_u64)
            .ok_or(RpcFailureKind::InvalidResponse)
    }

    pub async fn signatures_for_address_page(
        &self,
        address: &str,
        before: Option<&str>,
        limit: u16,
    ) -> Result<Vec<ProgramSignatureV1>, RpcFailureKind> {
        Ok(self
            .signatures_for_address_page_with_proof(address, before, limit)
            .await?
            .value)
    }

    pub async fn signatures_for_address_page_with_proof(
        &self,
        address: &str,
        before: Option<&str>,
        limit: u16,
    ) -> Result<RpcResponseProofV1<Vec<ProgramSignatureV1>>, RpcFailureKind> {
        if address.is_empty() || limit == 0 || limit > 1_000 {
            return Err(RpcFailureKind::InvalidResponse);
        }
        let mut options = json!({
            "commitment": "finalized",
            "limit": limit
        });
        if let Some(before) = before {
            options["before"] = JsonValue::String(before.to_owned());
        }
        let request = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "getSignaturesForAddress",
            "params": [address, options]
        });
        let response = self.post_with_retry_proof(&request).await?;
        let value = &response.value;
        if value.get("id").and_then(JsonValue::as_u64) != Some(2) || value.get("error").is_some() {
            return Err(RpcFailureKind::InvalidResponse);
        }
        let rows = value
            .get("result")
            .and_then(JsonValue::as_array)
            .ok_or(RpcFailureKind::InvalidResponse)?;
        Ok(RpcResponseProofV1 {
            raw_response_sha256: response.raw_response_sha256,
            value: rows
                .iter()
                .map(parse_program_signature)
                .collect::<Result<_, _>>()?,
        })
    }

    pub async fn epoch_schedule_with_proof(
        &self,
    ) -> Result<RpcResponseProofV1<EpochScheduleV1>, RpcFailureKind> {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "getEpochSchedule"
        });
        let response = self.post_with_retry_proof(&request).await?;
        let schedule =
            parse_epoch_schedule_response(&response.value, 3, &response.raw_response_sha256)?;
        Ok(RpcResponseProofV1 {
            raw_response_sha256: response.raw_response_sha256,
            value: schedule,
        })
    }

    pub async fn finalized_block_with_proof(
        &self,
        request: GetBlockRequest,
    ) -> Result<RpcResponseProofV1<Option<ArchivedBlock>>, RpcFailureKind> {
        let slot = request
            .params
            .get(0)
            .and_then(JsonValue::as_u64)
            .ok_or(RpcFailureKind::InvalidResponse)?;
        let request_id = request.id;
        let request_value =
            serde_json::to_value(request).map_err(|_| RpcFailureKind::InvalidResponse)?;
        let response = self.post_with_retry_proof(&request_value).await?;
        if response.value.get("id").and_then(JsonValue::as_u64) != Some(request_id) {
            return Err(RpcFailureKind::InvalidResponse);
        }
        if let Some(error) = response.value.get("error") {
            let code = error.get("code").and_then(JsonValue::as_i64);
            return match code {
                Some(-32004 | -32009) => Ok(RpcResponseProofV1 {
                    raw_response_sha256: response.raw_response_sha256,
                    value: None,
                }),
                Some(-32005 | 429) => Err(RpcFailureKind::RateLimited),
                _ => Err(RpcFailureKind::InvalidResponse),
            };
        }
        let Some(result) = response.value.get("result") else {
            return Err(RpcFailureKind::InvalidResponse);
        };
        let block = if result.is_null() {
            None
        } else {
            Some(parse_block(slot, result)?)
        };
        Ok(RpcResponseProofV1 {
            raw_response_sha256: response.raw_response_sha256,
            value: block,
        })
    }

    pub async fn finalized_ordered_signature_block_with_proof(
        &self,
        request: GetBlockRequest,
    ) -> Result<RpcResponseProofV1<Option<ArchivedBlock>>, RpcFailureKind> {
        let slot = request
            .params
            .get(0)
            .and_then(JsonValue::as_u64)
            .ok_or(RpcFailureKind::InvalidResponse)?;
        if request != finalized_get_block_signatures_request(slot, request.id) {
            return Err(RpcFailureKind::InvalidResponse);
        }
        let request_id = request.id;
        let request_value =
            serde_json::to_value(request).map_err(|_| RpcFailureKind::InvalidResponse)?;
        let response = self.post_with_retry_proof(&request_value).await?;
        if response.value.get("id").and_then(JsonValue::as_u64) != Some(request_id) {
            return Err(RpcFailureKind::InvalidResponse);
        }
        if let Some(error) = response.value.get("error") {
            let code = error.get("code").and_then(JsonValue::as_i64);
            return match code {
                Some(-32004 | -32009) => Ok(RpcResponseProofV1 {
                    raw_response_sha256: response.raw_response_sha256,
                    value: None,
                }),
                Some(-32005 | 429) => Err(RpcFailureKind::RateLimited),
                _ => Err(RpcFailureKind::InvalidResponse),
            };
        }
        let Some(result) = response.value.get("result") else {
            return Err(RpcFailureKind::InvalidResponse);
        };
        let block = if result.is_null() {
            None
        } else {
            Some(parse_ordered_signature_block(
                slot,
                result,
                &response.raw_response_sha256,
            )?)
        };
        Ok(RpcResponseProofV1 {
            raw_response_sha256: response.raw_response_sha256,
            value: block,
        })
    }

    pub async fn finalized_independent_transaction_with_proof(
        &self,
        request: GetTransactionRequest,
    ) -> Result<RpcResponseProofV1<Option<IndependentGetTransactionProofV1>>, RpcFailureKind> {
        let signature = request
            .params
            .get(0)
            .and_then(JsonValue::as_str)
            .ok_or(RpcFailureKind::InvalidResponse)?
            .to_owned();
        if request != finalized_get_transaction_request(&signature, request.id) {
            return Err(RpcFailureKind::InvalidResponse);
        }
        let request_id = request.id;
        let request_value =
            serde_json::to_value(request).map_err(|_| RpcFailureKind::InvalidResponse)?;
        let response = self.post_with_retry_proof(&request_value).await?;
        if response.value.get("id").and_then(JsonValue::as_u64) != Some(request_id) {
            return Err(RpcFailureKind::InvalidResponse);
        }
        if let Some(error) = response.value.get("error") {
            let code = error.get("code").and_then(JsonValue::as_i64);
            return match code {
                Some(-32004 | -32009) => Ok(RpcResponseProofV1 {
                    raw_response_sha256: response.raw_response_sha256,
                    value: None,
                }),
                Some(-32005 | 429) => Err(RpcFailureKind::RateLimited),
                _ => Err(RpcFailureKind::InvalidResponse),
            };
        }
        let Some(result) = response.value.get("result") else {
            return Err(RpcFailureKind::InvalidResponse);
        };
        let transaction = if result.is_null() {
            None
        } else {
            Some(parse_independent_get_transaction(
                &signature,
                result,
                &response.raw_response_sha256,
            )?)
        };
        Ok(RpcResponseProofV1 {
            raw_response_sha256: response.raw_response_sha256,
            value: transaction,
        })
    }

    /// Fetches a finalized account at a context no older than `minimum_slot`.
    /// The returned context is preserved and must not be described as
    /// historical state at `minimum_slot`.
    pub async fn finalized_account_info_at_or_after_slot_with_proof(
        &self,
        address: &str,
        cutoff_slot: u64,
    ) -> Result<BoundRpcResponseProofV1<FinalizedAccountInfoV1>, RpcFailureKind> {
        let request = finalized_account_info_request(address, cutoff_slot)?;
        let request_bytes =
            serde_json::to_vec(&request).map_err(|_| RpcFailureKind::InvalidResponse)?;
        let response = self.post_with_retry_proof(&request).await?;
        let value = parse_finalized_account_info_response(
            &response.value,
            FINALIZED_ACCOUNT_INFO_REQUEST_ID,
            cutoff_slot,
        )?;
        Ok(BoundRpcResponseProofV1 {
            canonical_request_sha256: sha256_hex(&request_bytes),
            raw_response_sha256: response.raw_response_sha256,
            value,
        })
    }

    async fn get_block_inner(
        &self,
        request: GetBlockRequest,
    ) -> Result<Option<ArchivedBlock>, RpcFailureKind> {
        Ok(self.finalized_block_with_proof(request).await?.value)
    }

    async fn post_with_retry(&self, request: &JsonValue) -> Result<JsonValue, RpcFailureKind> {
        Ok(self.post_with_retry_proof(request).await?.value)
    }

    async fn post_with_retry_proof(
        &self,
        request: &JsonValue,
    ) -> Result<RpcResponseProofV1<JsonValue>, RpcFailureKind> {
        let mut last = RpcFailureKind::Transport;
        for attempt in 0..self.maximum_attempts {
            match self.client.post(&self.endpoint).json(request).send().await {
                Ok(response) if response.status() == StatusCode::TOO_MANY_REQUESTS => {
                    last = RpcFailureKind::RateLimited;
                }
                Ok(response) if response.status().is_success() => {
                    match response.bytes().await {
                        Ok(bytes) => match serde_json::from_slice::<JsonValue>(&bytes) {
                            Ok(value)
                                if value
                                    .get("error")
                                    .and_then(|error| error.get("code"))
                                    .and_then(JsonValue::as_i64)
                                    .is_some_and(|code| matches!(code, -32005 | 429)) =>
                            {
                                // Some Solana providers report throttling inside a
                                // successful HTTP response. Treat it exactly like
                                // HTTP 429 so one transient provider response does
                                // not become a permanent archival coverage gap.
                                last = RpcFailureKind::RateLimited;
                            }
                            Ok(value) => {
                                return Ok(RpcResponseProofV1 {
                                    raw_response_sha256: sha256_hex(&bytes),
                                    value,
                                });
                            }
                            Err(_) => last = RpcFailureKind::InvalidResponse,
                        },
                        Err(_) => last = RpcFailureKind::Transport,
                    }
                }
                Ok(_) => last = RpcFailureKind::Transport,
                Err(_) => last = RpcFailureKind::Transport,
            }
            if attempt + 1 < self.maximum_attempts {
                let delay_ms = 250_u64.saturating_mul(1_u64 << attempt.min(5));
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
        }
        Err(last)
    }
}

pub fn finalized_account_info_request_sha256(
    address: &str,
    cutoff_slot: u64,
) -> Result<String, RpcFailureKind> {
    let request = finalized_account_info_request(address, cutoff_slot)?;
    let bytes = serde_json::to_vec(&request).map_err(|_| RpcFailureKind::InvalidResponse)?;
    Ok(sha256_hex(&bytes))
}

fn finalized_account_info_request(
    address: &str,
    cutoff_slot: u64,
) -> Result<JsonValue, RpcFailureKind> {
    Pubkey::from_str(address).map_err(|_| RpcFailureKind::InvalidResponse)?;
    Ok(json!({
        "jsonrpc": "2.0",
        "id": FINALIZED_ACCOUNT_INFO_REQUEST_ID,
        "method": "getAccountInfo",
        "params": [address, {
            "commitment": "finalized",
            "encoding": "base64",
            "minContextSlot": cutoff_slot
        }]
    }))
}

fn parse_finalized_account_info_response(
    value: &JsonValue,
    request_id: u64,
    cutoff_slot: u64,
) -> Result<FinalizedAccountInfoV1, RpcFailureKind> {
    if value.get("id").and_then(JsonValue::as_u64) != Some(request_id)
        || value.get("error").is_some()
    {
        return Err(RpcFailureKind::InvalidResponse);
    }
    let result = value
        .get("result")
        .and_then(JsonValue::as_object)
        .ok_or(RpcFailureKind::InvalidResponse)?;
    let context_slot = result
        .get("context")
        .and_then(JsonValue::as_object)
        .and_then(|context| context.get("slot"))
        .and_then(JsonValue::as_u64)
        .ok_or(RpcFailureKind::InvalidResponse)?;
    if context_slot < cutoff_slot {
        return Err(RpcFailureKind::InvalidResponse);
    }
    let account = result
        .get("value")
        .and_then(JsonValue::as_object)
        .ok_or(RpcFailureKind::InvalidResponse)?;
    let data = account
        .get("data")
        .and_then(JsonValue::as_array)
        .filter(|items| items.len() == 2)
        .ok_or(RpcFailureKind::InvalidResponse)?;
    if data.get(1).and_then(JsonValue::as_str) != Some("base64") {
        return Err(RpcFailureKind::InvalidResponse);
    }
    let data = decode_standard_base64(
        data.first()
            .and_then(JsonValue::as_str)
            .ok_or(RpcFailureKind::InvalidResponse)?,
    )
    .map_err(|()| RpcFailureKind::InvalidResponse)?;
    if account
        .get("space")
        .and_then(JsonValue::as_u64)
        .is_some_and(|space| space != data.len() as u64)
    {
        return Err(RpcFailureKind::InvalidResponse);
    }
    Ok(FinalizedAccountInfoV1 {
        context_slot,
        owner: required_string(&JsonValue::Object(account.clone()), "owner")?,
        lamports: account
            .get("lamports")
            .and_then(JsonValue::as_u64)
            .ok_or(RpcFailureKind::InvalidResponse)?,
        executable: account
            .get("executable")
            .and_then(JsonValue::as_bool)
            .ok_or(RpcFailureKind::InvalidResponse)?,
        rent_epoch: account
            .get("rentEpoch")
            .and_then(JsonValue::as_u64)
            .ok_or(RpcFailureKind::InvalidResponse)?,
        account_data_sha256: sha256_hex(&data),
        data,
    })
}

fn parse_epoch_schedule_response(
    value: &JsonValue,
    request_id: u64,
    raw_response_sha256: &str,
) -> Result<EpochScheduleV1, RpcFailureKind> {
    if value.get("id").and_then(JsonValue::as_u64) != Some(request_id)
        || value.get("error").is_some()
        || raw_response_sha256.len() != 64
    {
        return Err(RpcFailureKind::InvalidResponse);
    }
    let result = value
        .get("result")
        .and_then(JsonValue::as_object)
        .ok_or(RpcFailureKind::InvalidResponse)?;
    let schedule = EpochScheduleV1 {
        slots_per_epoch: result
            .get("slotsPerEpoch")
            .and_then(JsonValue::as_u64)
            .ok_or(RpcFailureKind::InvalidResponse)?,
        leader_schedule_slot_offset: result
            .get("leaderScheduleSlotOffset")
            .and_then(JsonValue::as_u64)
            .ok_or(RpcFailureKind::InvalidResponse)?,
        warmup: result
            .get("warmup")
            .and_then(JsonValue::as_bool)
            .ok_or(RpcFailureKind::InvalidResponse)?,
        first_normal_epoch: result
            .get("firstNormalEpoch")
            .and_then(JsonValue::as_u64)
            .ok_or(RpcFailureKind::InvalidResponse)?,
        first_normal_slot: result
            .get("firstNormalSlot")
            .and_then(JsonValue::as_u64)
            .ok_or(RpcFailureKind::InvalidResponse)?,
        source_response_sha256: raw_response_sha256.to_owned(),
    };
    schedule
        .validate()
        .map_err(|_| RpcFailureKind::InvalidResponse)?;
    Ok(schedule)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramSignatureV1 {
    pub signature: String,
    pub slot: u64,
    pub succeeded: bool,
    pub rpc_block_time_hint: Option<i64>,
}

fn parse_program_signature(value: &JsonValue) -> Result<ProgramSignatureV1, RpcFailureKind> {
    let err = value.get("err").ok_or(RpcFailureKind::InvalidResponse)?;
    if !err.is_null() && !err.is_object() && !err.is_array() && !err.is_string() {
        return Err(RpcFailureKind::InvalidResponse);
    }
    Ok(ProgramSignatureV1 {
        signature: required_string(value, "signature")?,
        slot: value
            .get("slot")
            .and_then(JsonValue::as_u64)
            .ok_or(RpcFailureKind::InvalidResponse)?,
        succeeded: err.is_null(),
        // This is retained only as a diagnostic hint. Canonical block time is
        // always sourced again from getBlock.
        rpc_block_time_hint: value.get("blockTime").and_then(JsonValue::as_i64),
    })
}

impl ArchivalRpcTransport for HttpArchivalRpc {
    fn get_block<'a>(
        &'a self,
        request: GetBlockRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Option<ArchivedBlock>, RpcFailureKind>> + Send + 'a>>
    {
        Box::pin(async move { self.get_block_inner(request).await })
    }
}

fn parse_optional_block_time(value: &JsonValue) -> Result<Option<i64>, RpcFailureKind> {
    match value.get("blockTime") {
        Some(JsonValue::Number(number)) => number
            .as_i64()
            .map(Some)
            .ok_or(RpcFailureKind::InvalidResponse),
        Some(JsonValue::Null) | None => Ok(None),
        _ => Err(RpcFailureKind::InvalidResponse),
    }
}

fn parse_ordered_signature_block(
    slot: u64,
    result: &JsonValue,
    raw_response_sha256: &str,
) -> Result<ArchivedBlock, RpcFailureKind> {
    let ordered_signatures = result
        .get("signatures")
        .and_then(JsonValue::as_array)
        .ok_or(RpcFailureKind::InvalidResponse)?
        .iter()
        .map(|signature| {
            signature
                .as_str()
                .map(str::to_owned)
                .ok_or(RpcFailureKind::InvalidResponse)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let block = ArchivedBlock {
        slot,
        block_time_unix_seconds: parse_optional_block_time(result)?,
        blockhash: required_string(result, "blockhash")?,
        previous_blockhash: required_string(result, "previousBlockhash")?,
        parent_slot: result
            .get("parentSlot")
            .and_then(JsonValue::as_u64)
            .ok_or(RpcFailureKind::InvalidResponse)?,
        full_transaction_count: ordered_signatures.len() as u64,
        full_transaction_inventory_sha256: ordered_signature_inventory_sha256(&ordered_signatures)?,
        proof_basis: ArchivedBlockProofBasisV1::OrderedSignaturesV2 {
            ordered_signatures,
            canonical_request_sha256: ordered_signature_block_request_sha256(slot),
            raw_response_sha256: raw_response_sha256.to_owned(),
        },
        transactions: Vec::new(),
    };
    block.validate_proof_basis()?;
    Ok(block)
}

fn parse_independent_get_transaction(
    requested_signature: &str,
    result: &JsonValue,
    raw_response_sha256: &str,
) -> Result<IndependentGetTransactionProofV1, RpcFailureKind> {
    let transaction = result
        .get("transaction")
        .cloned()
        .ok_or(RpcFailureKind::InvalidResponse)?;
    if transaction
        .get("signatures")
        .and_then(JsonValue::as_array)
        .and_then(|signatures| signatures.first())
        .and_then(JsonValue::as_str)
        != Some(requested_signature)
    {
        return Err(RpcFailureKind::InvalidResponse);
    }
    let raw_meta = result
        .get("meta")
        .cloned()
        .ok_or(RpcFailureKind::InvalidResponse)?;
    let err = raw_meta.get("err").ok_or(RpcFailureKind::InvalidResponse)?;
    if !err.is_null() && !err.is_object() && !err.is_array() && !err.is_string() {
        return Err(RpcFailureKind::InvalidResponse);
    }
    let succeeded = err.is_null();
    let meta = normalize_independent_get_transaction_meta(raw_meta)?;
    let proof = IndependentGetTransactionProofV1 {
        schema_version: INDEPENDENT_GET_TRANSACTION_PROOF_SCHEMA_VERSION,
        slot: result
            .get("slot")
            .and_then(JsonValue::as_u64)
            .ok_or(RpcFailureKind::InvalidResponse)?,
        block_time_unix_seconds: parse_optional_block_time(result)?,
        signature: requested_signature.to_owned(),
        succeeded,
        rpc_payload_sha256: canonical_transaction_payload_sha256(&transaction, &meta),
        canonical_request_sha256: independent_get_transaction_request_sha256(requested_signature),
        raw_response_sha256: raw_response_sha256.to_owned(),
        normalization: IndependentGetTransactionNormalizationV1::RemoveMetaRewardsV1,
        transaction,
        meta,
    };
    proof.validate()?;
    Ok(proof)
}

fn parse_block(slot: u64, result: &JsonValue) -> Result<ArchivedBlock, RpcFailureKind> {
    let block_time_unix_seconds = parse_optional_block_time(result)?;
    let blockhash = required_string(result, "blockhash")?;
    let previous_blockhash = required_string(result, "previousBlockhash")?;
    let parent_slot = result
        .get("parentSlot")
        .and_then(JsonValue::as_u64)
        .ok_or(RpcFailureKind::InvalidResponse)?;
    let raw_transactions = result
        .get("transactions")
        .and_then(JsonValue::as_array)
        .ok_or(RpcFailureKind::InvalidResponse)?;
    let mut transactions = Vec::with_capacity(raw_transactions.len());
    for (transaction_index, raw) in raw_transactions.iter().enumerate() {
        let rpc_payload_sha256 =
            sha256_hex(&serde_json::to_vec(raw).map_err(|_| RpcFailureKind::InvalidResponse)?);
        let transaction = raw
            .get("transaction")
            .cloned()
            .ok_or(RpcFailureKind::InvalidResponse)?;
        let meta = raw
            .get("meta")
            .cloned()
            .ok_or(RpcFailureKind::InvalidResponse)?;
        let signature = transaction
            .get("signatures")
            .and_then(JsonValue::as_array)
            .and_then(|signatures| signatures.first())
            .and_then(JsonValue::as_str)
            .ok_or(RpcFailureKind::InvalidResponse)?
            .to_owned();
        let err = meta.get("err").ok_or(RpcFailureKind::InvalidResponse)?;
        if !err.is_null() && !err.is_object() && !err.is_array() && !err.is_string() {
            return Err(RpcFailureKind::InvalidResponse);
        }
        let succeeded = err.is_null();
        transactions.push(ArchivedTransaction {
            signature,
            transaction_index: u32::try_from(transaction_index)
                .map_err(|_| RpcFailureKind::InvalidResponse)?,
            succeeded,
            rpc_payload_sha256,
            proof_basis: ArchivedTransactionProofBasisV1::FullBlockPayloadV1,
            transaction,
            meta,
        });
    }
    let full_transaction_inventory_sha256 =
        full_payload_transaction_inventory_sha256(&transactions)?;
    Ok(ArchivedBlock {
        slot,
        block_time_unix_seconds,
        blockhash,
        previous_blockhash,
        parent_slot,
        full_transaction_count: transactions.len() as u64,
        full_transaction_inventory_sha256,
        proof_basis: ArchivedBlockProofBasisV1::FullPayloadV1,
        transactions,
    })
}

fn required_string(value: &JsonValue, key: &str) -> Result<String, RpcFailureKind> {
    value
        .get(key)
        .and_then(JsonValue::as_str)
        .map(str::to_owned)
        .ok_or(RpcFailureKind::InvalidResponse)
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
        if c_padding && !d_padding || (c_padding || d_padding) && !is_last {
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
    use std::collections::BTreeSet;

    use super::*;
    use crate::extract::{compose_ordered_signature_block, validate_full_vs_composed_equivalence};

    fn test_signature(byte: u8) -> String {
        bs58::encode([byte; 64]).into_string()
    }

    #[test]
    fn response_transaction_order_is_array_order() {
        let result = json!({
            "blockTime": 100,
            "blockhash": "current",
            "previousBlockhash": "previous",
            "parentSlot": 9,
            "transactions": [
                {"transaction": {"signatures": ["a"], "message": {}}, "meta": {"err": null}},
                {"transaction": {"signatures": ["b"], "message": {}}, "meta": {"err": {"InstructionError": [0, "x"]}}}
            ]
        });
        let block = parse_block(10, &result).unwrap();
        assert_eq!(block.full_transaction_count, 2);
        assert_eq!(block.full_transaction_inventory_sha256.len(), 64);
        assert_eq!(block.transactions[0].transaction_index, 0);
        assert_eq!(block.transactions[1].transaction_index, 1);
        assert!(block.transactions[0].succeeded);
        assert!(!block.transactions[1].succeeded);
    }

    #[test]
    fn full_payload_and_lightweight_composition_are_exactly_equivalent_after_rewards_rule() {
        let unrelated = test_signature(1);
        let target = test_signature(2);
        let unrelated_transaction = json!({
            "signatures": [unrelated],
            "message": {"accountKeys": [], "instructions": [], "recentBlockhash": "recent"}
        });
        let target_transaction = json!({
            "signatures": [target],
            "message": {"accountKeys": ["account"], "instructions": [{"programIdIndex": 0}], "recentBlockhash": "recent"}
        });
        let unrelated_meta = json!({
            "err": null,
            "fee": 5,
            "innerInstructions": null,
            "logMessages": [],
            "postBalances": [9],
            "preBalances": [10]
        });
        let target_meta = json!({
            "err": null,
            "fee": 7,
            "innerInstructions": [{"index": 0, "instructions": []}],
            "logMessages": ["Program log: exact"],
            "nested": {"rewards": ["must-remain"]},
            "postBalances": [18],
            "preBalances": [20]
        });
        let full_result = json!({
            "blockTime": 100,
            "blockhash": "current",
            "previousBlockhash": "previous",
            "parentSlot": 9,
            "transactions": [
                {"transaction": unrelated_transaction, "meta": unrelated_meta},
                {"transaction": target_transaction, "meta": target_meta}
            ]
        });
        let full = parse_block(10, &full_result).unwrap();
        full.validate_proof_basis().unwrap();

        let signature_result = json!({
            "blockTime": 100,
            "blockhash": "current",
            "previousBlockhash": "previous",
            "parentSlot": 9,
            "signatures": [unrelated, target]
        });
        let signature_block =
            parse_ordered_signature_block(10, &signature_result, &"a".repeat(64)).unwrap();
        let get_transaction_result = json!({
            "slot": 10,
            "blockTime": 100,
            "transaction": target_transaction,
            "meta": {
                "err": null,
                "fee": 7,
                "innerInstructions": [{"index": 0, "instructions": []}],
                "logMessages": ["Program log: exact"],
                "nested": {"rewards": ["must-remain"]},
                "postBalances": [18],
                "preBalances": [20],
                "rewards": [{"lamports": 1, "pubkey": "ignored-only-at-this-path"}]
            }
        });
        let target_proof =
            parse_independent_get_transaction(&target, &get_transaction_result, &"b".repeat(64))
                .unwrap();
        assert!(target_proof.meta.get("rewards").is_none());
        assert_eq!(
            target_proof
                .meta
                .get("nested")
                .and_then(|nested| nested.get("rewards")),
            Some(&json!(["must-remain"]))
        );
        let mut non_normalized = target_proof.clone();
        non_normalized.meta["rewards"] = json!([]);
        non_normalized.rpc_payload_sha256 =
            canonical_transaction_payload_sha256(&non_normalized.transaction, &non_normalized.meta);
        assert_eq!(
            non_normalized.validate(),
            Err(RpcFailureKind::InvalidResponse)
        );

        let expected = BTreeSet::from([target.clone()]);
        let composed =
            compose_ordered_signature_block(signature_block, &expected, vec![target_proof])
                .unwrap();
        validate_full_vs_composed_equivalence(&full, &composed, &expected).unwrap();
        assert_eq!(composed.transactions[0].transaction_index, 1);
        assert_eq!(composed.transactions[0].transaction, target_transaction);
        assert_eq!(composed.transactions[0].meta, target_meta);
        assert_eq!(
            composed.transactions[0].rpc_payload_sha256,
            full.transactions[1].rpc_payload_sha256
        );

        let mut tampered = composed.clone();
        tampered.transactions[0].meta["fee"] = json!(8);
        tampered.transactions[0].rpc_payload_sha256 = canonical_transaction_payload_sha256(
            &tampered.transactions[0].transaction,
            &tampered.transactions[0].meta,
        );
        tampered.validate_proof_basis().unwrap();
        assert_eq!(
            validate_full_vs_composed_equivalence(&full, &tampered, &expected),
            Err(RpcFailureKind::InvalidResponse)
        );
    }

    #[test]
    fn missing_block_time_is_retained_for_hard_gate() {
        let result = json!({
            "blockTime": null,
            "blockhash": "current",
            "previousBlockhash": "previous",
            "parentSlot": 9,
            "transactions": []
        });
        assert_eq!(
            parse_block(10, &result).unwrap().block_time_unix_seconds,
            None
        );
    }

    #[test]
    fn missing_transaction_status_never_becomes_a_failed_coverage_row() {
        let signature = json!({
            "signature": "signature",
            "slot": 10,
            "blockTime": 100
        });
        assert_eq!(
            parse_program_signature(&signature),
            Err(RpcFailureKind::InvalidResponse)
        );

        let block = json!({
            "blockTime": 100,
            "blockhash": "current",
            "previousBlockhash": "previous",
            "parentSlot": 9,
            "transactions": [
                {"transaction": {"signatures": ["a"], "message": {}}, "meta": {}}
            ]
        });
        assert_eq!(
            parse_block(10, &block),
            Err(RpcFailureKind::InvalidResponse)
        );
    }

    #[test]
    fn epoch_schedule_is_bound_to_the_raw_response_hash() {
        let hash = "a".repeat(64);
        let response = json!({
            "jsonrpc": "2.0",
            "id": 3,
            "result": {
                "slotsPerEpoch": 432000,
                "leaderScheduleSlotOffset": 432000,
                "warmup": false,
                "firstNormalEpoch": 0,
                "firstNormalSlot": 0
            }
        });
        let schedule = parse_epoch_schedule_response(&response, 3, &hash).unwrap();
        assert_eq!(schedule.source_response_sha256, hash);
        assert_eq!(schedule.slots_per_epoch, 432_000);
    }

    #[test]
    fn account_info_requires_a_context_at_or_after_the_minimum_slot() {
        let response = json!({
            "jsonrpc": "2.0",
            "id": FINALIZED_ACCOUNT_INFO_REQUEST_ID,
            "result": {
                "context": {"slot": 42},
                "value": {
                    "data": ["AQID", "base64"],
                    "executable": false,
                    "lamports": 123,
                    "owner": Pubkey::new_from_array([9; 32]).to_string(),
                    "rentEpoch": 7,
                    "space": 3
                }
            }
        });
        let account =
            parse_finalized_account_info_response(&response, FINALIZED_ACCOUNT_INFO_REQUEST_ID, 42)
                .unwrap();
        assert_eq!(account.data, vec![1, 2, 3]);
        assert_eq!(account.account_data_sha256, sha256_hex(&[1, 2, 3]));
        assert_eq!(
            parse_finalized_account_info_response(&response, FINALIZED_ACCOUNT_INFO_REQUEST_ID, 43),
            Err(RpcFailureKind::InvalidResponse)
        );
        assert!(parse_finalized_account_info_response(
            &response,
            FINALIZED_ACCOUNT_INFO_REQUEST_ID,
            41
        )
        .is_ok());
    }

    #[test]
    fn post_cutoff_account_request_hash_binds_address_and_minimum_slot() {
        let first = Pubkey::new_from_array([1; 32]).to_string();
        let second = Pubkey::new_from_array([2; 32]).to_string();
        let hash = finalized_account_info_request_sha256(&first, 42).unwrap();
        assert_eq!(hash.len(), 64);
        assert_ne!(
            hash,
            finalized_account_info_request_sha256(&first, 43).unwrap()
        );
        assert_ne!(
            hash,
            finalized_account_info_request_sha256(&second, 42).unwrap()
        );
    }
}
