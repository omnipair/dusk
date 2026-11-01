//! Natural-grain reconciliation of indexer rows against RPC-canonical events.
//!
//! PostgreSQL is evidence, not ordering truth. Rows are matched only within the
//! same finalized slot/signature/event kind and every populated raw field that
//! exists in both sources is compared after a lossless scalar normalization.
//! SQL NULL payload columns are indexer omissions, not observed values.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::{
    audit::{ReconciliationClassification, ReconciliationRecord},
    canonical::{CanonicalEventV1, EventPayloadV1, LiquidityAction},
    extract::EnrichedDbEventSeed,
};

pub const SOURCE_RECONCILIATION_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketSourceCoverageV1 {
    pub expected_records: u64,
    pub reconciled_records: u64,
    pub blocking_records: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceReconciliationSummaryV1 {
    pub schema_version: u16,
    pub database_rows: u64,
    pub rpc_events_in_db_covered_families: u64,
    pub expected_records: u64,
    pub reconciled_records: u64,
    pub blocking_records: u64,
    pub classifications: BTreeMap<String, u64>,
    pub by_market: BTreeMap<String, MarketSourceCoverageV1>,
}

impl SourceReconciliationSummaryV1 {
    pub fn new() -> Self {
        Self {
            schema_version: SOURCE_RECONCILIATION_SCHEMA_VERSION,
            ..Self::default()
        }
    }

    pub fn record(&mut self, market: Option<&str>, classification: ReconciliationClassification) {
        self.expected_records = self.expected_records.saturating_add(1);
        let blocking = classification.blocks_acceptance();
        if blocking {
            self.blocking_records = self.blocking_records.saturating_add(1);
        } else {
            self.reconciled_records = self.reconciled_records.saturating_add(1);
        }
        *self
            .classifications
            .entry(format!("{classification:?}").to_ascii_lowercase())
            .or_default() += 1;
        if let Some(market) = market {
            let coverage = self.by_market.entry(market.to_owned()).or_default();
            coverage.expected_records = coverage.expected_records.saturating_add(1);
            if blocking {
                coverage.blocking_records = coverage.blocking_records.saturating_add(1);
            } else {
                coverage.reconciled_records = coverage.reconciled_records.saturating_add(1);
            }
        }
    }
}

#[derive(Clone, Debug)]
struct RpcCandidate<'a> {
    event: &'a CanonicalEventV1,
    kind: &'static str,
    fields: BTreeMap<String, String>,
}

/// Reconciles one finalized slot. Callers stream and group JSONL by slot so the
/// full history never needs to be retained in memory.
pub fn reconcile_slot_events(
    database: &[EnrichedDbEventSeed],
    rpc: &[CanonicalEventV1],
) -> Vec<ReconciliationRecord> {
    let mut candidates: Vec<_> = rpc
        .iter()
        .filter_map(|event| {
            let kind = canonical_event_kind(&event.payload)?;
            Some(RpcCandidate {
                event,
                kind,
                fields: canonical_fields(event),
            })
        })
        .collect();
    candidates.sort_by_key(|candidate| candidate.event.order);
    let mut used = BTreeSet::new();
    let mut records = Vec::new();

    for db in database {
        let db_ref = format!("{}:{}", db.db.source_table, db.db.source_id);
        let db_fields = database_fields(db);
        let matching_group: Vec<usize> = candidates
            .iter()
            .enumerate()
            .filter(|(_, candidate)| {
                candidate.event.signature == db.db.signature && candidate.kind == db.db.event_kind
            })
            .map(|(index, _)| index)
            .collect();
        let mut ranked: Vec<(usize, Vec<String>)> = matching_group
            .iter()
            .copied()
            .filter(|index| !used.contains(index))
            .map(|index| {
                let differing = differing_db_fields(&db_fields, &candidates[index].fields);
                (index, differing)
            })
            .collect();
        ranked.sort_by(|left, right| {
            left.1.len().cmp(&right.1.len()).then_with(|| {
                candidates[left.0]
                    .event
                    .order
                    .cmp(&candidates[right.0].event.order)
            })
        });

        if let Some((index, differing_fields)) = ranked.into_iter().next() {
            used.insert(index);
            let candidate = &candidates[index];
            let classification = if differing_fields.is_empty() {
                if db.parsed_instruction_path.is_none() {
                    ReconciliationClassification::OrderingRecoveredFromRpc
                } else {
                    ReconciliationClassification::Exact
                }
            } else {
                ReconciliationClassification::FieldMismatch
            };
            records.push(ReconciliationRecord {
                signature: db.db.signature.clone(),
                db_event_ref: Some(db_ref),
                rpc_event_id: Some(candidate.event.event_id.clone()),
                classification,
                differing_fields,
                detail_code: None,
            });
            continue;
        }

        let duplicate = matching_group
            .iter()
            .any(|index| differing_db_fields(&db_fields, &candidates[*index].fields).is_empty());
        records.push(ReconciliationRecord {
            signature: db.db.signature.clone(),
            db_event_ref: Some(db_ref),
            rpc_event_id: None,
            classification: if duplicate {
                ReconciliationClassification::DbDuplicate
            } else {
                ReconciliationClassification::RpcMissingCritical
            },
            differing_fields: Vec::new(),
            detail_code: Some(
                if duplicate {
                    "duplicate_database_event"
                } else {
                    "rpc_event_absent"
                }
                .to_owned(),
            ),
        });
    }

    for (index, candidate) in candidates.iter().enumerate() {
        if used.contains(&index) {
            continue;
        }
        let same_transaction_family_exists = database.iter().any(|db| {
            db.db.signature == candidate.event.signature && db.db.event_kind == candidate.kind
        });
        records.push(ReconciliationRecord {
            signature: candidate.event.signature.clone(),
            db_event_ref: None,
            rpc_event_id: Some(candidate.event.event_id.clone()),
            classification: if same_transaction_family_exists {
                ReconciliationClassification::DbCollapsedSameTransaction
            } else {
                ReconciliationClassification::DbMissingRpcCanonical
            },
            differing_fields: Vec::new(),
            detail_code: Some(
                if same_transaction_family_exists {
                    "database_uniqueness_collapsed_event"
                } else {
                    "database_row_absent"
                }
                .to_owned(),
            ),
        });
    }
    records
}

