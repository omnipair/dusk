use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
    sync::Mutex,
};

use serde_json::json;
use v1_vs_dusk_benchmark::{
    audit::{
        evaluate_hard_gates, reconcile_fields, visible_market_eligibility, CoverageCount,
        EligibilityReason, HardGateId, HardGateInput, MarketEvidence, ReconciliationClassification,
        UsdHeadlineEligibilityReason,
    },
    canonical::{
        deterministic_event_id, ArtifactChecksum, BlockArtifactChecksumV1, BlockArtifactManifestV1,
        ChainOrder, DatasetManifestV1, DecoderVersion, ExactDecimal, HistoricalPriceMarkV1,
        SanitizedSource, SignedTokenAmount, TokenAmount,
    },
    extract::{
        checksum_archived_block_artifact, enrich_db_event, fetch_blocks_resumable,
        finalized_get_block_request, ArchivalRpcTransport, ArchivedBlock,
        ArchivedBlockProofBasisV1, ArchivedTransaction, ArchivedTransactionProofBasisV1,
        DbEventSeed, GetBlockRequest, HistoricalPriceExportCheckpoint, RpcCheckpoint,
        RpcFailureKind, EVENTS_SQL, PRICES_SQL,
    },
};

const DB_EVENT_FIXTURE: &str = r#"
{
  "source_table": "swaps",
  "source_id": 7,
  "event_kind": "swap",
  "pair": "pair-a",
  "actor": "user-a",
  "signature": "sig-a",
  "slot": 123,
  "instruction_index": 2,
  "instruction_path": "000002.000001",
  "payload": {
    "token0_in": true,
    "amount_in": "900719925474099300000",
    "amount_out": "42"
  }
}
"#;

const HISTORICAL_PRICE_FIXTURE: &str = r#"
{
  "mint": "mint-a",
  "bucket_unix_seconds": 1699999200,
  "price_usd": "12345678901234567890.00000012345678901234567890",
  "token_decimals": 9,
  "provider": "birdeye",
  "quality": "historical"
}
"#;

fn order(event_sequence: u32) -> ChainOrder {
    ChainOrder {
        slot: 123,
        transaction_index: 4,
        outer_instruction_index: 2,
        inner_execution_index: Some(1),
        event_sequence,
    }
}

#[test]
fn integer_string_amounts_are_exact_and_canonical() {
    let amount: TokenAmount = "340282366920938463463374607431768211455".parse().unwrap();
    assert_eq!(amount.as_str(), "340282366920938463463374607431768211455");
    assert_eq!(
        serde_json::to_string(&amount).unwrap(),
        format!("\"{}\"", amount)
    );
    assert!("01".parse::<TokenAmount>().is_err());
    assert!("-1".parse::<TokenAmount>().is_err());
    assert!("-0".parse::<SignedTokenAmount>().is_err());
    assert_eq!("-42".parse::<SignedTokenAmount>().unwrap().as_str(), "-42");
}

#[test]
fn event_ids_depend_on_chain_location_not_decoder_output() {
    let first = deterministic_event_id("mainnet-beta", "program", "signature", order(0));
    let repeated = deterministic_event_id("mainnet-beta", "program", "signature", order(0));
    let next = deterministic_event_id("mainnet-beta", "program", "signature", order(1));
    assert_eq!(first, repeated);
    assert_ne!(first, next);
    assert_eq!(first.len(), 64);
}

#[test]
fn manifest_never_serializes_database_credentials() {
    let secret = "postgresql://user:very-secret@example.invalid/database?sslmode=require";
    let source =
        SanitizedSource::from_secret("postgresql", "runtime_DATABASE_URL", secret).unwrap();
    let decoder = DecoderVersion {
        version_id: "v0.9.8".to_owned(),
        valid_from_slot: 100,
        valid_to_slot_exclusive: None,
        git_commit: "abc123".to_owned(),
        idl_sha256: "11".repeat(32),
        program_binary_sha256: None,
    };
    let artifact = ArtifactChecksum::from_bytes("events-0001.jsonl", b"fixture");
    let manifest = DatasetManifestV1::new(
        "2026-08-20T00:00:00Z".to_owned(),
        "mainnet-beta".to_owned(),
        "program".to_owned(),
        100,
        200,
        vec![source],
        vec![decoder],
        vec![artifact],
    )
    .unwrap();
    let serialized = serde_json::to_string(&manifest).unwrap();
    assert!(!serialized.contains("very-secret"));
    assert!(!serialized.contains("postgresql://"));
    assert!(!serialized.contains("example.invalid"));
    assert_eq!(manifest.dataset_id, manifest.compute_dataset_id());
}

