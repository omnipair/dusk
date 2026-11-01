//! Construction of the single immutable machine-readable results manifest.
//!
//! This module consumes only committed holdout ledgers and exact aggregation
//! evidence. It never opens calibration inputs and never rounds through a
//! binary floating-point representation.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{BufRead, BufReader, Read},
    path::Path,
    str::FromStr,
};

use anyhow::{bail, Context, Result};
use num_bigint::{BigInt, BigUint, Sign};
use num_rational::BigRational;
use num_traits::{ToPrimitive, Zero};
use sha2::{Digest, Sha256};

use crate::{
    aggregate::{
        AvailableExactRationalV1, BorrowProjectedTotalDebtAggregateV1, CandidateScoreEvidenceV1,
        DuskBorrowCapacityAggregateV1, ExactRationalV1, RevenueReturnComponentV1,
        RevenueReturnEvidenceV1, RevenueYieldEvidenceV1, SpotProbeAggregateCellV1,
        AGGREGATE_SCHEMA_VERSION,
    },
    canonical::{SignedTokenAmount, TokenAmount},
    counterfactual::DuskBorrowCapacityConstraintV1,
    leverage::{
        ExactRatioV1, LeverageBenchmarkResultV1, LeverageDirectionV1, LiquidationThresholdKindV1,
    },
    leverage_campaign::{
        LeverageCampaignArtifactV1, LeverageCampaignCellV1, LeverageCheckpointKindV1,
    },
    metric_engine::{
        revenue_origin_from_flags, PairedMetricLedgerSummaryV1, PairedOperationMetricRowV1,
        RevenueLaneV1, RevenueOriginV1, RevenueSideDeltaV1, RevenueVenueV1,
    },
    metrics::{
        AccountingVerificationV1, BootstrapIntervalV1, CoverageMetricV1, HeadlineCoverageV1,
        MarketMetricSetV1, MarketOpportunityCeilingV1, MetricAvailability, MetricFamily,
        MetricValueV1, NamedMetricV1, ResultManifestV1, ResultScopeV1, RevenueMetricDimensionsV1,
        SensitivityEvidenceSummaryV1, METRICS_SCHEMA_VERSION,
    },
    replay::{HlpScenario, ReplayArm},
    spot_probe::{SpotProbeDirectionV1, SpotProbeSizeKindV1},
    universe::UniverseWindowArtifactV1,
    v1_reconciliation::V1ReconciliationSummaryV1,
};

pub struct HoldoutMetricInputV1<'a> {
    pub evidence: &'a CandidateScoreEvidenceV1,
    pub paired_ledger_path: &'a Path,
    pub paired_summary: &'a PairedMetricLedgerSummaryV1,
    pub hlp_scenario: HlpScenario,
    /// `None` identifies a global-profile replay. A singleton market identifies
    /// a separately labeled opportunity-ceiling replay and is excluded from
    /// collective aggregate rows.
    pub opportunity_ceiling_market: Option<&'a str>,
}