pub fn canonical_event_kind(payload: &EventPayloadV1) -> Option<&'static str> {
    Some(match payload {
        EventPayloadV1::Swap(_) => "swap",
        EventPayloadV1::Liquidity(_) => "liquidity",
        EventPayloadV1::LpPosition(_) => "lp_position",
        EventPayloadV1::CollateralDelta(_) => "collateral_delta",
        EventPayloadV1::DebtDelta(_) => "debt_delta",
        EventPayloadV1::Position(_) => "position",
        EventPayloadV1::Liquidation(_) => "liquidation",
        EventPayloadV1::PairUpdate(_) => "pair_update",
        // These families do not exist in the canonical DB query and therefore
        // have no shared raw fields to compare.
        EventPayloadV1::Flashloan(_)
        | EventPayloadV1::ProtocolFeeClaim(_)
        | EventPayloadV1::Config(_) => return None,
    })
}

fn canonical_fields(event: &CanonicalEventV1) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    fields.insert("pair".to_owned(), event.pair.clone().unwrap_or_default());
    fields.insert("actor".to_owned(), event.actor.clone().unwrap_or_default());
    fields.insert("slot".to_owned(), event.order.slot.to_string());
    fields.insert(
        "transaction_index".to_owned(),
        event.order.transaction_index.to_string(),
    );
    fields.insert(
        "block_time_unix_seconds".to_owned(),
        event.block_time_unix_seconds.to_string(),
    );
    fields.insert(
        "instruction_index".to_owned(),
        event.order.outer_instruction_index.to_string(),
    );
    fields.insert(
        "instruction_path".to_owned(),
        path_text(&event.execution_path),
    );
    let value = serde_json::to_value(&event.payload).expect("canonical event payload serializes");
    if let Some(payload) = value.get("payload").and_then(JsonValue::as_object) {
        flatten_scalars("payload", payload, &mut fields);
    }
    if let EventPayloadV1::Liquidity(payload) = &event.payload {
        fields.insert(
            "payload.action".to_owned(),
            match payload.action {
                LiquidityAction::Mint => "add",
                LiquidityAction::Burn => "remove",
            }
            .to_owned(),
        );
    }
    fields
}

fn database_fields(event: &EnrichedDbEventSeed) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    fields.insert("pair".to_owned(), event.db.pair.clone());
    fields.insert(
        "actor".to_owned(),
        event.db.actor.clone().unwrap_or_default(),
    );
    fields.insert("slot".to_owned(), event.db.slot.to_string());
    fields.insert(
        "transaction_index".to_owned(),
        event.transaction_index.to_string(),
    );
    fields.insert(
        "block_time_unix_seconds".to_owned(),
        event.block_time_unix_seconds.to_string(),
    );
    if let Some(index) = event.db.instruction_index {
        fields.insert("instruction_index".to_owned(), index.to_string());
    }
    if let Some(path) = &event.parsed_instruction_path {
        fields.insert("instruction_path".to_owned(), path_text(path));
    }
    if let Some(payload) = event.db.payload.as_object() {
        flatten_scalars("payload", payload, &mut fields);
    }
    fields
}

fn differing_db_fields(
    database: &BTreeMap<String, String>,
    rpc: &BTreeMap<String, String>,
) -> Vec<String> {
    database
        .iter()
        .filter_map(|(field, value)| (rpc.get(field) != Some(value)).then(|| field.clone()))
        .collect()
}

fn flatten_scalars(
    prefix: &str,
    object: &serde_json::Map<String, JsonValue>,
    output: &mut BTreeMap<String, String>,
) {
    for (key, value) in object {
        let path = format!("{prefix}.{key}");
        match value {
            JsonValue::Object(nested) => flatten_scalars(&path, nested, output),
            JsonValue::Array(values) => {
                output.insert(path, serde_json::to_string(values).unwrap_or_default());
            }
            JsonValue::String(value) => {
                output.insert(path, value.clone());
            }
            JsonValue::Number(value) => {
                output.insert(path, value.to_string());
            }
            JsonValue::Bool(value) => {
                output.insert(path, value.to_string());
            }
            JsonValue::Null => {}
        }
    }
}

