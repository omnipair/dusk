//! Canonical Data Analytics report artifacts for the verified benchmark.
//!
//! Reports are data contracts, not hand-authored HTML. This module writes
//! three bounded `artifact.json` inputs plus generated SQL provenance. A
//! caller must explicitly provide the portable builder script and Node
//! executable when HTML delivery is requested.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs::{self, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
    process::Command,
    str::FromStr,
};

use anyhow::{bail, Context, Result};
use num_bigint::BigInt;
use num_rational::BigRational;
use num_traits::ToPrimitive;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use sha2::{Digest, Sha256};

use crate::{
    audit::{CertificationStatus, HardGateId},
    config::{EvidenceTier, ScopeMode},
    metrics::{
        MetricAvailability, MetricFamily, MetricValueV1, NamedMetricV1, ResultManifestV1,
        ScopeActivityCoverageV1,
    },
    replay::{HlpScenario, ReplayArm},
    sensitivity::TopMarketRankingBasisV1,
};

pub const REPORT_ARTIFACT_SCHEMA_VERSION: u16 = 1;
pub const MAX_REPORT_METRIC_ROWS: usize = 64;
pub const MAX_REPORT_ARTIFACT_BYTES: usize = 3_000_000;
pub const MAX_REPORT_INLINE_SOURCE_BYTES: usize = 200_000;
const TEMPLATE_BYTES: &str = include_str!("../templates/report-templates.json");
const RECEIPT_HASH_DOMAIN: &[u8] = b"v1-vs-dusk/report-render-receipt/v1\0";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportAudienceV1 {
    TechnicalAudit,
    GovernanceProduct,
    Public,
}

impl ReportAudienceV1 {
    pub const ALL: [Self; 3] = [Self::TechnicalAudit, Self::GovernanceProduct, Self::Public];