#[test]
fn historical_price_marks_preserve_exact_fixed_point_text() {
    let mark: HistoricalPriceMarkV1 = serde_json::from_str(HISTORICAL_PRICE_FIXTURE).unwrap();
    mark.validate().unwrap();
    assert_eq!(
        mark.price_usd.as_str(),
        "12345678901234567890.00000012345678901234567890"
    );
    assert!(serde_json::to_string(&mark)
        .unwrap()
        .contains("\"price_usd\":\"12345678901234567890.00000012345678901234567890\""));

    assert!("0.000000000000000001".parse::<ExactDecimal>().is_ok());
    assert!("1e-18".parse::<ExactDecimal>().is_err());
    assert!(".5".parse::<ExactDecimal>().is_err());
    assert!("01.5".parse::<ExactDecimal>().is_err());
    assert!("-1".parse::<ExactDecimal>().is_err());

    let estimated = HISTORICAL_PRICE_FIXTURE.replace("historical", "estimated");
    assert!(serde_json::from_str::<HistoricalPriceMarkV1>(&estimated).is_err());
}

#[test]
fn historical_price_checkpoint_is_keyset_resumable() {
    let mark: HistoricalPriceMarkV1 = serde_json::from_str(HISTORICAL_PRICE_FIXTURE).unwrap();
    let mut checkpoint = HistoricalPriceExportCheckpoint::new(1_699_999_200);
    checkpoint.advance(&[mark], 2);
    assert_eq!(checkpoint.last_bucket_unix_seconds, 1_699_999_200);
    assert_eq!(checkpoint.last_mint, "mint-a");
    assert_eq!(checkpoint.last_provider, "birdeye");
    assert_eq!(checkpoint.rows_exported, 1);
    assert!(checkpoint.complete);
}

#[test]
fn block_artifact_manifest_checks_each_block_and_its_index() {
    let block_12 = BlockArtifactChecksumV1::from_bytes(
        12,
        "hash-12",
        11,
        "hash-11",
        2,
        "blocks/12.json",
        b"block twelve",
    );
    let block_10 = BlockArtifactChecksumV1::from_bytes(
        10,
        "hash-10",
        9,
        "hash-9",
        1,
        "blocks/10.json",
        b"block ten",
    );
    let manifest = BlockArtifactManifestV1::new(10, 12, vec![block_12, block_10]).unwrap();
    assert_eq!(
        manifest
            .blocks
            .iter()
            .map(|block| block.slot)
            .collect::<Vec<_>>(),
        vec![10, 12]
    );
    manifest.validate().unwrap();
    assert!(manifest.verify_block_bytes(10, b"block ten"));
    assert!(!manifest.verify_block_bytes(10, b"tampered"));

    let mut tampered = manifest.clone();
    tampered.blocks[0].artifact.sha256 = "00".repeat(32);
    assert!(tampered.validate().is_err());
    assert!(BlockArtifactManifestV1::new(
        10,
        12,
        vec![manifest.blocks[0].clone(), manifest.blocks[0].clone()]
    )
    .is_err());

    let archived = archived_block(123, "sig-a", 0);
    let checksum = checksum_archived_block_artifact(&archived, "blocks/123.json", b"raw block");
    assert_eq!(checksum.slot, 123);
    assert_eq!(checksum.transaction_count, 1);
    assert!(checksum.artifact.matches_bytes(b"raw block"));
}

#[test]
fn get_block_request_is_finalized_and_full() {
    let request = finalized_get_block_request(123, 9);
    assert_eq!(request.method, "getBlock");
    assert_eq!(request.params[0], 123);
    assert_eq!(request.params[1]["commitment"], "finalized");
    assert_eq!(request.params[1]["transactionDetails"], "full");
    assert_eq!(request.params[1]["rewards"], false);
}

fn archived_block(slot: u64, signature: &str, tx_index: u32) -> ArchivedBlock {
    ArchivedBlock {
        slot,
        block_time_unix_seconds: Some(1_700_000_000),
        blockhash: format!("block-{slot}"),
        previous_blockhash: format!("block-{}", slot.saturating_sub(1)),
        parent_slot: slot.saturating_sub(1),
        full_transaction_count: 1,
        full_transaction_inventory_sha256: "inventory-hash".to_owned(),
        proof_basis: ArchivedBlockProofBasisV1::FullPayloadV1,
        transactions: vec![ArchivedTransaction {
            signature: signature.to_owned(),
            transaction_index: tx_index,
            succeeded: true,
            rpc_payload_sha256: "rpc-payload-hash".to_owned(),
            proof_basis: ArchivedTransactionProofBasisV1::FullBlockPayloadV1,
            transaction: json!({"signatures": [signature]}),
            meta: json!({"err": null, "innerInstructions": []}),
        }],
    }
}

