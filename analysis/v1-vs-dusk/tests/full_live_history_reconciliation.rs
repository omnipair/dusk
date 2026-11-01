use std::{
    env, fs,
    path::{Path, PathBuf},
    process,
    time::{SystemTime, UNIX_EPOCH},
};

use v1_vs_dusk_benchmark::v1_reconciliation::{
    reconcile_history_to_ledger, V1ReplayCertificationPolicyV1,
};

const OPERATIONS_ENV: &str = "V1_FULL_HISTORY_OPERATIONS_JSONL";
const CONFIG_HISTORY_ENV: &str = "V1_FULL_HISTORY_CONFIG_HISTORY_JSONL";
const LEDGER_ENV: &str = "V1_FULL_HISTORY_LEDGER_JSONL";
const EXPECTED_CUTOFF_SLOT: u64 = 440_459_364;
const EXPECTED_CANONICAL_OPERATIONS: u64 = 139_812;
const EXPECTED_REPLAYED_TRANSITIONS: u64 = 139_825;
const EXPECTED_AUTHORITY_SEED_OPERATIONS: u64 = 1;
const EXPECTED_BROADCAST_GLOBAL_CONFIG_APPLICATIONS: u64 = 21;
const EXPECTED_MARKETS: u64 = 5;

struct TemporaryLedger {
    directory: Option<PathBuf>,
    path: PathBuf,
}

impl TemporaryLedger {
    fn from_environment_or_target_tmp() -> Self {
        if let Some(path) = env::var_os(LEDGER_ENV).map(PathBuf::from) {
            return Self {
                directory: None,
                path,
            };
        }

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must be after the Unix epoch")
            .as_nanos();
        let directory = env::temp_dir().join(format!(
            "v1-full-history-reconciliation-{}-{nonce}",
            process::id()
        ));
        fs::create_dir_all(&directory).unwrap_or_else(|error| {
            panic!(
                "creating temporary preflight directory {}: {error}",
                directory.display()
            )
        });
        Self {
            path: directory.join("v1-replay-ledger.jsonl"),
            directory: Some(directory),
        }
    }
}

impl Drop for TemporaryLedger {
    fn drop(&mut self) {
        if let Some(directory) = &self.directory {
            let _ = fs::remove_dir_all(directory);
        }
    }
}

fn required_existing_file(variable: &str) -> PathBuf {
    let path = env::var_os(variable).map(PathBuf::from).unwrap_or_else(|| {
        panic!(
            "{variable} must name an existing extracted JSONL file; this preflight never discovers or downloads source data"
        )
    });
    assert!(
        path.is_file(),
        "{variable} does not name a file: {}",
        path.display()
    );
    path
}

fn assert_parent_exists(path: &Path) {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    assert!(
        parent.is_dir(),
        "ledger parent directory does not exist: {}",
        parent.display()
    );
}

/// Runs only when an operator supplies the two immutable extracted inputs.
///
/// Example:
/// V1_FULL_HISTORY_OPERATIONS_JSONL=/path/to/canonical-operations.jsonl \
/// V1_FULL_HISTORY_CONFIG_HISTORY_JSONL=/path/to/config-history.jsonl \
/// cargo test --release --test full_live_history_reconciliation -- --ignored --nocapture
#[test]
#[ignore = "operator preflight requires explicit immutable full-history JSONL inputs"]
fn decision_grade_full_live_history_matches_frozen_reconciliation_summary() {
    let operations = required_existing_file(OPERATIONS_ENV);
    let config_history = required_existing_file(CONFIG_HISTORY_ENV);
    let ledger = TemporaryLedger::from_environment_or_target_tmp();
    assert_parent_exists(&ledger.path);

    let summary = reconcile_history_to_ledger(
        &operations,
        &config_history,
        &ledger.path,
        "full-live-history-preflight",
        EXPECTED_CUTOFF_SLOT,
        V1ReplayCertificationPolicyV1::DecisionGradeExactSelectedScope,
    )
    .expect("production DecisionGrade reconciliation must replay the frozen full history");

    assert_eq!(summary.cutoff_slot, EXPECTED_CUTOFF_SLOT);
    assert_eq!(
        summary.canonical_operation_count,
        EXPECTED_CANONICAL_OPERATIONS
    );
    assert_eq!(
        summary.replayed_transition_count,
        EXPECTED_REPLAYED_TRANSITIONS
    );
    assert_eq!(
        summary.authority_seed_operation_count,
        EXPECTED_AUTHORITY_SEED_OPERATIONS
    );
    assert_eq!(
        summary.broadcast_global_config_applications,
        EXPECTED_BROADCAST_GLOBAL_CONFIG_APPLICATIONS
    );
    assert_eq!(summary.market_count, EXPECTED_MARKETS);
    assert_eq!(summary.markets.len(), EXPECTED_MARKETS as usize);
    assert_eq!(summary.nonzero_raw_unit_difference_count, 0);
    assert_eq!(summary.conservation_failure_count, 0);
    assert!(summary.zero_raw_unit_difference);
    assert!(summary.all_conservation_identities_hold);

    eprintln!(
        "full-live-history reconciliation passed: {} canonical operations, {} replayed transitions, {} markets, ledger {}",
        summary.canonical_operation_count,
        summary.replayed_transition_count,
        summary.market_count,
        ledger.path.display()
    );
}
