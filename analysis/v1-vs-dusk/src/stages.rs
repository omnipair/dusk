//! CLI stage orchestration and immutable artifact boundaries.

use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{config::BenchmarkConfig, rpc::HttpArchivalRpc};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum StageName {
    Extract,
    Audit,
    ReconcileV1,
    Calibrate,
    Evaluate,
    Verify,
    Render,
}

impl StageName {
    fn parse(value: &str) -> Result<Self> {
        Ok(match value {
            "extract" => Self::Extract,
            "audit" => Self::Audit,
            "reconcile-v1" => Self::ReconcileV1,
            "calibrate" => Self::Calibrate,
            "evaluate" => Self::Evaluate,
            "verify" => Self::Verify,
            "render" => Self::Render,
            _ => bail!("unsupported stage {value:?}"),
        })
    }

    const fn predecessor(self) -> Option<Self> {
        match self {
            Self::Extract => None,
            Self::Audit => Some(Self::Extract),
            Self::ReconcileV1 => Some(Self::Audit),
            Self::Calibrate => Some(Self::ReconcileV1),
            Self::Evaluate => Some(Self::Calibrate),
            Self::Verify => Some(Self::Evaluate),
            Self::Render => Some(Self::Verify),
        }
    }

    const fn file_stem(self) -> &'static str {
        match self {
            Self::Extract => "01-extract",
            Self::Audit => "02-audit",
            Self::ReconcileV1 => "03-reconcile-v1",
            Self::Calibrate => "04-calibrate",
            Self::Evaluate => "05-evaluate",
            Self::Verify => "06-verify",
            Self::Render => "07-render",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StageStatus {
    Complete,
    Blocked,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct StageReceiptV1 {
    schema_version: u16,
    stage: StageName,
    status: StageStatus,
    run_id: String,
    config_sha256: String,
    source_identity_sha256: String,
    cutoff_slot: u64,
    input_receipt_sha256: Option<String>,
    artifact_sha256: BTreeMap<String, String>,
    blocker_codes: Vec<String>,
    receipt_sha256: String,
}

impl StageReceiptV1 {
    fn seal(mut self) -> Result<Self> {
        self.receipt_sha256.clear();
        let bytes = serde_json::to_vec(&self)?;
        self.receipt_sha256 = hex::encode(Sha256::digest(bytes));
        Ok(self)
    }

    fn verify(&self) -> Result<()> {
        let expected = self.clone().seal()?.receipt_sha256;
        if expected != self.receipt_sha256 {
            bail!("stage receipt checksum mismatch for {:?}", self.stage);
        }
        Ok(())
    }
}

pub(crate) struct RunContext {
    root: PathBuf,
    config_sha256: String,
    source_identity_sha256: String,
    cutoff_slot: u64,
    run_id: String,
    run_dir: PathBuf,
    data_dir: PathBuf,
    report_dir: PathBuf,
    runtime_env: BTreeMap<String, String>,
}

pub async fn run(
    stage: &str,
    config_path: &Path,
    config: BenchmarkConfig,
    force: bool,
) -> Result<()> {
    let stage = StageName::parse(stage)?;
    let context = RunContext::resolve(config_path, &config).await?;
    fs::create_dir_all(&context.run_dir)
        .with_context(|| format!("creating {}", context.run_dir.display()))?;
    fs::create_dir_all(&context.data_dir)
        .with_context(|| format!("creating {}", context.data_dir.display()))?;
    fs::create_dir_all(&context.report_dir)
        .with_context(|| format!("creating {}", context.report_dir.display()))?;

    let prior = load_predecessor(stage, &context)?;
    let receipt_path = context
        .run_dir
        .join(format!("{}.receipt.json", stage.file_stem()));
    if receipt_path.exists() && !force {
        let receipt: StageReceiptV1 = serde_json::from_slice(&fs::read(&receipt_path)?)?;
        receipt.verify()?;
        if receipt.status == StageStatus::Complete {
            println!(
                "stage {:?} is already complete for immutable run {}",
                stage, context.run_id
            );
            return Ok(());
        }
        bail!(
            "stage {:?} has a blocked receipt; use --force only after fixing its recorded blocker",
            stage
        );
    }

    let outcome = match stage {
        StageName::Extract => extract_stage(&context, &config).await,
        StageName::Audit => audit_stage(&context, &config).await,
        StageName::ReconcileV1 => reconcile_v1_stage(&context, &config).await,
        StageName::Calibrate => calibrate_stage(&context, &config).await,
        StageName::Evaluate => evaluate_stage(&context, &config).await,
        StageName::Verify => verify_stage(&context, &config).await,
        StageName::Render => render_stage(&context, &config).await,
    };
    let input_receipt_sha256 = prior.map(|receipt| receipt.receipt_sha256);
    match outcome {
        Ok(artifacts) => {
            let receipt = StageReceiptV1 {
                schema_version: 1,
                stage,
                status: StageStatus::Complete,
                run_id: context.run_id.clone(),
                config_sha256: context.config_sha256.clone(),
                source_identity_sha256: context.source_identity_sha256.clone(),
                cutoff_slot: context.cutoff_slot,
                input_receipt_sha256,
                artifact_sha256: artifacts,
                blocker_codes: Vec::new(),
                receipt_sha256: String::new(),
            }
            .seal()?;
            write_json_immutable(&receipt_path, &receipt, force)?;
            println!("completed {:?} for run {}", stage, context.run_id);
            Ok(())
        }
        Err(error) => {
            let receipt = StageReceiptV1 {
                schema_version: 1,
                stage,
                status: StageStatus::Blocked,
                run_id: context.run_id.clone(),
                config_sha256: context.config_sha256.clone(),
                source_identity_sha256: context.source_identity_sha256.clone(),
                cutoff_slot: context.cutoff_slot,
                input_receipt_sha256,
                artifact_sha256: BTreeMap::new(),
                blocker_codes: vec![categorical_error(&error)],
                receipt_sha256: String::new(),
            }
            .seal()?;
            write_json_immutable(&receipt_path, &receipt, force)?;
            Err(error)
        }
    }
}

impl RunContext {
    async fn resolve(config_path: &Path, config: &BenchmarkConfig) -> Result<Self> {
        let config_path = fs::canonicalize(config_path)
            .with_context(|| format!("resolving {}", config_path.display()))?;
        let root = config_path
            .parent()
            .context("benchmark config has no parent directory")?
            .to_path_buf();
        let config_bytes = fs::read(&config_path)?;
        let config_sha256 = hex::encode(Sha256::digest(&config_bytes));
        let env_path = resolve_path(&root, &config.source.env_file);
        let mut runtime_env = read_env_file(&env_path)?;
        for key in [
            &config.source.database_url_env,
            &config.source.rpc_url_env,
            &config.source.program_id_env,
        ] {
            if let Ok(value) = env::var(key) {
                if !value.is_empty() {
                    runtime_env.insert(key.clone(), value);
                }
            }
        }
        let rpc_url = required_runtime_value(&runtime_env, &config.source.rpc_url_env, "RPC URL")?;
        let rpc = HttpArchivalRpc::new(rpc_url.to_owned(), config.source.rpc_max_attempts)
            .map_err(|_| anyhow::anyhow!("invalid archival RPC configuration"))?;
        let cutoff_slot = if config.window.cutoff_slot == 0 {
            let freeze_dir = resolve_path(&root, &config.output.run_dir);
            fs::create_dir_all(&freeze_dir)?;
            let freeze_path = freeze_dir.join(format!("cutoff-{}.txt", &config_sha256[..16]));
            if freeze_path.exists() {
                fs::read_to_string(&freeze_path)?
                    .trim()
                    .parse::<u64>()
                    .context("parsing frozen finalized cutoff")?
            } else {
                let resolved = rpc.finalized_slot().await.map_err(|kind| {
                    anyhow::anyhow!("failed to resolve finalized cutoff: {kind:?}")
                })?;
                fs::write(&freeze_path, format!("{resolved}\n"))?;
                resolved
            }
        } else {
            config.window.cutoff_slot
        };
        let source_identity_sha256 = benchmark_source_identity(&root)?;
        let run_id_material = format!("{}:{cutoff_slot}:{source_identity_sha256}", config_sha256);
        let run_digest = hex::encode(Sha256::digest(run_id_material));
        let run_id = format!("slot-{cutoff_slot}-{}", &run_digest[..16]);
        let run_dir = resolve_path(&root, &config.output.run_dir).join(&run_id);
        let data_dir = resolve_path(&root, &config.output.data_dir).join(&run_id);
        let report_dir = resolve_path(&root, &config.output.report_dir).join(&run_id);
        Ok(Self {
            root,
            config_sha256,
            source_identity_sha256,
            cutoff_slot,
            run_id,
            run_dir,
            data_dir,
            report_dir,
            runtime_env,
        })
    }
}

fn load_predecessor(stage: StageName, context: &RunContext) -> Result<Option<StageReceiptV1>> {
    let Some(predecessor) = stage.predecessor() else {
        return Ok(None);
    };
    let path = context
        .run_dir
        .join(format!("{}.receipt.json", predecessor.file_stem()));
    let bytes = fs::read(&path).with_context(|| {
        format!(
            "stage {:?} requires completed predecessor {:?} at {}",
            stage,
            predecessor,
            path.display()
        )
    })?;
    let receipt: StageReceiptV1 = serde_json::from_slice(&bytes)?;
    receipt.verify()?;
    if receipt.status != StageStatus::Complete
        || receipt.run_id != context.run_id
        || receipt.config_sha256 != context.config_sha256
        || receipt.source_identity_sha256 != context.source_identity_sha256
        || receipt.cutoff_slot != context.cutoff_slot
    {
        bail!("predecessor receipt is blocked or belongs to a different immutable run");
    }
    Ok(Some(receipt))
}

/// Fingerprints the exact benchmark runner plus every native implementation it
/// executes. Generated datasets/results are excluded; uncommitted and untracked
/// source files are included by their bytes, so a working-tree change creates a
/// different immutable run ID even when the Git revision is unchanged.
fn benchmark_source_identity(root: &Path) -> Result<String> {
    let sources = [
        ("benchmark", fs::canonicalize(root)?, true),
        (
            "dusk",
            fs::canonicalize(root.join("../../programs/dusk"))?,
            false,
        ),
        (
            "v1",
            fs::canonicalize(root.join("../../../omnipair-rs/programs/omnipair"))?,
            false,
        ),
        (
            "indexer_decoder",
            fs::canonicalize(
                root.join("../../../omnipair-indexer/indexer/decoders/omnipair_decoder"),
            )?,
            false,
        ),
        (
            "indexer_core",
            fs::canonicalize(root.join("../../../omnipair-indexer/indexer/crates/core"))?,
            false,
        ),
    ];
    let mut digest = Sha256::new();
    for (label, source_root, benchmark_tree) in sources {
        let mut files = Vec::new();
        collect_source_files(&source_root, &source_root, benchmark_tree, &mut files)?;
        files.sort();
        digest.update((label.len() as u64).to_be_bytes());
        digest.update(label.as_bytes());
        for relative in files {
            let name = relative.to_string_lossy();
            let bytes = fs::read(source_root.join(&relative))?;
            digest.update((name.len() as u64).to_be_bytes());
            digest.update(name.as_bytes());
            digest.update((bytes.len() as u64).to_be_bytes());
            digest.update(bytes);
        }
    }
    Ok(hex::encode(digest.finalize()))
}

fn collect_source_files(
    root: &Path,
    current: &Path,
    benchmark_tree: bool,
    output: &mut Vec<PathBuf>,
) -> Result<()> {
    let mut entries: Vec<_> = fs::read_dir(current)?.collect::<std::io::Result<_>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let relative = path.strip_prefix(root)?.to_path_buf();
        let first = relative
            .components()
            .next()
            .and_then(|part| part.as_os_str().to_str());
        let generated_benchmark_path = benchmark_tree
            && (matches!(first, Some("data" | "target"))
                || relative.starts_with("results/runs")
                || relative.starts_with("reports/generated"));
        let name = entry.file_name();
        if generated_benchmark_path || name == ".git" || name == "target" || name == ".env" {
            continue;
        }
        if path.is_dir() {
            collect_source_files(root, &path, benchmark_tree, output)?;
        } else if path.is_file() {
            output.push(relative);
        }
    }
    Ok(())
}

fn resolve_path(root: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn read_env_file(path: &Path) -> Result<BTreeMap<String, String>> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("reading runtime environment {}", path.display()))?;
    let mut values = BTreeMap::new();
    for (index, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .with_context(|| format!("invalid environment entry on line {}", index + 1))?;
        let key = key.trim();
        if key.is_empty()
            || !key
                .bytes()
                .all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
        {
            bail!("invalid environment key on line {}", index + 1);
        }
        let value = value
            .trim()
            .trim_matches(|character| character == '\'' || character == '"');
        values.insert(key.to_owned(), value.to_owned());
    }
    Ok(values)
}

fn required_runtime_value<'a>(
    file_values: &'a BTreeMap<String, String>,
    key: &str,
    label: &str,
) -> Result<&'a str> {
    if let Some(value) = file_values.get(key) {
        if !value.is_empty() {
            return Ok(value);
        }
    }
    bail!("missing {label} runtime key {key}")
}