fn path_text(path: &[u32]) -> String {
    path.iter()
        .map(|component| format!("{component:06}"))
        .collect::<Vec<_>>()
        .join(".")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        canonical::{DbSourceRef, EventPayloadV1, SwapPayloadV1, TokenAmount},
        extract::DbEventSeed,
    };

    fn amount(value: u64) -> TokenAmount {
        TokenAmount::new(value.to_string()).unwrap()
    }

    fn rpc_event(path: Vec<u32>, amount_out: u64) -> CanonicalEventV1 {
        CanonicalEventV1 {
            event_id: format!("event-{amount_out}"),
            cluster: "mainnet-beta".to_owned(),
            program_id: "program".to_owned(),
            pair: Some("pair".to_owned()),
            actor: Some("user".to_owned()),
            signature: "sig".to_owned(),
            order: crate::canonical::ChainOrder {
                slot: 10,
                transaction_index: 3,
                outer_instruction_index: 2,
                inner_execution_index: Some(1),
                event_sequence: 0,
            },
            execution_path: path,
            block_time_unix_seconds: 100,
            decoder_version_id: "decoder".to_owned(),
            raw_payload_sha256: "hash".to_owned(),
            db_source: None::<DbSourceRef>,
            payload: EventPayloadV1::Swap(SwapPayloadV1 {
                token0_in: true,
                amount_in: amount(10),
                amount_out: amount(amount_out),
                amount_in_after_fee: amount(9),
                lp_fee: amount(1),
                protocol_fee: amount(0),
                reserve0_after: amount(110),
                reserve1_after: amount(90),
            }),
        }
    }

    fn db_event(path: Option<&str>, amount_out: u64) -> EnrichedDbEventSeed {
        EnrichedDbEventSeed {
            db: DbEventSeed {
                source_table: "swaps".to_owned(),
                source_id: 7,
                event_kind: "swap".to_owned(),
                pair: "pair".to_owned(),
                actor: Some("user".to_owned()),
                signature: "sig".to_owned(),
                slot: 10,
                instruction_index: Some(2),
                instruction_path: path.map(str::to_owned),
                payload: serde_json::json!({
                    "token0_in": true,
                    "amount_in": "10",
                    "amount_out": amount_out.to_string(),
                    "reserve0_after": "110",
                    "reserve1_after": "90",
                    "lp_fee": "1",
                    "protocol_fee": "0"
                }),
            },
            block_time_unix_seconds: 100,
            transaction_index: 3,
            parsed_instruction_path: path.map(|value| {
                value
                    .split('.')
                    .map(|component| component.parse::<u32>().unwrap())
                    .collect()
            }),
        }
    }

    #[test]
    fn exact_shared_fields_and_execution_path_reconcile() {
        let records = reconcile_slot_events(
            &[db_event(Some("000002.000000"), 8)],
            &[rpc_event(vec![2, 0], 8)],
        );
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].classification,
            ReconciliationClassification::Exact
        );
    }

    #[test]
    fn rpc_order_recovery_is_documented_and_nonblocking() {
        let records = reconcile_slot_events(&[db_event(None, 8)], &[rpc_event(vec![2, 0], 8)]);
        assert_eq!(
            records[0].classification,
            ReconciliationClassification::OrderingRecoveredFromRpc
        );
        assert!(!records[0].classification.blocks_acceptance());
    }

    #[test]
    fn wrong_payload_or_path_blocks_acceptance() {
        let records = reconcile_slot_events(
            &[db_event(Some("000002.000001"), 7)],
            &[rpc_event(vec![2, 0], 8)],
        );
        assert_eq!(
            records[0].classification,
            ReconciliationClassification::FieldMismatch
        );
        assert_eq!(
            records[0].differing_fields,
            vec!["instruction_path", "payload.amount_out"]
        );
    }

    #[test]
    fn null_database_payload_field_is_a_documented_omission() {
        let mut database = db_event(Some("000002.000000"), 8);
        database
            .db
            .payload
            .as_object_mut()
            .expect("fixture payload object")
            .insert("lp_fee".to_owned(), JsonValue::Null);

        let records = reconcile_slot_events(&[database], &[rpc_event(vec![2, 0], 8)]);
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].classification,
            ReconciliationClassification::Exact
        );
        assert!(!records[0].classification.blocks_acceptance());
    }

    #[test]
    fn field_identical_database_duplicate_is_documented_and_nonblocking() {
        let database = db_event(Some("000002.000000"), 8);
        let records =
            reconcile_slot_events(&[database.clone(), database], &[rpc_event(vec![2, 0], 8)]);
        assert_eq!(records.len(), 2);
        assert_eq!(
            records[1].classification,
            ReconciliationClassification::DbDuplicate
        );
        assert!(!records[1].classification.blocks_acceptance());
    }
}