#[test]
fn enrichment_uses_rpc_block_time_and_transaction_index() {
    let seed: DbEventSeed = serde_json::from_str(DB_EVENT_FIXTURE).unwrap();
    let enriched = enrich_db_event(seed, &archived_block(123, "sig-a", 4)).unwrap();
    assert_eq!(enriched.block_time_unix_seconds, 1_700_000_000);
    assert_eq!(enriched.transaction_index, 4);
    assert_eq!(enriched.parsed_instruction_path, Some(vec![2, 1]));
}

struct MockRpc {
    blocks: Mutex<BTreeMap<u64, Option<ArchivedBlock>>>,
}

impl ArchivalRpcTransport for MockRpc {
    fn get_block<'a>(
        &'a self,
        request: GetBlockRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Option<ArchivedBlock>, RpcFailureKind>> + Send + 'a>>
    {
        let slot = request.params[0].as_u64().unwrap();
        let response = self.blocks.lock().unwrap().get(&slot).cloned().flatten();
        Box::pin(async move { Ok(response) })
    }
}

#[tokio::test]
async fn archival_fetch_is_checkpointed_and_resumable() {
    let rpc = MockRpc {
        blocks: Mutex::new(BTreeMap::from([
            (10, Some(archived_block(10, "sig-10", 0))),
            (11, None),
            (12, Some(archived_block(12, "sig-12", 0))),
        ])),
    };
    let mut checkpoint = RpcCheckpoint::default();
    let first = fetch_blocks_resumable(&rpc, &[10, 11, 12], &mut checkpoint, 2).await;
    assert_eq!(first.len(), 1);
    assert_eq!(checkpoint.completed_slots, BTreeSet::from([10]));
    assert_eq!(
        checkpoint.failures.get(&11),
        Some(&RpcFailureKind::BlockUnavailable)
    );

    let second = fetch_blocks_resumable(&rpc, &[10, 12], &mut checkpoint, 2).await;
    assert_eq!(
        second.iter().map(|block| block.slot).collect::<Vec<_>>(),
        vec![12]
    );
    assert_eq!(checkpoint.completed_slots, BTreeSet::from([10, 12]));
}

#[test]
fn reconciliation_distinguishes_documented_omissions_from_mismatches() {
    let rpc = BTreeMap::from([("amount_in".to_owned(), "10".to_owned())]);
    let omission = reconcile_fields("sig", None, Some("event".to_owned()), None, Some(&rpc));
    assert_eq!(
        omission.classification,
        ReconciliationClassification::DbMissingRpcCanonical
    );
    assert!(!omission.classification.blocks_acceptance());

    let db = BTreeMap::from([("amount_in".to_owned(), "9".to_owned())]);
    let mismatch = reconcile_fields(
        "sig",
        Some("swaps:1".to_owned()),
        Some("event".to_owned()),
        Some(&db),
        Some(&rpc),
    );
    assert_eq!(
        mismatch.classification,
        ReconciliationClassification::FieldMismatch
    );
    assert_eq!(mismatch.differing_fields, vec!["amount_in"]);
    assert!(mismatch.classification.blocks_acceptance());
}

fn complete() -> CoverageCount {
    CoverageCount {
        observed: 10,
        expected: 10,
    }
}

#[test]
fn every_hard_gate_is_exact() {
    let mut input = HardGateInput {
        transaction_inventory: complete(),
        state_changing_instruction_decode: complete(),
        state_changing_decode_blockers: 0,
        canonical_event_order: complete(),
        authoritative_block_time: complete(),
        versioned_config_coverage: complete(),
        historical_transfer_fee_coverage: complete(),
        transfer_fee_history_blockers: 0,
        db_rpc_raw_field_agreement: complete(),
        terminal_state_fields: complete(),
        economic_replay_events: complete(),
        unresolved_rpc_slots: 0,
        reconciliations: Vec::new(),
        historical_price_coverage: Some(complete()),
    };
    assert!(evaluate_hard_gates(&input).accepted);

    input.authoritative_block_time.observed = 9;
    let report = evaluate_hard_gates(&input);
    assert!(!report.accepted);
    assert_eq!(
        report
            .gates
            .iter()
            .filter(|gate| gate.blocking_records > 0)
            .count(),
        1
    );

    input.authoritative_block_time = complete();
    input.historical_transfer_fee_coverage.observed = 9;
    input.transfer_fee_history_blockers = 1;
    let report = evaluate_hard_gates(&input);
    let transfer_fee_gate = report
        .gates
        .iter()
        .find(|gate| gate.gate == HardGateId::HistoricalTransferFees)
        .unwrap();
    assert_eq!(
        transfer_fee_gate.status,
        v1_vs_dusk_benchmark::audit::GateStatus::Fail
    );
    assert_eq!(transfer_fee_gate.blocking_records, 1);
}