#[allow(clippy::too_many_arguments)]
pub fn build_result_manifest(
    run_id: &str,
    dataset_id: &str,
    profile_freeze_sha256: &str,
    dusk_source_sha256: &str,
    cutoff_slot: u64,
    universe: &UniverseWindowArtifactV1,
    scope: ResultScopeV1,
    inputs: &[HoldoutMetricInputV1<'_>],
    visible_denominator: &CandidateScoreEvidenceV1,
    v1_reconciliation: &V1ReconciliationSummaryV1,
    exact_transaction_fetch_bps: u16,
    exact_decode_bps: u16,
    swap_coverage_required_bps: u16,
    lending_coverage_required_bps: u16,
    headline_hlp_scenario: HlpScenario,
    global_winner_profile_id: Option<String>,
    pareto_profile_ids: Vec<String>,
    per_market_calibration_qualification_sha256: String,
    per_market_holdout_qualification_sha256: String,
    per_market_opportunity_ceilings: Vec<MarketOpportunityCeilingV1>,
    sensitivity_evidence: Vec<SensitivityEvidenceSummaryV1>,
    holdout_evidence_sha256: String,
    universe_window_sha256: String,
    source_artifact_sha256: BTreeMap<String, String>,
    leverage_campaigns: &[LeverageCampaignArtifactV1],
    bootstrap_intervals: Vec<BootstrapIntervalV1>,
    limitations: Vec<String>,
) -> Result<ResultManifestV1> {
    if inputs.is_empty() {
        bail!("result manifest requires at least one sealed holdout evidence set");
    }
    scope.validate()?;
    let common = universe
        .common_window
        .as_ref()
        .context("result manifest has no collective common window")?;
    let splits = universe
        .chronological_splits
        .as_ref()
        .context("result manifest has no chronological splits")?;
    let raw_eligible: BTreeSet<&str> = universe
        .market_eligibility
        .iter()
        .filter(|market| market.eligible)
        .map(|market| market.pair_address.as_str())
        .collect();

    let factual_reference = inputs
        .iter()
        .find(|input| input.opportunity_ceiling_market.is_none())
        .context("result manifest has no global-profile holdout evidence")?
        .evidence;
    let scoped_market_count = u64::try_from(scope.selected_markets.len())?;
    for input in inputs {
        validate_holdout_input(
            input,
            splits.holdout_start_unix_seconds,
            splits.holdout_end_unix_seconds_exclusive,
        )?;
        // Factual V1 quantities may not vary with Dusk policy, replay arm, or
        // hLP attribution scenario.
        if let Some(market) = input.opportunity_ceiling_market {
            if input.evidence.markets.len() != 1 || input.evidence.markets[0].market != market {
                bail!("opportunity-ceiling evidence is not scoped to exactly one market");
            }
        } else {
            if input.evidence.included_markets_sha256 != scope.selected_market_set_sha256
                || input.evidence.included_market_count != scoped_market_count
            {
                bail!("shared-profile holdout evidence does not bind to the result scope");
            }
            if input.evidence.stakeholder_revenue_v1_usd_micro
                != factual_reference.stakeholder_revenue_v1_usd_micro
                || input.evidence.factual_swap_input_notional_usd_micro
                    != factual_reference.factual_swap_input_notional_usd_micro
                || input.evidence.v1_debt_increase_notional_usd_micro
                    != factual_reference.v1_debt_increase_notional_usd_micro
            {
                bail!("factual V1 holdout totals changed across shared Dusk evidence sets");
            }
        }
    }

    if sensitivity_evidence
        .iter()
        .any(|evidence| evidence.included_markets_sha256 != scope.selected_market_set_sha256)
    {
        bail!("sensitivity evidence does not bind to the result scope");
    }

    let terminal_state_is_typed_limitation = scope
        .final_phase_gate_decision
        .limitations
        .iter()
        .any(|limitation| limitation.gate == crate::audit::HardGateId::TerminalState);
    if terminal_state_is_typed_limitation == v1_reconciliation.terminal_state_verified {
        bail!("terminal-state reconciliation and final-gate limitation disagree");
    }

    let coverage = HeadlineCoverageV1 {
        visible_swap_volume: exact_coverage(
            &factual_reference.factual_swap_input_notional_usd_micro,
            &visible_denominator.factual_swap_input_notional_usd_micro,
            swap_coverage_required_bps,
        )?,
        borrowed_notional: exact_coverage(
            &factual_reference.v1_debt_increase_notional_usd_micro,
            &visible_denominator.v1_debt_increase_notional_usd_micro,
            lending_coverage_required_bps,
        )?,
        revenue_notional: exact_coverage(
            available_exact(&factual_reference.stakeholder_revenue_v1_usd_micro)?,
            available_exact(&visible_denominator.stakeholder_revenue_v1_usd_micro)?,
            lending_coverage_required_bps,
        )?,
        exact_transaction_fetch_bps,
        exact_decode_bps,
        v1_zero_difference_reconciled: v1_reconciliation.zero_raw_unit_difference
            && v1_reconciliation.all_conservation_identities_hold
            && (v1_reconciliation.terminal_state_verified || terminal_state_is_typed_limitation),
    };
    if !coverage.passes() {
        bail!("collective headline coverage is below the frozen threshold");
    }

    let mut market_sets = BTreeMap::<String, MarketMetricSetV1>::new();
    let mut aggregate_metrics = Vec::new();
    let mut checked_ledgers = BTreeMap::<String, u64>::new();
    for input in inputs {
        if input.opportunity_ceiling_market.is_none() {
            aggregate_metrics.extend(aggregate_metrics_for(input)?);
        }
        if let Some(previous) = checked_ledgers.insert(
            input.paired_summary.ledger_sha256.clone(),
            input.paired_summary.row_count,
        ) {
            if previous != input.paired_summary.row_count {
                bail!("one paired-ledger hash is associated with two row counts");
            }
        }
        let raw = aggregate_raw_market_metrics(
            input,
            splits.holdout_start_unix_seconds,
            splits.holdout_end_unix_seconds_exclusive,
            &raw_eligible,
        )?;
        merge_market_metrics(&mut market_sets, raw)?;
        merge_market_metrics(&mut market_sets, usd_market_metrics_for(input)?)?;
    }
    let mut markets: Vec<MarketMetricSetV1> = market_sets.into_values().collect();
    let expected_campaign_profiles: BTreeSet<_> = inputs
        .iter()
        .filter(|input| input.opportunity_ceiling_market.is_none())
        .map(|input| input.evidence.profile_id.as_str())
        .collect();
    append_leverage_campaign_metrics(
        leverage_campaigns,
        profile_freeze_sha256,
        cutoff_slot,
        splits.holdout_start_unix_seconds,
        splits.holdout_end_unix_seconds_exclusive,
        &expected_campaign_profiles,
        &mut aggregate_metrics,
        &mut markets,
    )?;
    let paired_rows_checked = checked_ledgers.values().try_fold(0_u64, |total, rows| {
        total
            .checked_add(*rows)
            .context("paired accounting check count overflow")
    })?;
    let report_metric_ids = aggregate_metrics
        .iter()
        .chain(markets.iter().flat_map(|market| market.raw_metrics.iter()))
        .chain(markets.iter().flat_map(|market| market.usd_metrics.iter()))
        .map(|metric| metric.metric_id.clone())
        .collect();

    ResultManifestV1 {
        schema_version: METRICS_SCHEMA_VERSION,
        run_id: run_id.to_owned(),
        dataset_id: dataset_id.to_owned(),
        profile_freeze_sha256: profile_freeze_sha256.to_owned(),
        dusk_source_sha256: dusk_source_sha256.to_owned(),
        cutoff_slot,
        scope,
        common_window_start_unix_seconds: common.start_unix_seconds,
        common_window_end_unix_seconds_exclusive: common.end_unix_seconds_exclusive,
        holdout_start_unix_seconds: splits.holdout_start_unix_seconds,
        holdout_end_unix_seconds_exclusive: splits.holdout_end_unix_seconds_exclusive,
        holdout_evidence_sha256,
        universe_window_sha256,
        source_artifact_sha256,
        coverage,
        headline_hlp_scenario,
        no_hlp_control_included: inputs.iter().any(|input| {
            input.opportunity_ceiling_market.is_none() && input.hlp_scenario == HlpScenario::NoHlp
        }),
        no_global_winner: global_winner_profile_id.is_none(),
        global_winner_profile_id,
        pareto_profile_ids,
        per_market_calibration_qualification_sha256,
        per_market_holdout_qualification_sha256,
        per_market_opportunity_ceilings,
        sensitivity_evidence,
        markets,
        aggregate_metrics,
        bootstrap_intervals,
        conservation_identities: Vec::new(),
        accounting_verification: AccountingVerificationV1 {
            v1_conservation_checks: v1_reconciliation.conservation_check_count,
            v1_conservation_failures: v1_reconciliation.conservation_failure_count,
            dusk_atomic_transition_checks: paired_rows_checked,
            dusk_atomic_transition_failures: 0,
            paired_revenue_lane_checks: paired_rows_checked,
            paired_revenue_lane_failures: 0,
        },
        limitations,
        report_metric_ids,
        manifest_sha256: String::new(),
    }
    .seal()
    .map_err(Into::into)
}

fn validate_holdout_input(input: &HoldoutMetricInputV1<'_>, start: i64, end: i64) -> Result<()> {
    if input.evidence.schema_version != AGGREGATE_SCHEMA_VERSION
        || input.evidence.profile_id != input.paired_summary.profile_id
        || input.evidence.paired_metric_ledger_sha256 != input.paired_summary.ledger_sha256
        || input.evidence.hlp_scenario != input.hlp_scenario
        || input.paired_summary.hlp_scenario != input.hlp_scenario
        || input.evidence.window_start_unix_seconds != start
        || input.evidence.window_end_unix_seconds_exclusive != end
        || input.evidence.blocking_reasons.len() != 0
        || input.evidence.candidate_score.is_none()
        || sha256_file(input.paired_ledger_path)? != input.paired_summary.ledger_sha256
    {
        bail!("sealed holdout evidence is incomplete or has the wrong identity");
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex::encode(digest.finalize()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LeverageMetricKind {
    TheoreticalLeverage,
    ExecutableLeverage,
    OpeningCost,
    BorrowLiquidityLimit,
    FundingApr,
    InitialBuffer,
    MaintenanceBuffer,
    UnwindImpact,
    UnwindCost,
    RealizedAdverseExcursion,
}

const LEVERAGE_METRIC_KINDS: [LeverageMetricKind; 10] = [
    LeverageMetricKind::TheoreticalLeverage,
    LeverageMetricKind::ExecutableLeverage,
    LeverageMetricKind::OpeningCost,
    LeverageMetricKind::BorrowLiquidityLimit,
    LeverageMetricKind::FundingApr,
    LeverageMetricKind::InitialBuffer,
    LeverageMetricKind::MaintenanceBuffer,
    LeverageMetricKind::UnwindImpact,
    LeverageMetricKind::UnwindCost,
    LeverageMetricKind::RealizedAdverseExcursion,
];

impl LeverageMetricKind {
    const fn cell_suffix(self) -> &'static str {
        match self {
            Self::TheoreticalLeverage => "theoretical_leverage_ratio",
            Self::ExecutableLeverage => "executable_leverage_ratio",
            Self::OpeningCost => "opening.cost_debt_atoms",
            Self::BorrowLiquidityLimit => "opening.borrow_liquidity_debt_atoms",
            Self::FundingApr => "funding.instantaneous_borrow_apr_nad",
            Self::InitialBuffer => "liquidation.initial_buffer_bps",
            Self::MaintenanceBuffer => "liquidation.maintenance_buffer_bps",
            Self::UnwindImpact => "unwind.impact_bps",
            Self::UnwindCost => "unwind.cost_vs_starting_margin_debt_atoms",
            Self::RealizedAdverseExcursion => "path.realized_adverse_excursion_bps",
        }
    }

    const fn aggregate_suffix(self) -> &'static str {
        match self {
            Self::OpeningCost => "opening.cost_per_starting_margin",
            Self::BorrowLiquidityLimit => "opening.borrow_liquidity_per_factual_pool_nav",
            Self::UnwindCost => "unwind.cost_per_starting_margin",
            _ => self.cell_suffix(),
        }
    }

    const fn common_unit(self) -> Option<&'static str> {
        match self {
            Self::TheoreticalLeverage | Self::ExecutableLeverage => Some("leverage_multiple"),
            Self::FundingApr => Some("apr_nad"),
            Self::InitialBuffer
            | Self::MaintenanceBuffer
            | Self::UnwindImpact
            | Self::RealizedAdverseExcursion => Some("bps"),
            Self::OpeningCost | Self::BorrowLiquidityLimit | Self::UnwindCost => None,
        }
    }

    const fn aggregate_unit(self) -> &'static str {
        match self {
            Self::OpeningCost | Self::UnwindCost => "fraction_of_starting_gross_margin",
            Self::BorrowLiquidityLimit => "fraction_of_factual_accounted_pool_nav",
            _ => self
                .common_unit()
                .expect("every non-token leverage metric has a common unit"),
        }
    }

    const fn definition(self) -> &'static str {
        match self {
            Self::TheoreticalLeverage => {
                "Protocol-theoretical gross exposure divided by starting net margin. A genuinely unbounded native ratio is N/A, never a numeric sentinel."
            }
            Self::ExecutableLeverage => {
                "Native executable gross exposure divided by starting net margin at the exact pre-operation checkpoint."
            }
            Self::OpeningCost => {
                "Exact native opening cost in the direction's debt-token atoms."
            }
            Self::BorrowLiquidityLimit => {
                "Exact native borrow-liquidity field. Dusk reports pre-open cash while V1 reports post-loop remaining cash, so the paired delta is an accounting difference and not a standalone liquidity-advantage claim."
            }
            Self::FundingApr => "Exact instantaneous native borrower APR in NAD fixed-point units.",
            Self::InitialBuffer => {
                "Exact equity margin minus the venue's native initial-margin requirement, in basis points."
            }
            Self::MaintenanceBuffer => {
                "Exact equity margin minus the venue's native maintenance-margin requirement, in basis points."
            }
            Self::UnwindImpact => {
                "Exact native full-unwind price impact in basis points at the checkpoint."
            }
            Self::UnwindCost => {
                "Exact native full-unwind cost versus starting margin in debt-token atoms."
            }
            Self::RealizedAdverseExcursion => {
                "Realized maximum adverse excursion sourced from later factual V1 marginal prices through holdout end."
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn append_leverage_campaign_metrics(
    campaigns: &[LeverageCampaignArtifactV1],
    expected_profile_freeze_sha256: &str,
    expected_cutoff_slot: u64,
    expected_holdout_start: i64,
    expected_holdout_end: i64,
    expected_profiles: &BTreeSet<&str>,
    aggregate_metrics: &mut Vec<NamedMetricV1>,
    markets: &mut Vec<MarketMetricSetV1>,
) -> Result<()> {
    let mut metric_ids: BTreeSet<String> = aggregate_metrics
        .iter()
        .chain(markets.iter().flat_map(|market| market.raw_metrics.iter()))
        .chain(markets.iter().flat_map(|market| market.usd_metrics.iter()))
        .map(|metric| metric.metric_id.clone())
        .collect();
    let mut campaign_profiles = BTreeSet::new();
    let mut campaign_cell_ids = BTreeSet::new();

    for campaign in campaigns {
        campaign.verify().context("verifying leverage campaign")?;
        if campaign.spec.profile_freeze_sha256 != expected_profile_freeze_sha256
            || campaign.spec.cutoff_slot != expected_cutoff_slot
            || campaign.spec.holdout_start_unix_seconds != expected_holdout_start
            || campaign.spec.holdout_end_unix_seconds_exclusive != expected_holdout_end
            || !expected_profiles.contains(campaign.spec.profile_id.as_str())
        {
            bail!("leverage campaign does not match the sealed result-manifest identity");
        }
        if !campaign_profiles.insert(campaign.spec.profile_id.as_str()) {
            bail!("duplicate leverage campaign profile ID");
        }
        validate_leverage_campaign_partition(campaign)?;
        for cell in &campaign.cells {
            if !campaign_cell_ids.insert(cell.cell_id.as_str()) {
                bail!("duplicate leverage campaign cell ID across artifacts");
            }
        }
    }
    if campaign_profiles != *expected_profiles {
        bail!("leverage campaigns do not exactly cover the sealed global-profile set");
    }

    for campaign in campaigns {
        for cell in &campaign.cells {
            let market_index = markets
                .iter()
                .position(|market| market.market == cell.market)
                .with_context(|| {
                    format!(
                        "leverage lending market {} is absent from paired market metrics",
                        cell.market
                    )
                })?;
            let market = &mut markets[market_index];
            if market.token0.is_empty() || market.token1.is_empty() {
                bail!("leverage cell debt-token mint identity is unavailable");
            }
            market.has_lending_activity = true;
            validate_leverage_cell_economics(cell)?;
            for kind in LEVERAGE_METRIC_KINDS {
                let metric = leverage_cell_metric(cell, market, kind)?;
                push_unique_metric(&mut metric_ids, &mut market.raw_metrics, metric)?;
            }
            let unbounded_metrics = leverage_unbounded_metrics(cell)?;
            for metric in unbounded_metrics {
                push_unique_metric(&mut metric_ids, &mut market.raw_metrics, metric)?;
            }
            let threshold_metrics = leverage_threshold_metrics(cell, market)?;
            for metric in threshold_metrics {
                push_unique_metric(&mut metric_ids, &mut market.raw_metrics, metric)?;
            }
        }

        for market_name in &campaign.non_lending_usd_headline_markets {
            let market_index = match markets
                .iter()
                .position(|market| market.market == *market_name)
            {
                Some(index) => index,
                None => {
                    markets.push(MarketMetricSetV1 {
                        market: market_name.clone(),
                        token0: String::new(),
                        token1: String::new(),
                        has_lending_activity: false,
                        raw_metrics: Vec::new(),
                        usd_metrics: Vec::new(),
                    });
                    markets.len() - 1
                }
            };
            let market = &mut markets[market_index];
            if market.has_lending_activity {
                bail!("leverage campaign labels an active lending market as non-lending");
            }
            for arm in [ReplayArm::FixedFlow, ReplayArm::Intent] {
                for scenario in [HlpScenario::NoHlp, HlpScenario::MaximumFeasible] {
                    for metric in leverage_non_lending_metrics(
                        market_name,
                        &campaign.spec.profile_id,
                        arm,
                        scenario,
                    ) {
                        push_unique_metric(&mut metric_ids, &mut market.raw_metrics, metric)?;
                    }
                }
            }
        }

        for arm in [ReplayArm::FixedFlow, ReplayArm::Intent] {
            for scenario in [HlpScenario::NoHlp, HlpScenario::MaximumFeasible] {
                for kind in LEVERAGE_METRIC_KINDS {
                    push_unique_metric(
                        &mut metric_ids,
                        aggregate_metrics,
                        leverage_equal_market_metric(campaign, arm, scenario, kind)?,
                    )?;
                }
            }
        }
    }
    markets.sort_by(|left, right| left.market.cmp(&right.market));
    Ok(())
}

fn validate_leverage_campaign_partition(campaign: &LeverageCampaignArtifactV1) -> Result<()> {
    let lending: BTreeSet<_> = campaign
        .factual_lending_markets
        .iter()
        .map(String::as_str)
        .collect();
    let non_lending: BTreeSet<_> = campaign
        .non_lending_usd_headline_markets
        .iter()
        .map(String::as_str)
        .collect();
    if lending.len() != campaign.factual_lending_markets.len()
        || non_lending.len() != campaign.non_lending_usd_headline_markets.len()
        || !lending.is_disjoint(&non_lending)
        || lending
            .union(&non_lending)
            .copied()
            .collect::<BTreeSet<_>>()
            != campaign
                .spec
                .usd_headline_markets
                .iter()
                .map(String::as_str)
                .collect()
    {
        bail!("invalid leverage campaign lending/non-lending market partition");
    }
    if campaign.cells.iter().any(|cell| {
        cell.profile_id != campaign.spec.profile_id || !lending.contains(cell.market.as_str())
    }) {
        bail!("leverage campaign cell is outside its profile or lending-market partition");
    }
    Ok(())
}

fn validate_leverage_cell_economics(cell: &LeverageCampaignCellV1) -> Result<()> {
    let Some(benchmark) = &cell.benchmark else {
        return Ok(());
    };
    if benchmark.v1.liquidation.threshold_kind != LiquidationThresholdKindV1::CollateralPriceNad
        || benchmark.dusk.liquidation.threshold_kind != LiquidationThresholdKindV1::CloseoutValue
        || benchmark.dusk.liquidation.adverse_path_availability
            != cell.dusk_adverse_path_availability
    {
        bail!("leverage campaign has mismatched native liquidation semantics");
    }
    let v1_realized = (
        benchmark
            .v1
            .liquidation
            .realized_max_adverse_excursion_bps
            .as_ref(),
        benchmark.v1.liquidation.realized_mae_source_sha256.as_ref(),
    );
    let dusk_realized = (
        benchmark
            .dusk
            .liquidation
            .realized_max_adverse_excursion_bps
            .as_ref(),
        benchmark
            .dusk
            .liquidation
            .realized_mae_source_sha256
            .as_ref(),
    );
    match (&cell.realized_adverse_excursion, v1_realized, dusk_realized) {
        (None, (None, None), (None, None)) => {}
        (Some(source), (Some(v1), Some(v1_sha)), (Some(dusk), Some(dusk_sha)))
            if v1 == dusk
                && v1 == &source.adverse_excursion_bps
                && v1_sha == dusk_sha
                && v1_sha == &source.source_sha256 => {}
        _ => bail!("leverage campaign realized adverse-excursion evidence disagrees"),
    }
    Ok(())
}

fn leverage_cell_metric(
    cell: &LeverageCampaignCellV1,
    market: &MarketMetricSetV1,
    kind: LeverageMetricKind,
) -> Result<NamedMetricV1> {
    let unit = kind
        .common_unit()
        .map(str::to_owned)
        .unwrap_or_else(|| leverage_debt_unit(market, cell.direction));
    let (v1, dusk) = cell
        .benchmark
        .as_ref()
        .map(|benchmark| leverage_metric_pair(benchmark, kind))
        .transpose()?
        .unwrap_or((None, None));
    Ok(leverage_named_metric(
        leverage_cell_metric_id(cell, kind.cell_suffix()),
        &cell.profile_id,
        cell.arm,
        cell.hlp_scenario,
        kind.definition(),
        v1.as_ref(),
        dusk.as_ref(),
        &unit,
        MetricAvailability::NotEvaluated,
    )?)
}

fn leverage_threshold_metrics(
    cell: &LeverageCampaignCellV1,
    market: &MarketMetricSetV1,
) -> Result<[NamedMetricV1; 2]> {
    let (v1, dusk) = match &cell.benchmark {
        Some(benchmark) => (
            Some(token_rational(&benchmark.v1.liquidation.threshold)?),
            Some(token_rational(&benchmark.dusk.liquidation.threshold)?),
        ),
        None => (None, None),
    };
    let v1_unit = leverage_v1_price_unit(market, cell.direction);
    let dusk_unit = leverage_debt_unit(market, cell.direction);
    Ok([
        leverage_venue_only_metric(
            leverage_cell_metric_id(cell, "liquidation.v1_collateral_price_threshold_nad"),
            &cell.profile_id,
            cell.arm,
            cell.hlp_scenario,
            "V1-only native collateral liquidation price in debt-atoms per collateral-atom, NAD-scaled. It is not subtracted from Dusk's closeout-value threshold.",
            v1.as_ref(),
            true,
            &v1_unit,
            MetricAvailability::NotEvaluated,
        )?,
        leverage_venue_only_metric(
            leverage_cell_metric_id(cell, "liquidation.dusk_minimum_closeout_value_debt_atoms"),
            &cell.profile_id,
            cell.arm,
            cell.hlp_scenario,
            "Dusk-only native minimum healthy closeout value in debt-token atoms. It is not subtracted from V1's price threshold.",
            dusk.as_ref(),
            false,
            &dusk_unit,
            MetricAvailability::NotEvaluated,
        )?,
    ])
}

fn leverage_non_lending_metrics(
    market: &str,
    profile_id: &str,
    arm: ReplayArm,
    scenario: HlpScenario,
) -> Vec<NamedMetricV1> {
    let availability = MetricAvailability::NotApplicableNoLendingActivity;
    let definition =
        "No factual lending activity occurred in this market/window; leverage is N/A, not economic zero.";
    let mut metrics = LEVERAGE_METRIC_KINDS
        .iter()
        .copied()
        .map(|kind| {
            let unit = kind.common_unit().unwrap_or("debt_token_atoms");
            leverage_unavailable_named_metric(
                leverage_non_lending_metric_id(
                    market,
                    profile_id,
                    arm,
                    scenario,
                    kind.cell_suffix(),
                ),
                profile_id,
                arm,
                scenario,
                definition,
                unit,
                availability.clone(),
            )
        })
        .collect::<Vec<_>>();
    for suffix in [
        "theoretical_leverage_ratio.unbounded",
        "executable_leverage_ratio.unbounded",
    ] {
        metrics.push(leverage_unavailable_named_metric(
            leverage_non_lending_metric_id(market, profile_id, arm, scenario, suffix),
            profile_id,
            arm,
            scenario,
            definition,
            "boolean_0_or_1",
            availability.clone(),
        ));
    }
    for (suffix, unit) in [
        (
            "liquidation.v1_collateral_price_threshold_nad",
            "debt_atoms_per_collateral_atom_nad",
        ),
        (
            "liquidation.dusk_minimum_closeout_value_debt_atoms",
            "debt_token_atoms",
        ),
    ] {
        metrics.push(leverage_unavailable_named_metric(
            leverage_non_lending_metric_id(market, profile_id, arm, scenario, suffix),
            profile_id,
            arm,
            scenario,
            definition,
            unit,
            availability.clone(),
        ));
    }
    metrics
}

fn leverage_unbounded_metrics(cell: &LeverageCampaignCellV1) -> Result<[NamedMetricV1; 2]> {
    let ratio_metric = |suffix: &str,
                        definition: &str,
                        ratios: Option<(&ExactRatioV1, &ExactRatioV1)>|
     -> Result<NamedMetricV1> {
        let (v1, dusk) = match ratios {
            Some((v1, dusk)) => {
                // This also rejects a bounded zero denominator or an unbounded
                // nonzero denominator before exposing the flags.
                exact_leverage_ratio(v1)?;
                exact_leverage_ratio(dusk)?;
                (
                    Some(BigRational::from_integer(BigInt::from(u8::from(
                        v1.unbounded,
                    )))),
                    Some(BigRational::from_integer(BigInt::from(u8::from(
                        dusk.unbounded,
                    )))),
                )
            }
            None => (None, None),
        };
        leverage_named_metric(
            leverage_cell_metric_id(cell, suffix),
            &cell.profile_id,
            cell.arm,
            cell.hlp_scenario,
            definition,
            v1.as_ref(),
            dusk.as_ref(),
            "boolean_0_or_1",
            MetricAvailability::NotEvaluated,
        )
    };
    let benchmark = cell.benchmark.as_ref();
    Ok([
        ratio_metric(
            "theoretical_leverage_ratio.unbounded",
            "Exact native flag: one means theoretical leverage is genuinely unbounded and its ratio metric is N/A; zero means bounded. This is never encoded as a zero-denominator numeric sentinel.",
            benchmark.map(|value| {
                (
                    &value.v1.theoretical_leverage,
                    &value.dusk.theoretical_leverage,
                )
            }),
        )?,
        ratio_metric(
            "executable_leverage_ratio.unbounded",
            "Exact native flag: one means executable leverage is genuinely unbounded and its ratio metric is N/A; zero means bounded. This is never encoded as a zero-denominator numeric sentinel.",
            benchmark.map(|value| {
                (
                    &value.v1.executable_leverage,
                    &value.dusk.executable_leverage,
                )
            }),
        )?,
    ])
}

fn leverage_equal_market_metric(
    campaign: &LeverageCampaignArtifactV1,
    arm: ReplayArm,
    scenario: HlpScenario,
    kind: LeverageMetricKind,
) -> Result<NamedMetricV1> {
    let mut v1_market_cells = Vec::new();
    let mut dusk_market_cells = Vec::new();
    for market in &campaign.factual_lending_markets {
        let mut v1_cells = Vec::new();
        let mut dusk_cells = Vec::new();
        for cell in campaign.cells.iter().filter(|cell| {
            cell.market == *market && cell.arm == arm && cell.hlp_scenario == scenario
        }) {
            let Some(benchmark) = &cell.benchmark else {
                continue;
            };
            let (Some(v1), Some(dusk)) = leverage_metric_pair(benchmark, kind)? else {
                continue;
            };
            let Some((v1, dusk)) = normalize_leverage_aggregate_pair(cell, kind, v1, dusk)? else {
                continue;
            };
            v1_cells.push(v1);
            dusk_cells.push(dusk);
        }
        if !v1_cells.is_empty() && v1_cells.len() == dusk_cells.len() {
            v1_market_cells.push(v1_cells);
            dusk_market_cells.push(dusk_cells);
        }
    }
    let v1 = hierarchical_lower_median(v1_market_cells);
    let dusk = hierarchical_lower_median(dusk_market_cells);
    let definition = format!(
        "Equal-market lower median: first the lower median across paired-available leverage cells within each lending market, then the lower median across markets. {}{}",
        kind.definition(),
        match kind {
            LeverageMetricKind::OpeningCost | LeverageMetricKind::UnwindCost =>
                " Cross-market values are divided by the exact cell starting gross margin so unlike debt mints are never combined.",
            LeverageMetricKind::BorrowLiquidityLimit =>
                " Cross-market values are divided by the exact factual accounted pool NAV in debt atoms so unlike debt mints are never combined.",
            _ => "",
        }
    );
    leverage_named_metric(
        leverage_aggregate_metric_id(
            &campaign.spec.profile_id,
            arm,
            scenario,
            kind.aggregate_suffix(),
        ),
        &campaign.spec.profile_id,
        arm,
        scenario,
        &definition,
        v1.as_ref(),
        dusk.as_ref(),
        kind.aggregate_unit(),
        MetricAvailability::NotEvaluated,
    )
}

fn leverage_metric_pair(
    benchmark: &LeverageBenchmarkResultV1,
    kind: LeverageMetricKind,
) -> Result<(Option<BigRational>, Option<BigRational>)> {
    let pair = match kind {
        LeverageMetricKind::TheoreticalLeverage => (
            exact_leverage_ratio(&benchmark.v1.theoretical_leverage)?,
            exact_leverage_ratio(&benchmark.dusk.theoretical_leverage)?,
        ),
        LeverageMetricKind::ExecutableLeverage => (
            exact_leverage_ratio(&benchmark.v1.executable_leverage)?,
            exact_leverage_ratio(&benchmark.dusk.executable_leverage)?,
        ),
        LeverageMetricKind::OpeningCost => (
            Some(signed_rational(&benchmark.v1.opening.opening_cost)?),
            Some(signed_rational(&benchmark.dusk.opening.opening_cost)?),
        ),
        LeverageMetricKind::BorrowLiquidityLimit => (
            Some(token_rational(
                &benchmark.v1.opening.limiting_borrow_liquidity,
            )?),
            Some(token_rational(
                &benchmark.dusk.opening.limiting_borrow_liquidity,
            )?),
        ),
        LeverageMetricKind::FundingApr => (
            Some(token_rational(
                &benchmark.v1.funding.instantaneous_borrow_apr_nad,
            )?),
            Some(token_rational(
                &benchmark.dusk.funding.instantaneous_borrow_apr_nad,
            )?),
        ),
        LeverageMetricKind::InitialBuffer => (
            Some(signed_rational(
                &benchmark.v1.liquidation.initial_buffer_bps,
            )?),
            Some(signed_rational(
                &benchmark.dusk.liquidation.initial_buffer_bps,
            )?),
        ),
        LeverageMetricKind::MaintenanceBuffer => (
            Some(signed_rational(
                &benchmark.v1.liquidation.maintenance_buffer_bps,
            )?),
            Some(signed_rational(
                &benchmark.dusk.liquidation.maintenance_buffer_bps,
            )?),
        ),
        LeverageMetricKind::UnwindImpact => (
            Some(token_rational(&benchmark.v1.unwind.unwind_impact_bps)?),
            Some(token_rational(&benchmark.dusk.unwind.unwind_impact_bps)?),
        ),
        LeverageMetricKind::UnwindCost => (
            Some(signed_rational(
                &benchmark.v1.unwind.cost_vs_starting_margin,
            )?),
            Some(signed_rational(
                &benchmark.dusk.unwind.cost_vs_starting_margin,
            )?),
        ),
        LeverageMetricKind::RealizedAdverseExcursion => (
            benchmark
                .v1
                .liquidation
                .realized_max_adverse_excursion_bps
                .as_ref()
                .map(token_rational)
                .transpose()?,
            benchmark
                .dusk
                .liquidation
                .realized_max_adverse_excursion_bps
                .as_ref()
                .map(token_rational)
                .transpose()?,
        ),
    };
    Ok(pair)
}

fn normalize_leverage_aggregate_pair(
    cell: &LeverageCampaignCellV1,
    kind: LeverageMetricKind,
    v1: BigRational,
    dusk: BigRational,
) -> Result<Option<(BigRational, BigRational)>> {
    let denominator = match kind {
        LeverageMetricKind::OpeningCost | LeverageMetricKind::UnwindCost => {
            cell.starting_margin_gross.as_ref()
        }
        LeverageMetricKind::BorrowLiquidityLimit => {
            cell.factual_accounted_pool_nav_in_debt_atoms.as_ref()
        }
        _ => return Ok(Some((v1, dusk))),
    };
    let Some(denominator) = denominator else {
        return Ok(None);
    };
    let denominator = token_rational(denominator)?;
    if denominator.is_zero() {
        return Ok(None);
    }
    Ok(Some((v1 / &denominator, dusk / denominator)))
}

fn exact_leverage_ratio(value: &ExactRatioV1) -> Result<Option<BigRational>> {
    let numerator = parse_token(&value.numerator)?;
    let denominator = parse_token(&value.denominator)?;
    if value.unbounded {
        if !denominator.is_zero() {
            bail!("unbounded leverage ratio has a nonzero denominator");
        }
        return Ok(None);
    }
    if denominator.is_zero() {
        bail!("bounded leverage ratio has a zero denominator");
    }
    Ok(Some(BigRational::new(
        BigInt::from_biguint(Sign::Plus, numerator),
        BigInt::from_biguint(Sign::Plus, denominator),
    )))
}

fn token_rational(value: &TokenAmount) -> Result<BigRational> {
    Ok(BigRational::from_integer(BigInt::from_biguint(
        Sign::Plus,
        parse_token(value)?,
    )))
}

fn signed_rational(value: &SignedTokenAmount) -> Result<BigRational> {
    Ok(BigRational::from_integer(
        BigInt::from_str(value.as_str()).context("invalid signed leverage amount")?,
    ))
}

fn lower_median_rational(values: &mut [BigRational]) -> Option<BigRational> {
    if values.is_empty() {
        return None;
    }
    values.sort();
    Some(values[(values.len() - 1) / 2].clone())
}

fn hierarchical_lower_median(
    market_values: impl IntoIterator<Item = Vec<BigRational>>,
) -> Option<BigRational> {
    let mut market_medians: Vec<_> = market_values
        .into_iter()
        .filter_map(|mut values| lower_median_rational(&mut values))
        .collect();
    lower_median_rational(&mut market_medians)
}

#[allow(clippy::too_many_arguments)]
fn leverage_named_metric(
    metric_id: String,
    profile_id: &str,
    arm: ReplayArm,
    scenario: HlpScenario,
    definition: &str,
    v1: Option<&BigRational>,
    dusk: Option<&BigRational>,
    unit: &str,
    missing_availability: MetricAvailability,
) -> Result<NamedMetricV1> {
    let delta = match (v1, dusk) {
        (Some(v1), Some(dusk)) => Some(dusk - v1),
        _ => None,
    };
    Ok(NamedMetricV1 {
        metric_id,
        family: MetricFamily::Leverage,
        profile_id: Some(profile_id.to_owned()),
        replay_arm: Some(arm),
        hlp_scenario: Some(scenario),
        definition: definition.to_owned(),
        revenue_dimensions: None,
        v1: v1
            .map(|value| rational_metric(value, unit))
            .transpose()?
            .unwrap_or_else(|| MetricValueV1::unavailable(missing_availability.clone(), unit)),
        dusk: dusk
            .map(|value| rational_metric(value, unit))
            .transpose()?
            .unwrap_or_else(|| MetricValueV1::unavailable(missing_availability.clone(), unit)),
        paired_delta: delta
            .as_ref()
            .map(|value| rational_metric(value, unit))
            .transpose()?
            .unwrap_or_else(|| MetricValueV1::unavailable(missing_availability, unit)),
    })
}

#[allow(clippy::too_many_arguments)]
fn leverage_venue_only_metric(
    metric_id: String,
    profile_id: &str,
    arm: ReplayArm,
    scenario: HlpScenario,
    definition: &str,
    value: Option<&BigRational>,
    v1_only: bool,
    unit: &str,
    missing_availability: MetricAvailability,
) -> Result<NamedMetricV1> {
    let available = value
        .map(|value| rational_metric(value, unit))
        .transpose()?
        .unwrap_or_else(|| MetricValueV1::unavailable(missing_availability.clone(), unit));
    let unavailable = MetricValueV1::unavailable(missing_availability, unit);
    Ok(NamedMetricV1 {
        metric_id,
        family: MetricFamily::Leverage,
        profile_id: Some(profile_id.to_owned()),
        replay_arm: Some(arm),
        hlp_scenario: Some(scenario),
        definition: definition.to_owned(),
        revenue_dimensions: None,
        v1: if v1_only {
            available.clone()
        } else {
            unavailable.clone()
        },
        dusk: if v1_only {
            unavailable.clone()
        } else {
            available
        },
        paired_delta: unavailable,
    })
}

#[allow(clippy::too_many_arguments)]
fn leverage_unavailable_named_metric(
    metric_id: String,
    profile_id: &str,
    arm: ReplayArm,
    scenario: HlpScenario,
    definition: &str,
    unit: &str,
    availability: MetricAvailability,
) -> NamedMetricV1 {
    let value = MetricValueV1::unavailable(availability, unit);
    NamedMetricV1 {
        metric_id,
        family: MetricFamily::Leverage,
        profile_id: Some(profile_id.to_owned()),
        replay_arm: Some(arm),
        hlp_scenario: Some(scenario),
        definition: definition.to_owned(),
        revenue_dimensions: None,
        v1: value.clone(),
        dusk: value.clone(),
        paired_delta: value,
    }
}

fn push_unique_metric(
    metric_ids: &mut BTreeSet<String>,
    output: &mut Vec<NamedMetricV1>,
    metric: NamedMetricV1,
) -> Result<()> {
    if !metric_ids.insert(metric.metric_id.clone()) {
        bail!("duplicate result metric ID {}", metric.metric_id);
    }
    output.push(metric);
    Ok(())
}

fn leverage_cell_metric_id(cell: &LeverageCampaignCellV1, suffix: &str) -> String {
    format!(
        "market.{}.global_profile.profile.{}.arm.{}.scenario.{}.leverage.checkpoint.{}.direction.{}.margin_nav_{}_bps.cell.{}.{}",
        cell.market,
        cell.profile_id,
        leverage_arm_slug(cell.arm),
        leverage_hlp_slug(cell.hlp_scenario),
        leverage_checkpoint_slug(cell.checkpoint_kind),
        leverage_direction_slug(cell.direction),
        cell.starting_margin_nav_bps,
        cell.cell_id,
        suffix,
    )
}

fn leverage_non_lending_metric_id(
    market: &str,
    profile_id: &str,
    arm: ReplayArm,
    scenario: HlpScenario,
    suffix: &str,
) -> String {
    format!(
        "market.{market}.global_profile.profile.{profile_id}.arm.{}.scenario.{}.leverage.no_lending_activity.{suffix}",
        leverage_arm_slug(arm),
        leverage_hlp_slug(scenario),
    )
}

fn leverage_aggregate_metric_id(
    profile_id: &str,
    arm: ReplayArm,
    scenario: HlpScenario,
    suffix: &str,
) -> String {
    format!(
        "aggregate.global_profile.profile.{profile_id}.arm.{}.scenario.{}.leverage.equal_market_lower_median.{suffix}",
        leverage_arm_slug(arm),
        leverage_hlp_slug(scenario),
    )
}

const fn leverage_arm_slug(arm: ReplayArm) -> &'static str {
    match arm {
        ReplayArm::FixedFlow => "fixed_flow",
        ReplayArm::Intent => "intent",
    }
}

const fn leverage_hlp_slug(scenario: HlpScenario) -> &'static str {
    match scenario {
        HlpScenario::NoHlp => "no_hlp",
        HlpScenario::MaximumFeasible => "maximum_feasible_hlp",
    }
}

const fn leverage_checkpoint_slug(kind: LeverageCheckpointKindV1) -> &'static str {
    match kind {
        LeverageCheckpointKindV1::First => "first",
        LeverageCheckpointKindV1::Median => "median",
        LeverageCheckpointKindV1::Last => "last",
    }
}

