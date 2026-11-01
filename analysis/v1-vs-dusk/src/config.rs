use std::{fs, path::Path};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

pub const CONFIG_SCHEMA_VERSION: &str = "v4";
const PREVIOUS_CONFIG_SCHEMA_VERSION: &str = "v3";
const LEGACY_CONFIG_SCHEMA_VERSION: &str = "v2";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkConfig {
    pub schema_version: String,
    pub source: SourceConfig,
    pub window: WindowConfig,
    pub universe: UniverseConfig,
    pub scope: ScopeConfig,
    pub replay: ReplayConfig,
    pub calibration: CalibrationConfig,
    pub output: OutputConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceConfig {
    pub env_file: String,
    pub database_url_env: String,
    pub rpc_url_env: String,
    pub program_id_env: String,
    /// One explicit provider prevents multiple same-hour marks from being
    /// silently averaged or selected by database row order.
    pub historical_price_provider: String,
    pub commitment: String,
    pub rpc_concurrency: usize,
    pub rpc_max_attempts: u32,
    /// Optional immutable donor for proof-bound archival blocks. The donor is
    /// accepted only when every current target block validates exactly and can
    /// be hard-linked into the current run without an RPC fallback.
    #[serde(default)]
    pub archival_block_cache: Option<ArchivalBlockCacheConfig>,
    /// Optional immutable, read-only historical-price proof sidecar. The
    /// sidecar is accepted only after its manifest, marks, provenance, raw
    /// provider responses, and exact source inputs all validate.
    #[serde(default)]
    pub historical_price_sidecar: Option<HistoricalPriceSidecarConfig>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchivalBlockCachePolicy {
    RequireCompleteHardlink,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArchivalBlockCacheConfig {
    /// Config-relative root containing `block-artifacts.json` and `blocks/`.
    pub root: String,
    /// SHA-256 of the exact serialized donor `block-artifacts.json` bytes.
    pub manifest_file_sha256: String,
    /// Self-hash declared inside the donor block manifest.
    pub manifest_sha256: String,
    pub policy: ArchivalBlockCachePolicy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoricalPriceSidecarPolicy {
    RequireCompleteProofs,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HistoricalPriceSidecarConfig {
    /// Config-relative root containing `fetch-manifest.json`, the marks and
    /// provenance JSONL files, and `provider-responses/`.
    pub root: String,
    /// SHA-256 of the exact serialized `fetch-manifest.json` bytes.
    pub manifest_file_sha256: String,
    /// Independently pins the exact mark bytes used by the merge.
    pub historical_price_marks_file_sha256: String,
    /// Independently pins the exact per-mark provenance bytes.
    pub historical_price_mark_provenance_file_sha256: String,
    /// The complete missing mint-hour key set that the sidecar must cover.
    pub expected_unique_keys: u64,
    pub policy: HistoricalPriceSidecarPolicy,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WindowConfig {
    /// A zero value means `extract` must resolve the current finalized slot and
    /// write a frozen run config before any history is queried.
    pub cutoff_slot: u64,
    pub common_market_fraction_bps: u16,
    pub common_tvl_fraction_bps: u16,
    pub price_bucket_seconds: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UniverseConfig {
    pub visible_only: bool,
    pub minimum_swaps: u64,
    pub minimum_active_days: u64,
    pub median_tvl_percentile_bps: u16,
    pub headline_swap_coverage_bps: u16,
    pub headline_lending_coverage_bps: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeMode {
    HybridPanel,
    FullUniverse,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceTier {
    /// Decision artifacts may carry typed provisional decoder/terminal
    /// limitations. This setting does not relax any extraction or audit gate.
    DecisionGrade,
    /// Audit artifacts remain fail-closed on every required proof.
    AuditGrade,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeConfig {
    pub mode: ScopeMode,
    pub evidence_tier: EvidenceTier,
    pub target_count: usize,
    pub minimum_panel_markets: usize,
    pub selection_training_bps: u16,
    pub keep_global_signature_inventory: bool,
    pub per_market_opportunity_ceiling: bool,
    #[serde(default)]
    pub explicit_market_overrides: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayConfig {
    pub fixed_flow: bool,
    pub intent_with_arbitrage: bool,
    pub maximum_feasible_hlp: bool,
    pub no_hlp_control: bool,
    pub standardized_trade_nav_bps: Vec<u16>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CalibrationConfig {
    pub seed: u64,
    pub training_bps: u16,
    pub validation_bps: u16,
    pub holdout_bps: u16,
    pub exploration_points: usize,
    pub pareto_shortlist: usize,
    pub local_refinement_rounds: usize,
    pub minimum_optimized_swaps_per_split: u64,
    pub minimum_optimized_active_days_per_split: u64,
    pub bootstrap_samples: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OutputConfig {
    pub data_dir: String,
    pub run_dir: String,
    pub report_dir: String,
    pub python_executable: String,
    pub node_executable: String,
    /// Environment variable containing the explicit Data Analytics portable
    /// artifact builder script path used by the render stage.
    pub portable_report_builder_env: String,
}

impl BenchmarkConfig {
    pub fn from_path(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("reading benchmark config {}", path.display()))?;
        let config: Self = toml::from_str(&raw)
            .with_context(|| format!("parsing benchmark config {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != CONFIG_SCHEMA_VERSION
            && self.schema_version != PREVIOUS_CONFIG_SCHEMA_VERSION
            && self.schema_version != LEGACY_CONFIG_SCHEMA_VERSION
        {
            bail!(
                "unsupported config schema {}; expected {LEGACY_CONFIG_SCHEMA_VERSION}, {PREVIOUS_CONFIG_SCHEMA_VERSION}, or {CONFIG_SCHEMA_VERSION}",
                self.schema_version
            );
        }
        if self.schema_version == PREVIOUS_CONFIG_SCHEMA_VERSION
            && self.source.historical_price_sidecar.is_some()
        {
            bail!("config schema v3 cannot enable historical-price sidecar reuse");
        }
        if self.schema_version == LEGACY_CONFIG_SCHEMA_VERSION
            && (self.source.archival_block_cache.is_some()
                || self.source.historical_price_sidecar.is_some())
        {
            bail!("config schema v2 cannot enable immutable source-artifact reuse");
        }
        if self.source.commitment != "finalized" {
            bail!("source.commitment must be finalized");
        }
        if self.source.historical_price_provider.trim().is_empty() {
            bail!("source.historical_price_provider must be non-empty");
        }
        if self.source.rpc_concurrency == 0 || self.source.rpc_max_attempts == 0 {
            bail!("RPC concurrency and attempts must both be non-zero");
        }
        if let Some(cache) = &self.source.archival_block_cache {
            if self.window.cutoff_slot == 0 {
                bail!("archival block-cache reuse requires a fixed non-zero cutoff slot");
            }
            if cache.root.trim().is_empty() {
                bail!("source.archival_block_cache.root must be non-empty");
            }
            for (name, value) in [
                (
                    "source.archival_block_cache.manifest_file_sha256",
                    cache.manifest_file_sha256.as_str(),
                ),
                (
                    "source.archival_block_cache.manifest_sha256",
                    cache.manifest_sha256.as_str(),
                ),
            ] {
                if !valid_lower_sha256(value) {
                    bail!("{name} must be a lowercase SHA-256 digest");
                }
            }
            if cache.policy != ArchivalBlockCachePolicy::RequireCompleteHardlink {
                bail!("archival block-cache policy must require complete hard-link reuse");
            }
        }
        if let Some(sidecar) = &self.source.historical_price_sidecar {
            if self.window.cutoff_slot == 0 {
                bail!("historical-price sidecar reuse requires a fixed non-zero cutoff slot");
            }
            if sidecar.root.trim().is_empty() {
                bail!("source.historical_price_sidecar.root must be non-empty");
            }
            for (name, value) in [
                (
                    "source.historical_price_sidecar.manifest_file_sha256",
                    sidecar.manifest_file_sha256.as_str(),
                ),
                (
                    "source.historical_price_sidecar.historical_price_marks_file_sha256",
                    sidecar.historical_price_marks_file_sha256.as_str(),
                ),
                (
                    "source.historical_price_sidecar.historical_price_mark_provenance_file_sha256",
                    sidecar
                        .historical_price_mark_provenance_file_sha256
                        .as_str(),
                ),
            ] {
                if !valid_lower_sha256(value) {
                    bail!("{name} must be a lowercase SHA-256 digest");
                }
            }
            if sidecar.expected_unique_keys == 0 {
                bail!("source.historical_price_sidecar.expected_unique_keys must be non-zero");
            }
            if sidecar.policy != HistoricalPriceSidecarPolicy::RequireCompleteProofs {
                bail!("historical-price sidecar policy must require complete proofs");
            }
        }
        if self.scope.selection_training_bps != self.calibration.training_bps {
            bail!("scope.selection_training_bps must equal calibration.training_bps");
        }
        if !self.scope.keep_global_signature_inventory {
            bail!("scope.keep_global_signature_inventory must remain enabled");
        }
        if self
            .scope
            .explicit_market_overrides
            .iter()
            .any(|market| market.trim().is_empty())
        {
            bail!("scope explicit market overrides must be non-empty");
        }
        let unique_overrides = self
            .scope
            .explicit_market_overrides
            .iter()
            .collect::<std::collections::BTreeSet<_>>();
        if unique_overrides.len() != self.scope.explicit_market_overrides.len() {
            bail!("scope explicit market overrides must be unique");
        }
        match self.scope.mode {
            ScopeMode::HybridPanel => {
                if !(5..=6).contains(&self.scope.target_count) {
                    bail!("hybrid-panel scope.target_count must be between 5 and 6");
                }
                if self.scope.minimum_panel_markets == 0
                    || self.scope.minimum_panel_markets > self.scope.target_count
                {
                    bail!(
                        "hybrid-panel scope.minimum_panel_markets must be between 1 and target_count"
                    );
                }
                if self.scope.explicit_market_overrides.len() > self.scope.target_count {
                    bail!("scope explicit market overrides exceed target_count");
                }
            }
            ScopeMode::FullUniverse => {
                if self.scope.minimum_panel_markets == 0 {
                    bail!("scope.minimum_panel_markets must be non-zero");
                }
                if !self.scope.explicit_market_overrides.is_empty() {
                    bail!("full-universe scope cannot have explicit market overrides");
                }
            }
        }
        for (name, value) in [
            (
                "window.common_market_fraction_bps",
                self.window.common_market_fraction_bps,
            ),
            (
                "window.common_tvl_fraction_bps",
                self.window.common_tvl_fraction_bps,
            ),
            (
                "universe.median_tvl_percentile_bps",
                self.universe.median_tvl_percentile_bps,
            ),
            (
                "universe.headline_swap_coverage_bps",
                self.universe.headline_swap_coverage_bps,
            ),
            (
                "universe.headline_lending_coverage_bps",
                self.universe.headline_lending_coverage_bps,
            ),
        ] {
            if value > 10_000 {
                bail!("{name} must not exceed 10,000 bps");
            }
        }
        let split_total = u32::from(self.calibration.training_bps)
            + u32::from(self.calibration.validation_bps)
            + u32::from(self.calibration.holdout_bps);
        if split_total != 10_000 {
            bail!("training, validation, and holdout splits must total 10,000 bps");
        }
        if self.calibration.holdout_bps == 0 {
            bail!("holdout split must be non-zero");
        }
        if self.calibration.exploration_points == 0
            || self.calibration.pareto_shortlist == 0
            || self.calibration.bootstrap_samples == 0
            || self.calibration.minimum_optimized_swaps_per_split == 0
            || self.calibration.minimum_optimized_active_days_per_split == 0
        {
            bail!("calibration search, bootstrap, and per-market minimums must be non-zero");
        }
        if self.replay.standardized_trade_nav_bps != [10, 100, 500] {
            bail!("standardized trade sizes must be the ordered frozen 10/100/500 bps campaign");
        }
        if !self.replay.maximum_feasible_hlp || !self.replay.no_hlp_control {
            bail!("both maximum-feasible hLP and no-hLP attribution control must be enabled");
        }
        if self.window.price_bucket_seconds != 3_600 {
            bail!("window.price_bucket_seconds must be 3,600 for the persisted hourly marks");
        }
        if self.output.python_executable.trim().is_empty()
            || self.output.node_executable.trim().is_empty()
            || self.output.portable_report_builder_env.trim().is_empty()
        {
            bail!("output executable and portable-builder settings must be non-empty");
        }
        Ok(())
    }
}

fn valid_lower_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_config_is_valid() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        BenchmarkConfig::from_path(&root.join("benchmark.toml")).unwrap();
    }

    #[test]
    fn hybrid_scope_bounds_and_split_binding_are_enforced() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let config = BenchmarkConfig::from_path(&root.join("benchmark.toml")).unwrap();

        let mut too_small = config.clone();
        too_small.scope.target_count = 4;
        assert!(too_small.validate().is_err());

        let mut split_mismatch = config.clone();
        split_mismatch.scope.selection_training_bps -= 1;
        assert!(split_mismatch.validate().is_err());

        let mut duplicate_override = config;
        duplicate_override.scope.explicit_market_overrides =
            vec!["market-a".to_owned(), "market-a".to_owned()];
        assert!(duplicate_override.validate().is_err());
    }

    #[test]
    fn v2_without_cache_remains_valid_but_cannot_enable_cache_reuse() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let shipped = BenchmarkConfig::from_path(&root.join("benchmark.toml")).unwrap();

        let mut legacy = shipped.clone();
        legacy.schema_version = LEGACY_CONFIG_SCHEMA_VERSION.to_owned();
        legacy.source.archival_block_cache = None;
        legacy.source.historical_price_sidecar = None;
        legacy.validate().unwrap();

        legacy.source.archival_block_cache = shipped.source.archival_block_cache;
        assert!(legacy.validate().is_err());
    }

    #[test]
    fn v3_cannot_silently_enable_historical_price_sidecars() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let shipped = BenchmarkConfig::from_path(&root.join("benchmark.toml")).unwrap();

        let mut previous = shipped.clone();
        previous.schema_version = PREVIOUS_CONFIG_SCHEMA_VERSION.to_owned();
        previous.source.historical_price_sidecar = None;
        previous.validate().unwrap();

        previous.source.historical_price_sidecar = shipped.source.historical_price_sidecar;
        assert!(previous.validate().is_err());
    }

    #[test]
    fn cache_reuse_requires_fixed_cutoff_and_exact_hashes() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let config = BenchmarkConfig::from_path(&root.join("benchmark.toml")).unwrap();

        let mut floating = config.clone();
        floating.window.cutoff_slot = 0;
        assert!(floating.validate().is_err());

        let mut uppercase = config;
        uppercase
            .source
            .archival_block_cache
            .as_mut()
            .unwrap()
            .manifest_sha256 = "A".repeat(64);
        assert!(uppercase.validate().is_err());
    }

    #[test]
    fn historical_price_sidecar_requires_fixed_source_hashes_and_coverage() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let config = BenchmarkConfig::from_path(&root.join("benchmark.toml")).unwrap();

        let mut floating = config.clone();
        floating.window.cutoff_slot = 0;
        assert!(floating.validate().is_err());

        let mut missing_coverage = config.clone();
        missing_coverage
            .source
            .historical_price_sidecar
            .as_mut()
            .unwrap()
            .expected_unique_keys = 0;
        assert!(missing_coverage.validate().is_err());

        let mut uppercase = config;
        uppercase
            .source
            .historical_price_sidecar
            .as_mut()
            .unwrap()
            .historical_price_marks_file_sha256 = "A".repeat(64);
        assert!(uppercase.validate().is_err());
    }
}