#[test]
fn market_filter_applies_minima_then_historical_tvl_bottom_decile() {
    let markets: Vec<MarketEvidence> = (0..10)
        .map(|index| MarketEvidence {
            pair_address: format!("pair-{index:02}"),
            visible: true,
            hard_gates_pass: true,
            swap_count: 10 + index,
            active_swap_days: 3,
            median_time_weighted_tvl_usd_micros: Some(
                ((index + 1) * 1_000_000).to_string().parse().unwrap(),
            ),
        })
        .collect();
    let result = visible_market_eligibility(&markets);
    let lowest = result
        .iter()
        .find(|market| market.pair_address == "pair-00")
        .unwrap();
    assert!(!lowest.eligible);
    assert_eq!(lowest.reason, EligibilityReason::BelowVisibleMarketTvlP10);
    assert!(!lowest.usd_headline_eligible);
    assert_eq!(
        lowest.usd_headline_reason,
        UsdHeadlineEligibilityReason::BelowVisibleMarketTvlP10
    );
    assert_eq!(
        lowest
            .visible_market_p10_tvl_usd_micros
            .as_ref()
            .unwrap()
            .as_str(),
        "2000000"
    );
    assert_eq!(
        result
            .iter()
            .filter(|market| market.usd_headline_eligible)
            .count(),
        9
    );

    let missing_tvl = visible_market_eligibility(&[MarketEvidence {
        pair_address: "raw-only".to_owned(),
        visible: true,
        hard_gates_pass: true,
        swap_count: 100,
        active_swap_days: 20,
        median_time_weighted_tvl_usd_micros: None,
    }]);
    assert!(missing_tvl[0].eligible);
    assert!(!missing_tvl[0].usd_headline_eligible);
    assert_eq!(
        missing_tvl[0].usd_headline_reason,
        UsdHeadlineEligibilityReason::HistoricalTvlUnavailable
    );

    let minima = visible_market_eligibility(&[
        MarketEvidence {
            pair_address: "nine-swaps".to_owned(),
            visible: true,
            hard_gates_pass: true,
            swap_count: 9,
            active_swap_days: 10,
            median_time_weighted_tvl_usd_micros: Some("1000000".parse().unwrap()),
        },
        MarketEvidence {
            pair_address: "two-days".to_owned(),
            visible: true,
            hard_gates_pass: true,
            swap_count: 100,
            active_swap_days: 2,
            median_time_weighted_tvl_usd_micros: Some("1000000".parse().unwrap()),
        },
    ]);
    assert_eq!(minima[0].reason, EligibilityReason::FewerThanTenSwaps);
    assert_eq!(
        minima[1].reason,
        EligibilityReason::FewerThanThreeActiveDays
    );
}

#[test]
fn database_event_sql_cannot_supply_chain_time() {
    let sql = EVENTS_SQL.to_ascii_lowercase();
    assert!(!sql.contains("event_timestamp"));
    assert!(!sql.contains("\"timestamp\""));
    assert!(!sql.contains("date_trunc"));
    assert!(sql.contains("order by slot, source_table, source_id"));
}

#[test]
fn price_sql_exports_only_exact_historical_hourly_marks() {
    let sql = PRICES_SQL.to_ascii_lowercase();
    assert!(sql.contains("from token_price_snapshots"));
    assert!(sql.contains("where quality = 'historical'"));
    assert!(sql.contains("mint = any(string_to_array($7::text, ','))"));
    assert!(sql.contains("provider = $8::text"));
    assert!(sql.contains("extract(epoch from bucket)::numeric % 3600"));
    assert!(sql.contains("price_usd > 0"));
    assert!(sql.contains("price_usd::text"));
    assert!(!sql.contains("price_usd::float"));
    assert!(!sql.contains("double precision"));
    assert!(sql.contains("order by bucket_unix_seconds, mint, provider"));
}