const fn leverage_direction_slug(direction: LeverageDirectionV1) -> &'static str {
    match direction {
        LeverageDirectionV1::LongBase => "long_base",
        LeverageDirectionV1::LongQuote => "long_quote",
    }
}

fn leverage_debt_unit(market: &MarketMetricSetV1, direction: LeverageDirectionV1) -> String {
    let mint = match direction {
        LeverageDirectionV1::LongBase => &market.token1,
        LeverageDirectionV1::LongQuote => &market.token0,
    };
    format!("raw_token_atoms:{mint}")
}

fn leverage_v1_price_unit(market: &MarketMetricSetV1, direction: LeverageDirectionV1) -> String {
    let (debt, collateral) = match direction {
        LeverageDirectionV1::LongBase => (&market.token1, &market.token0),
        LeverageDirectionV1::LongQuote => (&market.token0, &market.token1),
    };
    format!("debt_atoms_per_collateral_atom_nad:{debt}_per_{collateral}")
}

fn exact_coverage(
    covered: &ExactRationalV1,
    total: &ExactRationalV1,
    required_bps: u16,
) -> Result<CoverageMetricV1> {
    if required_bps > 10_000 {
        bail!("coverage threshold exceeds 10,000 bps");
    }
    let covered = exact_rational(covered)?;
    let total = exact_rational(total)?;
    if covered < BigRational::zero() || total < BigRational::zero() || covered > total {
        bail!("coverage numerator or denominator is invalid");
    }
    if total.is_zero() {
        return CoverageMetricV1::from_raw(0, 0, required_bps).map_err(Into::into);
    }
    let ratio = covered / total;
    let numerator = ratio.numer();
    let denominator = ratio.denom();
    if numerator.sign() == Sign::Minus || denominator.sign() != Sign::Plus {
        bail!("coverage ratio is negative");
    }
    let actual_bps = ((numerator * BigInt::from(10_000_u16)) / denominator)
        .to_u16()
        .context("coverage basis points exceed u16")?;
    let metric = CoverageMetricV1 {
        availability: MetricAvailability::Available,
        covered_numerator: unsigned_token(numerator.to_biguint().context("negative coverage")?),
        total_denominator: unsigned_token(
            denominator
                .to_biguint()
                .context("negative coverage denominator")?,
        ),
        required_bps,
        actual_bps: Some(actual_bps),
        passes: actual_bps >= required_bps,
    };
    metric.validate()?;
    Ok(metric)
}

fn available_exact(value: &AvailableExactRationalV1) -> Result<&ExactRationalV1> {
    if value.availability != MetricAvailability::Available {
        bail!("required exact aggregate is unavailable");
    }
    value
        .value
        .as_ref()
        .context("available exact aggregate lacks a value")
}

const CAPACITY_CONSTRAINTS: [(DuskBorrowCapacityConstraintV1, &str); 5] = [
    (DuskBorrowCapacityConstraintV1::Underwriting, "underwriting"),
    (
        DuskBorrowCapacityConstraintV1::GlobalHealthFloor,
        "global_health_floor",
    ),
    (DuskBorrowCapacityConstraintV1::Cash, "cash"),
    (
        DuskBorrowCapacityConstraintV1::DailyBorrowBucket,
        "daily_borrow_bucket",
    ),
    (DuskBorrowCapacityConstraintV1::ReduceOnly, "reduce_only"),
];