fn write_json_immutable(path: &Path, value: &StageReceiptV1, replace_blocked: bool) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    if path.exists() {
        let current = fs::read(path)?;
        if current == bytes {
            return Ok(());
        }
        let current_receipt: StageReceiptV1 = serde_json::from_slice(&current)
            .context("decoding existing immutable stage receipt")?;
        current_receipt.verify()?;
        if !replace_blocked || current_receipt.status != StageStatus::Blocked {
            bail!("refusing to replace immutable artifact {}", path.display());
        }
    }
    fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))
}

fn checksum_file(path: &Path) -> Result<String> {
    Ok(hex::encode(Sha256::digest(fs::read(path)?)))
}

fn categorical_error(error: &anyhow::Error) -> String {
    let top = error.to_string().to_ascii_lowercase();
    if top.contains("rpc") || top.contains("archival") {
        "archival_rpc_unavailable".to_owned()
    } else if top.contains("database") || top.contains("postgres") {
        "database_unavailable".to_owned()
    } else if top.contains("coverage") {
        "coverage_gate_failed".to_owned()
    } else if top.contains("reconcil") {
        "v1_reconciliation_failed".to_owned()
    } else {
        "stage_execution_failed".to_owned()
    }
}

async fn extract_stage(
    context: &RunContext,
    config: &BenchmarkConfig,
) -> Result<BTreeMap<String, String>> {
    crate::pipeline::extract(context, config).await
}