    pub const fn slug(self) -> &'static str {
        match self {
            Self::TechnicalAudit => "technical",
            Self::GovernanceProduct => "governance",
            Self::Public => "public",
        }
    }

    const fn metric_cap(self) -> usize {
        match self {
            Self::TechnicalAudit => MAX_REPORT_METRIC_ROWS,
            Self::GovernanceProduct => 32,
            Self::Public => 16,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportTemplateV1 {
    pub audience: ReportAudienceV1,
    pub title: String,
    pub description: String,
    pub summary_heading: String,
    pub evidence_heading: String,
    pub scope_heading: String,
    pub methodology_heading: String,
    pub limitations_heading: String,
    pub next_steps_heading: String,
    pub questions_heading: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReportTemplateSetV1 {
    schema_version: u16,
    templates: Vec<ReportTemplateV1>,
}

/// Canonical top-level input accepted by the Data Analytics artifact builder.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DataAnalyticsArtifactV1 {
    pub surface: String,
    pub manifest: DataAnalyticsManifestV1,
    pub snapshot: DataAnalyticsSnapshotV1,
    pub sources: Vec<DataAnalyticsSourceV1>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DataAnalyticsManifestV1 {
    pub version: u16,
    pub surface: String,
    pub title: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cards: Vec<JsonValue>,
    pub charts: Vec<DataAnalyticsChartV1>,
    pub tables: Vec<DataAnalyticsTableV1>,
    pub sources: Vec<DataAnalyticsSourceV1>,
    pub blocks: Vec<DataAnalyticsBlockV1>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DataAnalyticsSnapshotV1 {
    pub version: u16,
    pub status: String,
    pub datasets: BTreeMap<String, Vec<JsonValue>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub access_issues: Vec<JsonValue>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DataAnalyticsSourceV1 {
    pub id: String,
    pub label: String,
    pub path: String,
    pub query: DataAnalyticsSourceQueryV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DataAnalyticsSourceQueryV1 {
    pub engine: String,
    pub id: String,
    pub sql: String,
    pub description: String,
    pub language: String,
    pub tables_used: Vec<String>,
    pub filters: Vec<String>,
    pub metric_definitions: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DataAnalyticsChartV1 {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    #[serde(rename = "type")]
    pub chart_type: String,
    pub dataset: String,
    pub source_id: String,
    pub encodings: DataAnalyticsChartEncodingsV1,
    pub x_axis_title: String,
    pub y_axis_title: String,
    pub value_format: String,
    pub unit: String,
    pub layout: String,
    pub max_rows: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DataAnalyticsChartEncodingsV1 {
    pub x: DataAnalyticsEncodingV1,
    pub y: DataAnalyticsEncodingV1,
    pub tooltip: Vec<DataAnalyticsEncodingV1>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DataAnalyticsEncodingV1 {
    pub field: String,
    #[serde(rename = "type")]
    pub data_type: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DataAnalyticsTableV1 {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub dataset: String,
    pub density: String,
    pub source_id: String,
    pub layout: String,
    pub default_sort: DataAnalyticsDefaultSortV1,
    pub columns: Vec<DataAnalyticsTableColumnV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DataAnalyticsDefaultSortV1 {
    pub field: String,
    pub direction: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DataAnalyticsTableColumnV1 {
    pub field: String,
    pub label: String,
    #[serde(rename = "type")]
    pub data_type: String,
    pub align: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum DataAnalyticsBlockV1 {
    Markdown {
        id: String,
        body: String,
        #[serde(rename = "sourceId", skip_serializing_if = "Option::is_none")]
        source_id: Option<String>,
    },
    Chart {
        id: String,
        #[serde(rename = "chartId")]
        chart_id: String,
    },
    Table {
        id: String,
        #[serde(rename = "tableId")]
        table_id: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WrittenReportSpecV1 {
    pub audience: ReportAudienceV1,
    pub directory: PathBuf,
    pub artifact_path: PathBuf,
    pub artifact_sha256: String,
    pub provenance_sql_path: PathBuf,
    pub provenance_sql_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WrittenReportSetV1 {
    pub schema_version: u16,
    pub run_id: String,
    pub result_manifest_sha256: String,
    pub reports: Vec<WrittenReportSpecV1>,
}

pub struct PortableBuilderConfigV1<'a> {
    /// May be a bare executable name resolved by `PATH`, or an explicit path.
    pub node_executable: &'a OsStr,
    /// Explicit portable builder supplied by the caller. No plugin path is
    /// inferred or embedded by this crate.
    pub builder_script: &'a Path,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportRenderReceiptV1 {
    pub schema_version: u16,
    pub audience: ReportAudienceV1,
    pub artifact_sha256: String,
    pub provenance_sql_sha256: String,
    pub builder_script_sha256: String,
    pub builder_receipt_sha256: String,
    pub builder_verification_stage: String,
    pub html_file_name: String,
    pub html_bytes: u64,
    pub html_sha256: String,
    pub receipt_sha256: String,
}

impl ReportRenderReceiptV1 {
    fn seal(mut self) -> Result<Self> {
        self.receipt_sha256.clear();
        self.receipt_sha256 = receipt_hash(&self)?;
        Ok(self)
    }

    pub fn verify(&self) -> Result<()> {
        if self.schema_version != REPORT_ARTIFACT_SCHEMA_VERSION
            || !valid_sha256(&self.artifact_sha256)
            || !valid_sha256(&self.provenance_sql_sha256)
            || !valid_sha256(&self.builder_script_sha256)
            || !valid_sha256(&self.builder_receipt_sha256)
            || !valid_sha256(&self.html_sha256)
            || !valid_sha256(&self.receipt_sha256)
            || !matches!(
                self.builder_verification_stage.as_str(),
                "passed" | "structural_only"
            )
            || self.html_file_name != "report.html"
            || self.html_bytes == 0
        {
            bail!("report render receipt identity is invalid");
        }
        let mut unsigned = self.clone();
        unsigned.receipt_sha256.clear();
        if self.receipt_sha256 != receipt_hash(&unsigned)? {
            bail!("report render receipt self-hash mismatch");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderedReportV1 {
    pub audience: ReportAudienceV1,
    pub html_path: PathBuf,
    pub receipt_path: PathBuf,
    pub receipt: ReportRenderReceiptV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderedReportSetV1 {
    pub schema_version: u16,
    pub run_id: String,
    pub reports: Vec<RenderedReportV1>,
}

#[derive(Clone)]
struct ReportMetricRowV1 {
    metric_id: String,
    metric_label: String,
    family: String,
    profile_id: Option<String>,
    replay_arm: Option<String>,
    hlp_scenario: Option<String>,
    definition: String,
    unit: String,
    v1_availability: String,
    v1_numerator: Option<String>,
    v1_denominator: Option<String>,
    dusk_availability: String,
    dusk_numerator: Option<String>,
    dusk_denominator: Option<String>,
    paired_delta_availability: String,
    paired_delta_numerator: Option<String>,
    paired_delta_denominator: Option<String>,
    presentation_delta: Option<f64>,
    presentation_conversion: String,
}

impl ReportMetricRowV1 {
    fn to_json(&self) -> JsonValue {
        json!({
            "metric_id": self.metric_id,
            "metric_label": self.metric_label,
            "family": self.family,
            "profile_id": self.profile_id,
            "replay_arm": self.replay_arm,
            "hlp_scenario": self.hlp_scenario,
            "definition": self.definition,
            "unit": self.unit,
            "v1_availability": self.v1_availability,
            "v1_numerator": self.v1_numerator,
            "v1_denominator": self.v1_denominator,
            "dusk_availability": self.dusk_availability,
            "dusk_numerator": self.dusk_numerator,
            "dusk_denominator": self.dusk_denominator,
            "paired_delta_availability": self.paired_delta_availability,
            "paired_delta_numerator": self.paired_delta_numerator,
            "paired_delta_denominator": self.paired_delta_denominator,
            "presentation_delta": self.presentation_delta,
            "presentation_conversion": self.presentation_conversion,
        })
    }
}

/// Writes three deterministic canonical artifact specs and their actual SQL
/// provenance files. The supplied manifest is verified before any output is
/// created.
pub fn write_report_artifacts(
    result: &ResultManifestV1,
    output_dir: &Path,
) -> Result<WrittenReportSetV1> {
    result.verify()?;
    let templates = load_templates()?;
    let mut reports = Vec::with_capacity(ReportAudienceV1::ALL.len());
    for audience in ReportAudienceV1::ALL {
        let template = templates
            .get(&audience)
            .context("report template is missing an audience")?;
        let metrics = select_metrics(result, audience)?;
        let rows = metrics
            .iter()
            .map(|metric| report_metric_row(metric, result.scope.scope_mode))
            .collect::<Result<Vec<_>>>()?;
        let chart_rows = select_chart_rows(&rows)?;
        let sql = provenance_sql(result, audience, &rows);
        let source = source_contract(result, audience, &metrics, &sql);
        let artifact = build_artifact(result, audience, template, &rows, &chart_rows, source)?;
        validate_artifact_contract(&artifact, result)?;

        let directory = output_dir.join(audience.slug());
        fs::create_dir_all(&directory)
            .with_context(|| format!("creating report directory {}", directory.display()))?;
        let provenance_sql_path = directory.join("provenance.sql");
        write_new_atomic(&provenance_sql_path, sql.as_bytes())?;
        let artifact_path = directory.join("artifact.json");
        let artifact_bytes = serde_json::to_vec_pretty(&artifact)?;
        write_new_atomic(&artifact_path, &artifact_bytes)?;
        verify_source_file_resolution(&artifact, &directory)?;
        reports.push(WrittenReportSpecV1 {
            audience,
            directory,
            artifact_path,
            artifact_sha256: sha256_bytes(&artifact_bytes),
            provenance_sql_path,
            provenance_sql_sha256: sha256_bytes(sql.as_bytes()),
        });
    }
    Ok(WrittenReportSetV1 {
        schema_version: REPORT_ARTIFACT_SCHEMA_VERSION,
        run_id: result.run_id.clone(),
        result_manifest_sha256: result.manifest_sha256.clone(),
        reports,
    })
}

/// Invokes the explicit caller-supplied portable builder once per artifact.
/// The builder must implement the standard `--input`/`--output` CLI and emit a
/// JSON receipt on stdout. Only sanitized hashes and verification status are
/// persisted.
pub fn render_report_artifacts(
    written: &WrittenReportSetV1,
    builder: &PortableBuilderConfigV1<'_>,
) -> Result<RenderedReportSetV1> {
    if written.schema_version != REPORT_ARTIFACT_SCHEMA_VERSION
        || written.reports.len() != ReportAudienceV1::ALL.len()
        || !valid_sha256(&written.result_manifest_sha256)
    {
        bail!("written report set identity is invalid");
    }
    if !builder.builder_script.is_file() {
        bail!("caller-supplied portable builder script is absent");
    }
    let builder_script_bytes = fs::read(builder.builder_script)?;
    let builder_script_sha256 = sha256_bytes(&builder_script_bytes);
    let mut rendered = Vec::with_capacity(written.reports.len());
    for report in &written.reports {
        verify_written_report(report)?;
        let html_path = report.directory.join("report.html");
        let receipt_path = report.directory.join("render-receipt.json");
        if html_path.exists() || receipt_path.exists() {
            bail!("refusing to overwrite an existing report render output");
        }
        let output = Command::new(builder.node_executable)
            .arg(builder.builder_script)
            .arg("--input")
            .arg(&report.artifact_path)
            .arg("--output")
            .arg(&html_path)
            .output()
            .context("starting caller-supplied portable builder with Node")?;
        if !output.status.success() {
            bail!("portable builder returned a non-success exit status");
        }
        if output.stdout.len() > 1_000_000 {
            bail!("portable builder receipt exceeds the bounded size");
        }
        let builder_receipt: JsonValue = serde_json::from_slice(&output.stdout)
            .context("portable builder stdout is not one JSON receipt")?;
        let verification_stage = validate_builder_receipt(&builder_receipt, &html_path)?;
        let html_bytes = fs::read(&html_path)
            .with_context(|| format!("reading rendered report {}", html_path.display()))?;
        if html_bytes.is_empty() || html_bytes.iter().all(u8::is_ascii_whitespace) {
            bail!("portable builder produced an empty HTML artifact");
        }
        // Inputs must remain immutable across the external builder invocation.
        verify_written_report(report)?;
        let receipt = ReportRenderReceiptV1 {
            schema_version: REPORT_ARTIFACT_SCHEMA_VERSION,
            audience: report.audience,
            artifact_sha256: report.artifact_sha256.clone(),
            provenance_sql_sha256: report.provenance_sql_sha256.clone(),
            builder_script_sha256: builder_script_sha256.clone(),
            builder_receipt_sha256: sha256_bytes(&output.stdout),
            builder_verification_stage: verification_stage,
            html_file_name: "report.html".to_owned(),
            html_bytes: u64::try_from(html_bytes.len())?,
            html_sha256: sha256_bytes(&html_bytes),
            receipt_sha256: String::new(),
        }
        .seal()?;
        receipt.verify()?;
        let receipt_bytes = serde_json::to_vec_pretty(&receipt)?;
        write_new_atomic(&receipt_path, &receipt_bytes)?;
        rendered.push(RenderedReportV1 {
            audience: report.audience,
            html_path,
            receipt_path,
            receipt,
        });
    }
    Ok(RenderedReportSetV1 {
        schema_version: REPORT_ARTIFACT_SCHEMA_VERSION,
        run_id: written.run_id.clone(),
        reports: rendered,
    })
}

pub fn write_and_render_reports(
    result: &ResultManifestV1,
    output_dir: &Path,
    builder: &PortableBuilderConfigV1<'_>,
) -> Result<RenderedReportSetV1> {
    let written = write_report_artifacts(result, output_dir)?;
    render_report_artifacts(&written, builder)
}

fn load_templates() -> Result<BTreeMap<ReportAudienceV1, ReportTemplateV1>> {
    let set: ReportTemplateSetV1 = serde_json::from_str(TEMPLATE_BYTES)?;
    if set.schema_version != REPORT_ARTIFACT_SCHEMA_VERSION
        || set.templates.len() != ReportAudienceV1::ALL.len()
    {
        bail!("report template set is incomplete or version-mismatched");
    }
    let mut templates = BTreeMap::new();
    for template in set.templates {
        if [
            &template.title,
            &template.description,
            &template.summary_heading,
            &template.evidence_heading,
            &template.scope_heading,
            &template.methodology_heading,
            &template.limitations_heading,
            &template.next_steps_heading,
            &template.questions_heading,
        ]
        .into_iter()
        .any(|value| value.trim().is_empty())
            || templates.insert(template.audience, template).is_some()
        {
            bail!("report template contains an empty field or duplicate audience");
        }
    }
    Ok(templates)
}

fn select_metrics<'a>(
    result: &'a ResultManifestV1,
    audience: ReportAudienceV1,
) -> Result<Vec<&'a NamedMetricV1>> {
    let mut metrics: Vec<&NamedMetricV1> = result
        .aggregate_metrics
        .iter()
        .filter(|metric| result.report_metric_ids.contains(&metric.metric_id))
        .collect();
    metrics.sort_by(|left, right| {
        family_rank(left.family)
            .cmp(&family_rank(right.family))
            .then_with(|| left.metric_id.cmp(&right.metric_id))
    });
    metrics.truncate(audience.metric_cap());
    if metrics.is_empty() {
        bail!("result manifest exposes no aggregate report metric");
    }
    let mut ids = BTreeSet::new();
    if metrics
        .iter()
        .any(|metric| !ids.insert(metric.metric_id.as_str()))
    {
        bail!("result manifest contains duplicate selected metric IDs");
    }
    Ok(metrics)
}

fn report_metric_row(metric: &NamedMetricV1, scope_mode: ScopeMode) -> Result<ReportMetricRowV1> {
    let (v1_numerator, v1_denominator) = exact_parts(&metric.v1);
    let (dusk_numerator, dusk_denominator) = exact_parts(&metric.dusk);
    let (paired_delta_numerator, paired_delta_denominator) = exact_parts(&metric.paired_delta);
    Ok(ReportMetricRowV1 {
        metric_id: metric.metric_id.clone(),
        metric_label: match scope_mode {
            ScopeMode::HybridPanel => metric
                .metric_id
                .replace(".global_profile.", ".shared_profile."),
            ScopeMode::FullUniverse => metric.metric_id.clone(),
        },
        family: family_name(metric.family).to_owned(),
        profile_id: metric.profile_id.clone(),
        replay_arm: metric.replay_arm.map(replay_arm_name).map(str::to_owned),
        hlp_scenario: metric
            .hlp_scenario
            .map(hlp_scenario_name)
            .map(str::to_owned),
        definition: metric.definition.clone(),
        unit: metric.paired_delta.unit.clone(),
        v1_availability: availability_name(&metric.v1.availability).to_owned(),
        v1_numerator,
        v1_denominator,
        dusk_availability: availability_name(&metric.dusk.availability).to_owned(),
        dusk_numerator,
        dusk_denominator,
        paired_delta_availability: availability_name(&metric.paired_delta.availability).to_owned(),
        paired_delta_numerator,
        paired_delta_denominator,
        presentation_delta: presentation_value(&metric.paired_delta)?,
        presentation_conversion: "Binary64 display approximation of paired_delta_numerator divided by paired_delta_denominator; exact decimal strings are authoritative.".to_owned(),
    })
}

fn select_chart_rows(rows: &[ReportMetricRowV1]) -> Result<Vec<ReportMetricRowV1>> {
    let mut counts = BTreeMap::<&str, usize>::new();
    for row in rows.iter().filter(|row| row.presentation_delta.is_some()) {
        *counts.entry(&row.unit).or_default() += 1;
    }
    let unit = counts
        .into_iter()
        .max_by(|(left_unit, left_count), (right_unit, right_count)| {
            left_count
                .cmp(right_count)
                .then_with(|| right_unit.cmp(left_unit))
        })
        .map(|(unit, _)| unit)
        .context("selected report metrics have no available paired numeric delta")?;
    let mut selected: Vec<ReportMetricRowV1> = rows
        .iter()
        .filter(|row| row.unit == unit && row.presentation_delta.is_some())
        .cloned()
        .collect();
    for (index, row) in selected.iter_mut().enumerate() {
        row.metric_label = format!("M{:02} · {}", index + 1, row.family.replace('_', " "));
    }
    Ok(selected)
}

fn build_artifact(
    result: &ResultManifestV1,
    audience: ReportAudienceV1,
    template: &ReportTemplateV1,
    all_rows: &[ReportMetricRowV1],
    chart_rows: &[ReportMetricRowV1],
    source: DataAnalyticsSourceV1,
) -> Result<DataAnalyticsArtifactV1> {
    let source_id = source.id.clone();
    let chart_dataset = "paired_metric_deltas".to_owned();
    let exact_dataset = "exact_metric_values".to_owned();
    let chart_unit = chart_rows
        .first()
        .map(|row| row.unit.clone())
        .context("native report chart has no rows")?;
    let chart = DataAnalyticsChartV1 {
        id: "paired_delta_chart".to_owned(),
        title: match result.scope.scope_mode {
            ScopeMode::HybridPanel => "Selected-panel paired Dusk-minus-V1 metric deltas",
            ScopeMode::FullUniverse => "Full-universe paired Dusk-minus-V1 metric deltas",
        }
        .to_owned(),
        subtitle: format!(
            "One common unit ({chart_unit}); plotted values are labeled display approximations and exact rationals remain attached to every row."
        ),
        chart_type: "horizontalBar".to_owned(),
        dataset: chart_dataset.clone(),
        source_id: source_id.clone(),
        encodings: DataAnalyticsChartEncodingsV1 {
            x: DataAnalyticsEncodingV1 {
                field: "metric_label".to_owned(),
                data_type: "nominal".to_owned(),
                label: "Metric ID".to_owned(),
                unit: None,
            },
            y: DataAnalyticsEncodingV1 {
                field: "presentation_delta".to_owned(),
                data_type: "quantitative".to_owned(),
                label: "Dusk minus V1 (display approximation)".to_owned(),
                unit: Some(chart_unit.clone()),
            },
            tooltip: vec![
                tooltip("metric_id", "Metric ID"),
                tooltip("paired_delta_numerator", "Exact delta numerator"),
                tooltip("paired_delta_denominator", "Exact delta denominator"),
                tooltip("unit", "Unit"),
                tooltip("definition", "Metric definition"),
                tooltip("profile_id", "Profile ID"),
            ],
        },
        x_axis_title: "Metric code and family (full ID in tooltip and table)".to_owned(),
        y_axis_title: format!("Dusk minus V1 ({chart_unit}; display approximation)"),
        value_format: "number".to_owned(),
        unit: chart_unit,
        layout: "full".to_owned(),
        max_rows: u64::try_from(chart_rows.len())?,
    };

    let tables = if audience == ReportAudienceV1::TechnicalAudit {
        vec![DataAnalyticsTableV1 {
            id: "exact_metric_table".to_owned(),
            title: "Exact metric components".to_owned(),
            subtitle:
                "Selected holdout metrics with authoritative integer-string rational components."
                    .to_owned(),
            dataset: exact_dataset.clone(),
            density: "compact".to_owned(),
            source_id: source_id.clone(),
            layout: "full".to_owned(),
            default_sort: DataAnalyticsDefaultSortV1 {
                field: "metric_id".to_owned(),
                direction: "asc".to_owned(),
            },
            columns: vec![
                table_column("metric_id", "Metric ID", "text", "left"),
                table_column("unit", "Unit", "text", "left"),
                table_column("v1_numerator", "V1 numerator", "text", "right"),
                table_column("v1_denominator", "V1 denominator", "text", "right"),
                table_column("dusk_numerator", "Dusk numerator", "text", "right"),
                table_column("dusk_denominator", "Dusk denominator", "text", "right"),
                table_column("paired_delta_numerator", "Delta numerator", "text", "right"),
                table_column(
                    "paired_delta_denominator",
                    "Delta denominator",
                    "text",
                    "right",
                ),
            ],
        }]
    } else {
        Vec::new()
    };

    let mut blocks = vec![
        markdown("title", format!("# {}", template.title), None),
        markdown(
            "summary",
            format!(
                "## {}\n\n{}",
                template.summary_heading,
                summary_body(result, audience)
            ),
            None,
        ),
        markdown(
            "certification",
            certification_body(result),
            None,
        ),
        markdown(
            "evidence",
            format!(
                "## {}\n\nThe chart compares Dusk-minus-V1 deltas only within one declared unit. Direction is metric-specific: a positive delta can be desirable for capacity or revenue and undesirable for execution cost or bad debt. Every plotted value carries its stable metric ID and authoritative exact numerator and denominator.",
                template.evidence_heading
            ),
            None,
        ),
        DataAnalyticsBlockV1::Chart {
            id: "evidence_chart".to_owned(),
            chart_id: chart.id.clone(),
        },
        markdown(
            "scope",
            format!(
                "## {}\n\n{}",
                template.scope_heading,
                scope_body(result),
            ),
            None,
        ),
    ];
    if audience == ReportAudienceV1::TechnicalAudit {
        blocks.push(DataAnalyticsBlockV1::Table {
            id: "exact_values".to_owned(),
            table_id: "exact_metric_table".to_owned(),
        });
    }
    blocks.extend([
        markdown(
            "methodology",
            format!(
                "## {}\n\nHistorical V1 operations are replayed factually before the frozen Dusk profiles are evaluated on the sealed holdout. The reporting layer does not recompute protocol economics: it selects verified manifest metrics, preserves exact rational components, and creates explicitly labeled display-only numeric approximations for native charts.",
                template.methodology_heading
            ),
            None,
        ),
        markdown("sensitivity", sensitivity_body(result), None),
        markdown(
            "limitations",
            limitations_body(result, template),
            None,
        ),
        markdown(
            "next_steps",
            format!(
                "## {}\n\n- Review the metric definitions and exact rational rows before relying on visual magnitude.\n- Apply the approved safety and execution constraints before selecting a profile.\n- Re-run the same frozen procedure when the source window, Dusk code, or provider marks change.",
                template.next_steps_heading
            ),
            None,
        ),
        markdown(
            "questions",
            format!(
                "## {}\n\n- Which trade-offs are governance constraints rather than optimization objectives?\n- Which findings remain stable under top-market exclusion and paired block bootstrap evidence?\n- What additional live-system behavior would be required before treating the counterfactual as a deployment forecast?",
                template.questions_heading
            ),
            None,
        ),
    ]);

    let mut datasets = BTreeMap::new();
    datasets.insert(
        chart_dataset,
        chart_rows.iter().map(ReportMetricRowV1::to_json).collect(),
    );
    if audience == ReportAudienceV1::TechnicalAudit {
        datasets.insert(
            exact_dataset,
            all_rows.iter().map(ReportMetricRowV1::to_json).collect(),
        );
    }
    let manifest = DataAnalyticsManifestV1 {
        version: 1,
        surface: "report".to_owned(),
        title: template.title.clone(),
        description: template.description.clone(),
        cards: Vec::new(),
        charts: vec![chart],
        tables,
        sources: vec![source.clone()],
        blocks,
    };
    Ok(DataAnalyticsArtifactV1 {
        surface: "report".to_owned(),
        manifest,
        snapshot: DataAnalyticsSnapshotV1 {
            version: 1,
            status: "ready".to_owned(),
            datasets,
            access_issues: Vec::new(),
        },
        sources: vec![source],
    })
}

fn source_contract(
    result: &ResultManifestV1,
    audience: ReportAudienceV1,
    metrics: &[&NamedMetricV1],
    sql: &str,
) -> DataAnalyticsSourceV1 {
    DataAnalyticsSourceV1 {
        id: "verified_result_metrics".to_owned(),
        label: "Verified benchmark result metrics".to_owned(),
        path: "provenance.sql".to_owned(),
        query: DataAnalyticsSourceQueryV1 {
            engine: "duckdb".to_owned(),
            id: format!("{}_{}", audience.slug(), &sha256_bytes(sql.as_bytes())[..16]),
            sql: sql.to_owned(),
            description: "Generated exact metric rows selected from the self-verified ResultManifestV1; chart decimals are presentation-only.".to_owned(),
            language: "sql".to_owned(),
            tables_used: vec![
                "metrics-manifest.json::aggregate_metrics".to_owned(),
                "exact_metric_values".to_owned(),
            ],
            filters: vec![
                format!(
                    "holdout Unix window [{}, {})",
                    result.holdout_start_unix_seconds,
                    result.holdout_end_unix_seconds_exclusive
                ),
                format!("audience template: {}", audience.slug()),
                "metric_id must be declared in ResultManifestV1.report_metric_ids".to_owned(),
                format!("deterministic bounded selection: at most {} rows", audience.metric_cap()),
            ],
            metric_definitions: metrics
                .iter()
                .map(|metric| format!("{}: {}", metric.metric_id, metric.definition))
                .collect(),
        },
    }
}

fn provenance_sql(
    result: &ResultManifestV1,
    audience: ReportAudienceV1,
    rows: &[ReportMetricRowV1],
) -> String {
    let mut sql = format!(
        "-- Generated from verified ResultManifestV1.\n-- manifest_sha256: {}\n-- audience: {}\n-- Exact numerator/denominator text is authoritative; presentation_delta is display-only.\nWITH exact_metric_values(metric_id, family, profile_id, replay_arm, hlp_scenario, definition, unit, v1_availability, v1_numerator, v1_denominator, dusk_availability, dusk_numerator, dusk_denominator, paired_delta_availability, paired_delta_numerator, paired_delta_denominator, presentation_delta, presentation_conversion) AS (\n    VALUES\n",
        result.manifest_sha256,
        audience.slug()
    );
    for (index, row) in rows.iter().enumerate() {
        let values = [
            sql_string(Some(&row.metric_id)),
            sql_string(Some(&row.family)),
            sql_string(row.profile_id.as_deref()),
            sql_string(row.replay_arm.as_deref()),
            sql_string(row.hlp_scenario.as_deref()),
            sql_string(Some(&row.definition)),
            sql_string(Some(&row.unit)),
            sql_string(Some(&row.v1_availability)),
            sql_string(row.v1_numerator.as_deref()),
            sql_string(row.v1_denominator.as_deref()),
            sql_string(Some(&row.dusk_availability)),
            sql_string(row.dusk_numerator.as_deref()),
            sql_string(row.dusk_denominator.as_deref()),
            sql_string(Some(&row.paired_delta_availability)),
            sql_string(row.paired_delta_numerator.as_deref()),
            sql_string(row.paired_delta_denominator.as_deref()),
            row.presentation_delta
                .map(|value| format!("CAST('{}' AS DOUBLE)", value))
                .unwrap_or_else(|| "NULL".to_owned()),
            sql_string(Some(&row.presentation_conversion)),
        ];
        sql.push_str("        (");
        sql.push_str(&values.join(", "));
        sql.push(')');
        sql.push_str(if index + 1 == rows.len() { "\n" } else { ",\n" });
    }
    sql.push_str(")\nSELECT * FROM exact_metric_values ORDER BY metric_id;\n");
    sql
}

fn summary_body(result: &ResultManifestV1, audience: ReportAudienceV1) -> String {
    if result.scope.scope_mode == ScopeMode::HybridPanel {
        if result.no_global_winner {
            return match audience {
                ReportAudienceV1::TechnicalAudit => "The verified selected-panel constraints do not support one shared-profile winner. The evidence retains the Pareto set and reports metric-specific trade-offs without collapsing unlike units into one score.".to_owned(),
                ReportAudienceV1::GovernanceProduct => "No single shared Dusk profile wins across every approved objective in the selected panel. Governance should choose among the feasible trade-offs only after deciding which safety, execution, borrowing, and revenue outcomes are constraints.".to_owned(),
                ReportAudienceV1::Public => "This selected-panel benchmark does not declare a winning Dusk profile. It reports where Dusk and V1 differ within the bound panel and keeps the trade-offs visible instead of promoting one headline score.".to_owned(),
            };
        }
        let profile = result
            .global_winner_profile_id
            .as_deref()
            .unwrap_or("<missing-profile>");
        return match audience {
            ReportAudienceV1::TechnicalAudit => format!("The configured feasibility gates identify profile `{}` as the selected-panel shared-profile winner for this frozen holdout. That designation is conditional on the bound panel, metric set, window, source data, and gate definitions.", markdown_inline(profile)),
            ReportAudienceV1::GovernanceProduct => format!("The configured gates identify Dusk profile `{}` as the selected-panel winner for this historical holdout. The decision remains conditional on the bound panel, approved objectives, and limitations.", markdown_inline(profile)),
            ReportAudienceV1::Public => format!("Within this selected historical panel, the configured gates identify Dusk profile `{}` as the winning shared profile. This is a bounded counterfactual result, not a promise of future performance.", markdown_inline(profile)),
        };
    }

    if result.no_global_winner {
        match audience {
            ReportAudienceV1::TechnicalAudit => "The verified constraints do not support one global winner. The evidence therefore retains the Pareto set and reports metric-specific trade-offs without collapsing unlike units into one score.".to_owned(),
            ReportAudienceV1::GovernanceProduct => "No single Dusk profile wins across every approved objective. Governance should choose among the feasible trade-offs only after deciding which safety, execution, borrowing, and revenue outcomes are constraints.".to_owned(),
            ReportAudienceV1::Public => "This benchmark does not declare an overall winner. It reports where Dusk and V1 differ and keeps the trade-offs visible instead of promoting one headline score.".to_owned(),
        }
    } else {
        let profile = result
            .global_winner_profile_id
            .as_deref()
            .unwrap_or("<missing-profile>");
        match audience {
            ReportAudienceV1::TechnicalAudit => format!("The configured feasibility gates identify profile `{}` as the global winner for this frozen holdout. That designation is conditional on this metric set, window, source data, and gate definitions.", markdown_inline(profile)),
            ReportAudienceV1::GovernanceProduct => format!("The configured gates identify Dusk profile `{}` as the global winner for this historical holdout. The decision remains conditional on the approved objectives and limitations.", markdown_inline(profile)),
            ReportAudienceV1::Public => format!("For this historical holdout, the configured gates identify Dusk profile `{}` as the overall winner. This is a bounded counterfactual result, not a promise of future performance.", markdown_inline(profile)),
        }
    }
}

fn certification_body(result: &ResultManifestV1) -> String {
    let decision = &result.scope.final_phase_gate_decision;
    let certification = match decision.usd_certification {
        CertificationStatus::Pass => "PASS",
        CertificationStatus::Limited => "LIMITED",
        CertificationStatus::Pending => "PENDING",
        CertificationStatus::Fail => "FAIL",
    };
    let scope = match result.scope.scope_mode {
        ScopeMode::HybridPanel => "selected-panel",
        ScopeMode::FullUniverse => "full-universe",
    };
    let tier = match result.scope.evidence_tier {
        EvidenceTier::DecisionGrade => "DecisionGrade",
        EvidenceTier::AuditGrade => "AuditGrade",
    };
    let mut body = format!(
        "## Evidence certification — {certification}\n\nThis is {tier} evidence for the exact {scope} replay encoded by the result manifest. Actual source agreement, transaction/decode coverage, economic replay, historical-price, metric-accounting, and conservation gates remain mandatory."
    );
    if decision.limitations.is_empty() {
        body.push_str(" The final phase gate records no certification limitation.");
    } else {
        body.push_str(" The final phase gate permits only these typed limitations:\n\n");
        body.push_str(
            &decision
                .limitations
                .iter()
                .map(|limitation| {
                    format!(
                        "- {} — {}",
                        gate_label(limitation.gate),
                        markdown_text(&limitation.detail_code)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    body
}

fn gate_label(gate: HardGateId) -> &'static str {
    match gate {
        HardGateId::DecoderRevisionCertification => "Decoder revision certification",
        HardGateId::TerminalState => "Terminal state certification",
        HardGateId::TransactionInventory => "Transaction inventory",
        HardGateId::StateChangingDecode => "State-changing decode",
        HardGateId::CanonicalOrder => "Canonical order",
        HardGateId::AuthoritativeBlockTime => "Authoritative block time",
        HardGateId::VersionedConfig => "Versioned configuration",
        HardGateId::HistoricalTransferFees => "Historical transfer fees",
        HardGateId::SourceAgreement => "Source agreement",
        HardGateId::EconomicReplay => "Economic replay",
        HardGateId::HistoricalPrices => "Historical prices",
    }
}

fn scope_body(result: &ResultManifestV1) -> String {
    let common = "Metrics retain their declared units, replay arm, hLP scenario, profile identity, and availability state. User-flow revenue excludes synthetic arbitrage and synthetic protocol settlement; each synthetic origin remains separately named.";
    match result.scope.scope_mode {
        ScopeMode::FullUniverse => format!(
            "The population is the full eligible market universe in the verified common-window holdout: {} USD-headline markets, bound by selected-market hash `{}`. {common}",
            result.scope.selected_markets.len(),
            markdown_inline(&result.scope.selected_market_set_sha256),
        ),
        ScopeMode::HybridPanel => {
            let evidence = result
                .scope
                .cohort_representativeness
                .as_ref()
                .expect("verified hybrid result must carry representativeness evidence");
            format!(
                "Claims are limited to a selected-panel scope of {} USD-headline markets, bound by selected-market hash `{}` and a separately sealed cohort selection. The extraction cohort represented the following visible-market activity:\n\n- Swaps: {}\n- Debt actions: {}\n- Liquidations: {}\n\n{common}",
                result.scope.selected_markets.len(),
                markdown_inline(&result.scope.selected_market_set_sha256),
                activity_coverage(&evidence.swaps),
                activity_coverage(&evidence.debt_actions),
                activity_coverage(&evidence.liquidations),
            )
        }
    }
}

fn activity_coverage(coverage: &ScopeActivityCoverageV1) -> String {
    format!(
        "{} of {} rows ({}.{:02}%)",
        coverage.selected_panel_rows,
        coverage.visible_universe_rows,
        coverage.selected_panel_coverage_bps / 100,
        coverage.selected_panel_coverage_bps % 100,
    )
}

fn sensitivity_body(result: &ResultManifestV1) -> String {
    let mut bullets = Vec::new();
    for evidence in result.sensitivity_evidence.iter().take(8) {
        let identity = format!(
            "profile `{}`, {} arm, {}",
            markdown_inline(&evidence.profile_id),
            replay_arm_name(evidence.replay_arm),
            hlp_scenario_name(evidence.hlp_scenario)
        );
        if let Some(reason) = &evidence.top_market_exclusion_unsupported_reason {
            bullets.push(format!(
                "{identity}: top-market exclusion is unavailable ({}) while the paired UTC 7-day bootstrap remains bound to {} samples.",
                markdown_text(reason),
                evidence.bootstrap_samples
            ));
            continue;
        }
        for exclusion in evidence.top_market_exclusions.iter().take(2) {
            let basis = match exclusion.basis {
                TopMarketRankingBasisV1::FactualUsdSwapInput => "factual USD swap input",
                TopMarketRankingBasisV1::FactualStakeholderRevenueUsd => {
                    "factual stakeholder revenue"
                }
            };
            let delta = &exclusion
                .estimate_without_top_market
                .stakeholder_user_flow_revenue_delta_usd_micro
                .exact;
            bullets.push(format!(
                "{identity}: excluding `{}` ranked first by {basis} leaves {} markets and an exact user-flow stakeholder-revenue delta of {}/{} USD-micro.",
                markdown_inline(&exclusion.excluded_market),
                exclusion.remaining_market_count,
                markdown_text(delta.numerator.as_str()),
                markdown_text(delta.denominator.as_str()),
            ));
        }
    }
    if bullets.is_empty() {
        bullets.push(
            "No top-market exclusion claim is present; consult the immutable sensitivity source artifacts before making a concentration claim."
                .to_owned(),
        );
    }
    format!(
        "## Sensitivity and concentration\n\n{}\n\nPaired uncertainty intervals use one deterministic UTC seven-day resampling path per frozen profile/arm/scenario; synthetic-arbitrage and synthetic protocol-settlement revenue are never merged into user-flow revenue or each other.",
        bullets
            .into_iter()
            .map(|bullet| format!("- {bullet}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

fn limitations_body(result: &ResultManifestV1, template: &ReportTemplateV1) -> String {
    let scope_limitation = match result.scope.scope_mode {
        ScopeMode::HybridPanel => "Claims apply only to the immutable selected panel and common-window holdout; cohort representativeness percentages describe source-row coverage and do not turn the panel into a market-wide estimate. Scoped block targets are selected-market DB signatures plus successful program signatures absent from every DB row; a transaction indexed only under a non-selected market could also carry a global side effect outside scoped blocks, and only replay across the complete visible market universe rules out that mixed-transaction edge.",
        ScopeMode::FullUniverse => "The full eligible market universe, common window, finalized chain history, historical price provider, and frozen Dusk profile define the scope.",
    };
    let mut bullets = vec![
        "This is a historical counterfactual replay, not a live deployment result, causal estimate, or forecast.".to_owned(),
        scope_limitation.to_owned(),
        "Synthetic arbitrage and synthetic protocol settlement are excluded from stakeholder user-flow revenue and remain separately labeled.".to_owned(),
        "Cash-realization, inventory, principal-disposition, claim, and flash lanes are not double counted as stakeholder economic revenue.".to_owned(),
        "Common-capital absolute return and APR use factual V1 accounted-LP capital-time only as a standardized comparison denominator; exact Dusk external-LP-owner and hLP-owner APY remain unavailable without native ownership capital-time evidence.".to_owned(),
        "Chart values are display-only binary approximations; exact integer-string numerators and denominators are authoritative.".to_owned(),
        "A favorable direction is metric-specific, so the chart does not encode a universal good-or-bad color judgment.".to_owned(),
    ];
    for limitation in &result.scope.final_phase_gate_decision.limitations {
        bullets.insert(
            0,
            format!(
                "Certification limitation — {}: {}",
                gate_label(limitation.gate),
                markdown_text(&limitation.detail_code)
            ),
        );
    }
    for limitation in result.limitations.iter().take(8) {
        let bounded: String = limitation
            .chars()
            .filter(|character| !character.is_control() || *character == ' ')
            .take(320)
            .collect();
        if !bounded.trim().is_empty() {
            bullets.push(format!(
                "Result-manifest limitation: {}",
                markdown_text(&bounded)
            ));
        }
    }
    format!(
        "## {}\n\n{}",
        template.limitations_heading,
        bullets
            .into_iter()
            .map(|bullet| format!("- {bullet}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

fn validate_artifact_contract(
    artifact: &DataAnalyticsArtifactV1,
    result: &ResultManifestV1,
) -> Result<()> {
    let encoded = serde_json::to_vec(artifact)?;
    let inline_source_bytes = artifact
        .sources
        .iter()
        .map(|source| source.query.sql.len())
        .try_fold(0_usize, usize::checked_add)
        .context("report inline source byte count overflowed")?;
    if artifact.surface != "report"
        || artifact.manifest.version != 1
        || artifact.manifest.surface != "report"
        || artifact.snapshot.version != 1
        || artifact.snapshot.status != "ready"
        || artifact.manifest.title.trim().is_empty()
        || artifact.manifest.charts.is_empty()
        || artifact.manifest.sources.len() != 1
        || artifact.sources != artifact.manifest.sources
        || artifact.snapshot.datasets.len() > 50
        || artifact
            .snapshot
            .datasets
            .values()
            .any(|rows| rows.len() > MAX_REPORT_METRIC_ROWS)
        || encoded.len() > MAX_REPORT_ARTIFACT_BYTES
        || inline_source_bytes > MAX_REPORT_INLINE_SOURCE_BYTES
        || artifact.manifest.blocks.iter().any(|block| {
            !matches!(
                block,
                DataAnalyticsBlockV1::Markdown { .. }
                    | DataAnalyticsBlockV1::Chart { .. }
                    | DataAnalyticsBlockV1::Table { .. }
            )
        })
    {
        bail!("report artifact violates the bounded native data contract");
    }
    let first = artifact
        .manifest
        .blocks
        .first()
        .context("report artifact has no title block")?;
    let expected_title = format!("# {}", artifact.manifest.title);
    if !matches!(first, DataAnalyticsBlockV1::Markdown { body, .. } if body == &expected_title) {
        bail!("report title block does not match the manifest title");
    }
    let chart_ids: BTreeSet<&str> = artifact
        .manifest
        .charts
        .iter()
        .map(|chart| chart.id.as_str())
        .collect();
    if artifact.manifest.blocks.iter().any(
        |block| matches!(block, DataAnalyticsBlockV1::Chart { chart_id, .. } if !chart_ids.contains(chart_id.as_str())),
    ) {
        bail!("report chart block does not resolve to a native chart");
    }
    for chart in &artifact.manifest.charts {
        let rows = artifact
            .snapshot
            .datasets
            .get(&chart.dataset)
            .context("native chart dataset is absent")?;
        if rows.is_empty() || chart.source_id != artifact.sources[0].id {
            bail!("native chart has no reviewed rows or source identity");
        }
        for row in rows {
            let metric_id = row
                .get("metric_id")
                .and_then(JsonValue::as_str)
                .context("chart row has no stable metric ID")?;
            if !result.report_metric_ids.contains(metric_id)
                || row
                    .get("paired_delta_numerator")
                    .and_then(JsonValue::as_str)
                    .is_none()
                || row
                    .get("paired_delta_denominator")
                    .and_then(JsonValue::as_str)
                    .is_none()
                || row
                    .get("presentation_delta")
                    .and_then(JsonValue::as_f64)
                    .is_none()
                || row
                    .get("presentation_conversion")
                    .and_then(JsonValue::as_str)
                    .is_none()
            {
                bail!("quantitative chart claim lacks exact metric provenance");
            }
        }
    }
    Ok(())
}

fn verify_source_file_resolution(
    artifact: &DataAnalyticsArtifactV1,
    artifact_directory: &Path,
) -> Result<()> {
    for source in &artifact.sources {
        let relative = Path::new(&source.path);
        if relative.is_absolute()
            || relative.extension() != Some(OsStr::new("sql"))
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            bail!("report source path is not a safe relative SQL path");
        }
        let actual = artifact_directory.join(relative);
        let sql = fs::read_to_string(&actual)
            .with_context(|| format!("reading generated SQL source {}", actual.display()))?;
        if sql != source.query.sql || sql.trim().is_empty() {
            bail!("report source metadata does not resolve to its generated SQL file");
        }
    }
    Ok(())
}

fn verify_written_report(report: &WrittenReportSpecV1) -> Result<()> {
    if !report.artifact_path.is_file()
        || !report.provenance_sql_path.is_file()
        || sha256_file(&report.artifact_path)? != report.artifact_sha256
        || sha256_file(&report.provenance_sql_path)? != report.provenance_sql_sha256
    {
        bail!("written report spec or provenance commitment mismatch");
    }
    let artifact: DataAnalyticsArtifactV1 =
        serde_json::from_slice(&fs::read(&report.artifact_path)?)?;
    verify_source_file_resolution(&artifact, &report.directory)
}

fn validate_builder_receipt(receipt: &JsonValue, html_path: &Path) -> Result<String> {
    if receipt.get("ok").and_then(JsonValue::as_bool) != Some(true)
        || receipt
            .pointer("/stages/validation")
            .and_then(JsonValue::as_str)
            != Some("passed")
        || receipt
            .pointer("/stages/package")
            .and_then(JsonValue::as_str)
            != Some("passed")
    {
        bail!("portable builder receipt did not pass validation and packaging");
    }
    let verification = receipt
        .pointer("/stages/verification")
        .and_then(JsonValue::as_str)
        .context("portable builder receipt has no verification stage")?;
    if !matches!(verification, "passed" | "structural_only") {
        bail!("portable builder receipt did not pass verification");
    }
    let receipt_html = receipt
        .get("html")
        .and_then(JsonValue::as_str)
        .context("portable builder receipt has no HTML path")?;
    let receipt_path = Path::new(receipt_html);
    let receipt_absolute = if receipt_path.is_absolute() {
        receipt_path.to_path_buf()
    } else {
        std::env::current_dir()?.join(receipt_path)
    };
    let expected_absolute = if html_path.is_absolute() {
        html_path.to_path_buf()
    } else {
        std::env::current_dir()?.join(html_path)
    };
    if receipt_absolute != expected_absolute {
        bail!("portable builder receipt names a different HTML output");
    }
    Ok(verification.to_owned())
}

fn presentation_value(value: &MetricValueV1) -> Result<Option<f64>> {
    if value.availability != MetricAvailability::Available {
        return Ok(None);
    }
    let numerator = BigInt::from_str(
        value
            .numerator
            .as_ref()
            .context("available metric has no numerator")?
            .as_str(),
    )?;
    let denominator = BigInt::from_str(
        value
            .denominator
            .as_ref()
            .context("available metric has no denominator")?
            .as_str(),
    )?;
    if denominator <= BigInt::from(0_u8) {
        bail!("available metric denominator is not positive");
    }
    let display = BigRational::new(numerator, denominator)
        .to_f64()
        .context("exact metric cannot be represented as a finite display approximation")?;
    if !display.is_finite() {
        bail!("exact metric display approximation is non-finite");
    }
    Ok(Some(display))
}

fn exact_parts(value: &MetricValueV1) -> (Option<String>, Option<String>) {
    (
        value.numerator.as_ref().map(ToString::to_string),
        value.denominator.as_ref().map(ToString::to_string),
    )
}

fn tooltip(field: &str, label: &str) -> DataAnalyticsEncodingV1 {
    DataAnalyticsEncodingV1 {
        field: field.to_owned(),
        data_type: "text".to_owned(),
        label: label.to_owned(),
        unit: None,
    }
}

fn table_column(
    field: &str,
    label: &str,
    data_type: &str,
    align: &str,
) -> DataAnalyticsTableColumnV1 {
    DataAnalyticsTableColumnV1 {
        field: field.to_owned(),
        label: label.to_owned(),
        data_type: data_type.to_owned(),
        align: align.to_owned(),
    }
}

fn markdown(id: &str, body: String, source_id: Option<String>) -> DataAnalyticsBlockV1 {
    DataAnalyticsBlockV1::Markdown {
        id: id.to_owned(),
        body,
        source_id,
    }
}

fn sql_string(value: Option<&str>) -> String {
    value
        .map(|value| format!("'{}'", value.replace('\'', "''")))
        .unwrap_or_else(|| "NULL".to_owned())
}

fn markdown_inline(value: &str) -> String {
    value.replace('`', "\\`").replace(['\r', '\n'], " ")
}

fn markdown_text(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace(['\r', '\n'], " ")
        .replace("- ", "\\- ")
}

fn write_new_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if path.exists() {
        bail!("refusing to overwrite report artifact {}", path.display());
    }
    let file_name = path
        .file_name()
        .and_then(OsStr::to_str)
        .context("report artifact filename is not UTF-8")?;
    let partial = path.with_file_name(format!(".{file_name}.partial-{}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial)
        .with_context(|| format!("creating report partial {}", partial.display()))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&partial, path)
        .with_context(|| format!("committing report artifact {}", path.display()))?;
    Ok(())
}

fn receipt_hash(receipt: &ReportRenderReceiptV1) -> Result<String> {
    let bytes = serde_json::to_vec(receipt)?;
    let mut digest = Sha256::new();
    digest.update(RECEIPT_HASH_DOMAIN);
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
    Ok(hex::encode(digest.finalize()))
}

fn sha256_file(path: &Path) -> Result<String> {
    Ok(sha256_bytes(&fs::read(path)?))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

const fn family_rank(family: MetricFamily) -> u8 {
    match family {
        MetricFamily::CollateralEfficiency => 0,
        MetricFamily::SpotCapitalEfficiency => 1,
        MetricFamily::Leverage => 2,
        MetricFamily::LpRevenue => 3,
        MetricFamily::ProtocolRevenue => 4,
        MetricFamily::BalanceSheet => 5,
        MetricFamily::Safety => 6,
        MetricFamily::Coverage => 7,
    }
}

const fn family_name(family: MetricFamily) -> &'static str {
    match family {
        MetricFamily::CollateralEfficiency => "collateral_efficiency",
        MetricFamily::SpotCapitalEfficiency => "spot_capital_efficiency",
        MetricFamily::Leverage => "leverage",
        MetricFamily::LpRevenue => "lp_revenue",
        MetricFamily::ProtocolRevenue => "protocol_revenue",
        MetricFamily::BalanceSheet => "balance_sheet",
        MetricFamily::Safety => "safety",
        MetricFamily::Coverage => "coverage",
    }
}

const fn availability_name(availability: &MetricAvailability) -> &'static str {
    match availability {
        MetricAvailability::Available => "available",
        MetricAvailability::NotApplicableNoLendingActivity => "not_applicable_no_lending_activity",
        MetricAvailability::NotApplicableVenueMechanism => "not_applicable_venue_mechanism",
        MetricAvailability::MissingHistoricalUsdMark => "missing_historical_usd_mark",
        MetricAvailability::WindowTooShortForAnnualization => "window_too_short_for_annualization",
        MetricAvailability::MissingOwnershipDenominator => "missing_ownership_denominator",
        MetricAvailability::MissingCashRealizationEvidence => "missing_cash_realization_evidence",
        MetricAvailability::OutsideCommonWindow => "outside_common_window",
        MetricAvailability::FailedCoverageGate => "failed_coverage_gate",
        MetricAvailability::NotEvaluated => "not_evaluated",
    }
}

const fn replay_arm_name(arm: ReplayArm) -> &'static str {
    match arm {
        ReplayArm::FixedFlow => "fixed_flow",
        ReplayArm::Intent => "intent",
    }
}

const fn hlp_scenario_name(scenario: HlpScenario) -> &'static str {
    match scenario {
        HlpScenario::NoHlp => "no_hlp",
        HlpScenario::MaximumFeasible => "maximum_feasible",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        audit::{
            CertificationStatus, EvidenceTier as GateEvidenceTier, GateLimitationV1, GateStatus,
            HardGateId, HardGatePhase, PhaseGateDecisionV1,
        },
        canonical::{SignedTokenAmount, TokenAmount},
        config::{EvidenceTier, ScopeMode},
        metrics::{
            AccountingVerificationV1, CohortRepresentativenessEvidenceV1, CoverageMetricV1,
            HeadlineCoverageV1, ResultScopeV1, ScopeActivityCoverageV1, METRICS_SCHEMA_VERSION,
        },
    };
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn writes_three_bounded_native_artifacts_with_exact_claim_rows() {
        let directory = temp_directory("specs");
        let result = fixture_manifest(true);
        let written = write_report_artifacts(&result, &directory).unwrap();
        assert_eq!(written.reports.len(), 3);
        for report in &written.reports {
            let artifact: DataAnalyticsArtifactV1 =
                serde_json::from_slice(&fs::read(&report.artifact_path).unwrap()).unwrap();
            assert!(!artifact.manifest.charts.is_empty());
            assert!(artifact
                .manifest
                .blocks
                .iter()
                .all(|block| !matches!(block, DataAnalyticsBlockV1::Markdown { body, .. } if body.contains("<html"))));
            let serialized = serde_json::to_string(&artifact).unwrap();
            assert!(
                serialized.contains("No single")
                    || serialized.contains("do not support one")
                    || serialized.contains("does not declare")
            );
            verify_source_file_resolution(&artifact, &report.directory).unwrap();
            validate_artifact_contract(&artifact, &result).unwrap();
            for rows in artifact.snapshot.datasets.values() {
                assert!(rows.len() <= MAX_REPORT_METRIC_ROWS);
            }
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn manifest_checksum_failure_prevents_any_report_output() {
        let directory = temp_directory("bad-hash");
        let mut result = fixture_manifest(false);
        result.limitations.push("mutated after sealing".to_owned());
        assert!(write_report_artifacts(&result, &directory).is_err());
        assert!(!directory.exists());
    }

    #[test]
    fn canonical_payload_bounds_fail_before_any_report_output() {
        let directory = temp_directory("payload-bound");
        let mut result = fixture_manifest(false);
        result.aggregate_metrics[0].definition = "x".repeat(MAX_REPORT_INLINE_SOURCE_BYTES + 1);
        result = result.seal().unwrap();
        assert!(write_report_artifacts(&result, &directory).is_err());
        assert!(!directory.exists());
    }

    #[test]
    fn decision_wording_is_derived_from_the_verified_winner_state() {
        for (no_winner, expected, forbidden) in [
            (
                true,
                "do not support one global winner",
                "profile `profile` as the global winner",
            ),
            (
                false,
                "profile `profile` as the global winner",
                "No single Dusk profile wins",
            ),
        ] {
            let directory = temp_directory(if no_winner {
                "no-winner-copy"
            } else {
                "winner-copy"
            });
            let written = write_report_artifacts(&fixture_manifest(no_winner), &directory).unwrap();
            let technical = written
                .reports
                .iter()
                .find(|report| report.audience == ReportAudienceV1::TechnicalAudit)
                .unwrap();
            let artifact_text = fs::read_to_string(&technical.artifact_path).unwrap();
            assert!(artifact_text.contains(expected));
            assert!(!artifact_text.contains(forbidden));
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn decision_grade_selected_panel_is_limited_and_never_claims_universe_scope() {
        let directory = temp_directory("selected-panel-limited");
        let written = write_report_artifacts(&hybrid_fixture(false), &directory).unwrap();
        for report in written.reports {
            let artifact: DataAnalyticsArtifactV1 =
                serde_json::from_slice(&fs::read(&report.artifact_path).unwrap()).unwrap();
            let markdown_blocks: Vec<&str> = artifact
                .manifest
                .blocks
                .iter()
                .filter_map(|block| match block {
                    DataAnalyticsBlockV1::Markdown { body, .. } => Some(body.as_str()),
                    _ => None,
                })
                .collect();
            let prose = markdown_blocks.join("\n");
            assert!(prose.contains("selected-panel"));
            assert!(prose.contains("Evidence certification — LIMITED"));
            assert!(prose.contains("Decoder revision certification"));
            assert!(prose.contains("Terminal state certification"));
            assert!(prose.contains("80.00%"));
            assert!(prose.contains("50.00%"));
            assert!(prose.contains("75.00%"));
            assert!(!prose.contains("global winner"));
            assert!(!prose.contains("full-universe"));
            let certification_index = artifact
                .manifest
                .blocks
                .iter()
                .position(|block| matches!(block, DataAnalyticsBlockV1::Markdown { id, .. } if id == "certification"))
                .unwrap();
            let evidence_index = artifact
                .manifest
                .blocks
                .iter()
                .position(|block| matches!(block, DataAnalyticsBlockV1::Markdown { id, .. } if id == "evidence"))
                .unwrap();
            assert!(certification_index < evidence_index);
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn caller_supplied_node_builder_is_receipted_and_hashed() {
        let directory = temp_directory("render");
        fs::create_dir_all(&directory).unwrap();
        let builder_path = directory.join("fake-builder.mjs");
        fs::write(
            &builder_path,
            r#"import fs from "node:fs";
const input = process.argv[process.argv.indexOf("--input") + 1];
const output = process.argv[process.argv.indexOf("--output") + 1];
const artifact = JSON.parse(fs.readFileSync(input, "utf8"));
fs.writeFileSync(output, `portable artifact: ${artifact.manifest.title}`);
process.stdout.write(JSON.stringify({ok:true,html:output,stages:{validation:"passed",package:"passed",verification:"structural_only"}}));
"#,
        )
        .unwrap();
        let reports_dir = directory.join("reports");
        let written = write_report_artifacts(&fixture_manifest(false), &reports_dir).unwrap();
        let rendered = render_report_artifacts(
            &written,
            &PortableBuilderConfigV1 {
                node_executable: OsStr::new("node"),
                builder_script: &builder_path,
            },
        )
        .unwrap();
        assert_eq!(rendered.reports.len(), 3);
        for report in rendered.reports {
            report.receipt.verify().unwrap();
            assert!(report.html_path.metadata().unwrap().len() > 0);
            assert_eq!(
                sha256_file(&report.html_path).unwrap(),
                report.receipt.html_sha256
            );
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    #[ignore = "requires an explicit DATA_ANALYTICS_REPORT_BUILDER path"]
    fn caller_supplied_canonical_builder_accepts_all_three_artifacts() {
        let builder_path = std::env::var_os("DATA_ANALYTICS_REPORT_BUILDER")
            .map(PathBuf::from)
            .expect("DATA_ANALYTICS_REPORT_BUILDER must name the portable builder");
        let directory = temp_directory("canonical-render");
        let written = write_report_artifacts(&fixture_manifest(true), &directory).unwrap();
        let rendered = render_report_artifacts(
            &written,
            &PortableBuilderConfigV1 {
                node_executable: OsStr::new("node"),
                builder_script: &builder_path,
            },
        )
        .unwrap();
        assert_eq!(rendered.reports.len(), ReportAudienceV1::ALL.len());
        assert!(rendered
            .reports
            .iter()
            .all(|report| report.receipt.verify().is_ok()));
        fs::remove_dir_all(directory).unwrap();
    }

    fn fixture_manifest(no_winner: bool) -> ResultManifestV1 {
        let metrics = vec![
            metric(
                "fixed_flow.no_hlp.collateral.actionable_capacity",
                MetricFamily::CollateralEfficiency,
                100,
                125,
                25,
                "usd_micro",
            ),
            metric(
                "fixed_flow.no_hlp.spot.execution_cost",
                MetricFamily::SpotCapitalEfficiency,
                0,
                5,
                5,
                "ppb",
            ),
            metric(
                "fixed_flow.no_hlp.revenue.user_flow",
                MetricFamily::LpRevenue,
                90,
                110,
                20,
                "usd_micro",
            ),
            metric(
                "fixed_flow.no_hlp.safety.bad_debt",
                MetricFamily::Safety,
                10,
                8,
                -2,
                "usd_micro",
            ),
        ];
        let report_metric_ids = metrics
            .iter()
            .map(|metric| metric.metric_id.clone())
            .collect();
        ResultManifestV1 {
            schema_version: METRICS_SCHEMA_VERSION,
            run_id: "run".to_owned(),
            dataset_id: "0".repeat(64),
            profile_freeze_sha256: "a".repeat(64),
            dusk_source_sha256: "b".repeat(64),
            cutoff_slot: 99,
            scope: audit_grade_scope(),
            common_window_start_unix_seconds: 0,
            common_window_end_unix_seconds_exclusive: 1_000,
            holdout_start_unix_seconds: 100,
            holdout_end_unix_seconds_exclusive: 900,
            holdout_evidence_sha256: "c".repeat(64),
            universe_window_sha256: "d".repeat(64),
            source_artifact_sha256: BTreeMap::from([(
                "canonical-operations.jsonl".to_owned(),
                "1".repeat(64),
            )]),
            coverage: HeadlineCoverageV1 {
                visible_swap_volume: CoverageMetricV1::from_raw(100, 100, 9_000).unwrap(),
                borrowed_notional: CoverageMetricV1::from_raw(100, 100, 9_000).unwrap(),
                revenue_notional: CoverageMetricV1::from_raw(100, 100, 9_000).unwrap(),
                exact_transaction_fetch_bps: 10_000,
                exact_decode_bps: 10_000,
                v1_zero_difference_reconciled: true,
            },
            headline_hlp_scenario: HlpScenario::MaximumFeasible,
            no_hlp_control_included: true,
            global_winner_profile_id: (!no_winner).then(|| "profile".to_owned()),
            no_global_winner: no_winner,
            pareto_profile_ids: vec!["profile-a".to_owned(), "profile-b".to_owned()],
            per_market_calibration_qualification_sha256: "e".repeat(64),
            per_market_holdout_qualification_sha256: "f".repeat(64),
            per_market_opportunity_ceilings: Vec::new(),
            sensitivity_evidence: Vec::new(),
            markets: Vec::new(),
            aggregate_metrics: metrics,
            bootstrap_intervals: Vec::new(),
            conservation_identities: Vec::new(),
            accounting_verification: AccountingVerificationV1 {
                v1_conservation_checks: 1,
                v1_conservation_failures: 0,
                dusk_atomic_transition_checks: 1,
                dusk_atomic_transition_failures: 0,
                paired_revenue_lane_checks: 1,
                paired_revenue_lane_failures: 0,
            },
            limitations: vec!["The counterfactual depends on frozen replay assumptions.".to_owned()],
            report_metric_ids,
            manifest_sha256: String::new(),
        }
        .seal()
        .unwrap()
    }

    fn audit_grade_scope() -> ResultScopeV1 {
        ResultScopeV1::new(
            ScopeMode::FullUniverse,
            EvidenceTier::AuditGrade,
            BTreeSet::from(["market".to_owned()]),
            None,
            None,
            "9".repeat(64),
            PhaseGateDecisionV1 {
                phase: HardGatePhase::PostReplayFinal,
                evidence_tier: GateEvidenceTier::AuditGrade,
                phase_eligible: true,
                raw_market_eligible: true,
                usd_headline_eligible: true,
                raw_certification: CertificationStatus::Pass,
                usd_certification: CertificationStatus::Pass,
                raw_blocking_gates: Vec::new(),
                usd_blocking_gates: Vec::new(),
                limitations: Vec::new(),
            },
        )
        .unwrap()
    }

    fn hybrid_fixture(no_winner: bool) -> ResultManifestV1 {
        let mut result = fixture_manifest(no_winner);
        let cohort_sha256 = "7".repeat(64);
        result.scope = ResultScopeV1::new(
            ScopeMode::HybridPanel,
            EvidenceTier::DecisionGrade,
            BTreeSet::from(["market".to_owned()]),
            Some(cohort_sha256.clone()),
            Some(CohortRepresentativenessEvidenceV1 {
                artifact_sha256: "8".repeat(64),
                cohort_selection_sha256: cohort_sha256,
                swaps: ScopeActivityCoverageV1 {
                    visible_universe_rows: 100,
                    selected_panel_rows: 80,
                    selected_panel_coverage_bps: 8_000,
                },
                debt_actions: ScopeActivityCoverageV1 {
                    visible_universe_rows: 40,
                    selected_panel_rows: 20,
                    selected_panel_coverage_bps: 5_000,
                },
                liquidations: ScopeActivityCoverageV1 {
                    visible_universe_rows: 4,
                    selected_panel_rows: 3,
                    selected_panel_coverage_bps: 7_500,
                },
            }),
            "9".repeat(64),
            PhaseGateDecisionV1 {
                phase: HardGatePhase::PostReplayFinal,
                evidence_tier: GateEvidenceTier::DecisionGrade,
                phase_eligible: true,
                raw_market_eligible: true,
                usd_headline_eligible: true,
                raw_certification: CertificationStatus::Limited,
                usd_certification: CertificationStatus::Limited,
                raw_blocking_gates: Vec::new(),
                usd_blocking_gates: Vec::new(),
                limitations: vec![
                    GateLimitationV1 {
                        gate: HardGateId::DecoderRevisionCertification,
                        underlying_status: GateStatus::Fail,
                        detail_code: "decoder_revision_certification_pending".to_owned(),
                    },
                    GateLimitationV1 {
                        gate: HardGateId::TerminalState,
                        underlying_status: GateStatus::Fail,
                        detail_code: "terminal_state_snapshot_unavailable".to_owned(),
                    },
                ],
            },
        )
        .unwrap();
        result.seal().unwrap()
    }

    fn metric(
        id: &str,
        family: MetricFamily,
        v1: i128,
        dusk: i128,
        delta: i128,
        unit: &str,
    ) -> NamedMetricV1 {
        NamedMetricV1 {
            metric_id: id.to_owned(),
            family,
            profile_id: Some("profile".to_owned()),
            replay_arm: Some(ReplayArm::FixedFlow),
            hlp_scenario: Some(HlpScenario::NoHlp),
            definition: format!("Exact test definition for {id}."),
            revenue_dimensions: None,
            v1: exact(v1, unit),
            dusk: exact(dusk, unit),
            paired_delta: exact(delta, unit),
        }
    }

    fn exact(value: i128, unit: &str) -> MetricValueV1 {
        MetricValueV1::exact_components(
            SignedTokenAmount::new(value.to_string()).unwrap(),
            TokenAmount::new("3").unwrap(),
            unit,
            Some("fixture denominator".to_owned()),
        )
        .unwrap()
    }

    fn temp_directory(label: &str) -> PathBuf {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "v1-vs-dusk-report-{label}-{}-{sequence}",
            std::process::id()
        ))
    }
}