#[allow(clippy::too_many_arguments)]
fn append_capacity_metrics(
    input: &HoldoutMetricInputV1<'_>,
    market: Option<&str>,
    v1_gross_headroom: &ExactRationalV1,
    v1_actionable_headroom: &ExactRationalV1,
    dusk_actionable_headroom: &ExactRationalV1,
    capacity: &DuskBorrowCapacityAggregateV1,
    projected: &BorrowProjectedTotalDebtAggregateV1,
    output: &mut Vec<NamedMetricV1>,
) -> Result<()> {
    validate_capacity_evidence(
        v1_gross_headroom,
        v1_actionable_headroom,
        dusk_actionable_headroom,
        capacity,
        projected,
    )?;
    let names_and_units = [
        (
            "collateral.additional_headroom.gross.matched_borrow_prestate_observation_sum_usd_micro",
            "usd_micro_matched_borrow_prestate_observation_sum",
        ),
        (
            "collateral.additional_headroom.actionable.matched_borrow_prestate_observation_sum_usd_micro",
            "usd_micro_matched_borrow_prestate_observation_sum",
        ),
        (
            "collateral.additional_headroom.gross_to_actionable_loss.matched_borrow_prestate_observation_sum_usd_micro",
            "usd_micro_matched_borrow_prestate_observation_sum",
        ),
        (
            "collateral.additional_headroom.actionable_to_gross.p50_bps",
            "bps",
        ),
        (
            "collateral.additional_headroom.actionable_to_gross.p95_bps",
            "bps",
        ),
        (
            "collateral.additional_headroom.matched_borrow_prestate_observation_count",
            "observation_count",
        ),
        (
            "collateral.additional_headroom.underwriting_comparable_observation_count",
            "observation_count",
        ),
        (
            "collateral.additional_headroom.headroom_ratio_observation_count",
            "observation_count",
        ),
        (
            "collateral.additional_headroom.zero_gross_headroom_observation_count",
            "observation_count",
        ),
        (
            "collateral.additional_headroom.current_underwriting_satisfied_observation_count",
            "observation_count",
        ),
        (
            "collateral.additional_headroom.current_underwriting_unsatisfied_observation_count",
            "observation_count",
        ),
        (
            "collateral.additional_headroom.current_global_health_floor_satisfied_observation_count",
            "observation_count",
        ),
        (
            "collateral.additional_headroom.current_global_health_floor_unsatisfied_observation_count",
            "observation_count",
        ),
        (
            "collateral.projected_total_debt.gross.matched_borrow_prestate_observation_sum_usd_micro",
            "usd_micro_matched_borrow_prestate_observation_sum",
        ),
        (
            "collateral.projected_total_debt.actionable.matched_borrow_prestate_observation_sum_usd_micro",
            "usd_micro_matched_borrow_prestate_observation_sum",
        ),
    ];
    if capacity.matched_borrow_prestate_observation_count == 0 {
        for (name, unit) in names_and_units {
            output.push(unavailable_lending_metric_scoped(input, market, name, unit));
        }
        for (_, slug) in CAPACITY_CONSTRAINTS {
            for (suffix, unit) in [
                ("matched_observation_count", "observation_count"),
                (
                    "tied_additional_headroom_loss_usd_micro",
                    "usd_micro_matched_borrow_prestate_observation_sum",
                ),
            ] {
                output.push(unavailable_lending_metric_scoped(
                    input,
                    market,
                    &format!("collateral.additional_headroom.limiting_constraint.{slug}.{suffix}"),
                    unit,
                ));
            }
        }
        return Ok(());
    }

    let comparable = capacity.underwriting_comparable_observation_count > 0;
    output.push(metric_rational(
        input,
        market,
        names_and_units[0].0,
        MetricFamily::CollateralEfficiency,
        "V1 and Dusk gross raw additional-borrow headroom summed over identical currently-underwritten factual borrow prestates; projected total debt is reported separately.",
        comparable.then_some(v1_gross_headroom),
        comparable
            .then_some(&capacity.gross_additional_headroom_matched_observation_sum_usd_micro),
        names_and_units[0].1,
    )?);
    output.push(metric_rational(
        input,
        market,
        names_and_units[1].0,
        MetricFamily::CollateralEfficiency,
        "V1 and Dusk actionable raw additional-borrow headroom after execution-capital gates, on the same matched prestates.",
        comparable.then_some(v1_actionable_headroom),
        comparable.then_some(dusk_actionable_headroom),
        names_and_units[1].1,
    )?);
    output.push(metric_rational(
        input,
        market,
        names_and_units[2].0,
        MetricFamily::CollateralEfficiency,
        "Dusk gross minus actionable raw additional headroom on the currently-underwritten matched population.",
        None,
        comparable.then_some(
            &capacity.gross_to_actionable_additional_headroom_loss_usd_micro,
        ),
        names_and_units[2].1,
    )?);
    for (index, value, definition) in [
        (
            3,
            capacity
                .actionable_to_gross_additional_headroom_bps_p50_exact
                .as_ref(),
            "Lower-median Dusk actionable-to-gross raw additional-headroom ratio.",
        ),
        (
            4,
            capacity
                .actionable_to_gross_additional_headroom_bps_p95_exact
                .as_ref(),
            "Nearest-rank p95 Dusk actionable-to-gross raw additional-headroom ratio.",
        ),
    ] {
        output.push(metric_rational(
            input,
            market,
            names_and_units[index].0,
            MetricFamily::CollateralEfficiency,
            definition,
            None,
            value,
            names_and_units[index].1,
        )?);
    }
    let global_health_satisfied = capacity
        .matched_borrow_prestate_observation_count
        .checked_sub(capacity.current_global_health_floor_unsatisfied_observation_count)
        .context("global-health-unsatisfied count exceeds matched capacity observations")?;
    for (index, count, definition) in [
        (
            5,
            capacity.matched_borrow_prestate_observation_count,
            "All matched factual borrow prestates in this evidence scope.",
        ),
        (
            6,
            capacity.underwriting_comparable_observation_count,
            "Matched prestates satisfying current Dusk position underwriting and included in capacity sums.",
        ),
        (
            7,
            capacity.headroom_ratio_observation_count,
            "Currently-underwritten matched prestates with positive gross raw headroom and a defined ratio.",
        ),
        (
            8,
            capacity.zero_gross_headroom_observation_count,
            "Currently-underwritten matched prestates with zero gross raw headroom; excluded from ratios.",
        ),
        (
            9,
            capacity.underwriting_comparable_observation_count,
            "Matched prestates satisfying current Dusk position underwriting.",
        ),
        (
            10,
            capacity.current_underwriting_unsatisfied_observation_count,
            "Matched prestates already above current Dusk position underwriting; excluded from comparable capacity sums.",
        ),
        (
            11,
            global_health_satisfied,
            "Matched prestates satisfying the current Dusk global-health floor.",
        ),
        (
            12,
            capacity.current_global_health_floor_unsatisfied_observation_count,
            "Matched prestates failing the current Dusk global-health floor.",
        ),
    ] {
        let exact = exact_count(count)?;
        output.push(metric_rational(
            input,
            market,
            names_and_units[index].0,
            MetricFamily::CollateralEfficiency,
            definition,
            None,
            Some(&exact),
            names_and_units[index].1,
        )?);
    }
    for (index, v1, dusk, definition) in [
        (
            13,
            &projected.v1_gross_max_debt_observation_sum_usd_micro,
            &projected.dusk_gross_max_debt_observation_sum_usd_micro,
            "Gross projected total-debt maxima on matched prestates; risk/LTV evidence only, not capacity-efficiency headroom.",
        ),
        (
            14,
            &projected.v1_actionable_max_debt_observation_sum_usd_micro,
            &projected.dusk_actionable_max_debt_observation_sum_usd_micro,
            "Actionable projected total-debt maxima on matched prestates; risk/LTV evidence only.",
        ),
    ] {
        output.push(metric_rational(
            input,
            market,
            names_and_units[index].0,
            MetricFamily::CollateralEfficiency,
            definition,
            comparable.then_some(v1),
            comparable.then_some(dusk),
            names_and_units[index].1,
        )?);
    }
    for (constraint, slug) in CAPACITY_CONSTRAINTS {
        let row = capacity
            .constraints
            .iter()
            .find(|row| row.constraint == constraint)
            .context("capacity evidence omits one stable native constraint")?;
        let count = exact_count(row.limiting_matched_observation_count)?;
        output.push(metric_rational(
            input,
            market,
            &format!("collateral.additional_headroom.limiting_constraint.{slug}.matched_observation_count"),
            MetricFamily::CollateralEfficiency,
            "Currently-underwritten matched prestates where this native constraint tied the actionable raw-headroom bound; ties are non-exclusive.",
            None,
            Some(&count),
            "observation_count",
        )?);
        output.push(metric_rational(
            input,
            market,
            &format!("collateral.additional_headroom.limiting_constraint.{slug}.tied_additional_headroom_loss_usd_micro"),
            MetricFamily::CollateralEfficiency,
            "Gross-to-actionable raw-headroom loss on prestates tied by this constraint; tied rows are non-additive.",
            None,
            comparable.then_some(&row.tied_additional_headroom_loss_usd_micro),
            "usd_micro_matched_borrow_prestate_observation_sum",
        )?);
    }
    Ok(())
}

fn validate_capacity_evidence(
    v1_gross_headroom: &ExactRationalV1,
    v1_actionable_headroom: &ExactRationalV1,
    dusk_actionable_headroom: &ExactRationalV1,
    capacity: &DuskBorrowCapacityAggregateV1,
    projected: &BorrowProjectedTotalDebtAggregateV1,
) -> Result<()> {
    let matched = capacity.matched_borrow_prestate_observation_count;
    if capacity
        .underwriting_comparable_observation_count
        .checked_add(capacity.current_underwriting_unsatisfied_observation_count)
        != Some(matched)
        || capacity
            .headroom_ratio_observation_count
            .checked_add(capacity.zero_gross_headroom_observation_count)
            != Some(capacity.underwriting_comparable_observation_count)
        || capacity.current_global_health_floor_unsatisfied_observation_count > matched
        || projected.matched_observation_count != capacity.underwriting_comparable_observation_count
        || capacity.constraints.len() != CAPACITY_CONSTRAINTS.len()
    {
        bail!("capacity evidence counts do not partition the matched borrow prestates");
    }
    let v1_gross = exact_rational(v1_gross_headroom)?;
    let v1_actionable = exact_rational(v1_actionable_headroom)?;
    let dusk_gross =
        exact_rational(&capacity.gross_additional_headroom_matched_observation_sum_usd_micro)?;
    let dusk_actionable = exact_rational(dusk_actionable_headroom)?;
    let dusk_nested_actionable =
        exact_rational(&capacity.actionable_additional_headroom_matched_observation_sum_usd_micro)?;
    let loss = exact_rational(&capacity.gross_to_actionable_additional_headroom_loss_usd_micro)?;
    let zero = BigRational::zero();
    if v1_gross < zero
        || v1_actionable < zero
        || dusk_gross < zero
        || dusk_actionable < zero
        || loss < zero
        || v1_actionable > v1_gross
        || dusk_actionable != dusk_nested_actionable
        || dusk_actionable > dusk_gross
        || &dusk_gross - &dusk_actionable != loss
    {
        bail!("capacity evidence mixes total debt with raw additional headroom");
    }
    let projected_values = [
        exact_rational(&projected.v1_gross_max_debt_observation_sum_usd_micro)?,
        exact_rational(&projected.v1_actionable_max_debt_observation_sum_usd_micro)?,
        exact_rational(&projected.dusk_gross_max_debt_observation_sum_usd_micro)?,
        exact_rational(&projected.dusk_actionable_max_debt_observation_sum_usd_micro)?,
    ];
    if projected_values.iter().any(|value| value < &zero)
        || projected_values[1] > projected_values[0]
        || projected_values[3] > projected_values[2]
    {
        bail!("projected total-debt evidence is negative or not ordered");
    }
    let observed_constraints: BTreeSet<_> = capacity
        .constraints
        .iter()
        .map(|row| row.constraint)
        .collect();
    let expected_constraints: BTreeSet<_> = CAPACITY_CONSTRAINTS
        .iter()
        .map(|(constraint, _)| *constraint)
        .collect();
    if observed_constraints != expected_constraints {
        bail!("capacity constraint evidence is incomplete or exceeds its comparable population");
    }
    for row in &capacity.constraints {
        let tied_loss = exact_rational(&row.tied_additional_headroom_loss_usd_micro)?;
        if row.limiting_matched_observation_count
            > capacity.underwriting_comparable_observation_count
            || tied_loss < zero
            || tied_loss > loss
        {
            bail!("capacity constraint evidence exceeds its comparable population or total loss");
        }
    }
    match (
        capacity.headroom_ratio_observation_count,
        &capacity.actionable_to_gross_additional_headroom_bps_p50_exact,
        &capacity.actionable_to_gross_additional_headroom_bps_p95_exact,
    ) {
        (0, None, None) => {}
        (count, Some(p50), Some(p95)) if count > 0 => {
            let p50 = exact_rational(p50)?;
            let p95 = exact_rational(p95)?;
            let ceiling = BigRational::from_integer(BigInt::from(10_000_u16));
            if p50 < zero || p50 > p95 || p95 > ceiling {
                bail!("capacity headroom quantiles are outside the exact bps domain");
            }
        }
        _ => bail!("capacity quantile availability disagrees with its observation count"),
    }
    if capacity.underwriting_comparable_observation_count == 0
        && (v1_gross != zero
            || v1_actionable != zero
            || dusk_gross != zero
            || dusk_actionable != zero
            || loss != zero
            || projected_values.iter().any(|value| value != &zero)
            || capacity.constraints.iter().any(|row| {
                row.limiting_matched_observation_count != 0
                    || exact_rational(&row.tied_additional_headroom_loss_usd_micro)
                        .is_ok_and(|value| value != zero)
            }))
    {
        bail!("underwriting-incomparable capacity evidence contains numeric observations");
    }
    Ok(())
}

fn aggregate_metrics_for(input: &HoldoutMetricInputV1<'_>) -> Result<Vec<NamedMetricV1>> {
    let evidence = input.evidence;
    let mut metrics = vec![
        comparison_available_rational(
            input,
            "stakeholder_revenue.user_flow_usd_micro",
            MetricFamily::LpRevenue,
            "Core LP plus protocol economic revenue from historical user flows; synthetic arbitrage and synthetic protocol settlement are excluded.",
            &evidence.stakeholder_revenue_v1_usd_micro,
            &evidence.stakeholder_revenue_dusk_user_usd_micro,
            "usd_micro",
        )?,
        comparison_rational(
            input,
            "safety.bad_debt_usd_micro",
            MetricFamily::Safety,
            "Final liquidation shortfall after exact in-window auction settlement.",
            &evidence.v1_bad_debt_usd_micro,
            &evidence.dusk_bad_debt_usd_micro,
            "usd_micro",
        )?,
        delta_only_rational(
            input,
            "spot.volume_weighted_execution_cost_delta_ppb",
            MetricFamily::SpotCapitalEfficiency,
            "Positive means Dusk returned less output than V1 for identical factual exact-in flow.",
            evidence.volume_weighted_execution_cost_delta_ppb_exact.as_ref(),
            "ppb",
        )?,
        delta_only_signed(
            input,
            "spot.p95_slippage_delta_ppb",
            MetricFamily::SpotCapitalEfficiency,
            "Empirical p95 of paired Dusk-minus-V1 slippage for factual swaps.",
            evidence.p95_slippage_delta_ppb.as_ref(),
            "ppb",
        ),
        delta_only_rational(
            input,
            "revenue.capital_weighted_uplift_bps",
            MetricFamily::LpRevenue,
            "Stakeholder user-flow revenue delta divided by time-weighted factual external LP capital.",
            evidence
                .capital_weighted_stakeholder_revenue_uplift_bps_exact
                .as_ref(),
            "bps",
        )?,
        delta_only_signed(
            input,
            "revenue.equal_market_median_uplift_bps",
            MetricFamily::LpRevenue,
            "Median of per-market stakeholder revenue uplift on factual capital.",
            evidence.equal_market_stakeholder_revenue_uplift_bps.as_ref(),
            "bps",
        ),
    ];
    if evidence
        .dusk_capacity_decomposition
        .matched_borrow_prestate_observation_count
        > 0
    {
        metrics.push(comparison_rational(
            input,
            "borrow.debt_increase_notional_usd_micro",
            MetricFamily::CollateralEfficiency,
            "Positive factual debt changes versus share-rounded Dusk debt increases.",
            &evidence.v1_debt_increase_notional_usd_micro,
            &evidence.dusk_debt_increase_notional_usd_micro,
            "usd_micro",
        )?);
        metrics.push(dusk_only_rational(
            input,
            "borrow.unmet_capacity_preserving_demand_usd_micro",
            MetricFamily::CollateralEfficiency,
            "Capacity-preserving post-debt demand rejected or left unfilled by Dusk constraints.",
            &evidence.dusk_unmet_borrow_notional_usd_micro,
            "usd_micro",
        )?);
    } else {
        for name in [
            "borrow.debt_increase_notional_usd_micro",
            "borrow.unmet_capacity_preserving_demand_usd_micro",
        ] {
            metrics.push(unavailable_lending_metric_scoped(
                input,
                None,
                name,
                "usd_micro",
            ));
        }
    }
    metrics.push(dusk_only_available_rational(
        input,
        "stakeholder_revenue.synthetic_arbitrage_usd_micro",
        MetricFamily::ProtocolRevenue,
        "Separately labeled counterfactual revenue from minimum block-boundary arbitrage; never user revenue.",
        &evidence.stakeholder_revenue_dusk_synthetic_arbitrage_usd_micro,
        "usd_micro",
    )?);
    metrics.push(dusk_only_available_rational(
        input,
        "stakeholder_revenue.synthetic_protocol_settlement_usd_micro",
        MetricFamily::ProtocolRevenue,
        "Separately labeled protocol-settlement revenue; never user flow or arbitrage revenue.",
        &evidence.stakeholder_revenue_dusk_synthetic_protocol_settlement_usd_micro,
        "usd_micro",
    )?);
    append_capacity_metrics(
        input,
        None,
        &evidence.v1_gross_headroom_matched_observation_sum_usd_micro,
        &evidence.v1_actionable_headroom_matched_observation_sum_usd_micro,
        &evidence.dusk_actionable_headroom_matched_observation_sum_usd_micro,
        &evidence.dusk_capacity_decomposition,
        &evidence.projected_total_debt_capacity,
        &mut metrics,
    )?;
    append_spot_probe_metrics(input, None, &evidence.spot_probes, &mut metrics)?;
    append_revenue_evidence_metrics(input, None, &evidence.revenue_yield, &mut metrics)?;
    Ok(metrics)
}

fn usd_market_metrics_for(
    input: &HoldoutMetricInputV1<'_>,
) -> Result<BTreeMap<String, MarketMetricSetV1>> {
    let mut output = BTreeMap::new();
    for market in &input.evidence.markets {
        let mut metrics = vec![
            comparison_available_rational_market(
                input,
                &market.market,
                "stakeholder_revenue.user_flow_usd_micro",
                MetricFamily::LpRevenue,
                "Core stakeholder revenue from historical user flows.",
                &market.stakeholder_revenue_v1_usd_micro,
                &market.stakeholder_revenue_dusk_user_usd_micro,
                "usd_micro",
            )?,
            comparison_rational_market(
                input,
                &market.market,
                "safety.bad_debt_usd_micro",
                MetricFamily::Safety,
                "Final liquidation shortfall.",
                &market.v1_bad_debt_usd_micro,
                &market.dusk_bad_debt_usd_micro,
                "usd_micro",
            )?,
        ];
        if market
            .dusk_capacity_decomposition
            .matched_borrow_prestate_observation_count
            != market.borrow_count
        {
            bail!("market capacity evidence count differs from factual borrow rows");
        }
        append_capacity_metrics(
            input,
            Some(&market.market),
            &market.v1_gross_headroom_matched_observation_sum_usd_micro,
            &market.v1_actionable_headroom_matched_observation_sum_usd_micro,
            &market.dusk_actionable_headroom_matched_observation_sum_usd_micro,
            &market.dusk_capacity_decomposition,
            &market.projected_total_debt_capacity,
            &mut metrics,
        )?;
        if market.borrow_count > 0 {
            metrics.push(comparison_rational_market(
                input,
                &market.market,
                "borrow.debt_increase_notional_usd_micro",
                MetricFamily::CollateralEfficiency,
                "Observed positive V1 debt increase versus share-rounded Dusk debt increase.",
                &market.v1_debt_increase_notional_usd_micro,
                &market.dusk_debt_increase_notional_usd_micro,
                "usd_micro",
            )?);
            metrics.push(dusk_only_rational_market(
                input,
                &market.market,
                "borrow.unmet_capacity_preserving_demand_usd_micro",
                MetricFamily::CollateralEfficiency,
                "Unfilled Dusk capacity-preserving post-debt demand.",
                &market.dusk_unmet_borrow_notional_usd_micro,
                "usd_micro",
            )?);
        } else {
            for name in [
                "borrow.debt_increase_notional_usd_micro",
                "borrow.unmet_capacity_preserving_demand_usd_micro",
            ] {
                metrics.push(unavailable_lending_metric_scoped(
                    input,
                    Some(&market.market),
                    name,
                    "usd_micro",
                ));
            }
        }
        append_spot_probe_metrics(
            input,
            Some(&market.market),
            &market.spot_probes,
            &mut metrics,
        )?;
        append_revenue_evidence_metrics(
            input,
            Some(&market.market),
            &market.revenue_yield,
            &mut metrics,
        )?;
        output.insert(
            market.market.clone(),
            MarketMetricSetV1 {
                market: market.market.clone(),
                token0: String::new(),
                token1: String::new(),
                has_lending_activity: market.borrow_count > 0,
                raw_metrics: Vec::new(),
                usd_metrics: metrics,
            },
        );
    }
    Ok(output)
}