async fn audit_stage(
    context: &RunContext,
    config: &BenchmarkConfig,
) -> Result<BTreeMap<String, String>> {
    crate::pipeline::audit(context, config).await
}

async fn reconcile_v1_stage(
    context: &RunContext,
    config: &BenchmarkConfig,
) -> Result<BTreeMap<String, String>> {
    crate::pipeline::reconcile_v1(context, config).await
}

async fn calibrate_stage(
    context: &RunContext,
    config: &BenchmarkConfig,
) -> Result<BTreeMap<String, String>> {
    crate::pipeline::calibrate(context, config).await
}

async fn evaluate_stage(
    context: &RunContext,
    config: &BenchmarkConfig,
) -> Result<BTreeMap<String, String>> {
    crate::pipeline::evaluate(context, config).await
}

async fn verify_stage(
    context: &RunContext,
    config: &BenchmarkConfig,
) -> Result<BTreeMap<String, String>> {
    crate::pipeline::verify(context, config).await
}

async fn render_stage(
    context: &RunContext,
    config: &BenchmarkConfig,
) -> Result<BTreeMap<String, String>> {
    crate::pipeline::render(context, config).await
}

// Narrow accessors used by the pipeline keep secrets and path policy private.
impl RunContext {
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
    pub(crate) fn config_sha256(&self) -> &str {
        &self.config_sha256
    }
    pub(crate) fn source_identity_sha256(&self) -> &str {
        &self.source_identity_sha256
    }
    pub(crate) fn cutoff_slot(&self) -> u64 {
        self.cutoff_slot
    }
    pub(crate) fn run_id(&self) -> &str {
        &self.run_id
    }
    pub(crate) fn run_dir(&self) -> &Path {
        &self.run_dir
    }
    pub(crate) fn data_dir(&self) -> &Path {
        &self.data_dir
    }
    pub(crate) fn report_dir(&self) -> &Path {
        &self.report_dir
    }
    pub(crate) fn runtime_value(&self, key: &str) -> Result<&str> {
        required_runtime_value(&self.runtime_env, key, "configured source")
    }
}

pub(crate) fn artifact_map(paths: &[(&str, &Path)]) -> Result<BTreeMap<String, String>> {
    paths
        .iter()
        .map(|(name, path)| Ok(((*name).to_owned(), checksum_file(path)?)))
        .collect()
}