fn append_spot_probe_metrics(
    input: &HoldoutMetricInputV1<'_>,
    market: Option<&str>,
    probes: &[SpotProbeAggregateCellV1],
    output: &mut Vec<NamedMetricV1>,
) -> Result<()> {
    for probe in probes {
        let prefix = format!(
            "spot.standardized.{}.{}",
            spot_probe_size_slug(probe.size_kind),
            spot_probe_direction_slug(probe.direction),
        );
        let v1_executable = exact_count(probe.v1_executable_count)?;
        let dusk_executable = exact_count(probe.dusk_executable_count)?;
        output.push(metric_rational(
            input,
            market,
            &format!("{prefix}.executable_count"),
            MetricFamily::SpotCapitalEfficiency,
            "Number of split-bounded standardized probes executable under each venue's exact cash, curve, transfer-fee, and hLP-settlement constraints.",
            Some(&v1_executable),
            Some(&dusk_executable),
            "probe_observations",
        )?);
        let paired = exact_count(probe.paired_executable_count)?;
        output.push(delta_only_rational_scoped(
            input,
            market,
            &format!("{prefix}.paired_executable_count"),
            MetricFamily::SpotCapitalEfficiency,
            "Count of probe observations executable on both venues; this is a paired-support denominator, not a venue delta.",
            Some(&paired),
            "paired_probe_observations",
        )?);
        for (suffix, definition, value, unit) in [
            (
                "execution_cost_delta_ppb",
                "Mean Dusk-minus-V1 effective execution cost against the common factual V1 pretrade marginal-price reference; lower is better.",
                probe.mean_execution_cost_delta_ppb_exact.as_ref(),
                "ppb",
            ),
            (
                "gross_quote_improvement_ppb",
                "Mean gross-output quote improvement for Dusk versus V1 on exactly paired probes; higher is better.",
                probe.mean_gross_quote_improvement_ppb_exact.as_ref(),
                "ppb",
            ),
            (
                "net_quote_improvement_ppb",
                "Mean wallet-net quote improvement after historical transfer fees for Dusk versus V1; higher is better.",
                probe.mean_net_quote_improvement_ppb_exact.as_ref(),
                "ppb",
            ),
            (
                "own_curve_slippage_delta_ppb",
                "Mean Dusk-minus-V1 slippage relative to each venue's own exact pretrade marginal curve; lower is better.",
                probe.mean_slippage_delta_ppb_exact.as_ref(),
                "ppb",
            ),
            (
                "cpmm_equivalent_depth_per_external_lp_delta",
                "Mean Dusk-minus-V1 CPMM-equivalent depth per exact external-LP ownership claim. N/A is retained when Dusk aggregate ownership NAV is unavailable.",
                probe.mean_cpmm_equivalent_depth_delta_exact.as_ref(),
                "depth_ratio",
            ),
        ] {
            output.push(delta_only_rational_scoped(
                input,
                market,
                &format!("{prefix}.{suffix}"),
                MetricFamily::SpotCapitalEfficiency,
                definition,
                value,
                unit,
            )?);
        }
    }
    Ok(())
}

fn append_revenue_evidence_metrics(
    input: &HoldoutMetricInputV1<'_>,
    market: Option<&str>,
    evidence: &RevenueYieldEvidenceV1,
    output: &mut Vec<NamedMetricV1>,
) -> Result<()> {
    if evidence.window_seconds == 0 {
        bail!("revenue evidence has an empty window");
    }
    let lane_index: BTreeMap<_, _> = evidence
        .lane_totals
        .iter()
        .map(|row| ((row.venue, row.origin, row.lane), row))
        .collect();
    if lane_index.len() != evidence.lane_totals.len()
        || lane_index.len() != RevenueLaneV1::ALL.len() * 4
    {
        bail!("revenue lane evidence is duplicate or incomplete");
    }
    for origin in [
        RevenueOriginV1::FactualUserFlow,
        RevenueOriginV1::SyntheticArbitrage,
        RevenueOriginV1::SyntheticProtocolSettlement,
    ] {
        for lane in RevenueLaneV1::ALL {
            let dusk = lane_index
                .get(&(RevenueVenueV1::Dusk, origin, lane))
                .context("Dusk revenue lane evidence is incomplete")?;
            if dusk.accounting_class != lane.accounting_class() {
                bail!("Dusk revenue lane accounting class changed");
            }
            let (v1, v1_venue) = if origin == RevenueOriginV1::FactualUserFlow {
                let v1 = lane_index
                    .get(&(RevenueVenueV1::V1, origin, lane))
                    .context("V1 factual revenue lane evidence is incomplete")?;
                if v1.accounting_class != lane.accounting_class() {
                    bail!("V1 revenue lane accounting class changed");
                }
                (Some(&v1.usd_micro), Some(RevenueVenueV1::V1))
            } else {
                (None, None)
            };
            let mut metric = metric_available_rational(
                input,
                market,
                &format!(
                    "revenue.lane.{}.origin.{}.usd_micro",
                    revenue_lane_slug(lane),
                    revenue_origin_slug(origin)
                ),
                revenue_lane_family(lane),
                &format!(
                    "Exact historical-price total for the {:?} lane ({:?}); lane, origin, venue, and accounting class are schema-bound.",
                    lane,
                    lane.accounting_class()
                ),
                v1,
                Some(&dusk.usd_micro),
                "usd_micro",
            )?;
            metric.revenue_dimensions = Some(RevenueMetricDimensionsV1 {
                lane,
                accounting_class: lane.accounting_class(),
                origin,
                v1_venue,
                dusk_venue: Some(RevenueVenueV1::Dusk),
            });
            output.push(metric);
        }
    }

    let mut return_keys = BTreeSet::new();
    for row in &evidence.common_factual_capital_returns {
        let key = (
            row.venue,
            row.origin,
            row.beneficiary,
            row.component,
            row.denominator_basis,
        );
        if !return_keys.insert(key) {
            bail!("revenue return evidence contains a duplicate component key");
        }
        append_return_metric(input, market, row, false, output)?;
        append_return_metric(input, market, row, true, output)?;
    }
    let external_owner = evidence
        .common_factual_capital_returns
        .iter()
        .find(|row| {
            row.venue == RevenueVenueV1::Dusk
                && row.origin == RevenueOriginV1::FactualUserFlow
                && row.component == RevenueReturnComponentV1::ExternalLpCombinedEconomic
                && row.denominator_basis
                    == crate::aggregate::RevenueDenominatorBasisV1::ExactDuskExternalLpOwnerClaimCapitalTime
        })
        .context("Dusk external-LP owner-denominator N/A row is missing")?;
    let hlp_owner = evidence
        .common_factual_capital_returns
        .iter()
        .find(|row| {
            row.venue == RevenueVenueV1::Dusk
                && row.origin == RevenueOriginV1::FactualUserFlow
                && row.component == RevenueReturnComponentV1::NetHlpCommonCapitalContribution
                && row.denominator_basis
                    == crate::aggregate::RevenueDenominatorBasisV1::ExactDuskHlpCapitalTime
        })
        .context("Dusk hLP owner-denominator N/A row is missing")?;
    if external_owner.apr_bps_exact != evidence.actual_dusk_external_lp_apy_bps_exact
        || hlp_owner.apr_bps_exact != evidence.actual_dusk_hlp_apy_bps_exact
        || evidence.actual_dusk_external_lp_apy_bps_exact.availability
            != MetricAvailability::MissingOwnershipDenominator
        || evidence.actual_dusk_hlp_apy_bps_exact.availability
            != MetricAvailability::MissingOwnershipDenominator
    {
        bail!("top-level owner APY compatibility fields disagree with typed return rows");
    }
    output.push(delta_only_rational_scoped(
        input,
        market,
        "lp.inventory_pnl_delta",
        MetricFamily::LpRevenue,
        "Inventory PnL requires one exact passive yLP/hLP ownership ledger including withdrawal claims, protected/recenter inventory, and claimable YieldAccount balances. N/A is never replaced with zero.",
        evidence.inventory_pnl_exact.as_ref(),
        "ownership_value_delta",
    )?);
    Ok(())
}

fn append_return_metric(
    input: &HoldoutMetricInputV1<'_>,
    market: Option<&str>,
    row: &RevenueReturnEvidenceV1,
    annualized: bool,
    output: &mut Vec<NamedMetricV1>,
) -> Result<()> {
    let value = if annualized {
        &row.apr_bps_exact
    } else {
        &row.absolute_return_bps_exact
    };
    let name = format!(
        "revenue.return.{}.origin.{}.venue.{}.denominator.{}.{}",
        revenue_component_slug(row.component),
        revenue_origin_slug(row.origin),
        revenue_venue_slug(row.venue),
        revenue_denominator_slug(row.denominator_basis),
        if annualized {
            "apr_bps"
        } else {
            "absolute_bps"
        },
    );
    let (v1, dusk) = match row.venue {
        RevenueVenueV1::V1 => (Some(value), None),
        RevenueVenueV1::Dusk => (None, Some(value)),
    };
    output.push(metric_available_rational(
        input,
        market,
        &name,
        match row.beneficiary {
            crate::aggregate::RevenueBeneficiaryV1::Protocol => MetricFamily::ProtocolRevenue,
            _ => MetricFamily::LpRevenue,
        },
        if annualized {
            "Linear APR, available only for windows of at least 30 days. Exact Dusk owner-claim APY remains N/A without ownership capital-time."
        } else {
            "Absolute exact return over the holdout window. The common factual-capital denominator is a standardized comparison, not actual Dusk ownership APY."
        },
        v1,
        dusk,
        if annualized { "annualized_bps" } else { "bps" },
    )?);
    Ok(())
}

const fn revenue_lane_family(lane: RevenueLaneV1) -> MetricFamily {
    match lane {
        RevenueLaneV1::ProtocolFeeAuctionSwapEarned
        | RevenueLaneV1::ProtocolBuybackSwapEarned
        | RevenueLaneV1::ProtocolPublicInterestAccrued
        | RevenueLaneV1::ProtocolFeeAuctionPublicInterestCashRealized
        | RevenueLaneV1::ProtocolBuybackPublicInterestCashRealized
        | RevenueLaneV1::ProtocolFeeAuctionHlpFundingInterestEarned
        | RevenueLaneV1::ProtocolBuybackHlpFundingInterestEarned
        | RevenueLaneV1::ProtocolClaimRealizedV1Only => MetricFamily::ProtocolRevenue,
        RevenueLaneV1::RetainedRecenterInventory | RevenueLaneV1::CompoundedLpSwapPrincipal => {
            MetricFamily::BalanceSheet
        }
        _ => MetricFamily::LpRevenue,
    }
}

fn revenue_lane_slug(lane: RevenueLaneV1) -> &'static str {
    match lane {
        RevenueLaneV1::LpSwapEarned => "lp_swap_earned",
        RevenueLaneV1::ProtocolFeeAuctionSwapEarned => "protocol_fee_auction_swap_earned",
        RevenueLaneV1::ProtocolBuybackSwapEarned => "protocol_buyback_swap_earned",
        RevenueLaneV1::RetainedRecenterInventory => "retained_recenter_inventory",
        RevenueLaneV1::CompoundedLpSwapPrincipal => "compounded_lp_swap_principal",
        RevenueLaneV1::LpPublicInterestAccrued => "lp_public_interest_accrued",
        RevenueLaneV1::ProtocolPublicInterestAccrued => "protocol_public_interest_accrued",
        RevenueLaneV1::LpPublicInterestCashRealized => "lp_public_interest_cash_realized",
        RevenueLaneV1::ProtocolFeeAuctionPublicInterestCashRealized => {
            "protocol_fee_auction_public_interest_cash_realized"
        }
        RevenueLaneV1::ProtocolBuybackPublicInterestCashRealized => {
            "protocol_buyback_public_interest_cash_realized"
        }
        RevenueLaneV1::HlpFundingInterestEarned => "hlp_funding_interest_earned",
        RevenueLaneV1::ProtocolFeeAuctionHlpFundingInterestEarned => {
            "protocol_fee_auction_hlp_funding_interest_earned"
        }
        RevenueLaneV1::ProtocolBuybackHlpFundingInterestEarned => {
            "protocol_buyback_hlp_funding_interest_earned"
        }
        RevenueLaneV1::FlashloanFeeV1Only => "flashloan_fee_v1_only",
        RevenueLaneV1::ProtocolClaimRealizedV1Only => "protocol_claim_realized_v1_only",
    }
}

fn revenue_origin_slug(origin: RevenueOriginV1) -> &'static str {
    match origin {
        RevenueOriginV1::FactualUserFlow => "factual_user_flow",
        RevenueOriginV1::SyntheticArbitrage => "synthetic_arbitrage",
        RevenueOriginV1::SyntheticProtocolSettlement => "synthetic_protocol_settlement",
    }
}

fn revenue_venue_slug(venue: RevenueVenueV1) -> &'static str {
    match venue {
        RevenueVenueV1::V1 => "v1",
        RevenueVenueV1::Dusk => "dusk",
    }
}

fn revenue_component_slug(component: RevenueReturnComponentV1) -> &'static str {
    match component {
        RevenueReturnComponentV1::LpSwap => "lp_swap",
        RevenueReturnComponentV1::LpPublicInterestAccrued => "lp_public_interest_accrued",
        RevenueReturnComponentV1::ExternalLpCombinedEconomic => "external_lp_combined_economic",
        RevenueReturnComponentV1::LpPublicInterestCashMemo => "lp_public_interest_cash_memo",
        RevenueReturnComponentV1::ProtocolPublicInterestCashMemo => {
            "protocol_public_interest_cash_memo"
        }
        RevenueReturnComponentV1::NetHlpCommonCapitalContribution => {
            "net_hlp_common_capital_contribution"
        }
        RevenueReturnComponentV1::ProtocolEconomic => "protocol_economic",
        RevenueReturnComponentV1::AllStakeholderEconomic => "all_stakeholder_economic",
    }
}

fn revenue_denominator_slug(basis: crate::aggregate::RevenueDenominatorBasisV1) -> &'static str {
    match basis {
        crate::aggregate::RevenueDenominatorBasisV1::FactualV1AccountedLpCapitalTime => {
            "factual_v1_accounted_lp_capital_time"
        }
        crate::aggregate::RevenueDenominatorBasisV1::ExactDuskExternalLpOwnerClaimCapitalTime => {
            "exact_dusk_external_lp_owner_claim_capital_time"
        }
        crate::aggregate::RevenueDenominatorBasisV1::ExactDuskHlpCapitalTime => {
            "exact_dusk_hlp_capital_time"
        }
    }
}

fn exact_count(value: u64) -> Result<ExactRationalV1> {
    Ok(ExactRationalV1 {
        numerator: SignedTokenAmount::new(value.to_string())?,
        denominator: TokenAmount::new("1")?,
    })
}

const fn spot_probe_size_slug(size: SpotProbeSizeKindV1) -> &'static str {
    match size {
        SpotProbeSizeKindV1::FactualNav10Bps => "nav_10_bps",
        SpotProbeSizeKindV1::FactualNav100Bps => "nav_100_bps",
        SpotProbeSizeKindV1::FactualNav500Bps => "nav_500_bps",
        SpotProbeSizeKindV1::FactualGrossInputP50 => "factual_input_p50",
        SpotProbeSizeKindV1::FactualGrossInputP90 => "factual_input_p90",
    }
}

const fn spot_probe_direction_slug(direction: SpotProbeDirectionV1) -> &'static str {
    match direction {
        SpotProbeDirectionV1::Token0In => "token0_in",
        SpotProbeDirectionV1::Token1In => "token1_in",
    }
}

#[derive(Default)]
struct RawLaneTotals {
    matched_borrow_prestate_observation_count: u64,
    underwriting_comparable_observation_count: u64,
    v1_gross_additional_headroom: BigUint,
    v1_actionable_additional_headroom: BigUint,
    dusk_gross_additional_headroom: BigUint,
    dusk_actionable_additional_headroom: BigUint,
    dusk_gross_to_actionable_additional_headroom_loss: BigUint,
    v1_gross_projected_total_debt: BigUint,
    v1_actionable_projected_total_debt: BigUint,
    dusk_gross_projected_total_debt: BigUint,
    dusk_actionable_projected_total_debt: BigUint,
    v1_borrowed: BigUint,
    dusk_borrowed: BigUint,
    dusk_unmet: BigUint,
    v1_supported_swap_input: BigUint,
    dusk_supported_swap_input: BigUint,
    revenue: BTreeMap<(RevenueVenueV1, RevenueOriginV1, RevenueLaneV1), BigUint>,
}

struct RawMarketTotals {
    token0: String,
    token1: String,
    base: RawLaneTotals,
    quote: RawLaneTotals,
    has_lending: bool,
}

fn aggregate_raw_market_metrics(
    input: &HoldoutMetricInputV1<'_>,
    window_start: i64,
    window_end: i64,
    raw_eligible: &BTreeSet<&str>,
) -> Result<BTreeMap<String, MarketMetricSetV1>> {
    let mut totals = BTreeMap::<String, RawMarketTotals>::new();
    let mut rows = 0_u64;
    for (line_index, line) in BufReader::new(File::open(input.paired_ledger_path)?)
        .lines()
        .enumerate()
    {
        let line =
            line.with_context(|| format!("reading paired ledger line {}", line_index + 1))?;
        if line.trim().is_empty() {
            bail!("blank paired ledger row at line {}", line_index + 1);
        }
        let row: PairedOperationMetricRowV1 = serde_json::from_str(&line)
            .with_context(|| format!("decoding paired ledger line {}", line_index + 1))?;
        rows = rows.checked_add(1).context("paired row count overflow")?;
        if row.profile_id != input.evidence.profile_id
            || row.arm != input.evidence.arm
            || row.hlp_scenario != input.hlp_scenario
        {
            bail!("raw metric row has the wrong profile or replay arm");
        }
        if input
            .opportunity_ceiling_market
            .is_some_and(|market| row.market != market)
        {
            continue;
        }
        if !raw_eligible.contains(row.market.as_str())
            || !(window_start..window_end).contains(&row.block_time_unix_seconds)
        {
            continue;
        }
        let market = totals
            .entry(row.market.clone())
            .or_insert_with(|| RawMarketTotals {
                token0: row.token0.clone(),
                token1: row.token1.clone(),
                base: RawLaneTotals::default(),
                quote: RawLaneTotals::default(),
                has_lending: false,
            });
        if market.token0 != row.token0 || market.token1 != row.token1 {
            bail!("raw metric market token identity changed");
        }
        if let Some(swap) = &row.swap {
            if !row.synthetic_arbitrage && !row.synthetic_protocol_settlement {
                if let Some(v1) = &swap.v1 {
                    let lane = if v1.asset_in_base {
                        &mut market.base
                    } else {
                        &mut market.quote
                    };
                    lane.v1_supported_swap_input += parse_token(&v1.reserve_credit_in)?;
                    if swap.dusk_realized {
                        if let Some(dusk) = &swap.dusk_quote {
                            lane.dusk_supported_swap_input += parse_token(&dusk.reserve_credit_in)?;
                        }
                    }
                }
            }
        }
        if let Some(borrow) = &row.borrow {
            market.has_lending = true;
            let lane = if borrow.debt_asset_base {
                &mut market.base
            } else {
                &mut market.quote
            };
            lane.matched_borrow_prestate_observation_count = lane
                .matched_borrow_prestate_observation_count
                .checked_add(1)
                .context("raw borrow observation count overflow")?;
            if borrow.dusk_current_underwriting_satisfied {
                lane.underwriting_comparable_observation_count = lane
                    .underwriting_comparable_observation_count
                    .checked_add(1)
                    .context("raw comparable borrow observation count overflow")?;
                let v1_gross = parse_token(&borrow.v1_gross_additional_headroom)?;
                let v1_actionable = parse_token(&borrow.v1_actionable_additional_headroom)?;
                let dusk_gross = parse_token(&borrow.dusk_gross_additional_headroom)?;
                let dusk_actionable = parse_token(&borrow.dusk_actionable_additional_headroom)?;
                if v1_actionable > v1_gross || dusk_actionable > dusk_gross {
                    bail!("raw additional-borrow headroom is not ordered");
                }
                lane.v1_gross_additional_headroom += v1_gross;
                lane.v1_actionable_additional_headroom += v1_actionable;
                lane.dusk_gross_to_actionable_additional_headroom_loss +=
                    &dusk_gross - &dusk_actionable;
                lane.dusk_gross_additional_headroom += dusk_gross;
                lane.dusk_actionable_additional_headroom += dusk_actionable;

                let v1_gross_total = parse_token(&borrow.v1_gross_admissible_max_debt)?;
                let v1_actionable_total = parse_token(&borrow.v1_actionable_max_debt)?;
                let dusk_gross_total = parse_token(&borrow.dusk_gross_admissible_max_debt)?;
                let dusk_actionable_total = parse_token(&borrow.dusk_actionable_max_debt)?;
                if v1_actionable_total > v1_gross_total || dusk_actionable_total > dusk_gross_total
                {
                    bail!("projected total-debt capacity is not ordered");
                }
                lane.v1_gross_projected_total_debt += v1_gross_total;
                lane.v1_actionable_projected_total_debt += v1_actionable_total;
                lane.dusk_gross_projected_total_debt += dusk_gross_total;
                lane.dusk_actionable_projected_total_debt += dusk_actionable_total;
            }
            let before = parse_token(&borrow.v1_actual_debt_before)?;
            let after = parse_token(&borrow.v1_actual_debt_after)?;
            if after > before {
                lane.v1_borrowed += after - before;
            }
            lane.dusk_borrowed += parse_token(&borrow.dusk_filled_change)?;
            lane.dusk_unmet += parse_token(&borrow.dusk_unmet_change)?;
        }
        let origin =
            revenue_origin_from_flags(row.synthetic_arbitrage, row.synthetic_protocol_settlement)?;
        if row.revenue.dusk_revenue_origin != origin {
            bail!("raw metric revenue origin differs from row provenance flags");
        }
        if origin == RevenueOriginV1::FactualUserFlow {
            add_revenue(
                &mut market.base.revenue,
                RevenueVenueV1::V1,
                RevenueOriginV1::FactualUserFlow,
                &row.revenue.v1_base,
            )?;
            add_revenue(
                &mut market.quote.revenue,
                RevenueVenueV1::V1,
                RevenueOriginV1::FactualUserFlow,
                &row.revenue.v1_quote,
            )?;
        }
        add_revenue(
            &mut market.base.revenue,
            RevenueVenueV1::Dusk,
            origin,
            &row.revenue.dusk_base,
        )?;
        add_revenue(
            &mut market.quote.revenue,
            RevenueVenueV1::Dusk,
            origin,
            &row.revenue.dusk_quote,
        )?;
    }
    if rows != input.paired_summary.row_count {
        bail!("raw metric ledger row count differs from committed summary");
    }

    totals
        .into_iter()
        .map(|(market, totals)| {
            let mut metrics = Vec::new();
            append_raw_lane_metrics(
                input,
                &market,
                "base",
                &totals.token0,
                &totals.base,
                &mut metrics,
            )?;
            append_raw_lane_metrics(
                input,
                &market,
                "quote",
                &totals.token1,
                &totals.quote,
                &mut metrics,
            )?;
            Ok((
                market.clone(),
                MarketMetricSetV1 {
                    market,
                    token0: totals.token0,
                    token1: totals.token1,
                    has_lending_activity: totals.has_lending,
                    raw_metrics: metrics,
                    usd_metrics: Vec::new(),
                },
            ))
        })
        .collect()
}

fn append_raw_lane_metrics(
    input: &HoldoutMetricInputV1<'_>,
    market: &str,
    lane_name: &str,
    mint: &str,
    lane: &RawLaneTotals,
    output: &mut Vec<NamedMetricV1>,
) -> Result<()> {
    let unit = format!("raw_token_atoms:{mint}");
    output.push(comparison_integer_market(
        input,
        market,
        &format!("spot.supported_factual_input.{lane_name}"),
        MetricFamily::SpotCapitalEfficiency,
        "Factual swap input executed by each mechanism.",
        &lane.v1_supported_swap_input,
        &lane.dusk_supported_swap_input,
        &unit,
    )?);
    append_raw_revenue_lane_metrics(input, market, lane_name, lane, &unit, output)?;
    if lane.matched_borrow_prestate_observation_count > 0 {
        if lane.underwriting_comparable_observation_count > 0 {
            output.push(comparison_integer_market(
                input,
                market,
                &format!(
                    "collateral.additional_headroom.gross.matched_borrow_prestate_observation_sum.{lane_name}"
                ),
                MetricFamily::CollateralEfficiency,
                "V1 and Dusk gross raw additional-borrow headroom summed over identical currently-underwritten factual borrow prestates.",
                &lane.v1_gross_additional_headroom,
                &lane.dusk_gross_additional_headroom,
                &unit,
            )?);
            output.push(comparison_integer_market(
                input,
                market,
                &format!(
                    "collateral.additional_headroom.actionable.matched_borrow_prestate_observation_sum.{lane_name}"
                ),
                MetricFamily::CollateralEfficiency,
                "V1 and Dusk actionable raw additional-borrow headroom summed over the same matched prestates.",
                &lane.v1_actionable_additional_headroom,
                &lane.dusk_actionable_additional_headroom,
                &unit,
            )?);
            output.push(dusk_only_integer_market(
                input,
                market,
                &format!(
                    "collateral.additional_headroom.gross_to_actionable_loss.matched_borrow_prestate_observation_sum.{lane_name}"
                ),
                MetricFamily::CollateralEfficiency,
                "Dusk gross minus actionable raw additional headroom on the currently-underwritten matched population.",
                &lane.dusk_gross_to_actionable_additional_headroom_loss,
                &unit,
            )?);
            output.push(comparison_integer_market(
                input,
                market,
                &format!(
                    "collateral.projected_total_debt.gross.matched_borrow_prestate_observation_sum.{lane_name}"
                ),
                MetricFamily::CollateralEfficiency,
                "Gross projected total-debt maxima on matched prestates; risk/LTV evidence only, not capacity-efficiency headroom.",
                &lane.v1_gross_projected_total_debt,
                &lane.dusk_gross_projected_total_debt,
                &unit,
            )?);
            output.push(comparison_integer_market(
                input,
                market,
                &format!(
                    "collateral.projected_total_debt.actionable.matched_borrow_prestate_observation_sum.{lane_name}"
                ),
                MetricFamily::CollateralEfficiency,
                "Actionable projected total-debt maxima on matched prestates; risk/LTV evidence only.",
                &lane.v1_actionable_projected_total_debt,
                &lane.dusk_actionable_projected_total_debt,
                &unit,
            )?);
        } else {
            for name in [
                "collateral.additional_headroom.gross.matched_borrow_prestate_observation_sum",
                "collateral.additional_headroom.actionable.matched_borrow_prestate_observation_sum",
                "collateral.additional_headroom.gross_to_actionable_loss.matched_borrow_prestate_observation_sum",
                "collateral.projected_total_debt.gross.matched_borrow_prestate_observation_sum",
                "collateral.projected_total_debt.actionable.matched_borrow_prestate_observation_sum",
            ] {
                output.push(unavailable_capacity_comparison_metric(
                    input,
                    market,
                    &format!("{name}.{lane_name}"),
                    &unit,
                ));
            }
        }
        output.push(comparison_integer_market(
            input,
            market,
            &format!("borrow.debt_increase_notional.{lane_name}"),
            MetricFamily::CollateralEfficiency,
            "Positive factual debt change versus share-rounded Dusk debt increase.",
            &lane.v1_borrowed,
            &lane.dusk_borrowed,
            &unit,
        )?);
        output.push(dusk_only_integer_market(
            input,
            market,
            &format!("borrow.unmet_demand.{lane_name}"),
            MetricFamily::CollateralEfficiency,
            "Unfilled capacity-preserving Dusk debt demand.",
            &lane.dusk_unmet,
            &unit,
        )?);
    } else {
        for name in [
            "collateral.additional_headroom.gross.matched_borrow_prestate_observation_sum",
            "collateral.additional_headroom.actionable.matched_borrow_prestate_observation_sum",
            "collateral.additional_headroom.gross_to_actionable_loss.matched_borrow_prestate_observation_sum",
            "collateral.projected_total_debt.gross.matched_borrow_prestate_observation_sum",
            "collateral.projected_total_debt.actionable.matched_borrow_prestate_observation_sum",
            "borrow.debt_increase_notional",
            "borrow.unmet_demand",
        ] {
            output.push(unavailable_lending_metric(
                input,
                market,
                &format!("{name}.{lane_name}"),
                &unit,
            ));
        }
    }
    Ok(())
}

fn unavailable_capacity_comparison_metric(
    input: &HoldoutMetricInputV1<'_>,
    market: &str,
    name: &str,
    unit: &str,
) -> NamedMetricV1 {
    let value = MetricValueV1::unavailable(MetricAvailability::NotEvaluated, unit);
    NamedMetricV1 {
        metric_id: metric_id(input, Some(market), name),
        family: MetricFamily::CollateralEfficiency,
        profile_id: Some(input.evidence.profile_id.clone()),
        replay_arm: Some(input.evidence.arm),
        hlp_scenario: Some(input.hlp_scenario),
        definition: "Matched borrow prestates exist, but none satisfy current Dusk underwriting; comparable capacity is not evaluated as economic zero.".to_owned(),
        revenue_dimensions: None,
        v1: value.clone(),
        dusk: value.clone(),
        paired_delta: value,
    }
}

fn add_revenue(
    target: &mut BTreeMap<(RevenueVenueV1, RevenueOriginV1, RevenueLaneV1), BigUint>,
    venue: RevenueVenueV1,
    origin: RevenueOriginV1,
    side: &RevenueSideDeltaV1,
) -> Result<()> {
    for lane in RevenueLaneV1::ALL {
        *target.entry((venue, origin, lane)).or_default() += parse_token(side.amount(lane))?;
    }
    Ok(())
}

fn append_raw_revenue_lane_metrics(
    input: &HoldoutMetricInputV1<'_>,
    market: &str,
    token_lane: &str,
    totals: &RawLaneTotals,
    unit: &str,
    output: &mut Vec<NamedMetricV1>,
) -> Result<()> {
    for origin in [
        RevenueOriginV1::FactualUserFlow,
        RevenueOriginV1::SyntheticArbitrage,
        RevenueOriginV1::SyntheticProtocolSettlement,
    ] {
        for lane in RevenueLaneV1::ALL {
            let v1 = (origin == RevenueOriginV1::FactualUserFlow).then(|| {
                totals
                    .revenue
                    .get(&(RevenueVenueV1::V1, origin, lane))
                    .cloned()
                    .unwrap_or_default()
            });
            let dusk = totals
                .revenue
                .get(&(RevenueVenueV1::Dusk, origin, lane))
                .cloned()
                .unwrap_or_default();
            let v1_value = v1
                .as_ref()
                .map(|value| raw_revenue_metric_value(RevenueVenueV1::V1, lane, value, unit))
                .transpose()?
                .unwrap_or_else(|| {
                    MetricValueV1::unavailable(
                        MetricAvailability::NotApplicableVenueMechanism,
                        unit,
                    )
                });
            let dusk_value = raw_revenue_metric_value(RevenueVenueV1::Dusk, lane, &dusk, unit)?;
            let paired_delta = if v1_value.availability == MetricAvailability::Available
                && dusk_value.availability == MetricAvailability::Available
            {
                let v1 = v1.as_ref().context("available V1 raw revenue is absent")?;
                signed_integer_metric(
                    &(BigInt::from_biguint(Sign::Plus, dusk.clone())
                        - BigInt::from_biguint(Sign::Plus, v1.clone())),
                    unit,
                )?
            } else {
                MetricValueV1::unavailable(
                    if v1_value.availability != MetricAvailability::Available {
                        v1_value.availability.clone()
                    } else {
                        dusk_value.availability.clone()
                    },
                    unit,
                )
            };
            output.push(NamedMetricV1 {
                metric_id: metric_id(
                    input,
                    Some(market),
                    &format!(
                        "revenue.lane.{}.origin.{}.{}",
                        revenue_lane_slug(lane),
                        revenue_origin_slug(origin),
                        token_lane
                    ),
                ),
                family: revenue_lane_family(lane),
                profile_id: Some(input.evidence.profile_id.clone()),
                replay_arm: Some(input.evidence.arm),
                hlp_scenario: Some(input.hlp_scenario),
                definition: format!(
                    "Exact raw-token {:?} lane with {:?} accounting; no lane or origin is merged.",
                    lane,
                    lane.accounting_class()
                ),
                revenue_dimensions: Some(RevenueMetricDimensionsV1 {
                    lane,
                    accounting_class: lane.accounting_class(),
                    origin,
                    v1_venue: (origin == RevenueOriginV1::FactualUserFlow)
                        .then_some(RevenueVenueV1::V1),
                    dusk_venue: Some(RevenueVenueV1::Dusk),
                }),
                v1: v1_value,
                dusk: dusk_value,
                paired_delta,
            });
        }
    }
    Ok(())
}

fn raw_revenue_metric_value(
    venue: RevenueVenueV1,
    lane: RevenueLaneV1,
    value: &BigUint,
    unit: &str,
) -> Result<MetricValueV1> {
    let availability = match (venue, lane) {
        (
            RevenueVenueV1::V1,
            RevenueLaneV1::RetainedRecenterInventory
            | RevenueLaneV1::CompoundedLpSwapPrincipal
            | RevenueLaneV1::HlpFundingInterestEarned
            | RevenueLaneV1::ProtocolFeeAuctionHlpFundingInterestEarned
            | RevenueLaneV1::ProtocolBuybackHlpFundingInterestEarned,
        )
        | (
            RevenueVenueV1::Dusk,
            RevenueLaneV1::FlashloanFeeV1Only | RevenueLaneV1::ProtocolClaimRealizedV1Only,
        ) => Some(MetricAvailability::NotApplicableVenueMechanism),
        (RevenueVenueV1::V1, RevenueLaneV1::LpPublicInterestCashRealized) => {
            Some(MetricAvailability::MissingCashRealizationEvidence)
        }
        (
            RevenueVenueV1::V1,
            RevenueLaneV1::ProtocolBuybackSwapEarned
            | RevenueLaneV1::ProtocolBuybackPublicInterestCashRealized,
        ) => Some(MetricAvailability::NotApplicableVenueMechanism),
        _ => None,
    };
    if let Some(availability) = availability {
        if !value.is_zero() {
            bail!("structurally unavailable raw revenue lane is nonzero");
        }
        Ok(MetricValueV1::unavailable(availability, unit))
    } else {
        integer_metric(value, unit)
    }
}

fn merge_market_metrics(
    target: &mut BTreeMap<String, MarketMetricSetV1>,
    incoming: BTreeMap<String, MarketMetricSetV1>,
) -> Result<()> {
    for (market, mut set) in incoming {
        match target.get_mut(&market) {
            Some(existing) => {
                if !set.token0.is_empty() {
                    if (!existing.token0.is_empty() && existing.token0 != set.token0)
                        || (!existing.token1.is_empty() && existing.token1 != set.token1)
                    {
                        bail!("market metric token identity differs across evidence sets");
                    }
                    existing.token0 = set.token0;
                    existing.token1 = set.token1;
                }
                existing.has_lending_activity |= set.has_lending_activity;
                existing.raw_metrics.append(&mut set.raw_metrics);
                existing.usd_metrics.append(&mut set.usd_metrics);
            }
            None => {
                target.insert(market, set);
            }
        }
    }
    Ok(())
}

fn comparison_rational(
    input: &HoldoutMetricInputV1<'_>,
    name: &str,
    family: MetricFamily,
    definition: &str,
    v1: &ExactRationalV1,
    dusk: &ExactRationalV1,
    unit: &str,
) -> Result<NamedMetricV1> {
    metric_rational(
        input,
        None,
        name,
        family,
        definition,
        Some(v1),
        Some(dusk),
        unit,
    )
}

fn comparison_available_rational(
    input: &HoldoutMetricInputV1<'_>,
    name: &str,
    family: MetricFamily,
    definition: &str,
    v1: &AvailableExactRationalV1,
    dusk: &AvailableExactRationalV1,
    unit: &str,
) -> Result<NamedMetricV1> {
    metric_available_rational(
        input,
        None,
        name,
        family,
        definition,
        Some(v1),
        Some(dusk),
        unit,
    )
}

fn comparison_rational_market(
    input: &HoldoutMetricInputV1<'_>,
    market: &str,
    name: &str,
    family: MetricFamily,
    definition: &str,
    v1: &ExactRationalV1,
    dusk: &ExactRationalV1,
    unit: &str,
) -> Result<NamedMetricV1> {
    metric_rational(
        input,
        Some(market),
        name,
        family,
        definition,
        Some(v1),
        Some(dusk),
        unit,
    )
}

fn comparison_available_rational_market(
    input: &HoldoutMetricInputV1<'_>,
    market: &str,
    name: &str,
    family: MetricFamily,
    definition: &str,
    v1: &AvailableExactRationalV1,
    dusk: &AvailableExactRationalV1,
    unit: &str,
) -> Result<NamedMetricV1> {
    metric_available_rational(
        input,
        Some(market),
        name,
        family,
        definition,
        Some(v1),
        Some(dusk),
        unit,
    )
}

fn dusk_only_rational(
    input: &HoldoutMetricInputV1<'_>,
    name: &str,
    family: MetricFamily,
    definition: &str,
    dusk: &ExactRationalV1,
    unit: &str,
) -> Result<NamedMetricV1> {
    metric_rational(
        input,
        None,
        name,
        family,
        definition,
        None,
        Some(dusk),
        unit,
    )
}

fn dusk_only_available_rational(
    input: &HoldoutMetricInputV1<'_>,
    name: &str,
    family: MetricFamily,
    definition: &str,
    dusk: &AvailableExactRationalV1,
    unit: &str,
) -> Result<NamedMetricV1> {
    metric_available_rational(
        input,
        None,
        name,
        family,
        definition,
        None,
        Some(dusk),
        unit,
    )
}

fn dusk_only_rational_market(
    input: &HoldoutMetricInputV1<'_>,
    market: &str,
    name: &str,
    family: MetricFamily,
    definition: &str,
    dusk: &ExactRationalV1,
    unit: &str,
) -> Result<NamedMetricV1> {
    metric_rational(
        input,
        Some(market),
        name,
        family,
        definition,
        None,
        Some(dusk),
        unit,
    )
}

#[allow(clippy::too_many_arguments)]
fn metric_rational(
    input: &HoldoutMetricInputV1<'_>,
    market: Option<&str>,
    name: &str,
    family: MetricFamily,
    definition: &str,
    v1: Option<&ExactRationalV1>,
    dusk: Option<&ExactRationalV1>,
    unit: &str,
) -> Result<NamedMetricV1> {
    let delta = match (v1, dusk) {
        (Some(v1), Some(dusk)) => Some(exact_rational(dusk)? - exact_rational(v1)?),
        _ => None,
    };
    Ok(NamedMetricV1 {
        metric_id: metric_id(input, market, name),
        family,
        profile_id: Some(input.evidence.profile_id.clone()),
        replay_arm: Some(input.evidence.arm),
        hlp_scenario: Some(input.hlp_scenario),
        definition: definition.to_owned(),
        revenue_dimensions: None,
        v1: v1
            .map(|value| exact_metric(value, unit))
            .transpose()?
            .unwrap_or_else(|| unavailable(unit)),
        dusk: dusk
            .map(|value| exact_metric(value, unit))
            .transpose()?
            .unwrap_or_else(|| unavailable(unit)),
        paired_delta: delta
            .as_ref()
            .map(|value| rational_metric(value, unit))
            .transpose()?
            .unwrap_or_else(|| unavailable(unit)),
    })
}

#[allow(clippy::too_many_arguments)]
fn metric_available_rational(
    input: &HoldoutMetricInputV1<'_>,
    market: Option<&str>,
    name: &str,
    family: MetricFamily,
    definition: &str,
    v1: Option<&AvailableExactRationalV1>,
    dusk: Option<&AvailableExactRationalV1>,
    unit: &str,
) -> Result<NamedMetricV1> {
    let v1_metric = v1
        .map(|value| available_exact_metric(value, unit))
        .transpose()?
        .unwrap_or_else(|| {
            MetricValueV1::unavailable(MetricAvailability::NotApplicableVenueMechanism, unit)
        });
    let dusk_metric = dusk
        .map(|value| available_exact_metric(value, unit))
        .transpose()?
        .unwrap_or_else(|| {
            MetricValueV1::unavailable(MetricAvailability::NotApplicableVenueMechanism, unit)
        });
    let paired_delta = match (v1, dusk) {
        (Some(v1), Some(dusk))
            if v1.availability == MetricAvailability::Available
                && dusk.availability == MetricAvailability::Available =>
        {
            let delta =
                exact_rational(available_exact(dusk)?)? - exact_rational(available_exact(v1)?)?;
            rational_metric(&delta, unit)?
        }
        (Some(v1), Some(dusk)) => MetricValueV1::unavailable(
            if v1.availability != MetricAvailability::Available {
                v1.availability.clone()
            } else {
                dusk.availability.clone()
            },
            unit,
        ),
        _ => MetricValueV1::unavailable(MetricAvailability::NotApplicableVenueMechanism, unit),
    };
    Ok(NamedMetricV1 {
        metric_id: metric_id(input, market, name),
        family,
        profile_id: Some(input.evidence.profile_id.clone()),
        replay_arm: Some(input.evidence.arm),
        hlp_scenario: Some(input.hlp_scenario),
        definition: definition.to_owned(),
        revenue_dimensions: None,
        v1: v1_metric,
        dusk: dusk_metric,
        paired_delta,
    })
}

fn delta_only_rational(
    input: &HoldoutMetricInputV1<'_>,
    name: &str,
    family: MetricFamily,
    definition: &str,
    delta: Option<&ExactRationalV1>,
    unit: &str,
) -> Result<NamedMetricV1> {
    delta_only_rational_scoped(input, None, name, family, definition, delta, unit)
}

#[allow(clippy::too_many_arguments)]
fn delta_only_rational_scoped(
    input: &HoldoutMetricInputV1<'_>,
    market: Option<&str>,
    name: &str,
    family: MetricFamily,
    definition: &str,
    delta: Option<&ExactRationalV1>,
    unit: &str,
) -> Result<NamedMetricV1> {
    Ok(NamedMetricV1 {
        metric_id: metric_id(input, market, name),
        family,
        profile_id: Some(input.evidence.profile_id.clone()),
        replay_arm: Some(input.evidence.arm),
        hlp_scenario: Some(input.hlp_scenario),
        definition: definition.to_owned(),
        revenue_dimensions: None,
        v1: unavailable(unit),
        dusk: unavailable(unit),
        paired_delta: delta
            .map(|value| exact_metric(value, unit))
            .transpose()?
            .unwrap_or_else(|| unavailable(unit)),
    })
}

fn delta_only_signed(
    input: &HoldoutMetricInputV1<'_>,
    name: &str,
    family: MetricFamily,
    definition: &str,
    delta: Option<&SignedTokenAmount>,
    unit: &str,
) -> NamedMetricV1 {
    NamedMetricV1 {
        metric_id: metric_id(input, None, name),
        family,
        profile_id: Some(input.evidence.profile_id.clone()),
        replay_arm: Some(input.evidence.arm),
        hlp_scenario: Some(input.hlp_scenario),
        definition: definition.to_owned(),
        revenue_dimensions: None,
        v1: unavailable(unit),
        dusk: unavailable(unit),
        paired_delta: delta
            .cloned()
            .map(|value| MetricValueV1 {
                availability: MetricAvailability::Available,
                numerator: Some(value),
                denominator: Some(one()),
                unit: unit.to_owned(),
                denominator_definition: None,
            })
            .unwrap_or_else(|| unavailable(unit)),
    }
}

fn comparison_integer_market(
    input: &HoldoutMetricInputV1<'_>,
    market: &str,
    name: &str,
    family: MetricFamily,
    definition: &str,
    v1: &BigUint,
    dusk: &BigUint,
    unit: &str,
) -> Result<NamedMetricV1> {
    let delta = BigInt::from_biguint(Sign::Plus, dusk.clone())
        - BigInt::from_biguint(Sign::Plus, v1.clone());
    Ok(NamedMetricV1 {
        metric_id: metric_id(input, Some(market), name),
        family,
        profile_id: Some(input.evidence.profile_id.clone()),
        replay_arm: Some(input.evidence.arm),
        hlp_scenario: Some(input.hlp_scenario),
        definition: definition.to_owned(),
        revenue_dimensions: None,
        v1: integer_metric(v1, unit)?,
        dusk: integer_metric(dusk, unit)?,
        paired_delta: signed_integer_metric(&delta, unit)?,
    })
}

fn dusk_only_integer_market(
    input: &HoldoutMetricInputV1<'_>,
    market: &str,
    name: &str,
    family: MetricFamily,
    definition: &str,
    dusk: &BigUint,
    unit: &str,
) -> Result<NamedMetricV1> {
    Ok(NamedMetricV1 {
        metric_id: metric_id(input, Some(market), name),
        family,
        profile_id: Some(input.evidence.profile_id.clone()),
        replay_arm: Some(input.evidence.arm),
        hlp_scenario: Some(input.hlp_scenario),
        definition: definition.to_owned(),
        revenue_dimensions: None,
        v1: unavailable(unit),
        dusk: integer_metric(dusk, unit)?,
        paired_delta: unavailable(unit),
    })
}

fn unavailable_lending_metric(
    input: &HoldoutMetricInputV1<'_>,
    market: &str,
    name: &str,
    unit: &str,
) -> NamedMetricV1 {
    unavailable_lending_metric_scoped(input, Some(market), name, unit)
}

fn unavailable_lending_metric_scoped(
    input: &HoldoutMetricInputV1<'_>,
    market: Option<&str>,
    name: &str,
    unit: &str,
) -> NamedMetricV1 {
    let value =
        MetricValueV1::unavailable(MetricAvailability::NotApplicableNoLendingActivity, unit);
    NamedMetricV1 {
        metric_id: metric_id(input, market, name),
        family: MetricFamily::CollateralEfficiency,
        profile_id: Some(input.evidence.profile_id.clone()),
        replay_arm: Some(input.evidence.arm),
        hlp_scenario: Some(input.hlp_scenario),
        definition: "No lending activity occurred in this market/window; N/A is not economic zero."
            .to_owned(),
        revenue_dimensions: None,
        v1: value.clone(),
        dusk: value.clone(),
        paired_delta: value,
    }
}

fn exact_metric(value: &ExactRationalV1, unit: &str) -> Result<MetricValueV1> {
    MetricValueV1::exact_components(
        value.numerator.clone(),
        value.denominator.clone(),
        unit,
        None,
    )
    .map_err(Into::into)
}

fn available_exact_metric(value: &AvailableExactRationalV1, unit: &str) -> Result<MetricValueV1> {
    match value.availability {
        MetricAvailability::Available => exact_metric(available_exact(value)?, unit),
        _ => {
            if value.value.is_some() {
                bail!("unavailable exact aggregate carries a numeric value");
            }
            Ok(MetricValueV1::unavailable(value.availability.clone(), unit))
        }
    }
}

fn rational_metric(value: &BigRational, unit: &str) -> Result<MetricValueV1> {
    MetricValueV1::exact_components(
        signed_token(value.numer()),
        token_bigint(value.denom())?,
        unit,
        None,
    )
    .map_err(Into::into)
}

fn integer_metric(value: &BigUint, unit: &str) -> Result<MetricValueV1> {
    MetricValueV1::exact_components(
        signed_token(&BigInt::from_biguint(Sign::Plus, value.clone())),
        one(),
        unit,
        None,
    )
    .map_err(Into::into)
}

fn signed_integer_metric(value: &BigInt, unit: &str) -> Result<MetricValueV1> {
    MetricValueV1::exact_components(signed_token(value), one(), unit, None).map_err(Into::into)
}

fn unavailable(unit: &str) -> MetricValueV1 {
    MetricValueV1::unavailable(MetricAvailability::NotEvaluated, unit)
}

fn metric_id(input: &HoldoutMetricInputV1<'_>, market: Option<&str>, name: &str) -> String {
    let arm = match input.evidence.arm {
        ReplayArm::FixedFlow => "fixed_flow",
        ReplayArm::Intent => "intent",
    };
    let hlp = match input.hlp_scenario {
        HlpScenario::NoHlp => "no_hlp",
        HlpScenario::MaximumFeasible => "maximum_feasible_hlp",
    };
    let scope = market
        .map(|market| {
            if input.opportunity_ceiling_market == Some(market) {
                format!("market.{market}.opportunity_ceiling")
            } else {
                format!("market.{market}.global_profile")
            }
        })
        .unwrap_or_else(|| "aggregate.global_profile".to_owned());
    format!(
        "{scope}.profile.{}.arm.{arm}.scenario.{hlp}.{name}",
        input.evidence.profile_id
    )
}

fn exact_rational(value: &ExactRationalV1) -> Result<BigRational> {
    let numerator =
        BigInt::from_str(value.numerator.as_str()).context("invalid exact numerator")?;
    let denominator =
        BigInt::from_str(value.denominator.as_str()).context("invalid exact denominator")?;
    if denominator <= BigInt::zero() {
        bail!("exact rational denominator is not positive");
    }
    Ok(BigRational::new(numerator, denominator))
}

fn parse_token(value: &TokenAmount) -> Result<BigUint> {
    BigUint::from_str(value.as_str()).context("invalid raw token amount")
}

fn signed_token(value: &BigInt) -> SignedTokenAmount {
    SignedTokenAmount::new(value.to_string()).expect("BigInt formatting is canonical")
}

fn token_bigint(value: &BigInt) -> Result<TokenAmount> {
    if value.sign() == Sign::Minus {
        bail!("unsigned metric denominator is negative");
    }
    TokenAmount::new(value.to_string()).context("invalid metric denominator")
}

fn unsigned_token(value: BigUint) -> TokenAmount {
    TokenAmount::new(value.to_string()).expect("BigUint formatting is canonical")
}

fn one() -> TokenAmount {
    TokenAmount::new("1").expect("one is canonical")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        canonical::ChainOrder,
        leverage::{
            AdversePathAvailabilityV1, FundingAprBenchmarkV1, LiquidationBenchmarkV1,
            OpeningBenchmarkV1, UnwindBenchmarkV1, UnwindLimitV1, VenueLeverageBenchmarkV1,
            LEVERAGE_BENCHMARK_SCHEMA_VERSION,
        },
        leverage_campaign::{
            LeverageUnwindHorizonV1, RealizedAdverseExcursionEvidenceV1,
            LEVERAGE_CAMPAIGN_SCHEMA_VERSION,
        },
    };

    #[test]
    fn raw_v1_protocol_cash_is_available_without_fabricating_lp_or_buyback_cash() {
        let exact = raw_revenue_metric_value(
            RevenueVenueV1::V1,
            RevenueLaneV1::ProtocolFeeAuctionPublicInterestCashRealized,
            &BigUint::from(7_u8),
            "token_atoms",
        )
        .unwrap();
        assert_eq!(exact.availability, MetricAvailability::Available);
        assert_eq!(exact.numerator.as_ref().unwrap().as_str(), "7");

        let lp = raw_revenue_metric_value(
            RevenueVenueV1::V1,
            RevenueLaneV1::LpPublicInterestCashRealized,
            &BigUint::zero(),
            "token_atoms",
        )
        .unwrap();
        assert_eq!(
            lp.availability,
            MetricAvailability::MissingCashRealizationEvidence
        );
        let buyback = raw_revenue_metric_value(
            RevenueVenueV1::V1,
            RevenueLaneV1::ProtocolBuybackPublicInterestCashRealized,
            &BigUint::zero(),
            "token_atoms",
        )
        .unwrap();
        assert_eq!(
            buyback.availability,
            MetricAvailability::NotApplicableVenueMechanism
        );
    }

    fn exact(numerator: i128, denominator: u128) -> ExactRationalV1 {
        ExactRationalV1 {
            numerator: SignedTokenAmount::new(numerator.to_string()).unwrap(),
            denominator: TokenAmount::new(denominator.to_string()).unwrap(),
        }
    }

    fn token(value: u128) -> TokenAmount {
        TokenAmount::new(value.to_string()).unwrap()
    }

    fn signed(value: i128) -> SignedTokenAmount {
        SignedTokenAmount::new(value.to_string()).unwrap()
    }

    fn ratio(numerator: u128, denominator: u128, unbounded: bool) -> ExactRatioV1 {
        ExactRatioV1 {
            numerator: token(numerator),
            denominator: token(denominator),
            unbounded,
        }
    }

    fn venue(
        threshold_kind: LiquidationThresholdKindV1,
        theoretical: ExactRatioV1,
        executable: ExactRatioV1,
        opening_cost: i128,
        liquidity: u128,
        funding_apr: u128,
        initial_buffer: i128,
        maintenance_buffer: i128,
        unwind_impact: u128,
        unwind_cost: i128,
    ) -> VenueLeverageBenchmarkV1 {
        VenueLeverageBenchmarkV1 {
            theoretical_leverage: theoretical,
            executable_leverage: executable,
            opening: OpeningBenchmarkV1 {
                starting_margin_gross: token(100),
                starting_margin_net: token(100),
                borrowed_amount: token(100),
                deployed_notional: token(200),
                collateral_acquired: token(180),
                transfer_fees_paid: token(0),
                immediate_closeout_value: token(175),
                opening_cost: signed(opening_cost),
                limiting_borrow_liquidity: token(liquidity),
                loop_iterations: token(1),
                loop_iteration_limit_reached: false,
            },
            funding: FundingAprBenchmarkV1 {
                utilization_scale: "nad".to_owned(),
                utilization: token(500_000_000_000),
                target_utilization_bps: Some(token(8_000)),
                rate_at_target_nad: Some(token(50_000_000_000)),
                instantaneous_borrow_apr_nad: token(funding_apr),
            },
            liquidation: LiquidationBenchmarkV1 {
                threshold_kind,
                threshold: token(150),
                initial_margin_bps: token(1_000),
                maintenance_margin_bps: token(500),
                equity_bps: token(2_000),
                initial_buffer_bps: signed(initial_buffer),
                maintenance_buffer_bps: signed(maintenance_buffer),
                adverse_path_availability: AdversePathAvailabilityV1::Complete,
                first_adverse_path_breach: None,
                standardized_max_adverse_excursion_bps: token(2_500),
                realized_max_adverse_excursion_bps: Some(token(2_000)),
                realized_mae_source_sha256: Some("a".repeat(64)),
            },
            unwind: UnwindBenchmarkV1 {
                executable_full_unwind: true,
                limiting_reason: UnwindLimitV1::None,
                collateral_sold: token(180),
                gross_debt_output: token(170),
                terminal_owner_debt_balance: token(70),
                remaining_debt: token(0),
                remaining_collateral: token(0),
                transfer_fees_paid: token(0),
                unwind_impact_bps: token(unwind_impact),
                cost_vs_starting_margin: signed(unwind_cost),
                iterations: token(1),
            },
        }
    }

    fn leverage_cell() -> LeverageCampaignCellV1 {
        let cell_id = "c".repeat(64);
        LeverageCampaignCellV1 {
            schema_version: LEVERAGE_CAMPAIGN_SCHEMA_VERSION,
            cell_id: cell_id.clone(),
            market: "market-a".to_owned(),
            checkpoint_kind: LeverageCheckpointKindV1::Median,
            operation_id: "operation".to_owned(),
            order: ChainOrder {
                slot: 10,
                transaction_index: 0,
                outer_instruction_index: 0,
                inner_execution_index: None,
                event_sequence: 0,
            },
            block_time_unix_seconds: 10,
            profile_id: "p".repeat(64),
            arm: ReplayArm::Intent,
            hlp_scenario: HlpScenario::MaximumFeasible,
            direction: LeverageDirectionV1::LongBase,
            starting_margin_nav_bps: 100,
            factual_accounted_pool_nav_in_debt_atoms: Some(token(10_000)),
            starting_margin_gross: Some(token(100)),
            v1_pre_state_sha256: Some("1".repeat(64)),
            dusk_pre_state_sha256: Some("2".repeat(64)),
            realized_adverse_excursion: Some(RealizedAdverseExcursionEvidenceV1 {
                mark_count: 2,
                first_slot: 10,
                last_slot: 20,
                entry_collateral_price_nad: token(1_000),
                worst_collateral_price_nad: token(800),
                adverse_excursion_bps: token(2_000),
                path_end_unix_seconds_exclusive: 30,
                source_sha256: "a".repeat(64),
            }),
            dusk_adverse_path_availability: AdversePathAvailabilityV1::Complete,
            unwind_horizon: LeverageUnwindHorizonV1::ImmediateSameCheckpoint,
            core_benchmark_available: true,
            complete_for_headline_path_claim: true,
            blockers: Vec::new(),
            benchmark: Some(LeverageBenchmarkResultV1 {
                schema_version: LEVERAGE_BENCHMARK_SCHEMA_VERSION,
                benchmark_id: cell_id,
                direction: LeverageDirectionV1::LongBase,
                hlp_scenario: HlpScenario::MaximumFeasible,
                v1: venue(
                    LiquidationThresholdKindV1::CollateralPriceNad,
                    ratio(3, 2, false),
                    ratio(7, 4, false),
                    -5,
                    500,
                    40,
                    1_000,
                    1_500,
                    200,
                    -10,
                ),
                dusk: venue(
                    LiquidationThresholdKindV1::CloseoutValue,
                    ratio(2, 1, false),
                    ratio(9, 5, false),
                    7,
                    700,
                    30,
                    1_200,
                    1_700,
                    100,
                    5,
                ),
            }),
            cell_sha256: "d".repeat(64),
        }
    }

    fn leverage_market() -> MarketMetricSetV1 {
        MarketMetricSetV1 {
            market: "market-a".to_owned(),
            token0: "token-0".to_owned(),
            token1: "token-1".to_owned(),
            has_lending_activity: true,
            raw_metrics: Vec::new(),
            usd_metrics: Vec::new(),
        }
    }

    #[test]
    fn exact_coverage_cross_multiplies_without_rounding() {
        let coverage = exact_coverage(&exact(1, 3), &exact(2, 3), 5_000).unwrap();
        assert_eq!(coverage.covered_numerator.as_str(), "1");
        assert_eq!(coverage.total_denominator.as_str(), "2");
        assert_eq!(coverage.actual_bps, Some(5_000));
        assert!(coverage.passes);
    }

    #[test]
    fn zero_notional_coverage_is_explicitly_not_applicable() {
        let coverage = exact_coverage(&exact(0, 1), &exact(0, 1), 9_500).unwrap();
        assert_eq!(
            coverage.availability,
            MetricAvailability::NotApplicableNoLendingActivity
        );
        assert!(coverage.passes);
    }

    fn capacity_fixture() -> (
        ExactRationalV1,
        ExactRationalV1,
        ExactRationalV1,
        DuskBorrowCapacityAggregateV1,
        BorrowProjectedTotalDebtAggregateV1,
    ) {
        let constraints = CAPACITY_CONSTRAINTS
            .iter()
            .map(
                |(constraint, _)| crate::aggregate::DuskBorrowCapacityConstraintAggregateV1 {
                    constraint: *constraint,
                    limiting_matched_observation_count: u64::from(
                        *constraint == DuskBorrowCapacityConstraintV1::Cash,
                    ),
                    tied_additional_headroom_loss_usd_micro: if *constraint
                        == DuskBorrowCapacityConstraintV1::Cash
                    {
                        exact(20, 1)
                    } else {
                        exact(0, 1)
                    },
                },
            )
            .collect();
        (
            exact(100, 1),
            exact(90, 1),
            exact(80, 1),
            DuskBorrowCapacityAggregateV1 {
                matched_borrow_prestate_observation_count: 2,
                underwriting_comparable_observation_count: 1,
                headroom_ratio_observation_count: 1,
                gross_additional_headroom_matched_observation_sum_usd_micro: exact(100, 1),
                actionable_additional_headroom_matched_observation_sum_usd_micro: exact(80, 1),
                gross_to_actionable_additional_headroom_loss_usd_micro: exact(20, 1),
                actionable_to_gross_additional_headroom_bps_p50_exact: Some(exact(8_000, 1)),
                actionable_to_gross_additional_headroom_bps_p95_exact: Some(exact(8_000, 1)),
                zero_gross_headroom_observation_count: 0,
                current_underwriting_unsatisfied_observation_count: 1,
                current_global_health_floor_unsatisfied_observation_count: 1,
                constraints,
            },
            BorrowProjectedTotalDebtAggregateV1 {
                matched_observation_count: 1,
                v1_gross_max_debt_observation_sum_usd_micro: exact(120, 1),
                v1_actionable_max_debt_observation_sum_usd_micro: exact(110, 1),
                dusk_gross_max_debt_observation_sum_usd_micro: exact(130, 1),
                dusk_actionable_max_debt_observation_sum_usd_micro: exact(105, 1),
            },
        )
    }

    #[test]
    fn capacity_manifest_validation_preserves_headroom_and_total_debt_identities() {
        let (v1_gross, v1_actionable, dusk_actionable, mut capacity, mut projected) =
            capacity_fixture();
        validate_capacity_evidence(
            &v1_gross,
            &v1_actionable,
            &dusk_actionable,
            &capacity,
            &projected,
        )
        .unwrap();

        capacity.gross_to_actionable_additional_headroom_loss_usd_micro = exact(19, 1);
        assert!(validate_capacity_evidence(
            &v1_gross,
            &v1_actionable,
            &dusk_actionable,
            &capacity,
            &projected,
        )
        .is_err());
        capacity.gross_to_actionable_additional_headroom_loss_usd_micro = exact(20, 1);

        projected.dusk_actionable_max_debt_observation_sum_usd_micro = exact(131, 1);
        assert!(validate_capacity_evidence(
            &v1_gross,
            &v1_actionable,
            &dusk_actionable,
            &capacity,
            &projected,
        )
        .is_err());
    }

    #[test]
    fn capacity_manifest_validation_rejects_bad_ties_and_unhealthy_numeric_parity() {
        let (v1_gross, v1_actionable, dusk_actionable, mut capacity, projected) =
            capacity_fixture();
        capacity.constraints[0].limiting_matched_observation_count = 2;
        assert!(validate_capacity_evidence(
            &v1_gross,
            &v1_actionable,
            &dusk_actionable,
            &capacity,
            &projected,
        )
        .is_err());

        let constraints = CAPACITY_CONSTRAINTS
            .iter()
            .map(
                |(constraint, _)| crate::aggregate::DuskBorrowCapacityConstraintAggregateV1 {
                    constraint: *constraint,
                    limiting_matched_observation_count: 0,
                    tied_additional_headroom_loss_usd_micro: exact(0, 1),
                },
            )
            .collect();
        let incomparable = DuskBorrowCapacityAggregateV1 {
            matched_borrow_prestate_observation_count: 1,
            underwriting_comparable_observation_count: 0,
            headroom_ratio_observation_count: 0,
            gross_additional_headroom_matched_observation_sum_usd_micro: exact(0, 1),
            actionable_additional_headroom_matched_observation_sum_usd_micro: exact(0, 1),
            gross_to_actionable_additional_headroom_loss_usd_micro: exact(0, 1),
            actionable_to_gross_additional_headroom_bps_p50_exact: None,
            actionable_to_gross_additional_headroom_bps_p95_exact: None,
            zero_gross_headroom_observation_count: 0,
            current_underwriting_unsatisfied_observation_count: 1,
            current_global_health_floor_unsatisfied_observation_count: 1,
            constraints,
        };
        let empty_projected = BorrowProjectedTotalDebtAggregateV1 {
            matched_observation_count: 0,
            v1_gross_max_debt_observation_sum_usd_micro: exact(0, 1),
            v1_actionable_max_debt_observation_sum_usd_micro: exact(0, 1),
            dusk_gross_max_debt_observation_sum_usd_micro: exact(0, 1),
            dusk_actionable_max_debt_observation_sum_usd_micro: exact(0, 1),
        };
        validate_capacity_evidence(
            &exact(0, 1),
            &exact(0, 1),
            &exact(0, 1),
            &incomparable,
            &empty_projected,
        )
        .unwrap();
        assert!(validate_capacity_evidence(
            &exact(1, 1),
            &exact(0, 1),
            &exact(0, 1),
            &incomparable,
            &empty_projected,
        )
        .is_err());
    }

    #[test]
    fn leverage_cell_metrics_are_exact_and_thresholds_are_separate() {
        let cell = leverage_cell();
        let market = leverage_market();
        validate_leverage_cell_economics(&cell).unwrap();

        let leverage =
            leverage_cell_metric(&cell, &market, LeverageMetricKind::TheoreticalLeverage).unwrap();
        assert_eq!(leverage.family, MetricFamily::Leverage);
        assert_eq!(leverage.v1.numerator.as_ref().unwrap().as_str(), "3");
        assert_eq!(leverage.v1.denominator.as_ref().unwrap().as_str(), "2");
        assert_eq!(leverage.dusk.numerator.as_ref().unwrap().as_str(), "2");
        assert_eq!(
            leverage.paired_delta.numerator.as_ref().unwrap().as_str(),
            "1"
        );
        assert_eq!(
            leverage.paired_delta.denominator.as_ref().unwrap().as_str(),
            "2"
        );
        assert!(leverage
            .metric_id
            .contains(".checkpoint.median.direction.long_base.margin_nav_100_bps.cell."));

        let [v1_threshold, dusk_threshold] = leverage_threshold_metrics(&cell, &market).unwrap();
        assert_eq!(v1_threshold.v1.availability, MetricAvailability::Available);
        assert_eq!(
            v1_threshold.dusk.availability,
            MetricAvailability::NotEvaluated
        );
        assert_eq!(
            dusk_threshold.v1.availability,
            MetricAvailability::NotEvaluated
        );
        assert_eq!(
            dusk_threshold.dusk.availability,
            MetricAvailability::Available
        );
        assert_eq!(
            v1_threshold.paired_delta.availability,
            MetricAvailability::NotEvaluated
        );
        assert_ne!(v1_threshold.v1.unit, dusk_threshold.dusk.unit);
    }

    #[test]
    fn leverage_lower_median_is_hierarchical_not_flattened() {
        let integer = |value: i64| BigRational::from_integer(BigInt::from(value));
        let first_market = vec![integer(1); 101];
        let second_market = vec![integer(100)];
        let third_market = vec![integer(200)];
        let hierarchical = hierarchical_lower_median(vec![
            first_market.clone(),
            second_market.clone(),
            third_market.clone(),
        ])
        .unwrap();
        assert_eq!(hierarchical, integer(100));

        let mut flattened = first_market;
        flattened.extend(second_market);
        flattened.extend(third_market);
        assert_eq!(lower_median_rational(&mut flattened).unwrap(), integer(1));
    }

    #[test]
    fn non_lending_leverage_rows_are_na_without_numeric_sentinels() {
        let metrics = leverage_non_lending_metrics(
            "market-no-lending",
            &"p".repeat(64),
            ReplayArm::FixedFlow,
            HlpScenario::NoHlp,
        );
        assert_eq!(metrics.len(), 14);
        let mut ids = BTreeSet::new();
        for metric in metrics {
            assert!(ids.insert(metric.metric_id));
            for value in [metric.v1, metric.dusk, metric.paired_delta] {
                assert_eq!(
                    value.availability,
                    MetricAvailability::NotApplicableNoLendingActivity
                );
                assert!(value.numerator.is_none());
                assert!(value.denominator.is_none());
            }
        }
    }

    #[test]
    fn unbounded_leverage_ratio_never_serializes_zero_denominator() {
        let mut cell = leverage_cell();
        let benchmark = cell.benchmark.as_mut().unwrap();
        benchmark.v1.theoretical_leverage = ratio(1, 0, true);
        let metric = leverage_cell_metric(
            &cell,
            &leverage_market(),
            LeverageMetricKind::TheoreticalLeverage,
        )
        .unwrap();
        assert_eq!(metric.v1.availability, MetricAvailability::NotEvaluated);
        assert!(metric.v1.numerator.is_none());
        assert!(metric.v1.denominator.is_none());
        assert_eq!(metric.dusk.availability, MetricAvailability::Available);
        assert_eq!(
            metric.paired_delta.availability,
            MetricAvailability::NotEvaluated
        );

        let [flag, _] = leverage_unbounded_metrics(&cell).unwrap();
        assert_eq!(flag.v1.numerator.as_ref().unwrap().as_str(), "1");
        assert_eq!(flag.v1.denominator.as_ref().unwrap().as_str(), "1");
        assert_eq!(flag.dusk.numerator.as_ref().unwrap().as_str(), "0");

        cell.benchmark
            .as_mut()
            .unwrap()
            .v1
            .theoretical_leverage
            .unbounded = false;
        assert!(leverage_unbounded_metrics(&cell).is_err());
    }
}
