//! Machine-readable benchmark metrics, aggregation, and accounting identities.

use std::collections::{BTreeMap, BTreeSet};

use num_bigint::BigUint;
use num_traits::{ToPrimitive, Zero};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    audit::{
        CertificationStatus, EvidenceTier as GateEvidenceTier, GateStatus, HardGateId,
        HardGatePhase, PhaseGateDecisionV1,
    },
    canonical::{SignedTokenAmount, TokenAmount},
    config::{EvidenceTier as ConfigEvidenceTier, ScopeMode},
    metric_engine::{RevenueAccountingClassV1, RevenueLaneV1, RevenueOriginV1, RevenueVenueV1},
    replay::{HlpScenario, ReplayArm},
    sensitivity::{TopMarketExclusionV1, TopMarketRankingV1},
};

pub const METRICS_SCHEMA_VERSION: u16 = 3;
pub const LEGACY_GLOBAL_PROFILE_SEMANTICS: &str = "Legacy global_* fields and global_profile metric-ID segments identify the single protocol-wide shared Dusk profile evaluated over the explicitly bound evidence scope; they do not assert full-universe evidence.";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricAvailability {
    Available,
    NotApplicableNoLendingActivity,
    /// The compared venue has no structurally equivalent accounting lane.
    NotApplicableVenueMechanism,
    MissingHistoricalUsdMark,
    /// The observation window is shorter than the preregistered 30-day
    /// minimum for annualization. Absolute return remains publishable.
    WindowTooShortForAnnualization,
    /// Exact owner-claim capital-time was not captured. A common factual-
    /// capital return may still be reported, but it is not an actual APY.
    MissingOwnershipDenominator,
    /// A cash-realization memo cannot be reconstructed exactly even though
    /// the corresponding public accrual lane may be available.
    MissingCashRealizationEvidence,
    OutsideCommonWindow,
    FailedCoverageGate,
    NotEvaluated,
}

/// Exact rational metric. Token units must use denominator one. Ratios and USD
/// values retain their explicit denominator so JSON consumers never receive a
/// rounded binary float.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetricValueV1 {
    pub availability: MetricAvailability,
    pub numerator: Option<SignedTokenAmount>,
    pub denominator: Option<TokenAmount>,
    pub unit: String,
    pub denominator_definition: Option<String>,
}

impl MetricValueV1 {
    pub fn exact(
        numerator: i128,
        denominator: u128,
        unit: impl Into<String>,
        denominator_definition: Option<String>,
    ) -> Result<Self, MetricError> {
        if denominator == 0 {
            return Err(MetricError::ZeroDenominator);
        }
        Ok(Self {
            availability: MetricAvailability::Available,
            numerator: Some(
                SignedTokenAmount::new(numerator.to_string())
                    .map_err(|_| MetricError::InvalidInteger)?,
            ),
            denominator: Some(
                TokenAmount::new(denominator.to_string())
                    .map_err(|_| MetricError::InvalidInteger)?,
            ),
            unit: unit.into(),
            denominator_definition,
        })
    }

    pub fn unavailable(availability: MetricAvailability, unit: impl Into<String>) -> Self {
        debug_assert!(availability != MetricAvailability::Available);
        Self {
            availability,
            numerator: None,
            denominator: None,
            unit: unit.into(),
            denominator_definition: None,
        }
    }

    pub fn exact_components(
        numerator: SignedTokenAmount,
        denominator: TokenAmount,
        unit: impl Into<String>,
        denominator_definition: Option<String>,
    ) -> Result<Self, MetricError> {
        if denominator.as_str() == "0" {
            return Err(MetricError::ZeroDenominator);
        }
        let value = Self {
            availability: MetricAvailability::Available,
            numerator: Some(numerator),
            denominator: Some(denominator),
            unit: unit.into(),
            denominator_definition,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), MetricError> {
        match self.availability {
            MetricAvailability::Available => {
                if self.numerator.is_none()
                    || self.denominator.as_ref().map(TokenAmount::as_str) == Some("0")
                    || self.denominator.is_none()
                {
                    return Err(MetricError::MalformedAvailability);
                }
            }
            _ if self.numerator.is_some() || self.denominator.is_some() => {
                return Err(MetricError::MalformedAvailability)
            }
            _ => {}
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricFamily {
    CollateralEfficiency,
    SpotCapitalEfficiency,
    Leverage,
    LpRevenue,
    ProtocolRevenue,
    BalanceSheet,
    Safety,
    Coverage,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedMetricV1 {
    pub metric_id: String,
    pub family: MetricFamily,
    pub profile_id: Option<String>,
    pub replay_arm: Option<ReplayArm>,
    pub hlp_scenario: Option<HlpScenario>,
    pub definition: String,
    /// Present for every exhaustive revenue-lane metric. Venue IDs are
    /// explicit even though numeric values also occupy V1/Dusk columns.
    pub revenue_dimensions: Option<RevenueMetricDimensionsV1>,
    pub v1: MetricValueV1,
    pub dusk: MetricValueV1,
    pub paired_delta: MetricValueV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevenueMetricDimensionsV1 {
    pub lane: RevenueLaneV1,
    pub accounting_class: RevenueAccountingClassV1,
    pub origin: RevenueOriginV1,
    pub v1_venue: Option<RevenueVenueV1>,
    pub dusk_venue: Option<RevenueVenueV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketMetricSetV1 {
    pub market: String,
    pub token0: String,
    pub token1: String,
    pub has_lending_activity: bool,
    pub raw_metrics: Vec<NamedMetricV1>,
    pub usd_metrics: Vec<NamedMetricV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketOpportunityCeilingV1 {
    pub market: String,
    pub training_swap_count: u64,
    pub training_active_days: u64,
    pub validation_swap_count: u64,
    pub validation_active_days: u64,
    pub holdout_swap_count: u64,
    pub holdout_active_days: u64,
    pub selected_profile_id: Option<String>,
    pub claim_available: bool,
    pub unavailable_reason: Option<String>,
}

/// Compact, report-safe view of one immutable sensitivity artifact. The full
/// bootstrap draws remain in the hash-bound source artifact; the result
/// manifest carries the deterministic ranking/exclusion evidence needed by
/// every audience report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SensitivityEvidenceSummaryV1 {
    pub profile_id: String,
    pub replay_arm: ReplayArm,
    pub hlp_scenario: HlpScenario,
    pub sensitivity_artifact_sha256: String,
    pub included_markets_sha256: String,
    pub sampled_block_sequence_sha256: String,
    pub bootstrap_samples: u32,
    pub top_market_rankings: Vec<TopMarketRankingV1>,
    pub top_market_exclusions: Vec<TopMarketExclusionV1>,
    pub top_market_exclusion_unsupported_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageMetricV1 {
    pub availability: MetricAvailability,
    pub covered_numerator: TokenAmount,
    pub total_denominator: TokenAmount,
    pub required_bps: u16,
    pub actual_bps: Option<u16>,
    pub passes: bool,
}

impl CoverageMetricV1 {
    pub fn from_raw(covered: u128, total: u128, required_bps: u16) -> Result<Self, MetricError> {
        if required_bps > 10_000 || covered > total {
            return Err(MetricError::InvalidCoverage);
        }
        if total == 0 {
            return Ok(Self {
                availability: MetricAvailability::NotApplicableNoLendingActivity,
                covered_numerator: TokenAmount::zero(),
                total_denominator: TokenAmount::zero(),
                required_bps,
                actual_bps: None,
                // With no factual notional there is no uncovered population.
                passes: true,
            });
        }
        let actual = covered
            .checked_mul(10_000)
            .ok_or(MetricError::ArithmeticOverflow)?
            / total;
        let actual_bps = u16::try_from(actual).map_err(|_| MetricError::ArithmeticOverflow)?;
        Ok(Self {
            availability: MetricAvailability::Available,
            covered_numerator: TokenAmount::new(covered.to_string())
                .map_err(|_| MetricError::InvalidInteger)?,
            total_denominator: TokenAmount::new(total.to_string())
                .map_err(|_| MetricError::InvalidInteger)?,
            required_bps,
            actual_bps: Some(actual_bps),
            passes: actual_bps >= required_bps,
        })
    }

    pub fn validate(&self) -> Result<(), MetricError> {
        let covered = unsigned_big(&self.covered_numerator)?;
        let total = unsigned_big(&self.total_denominator)?;
        if self.required_bps > 10_000 || covered > total {
            return Err(MetricError::InvalidCoverage);
        }
        match self.availability {
            MetricAvailability::Available => {
                if total.is_zero() || self.actual_bps.is_none() {
                    return Err(MetricError::InvalidCoverage);
                }
                let expected = ((&covered * BigUint::from(10_000_u16)) / &total)
                    .to_u16()
                    .ok_or(MetricError::ArithmeticOverflow)?;
                if self.actual_bps != Some(expected)
                    || self.passes != (expected >= self.required_bps)
                {
                    return Err(MetricError::InvalidCoverage);
                }
            }
            MetricAvailability::NotApplicableNoLendingActivity => {
                if !total.is_zero()
                    || !covered.is_zero()
                    || self.actual_bps.is_some()
                    || !self.passes
                {
                    return Err(MetricError::InvalidCoverage);
                }
            }
            _ => return Err(MetricError::InvalidCoverage),
        }
        Ok(())
    }
}

fn unsigned_big(value: &TokenAmount) -> Result<BigUint, MetricError> {
    value
        .as_str()
        .parse::<BigUint>()
        .map_err(|_| MetricError::InvalidInteger)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeadlineCoverageV1 {
    pub visible_swap_volume: CoverageMetricV1,
    pub borrowed_notional: CoverageMetricV1,
    pub revenue_notional: CoverageMetricV1,
    pub exact_transaction_fetch_bps: u16,
    pub exact_decode_bps: u16,
    pub v1_zero_difference_reconciled: bool,
}

impl HeadlineCoverageV1 {
    pub fn passes(&self) -> bool {
        self.visible_swap_volume.validate().is_ok()
            && self.borrowed_notional.validate().is_ok()
            && self.revenue_notional.validate().is_ok()
            && self.visible_swap_volume.passes
            && self.borrowed_notional.passes
            && self.revenue_notional.passes
            && self.exact_transaction_fetch_bps == 10_000
            && self.exact_decode_bps == 10_000
            && self.v1_zero_difference_reconciled
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootstrapIntervalV1 {
    pub metric_id: String,
    pub block_days: u16,
    pub samples: u32,
    pub lower_025: SignedTokenAmount,
    pub median: SignedTokenAmount,
    pub upper_975: SignedTokenAmount,
    pub unit: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairedBlockObservation {
    pub seven_day_block: u64,
    pub v1_value: i128,
    pub dusk_value: i128,
}

pub fn paired_block_bootstrap(
    metric_id: impl Into<String>,
    unit: impl Into<String>,
    observations: &[PairedBlockObservation],
    samples: u32,
    seed: u64,
) -> Result<BootstrapIntervalV1, MetricError> {
    if observations.is_empty() || samples == 0 {
        return Err(MetricError::EmptyBootstrap);
    }
    let mut by_block = BTreeMap::<u64, i128>::new();
    for observation in observations {
        let delta = observation
            .dusk_value
            .checked_sub(observation.v1_value)
            .ok_or(MetricError::ArithmeticOverflow)?;
        let entry = by_block.entry(observation.seven_day_block).or_default();
        *entry = entry
            .checked_add(delta)
            .ok_or(MetricError::ArithmeticOverflow)?;
    }
    let blocks: Vec<i128> = by_block.into_values().collect();
    let mut rng = SplitMix64(seed);
    let mut draws = Vec::with_capacity(samples as usize);
    for _ in 0..samples {
        let mut total = 0_i128;
        for _ in 0..blocks.len() {
            let index = (rng.next() as usize) % blocks.len();
            total = total
                .checked_add(blocks[index])
                .ok_or(MetricError::ArithmeticOverflow)?;
        }
        draws.push(total);
    }
    draws.sort_unstable();
    let quantile = |parts: u64| -> i128 {
        let last = draws.len() - 1;
        let index = ((last as u128) * u128::from(parts) / 10_000) as usize;
        draws[index]
    };
    Ok(BootstrapIntervalV1 {
        metric_id: metric_id.into(),
        block_days: 7,
        samples,
        lower_025: signed(quantile(250))?,
        median: signed(quantile(5_000))?,
        upper_975: signed(quantile(9_750))?,
        unit: unit.into(),
    })
}

struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
        value ^ (value >> 31)
    }
}

fn signed(value: i128) -> Result<SignedTokenAmount, MetricError> {
    SignedTokenAmount::new(value.to_string()).map_err(|_| MetricError::InvalidInteger)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReturnPresentationV1 {
    Absolute {
        window_days: u32,
        revenue: SignedTokenAmount,
        unit: String,
    },
    AnnualizedLinearBps {
        window_days: u32,
        bps: SignedTokenAmount,
    },
}

pub fn present_return(
    revenue: i128,
    average_capital: u128,
    window_days: u32,
    unit: impl Into<String>,
) -> Result<ReturnPresentationV1, MetricError> {
    if window_days < 30 {
        return Ok(ReturnPresentationV1::Absolute {
            window_days,
            revenue: signed(revenue)?,
            unit: unit.into(),
        });
    }
    if average_capital == 0 {
        return Err(MetricError::ZeroDenominator);
    }
    let numerator = revenue
        .checked_mul(365)
        .and_then(|value| value.checked_mul(10_000))
        .ok_or(MetricError::ArithmeticOverflow)?;
    let denominator = i128::try_from(average_capital)
        .map_err(|_| MetricError::ArithmeticOverflow)?
        .checked_mul(i128::from(window_days))
        .ok_or(MetricError::ArithmeticOverflow)?;
    Ok(ReturnPresentationV1::AnnualizedLinearBps {
        window_days,
        bps: signed(numerator / denominator)?,
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConservationIdentityV1 {
    pub identity_id: String,
    pub initial: TokenAmount,
    pub external_credits: TokenAmount,
    pub external_debits: TokenAmount,
    pub final_balance: TokenAmount,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountingVerificationV1 {
    pub v1_conservation_checks: u64,
    pub v1_conservation_failures: u64,
    pub dusk_atomic_transition_checks: u64,
    pub dusk_atomic_transition_failures: u64,
    pub paired_revenue_lane_checks: u64,
    pub paired_revenue_lane_failures: u64,
}

impl AccountingVerificationV1 {
    pub fn passes(&self) -> bool {
        self.v1_conservation_checks > 0
            && self.dusk_atomic_transition_checks > 0
            && self.paired_revenue_lane_checks > 0
            && self.v1_conservation_failures == 0
            && self.dusk_atomic_transition_failures == 0
            && self.paired_revenue_lane_failures == 0
    }
}

impl ConservationIdentityV1 {
    pub fn verify(&self) -> Result<(), MetricError> {
        let initial = unsigned(&self.initial)?;
        let credits = unsigned(&self.external_credits)?;
        let debits = unsigned(&self.external_debits)?;
        let final_balance = unsigned(&self.final_balance)?;
        let left = initial
            .checked_add(credits)
            .ok_or(MetricError::ArithmeticOverflow)?;
        let right = debits
            .checked_add(final_balance)
            .ok_or(MetricError::ArithmeticOverflow)?;
        if left != right {
            return Err(MetricError::Conservation {
                identity: self.identity_id.clone(),
                left,
                right,
            });
        }
        Ok(())
    }
}

fn unsigned(value: &TokenAmount) -> Result<u128, MetricError> {
    value
        .as_str()
        .parse::<u128>()
        .map_err(|_| MetricError::InvalidInteger)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeActivityCoverageV1 {
    pub visible_universe_rows: u64,
    pub selected_panel_rows: u64,
    pub selected_panel_coverage_bps: u16,
}

impl ScopeActivityCoverageV1 {
    fn validate(&self) -> Result<(), MetricError> {
        if self.selected_panel_rows > self.visible_universe_rows {
            return Err(MetricError::InvalidScopeEvidence);
        }
        let expected_bps = if self.visible_universe_rows == 0 {
            0
        } else {
            u16::try_from(
                (u128::from(self.selected_panel_rows) * 10_000)
                    / u128::from(self.visible_universe_rows),
            )
            .map_err(|_| MetricError::InvalidScopeEvidence)?
        };
        if self.selected_panel_coverage_bps != expected_bps {
            return Err(MetricError::InvalidScopeEvidence);
        }
        Ok(())
    }
}

/// Self-hashed extraction evidence describing how much visible-universe
/// activity is represented by the selected hybrid panel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CohortRepresentativenessEvidenceV1 {
    pub artifact_sha256: String,
    pub cohort_selection_sha256: String,
    pub swaps: ScopeActivityCoverageV1,
    pub debt_actions: ScopeActivityCoverageV1,
    pub liquidations: ScopeActivityCoverageV1,
}

impl CohortRepresentativenessEvidenceV1 {
    fn validate(&self) -> Result<(), MetricError> {
        if !valid_sha256(&self.artifact_sha256) || !valid_sha256(&self.cohort_selection_sha256) {
            return Err(MetricError::InvalidScopeEvidence);
        }
        self.swaps.validate()?;
        self.debt_actions.validate()?;
        self.liquidations.validate()?;
        Ok(())
    }
}

/// The exact evidence population and final certification decision attached to
/// every metric and report claim. Legacy `global_*` machine fields remain
/// stable, but their meaning is explicitly constrained by this scope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResultScopeV1 {
    pub scope_mode: ScopeMode,
    pub evidence_tier: ConfigEvidenceTier,
    pub selected_markets: Vec<String>,
    pub selected_market_set_sha256: String,
    pub cohort_selection_sha256: Option<String>,
    pub cohort_representativeness: Option<CohortRepresentativenessEvidenceV1>,
    pub final_phase_gate_sha256: String,
    pub final_phase_gate_decision: PhaseGateDecisionV1,
    pub legacy_global_profile_semantics: String,
}

impl ResultScopeV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        scope_mode: ScopeMode,
        evidence_tier: ConfigEvidenceTier,
        selected_markets: BTreeSet<String>,
        cohort_selection_sha256: Option<String>,
        cohort_representativeness: Option<CohortRepresentativenessEvidenceV1>,
        final_phase_gate_sha256: String,
        final_phase_gate_decision: PhaseGateDecisionV1,
    ) -> Result<Self, MetricError> {
        let selected_market_set_sha256 = selected_market_set_sha256(&selected_markets);
        let scope = Self {
            scope_mode,
            evidence_tier,
            selected_markets: selected_markets.into_iter().collect(),
            selected_market_set_sha256,
            cohort_selection_sha256,
            cohort_representativeness,
            final_phase_gate_sha256,
            final_phase_gate_decision,
            legacy_global_profile_semantics: LEGACY_GLOBAL_PROFILE_SEMANTICS.to_owned(),
        };
        scope.validate()?;
        Ok(scope)
    }

    pub fn validate(&self) -> Result<(), MetricError> {
        let selected: BTreeSet<_> = self.selected_markets.iter().cloned().collect();
        if selected.is_empty()
            || selected.len() != self.selected_markets.len()
            || selected.iter().any(|market| market.trim().is_empty())
            || selected.iter().ne(self.selected_markets.iter())
            || self.selected_market_set_sha256 != selected_market_set_sha256(&selected)
            || !valid_sha256(&self.final_phase_gate_sha256)
            || self.legacy_global_profile_semantics != LEGACY_GLOBAL_PROFILE_SEMANTICS
        {
            return Err(MetricError::InvalidScopeEvidence);
        }

        match (
            self.scope_mode,
            self.evidence_tier,
            self.cohort_selection_sha256.as_deref(),
            self.cohort_representativeness.as_ref(),
        ) {
            (ScopeMode::HybridPanel, _, Some(cohort_sha256), Some(representativeness))
                if valid_sha256(cohort_sha256)
                    && representativeness.cohort_selection_sha256 == cohort_sha256 =>
            {
                representativeness.validate()?;
            }
            (ScopeMode::FullUniverse, ConfigEvidenceTier::AuditGrade, None, None) => {}
            _ => return Err(MetricError::InvalidScopeEvidence),
        }

        let decision = &self.final_phase_gate_decision;
        let expected_gate_tier = match self.evidence_tier {
            ConfigEvidenceTier::DecisionGrade => GateEvidenceTier::DecisionGrade,
            ConfigEvidenceTier::AuditGrade => GateEvidenceTier::AuditGrade,
        };
        if decision.phase != HardGatePhase::PostReplayFinal
            || decision.evidence_tier != expected_gate_tier
            || !decision.phase_eligible
            || !decision.raw_market_eligible
            || !decision.usd_headline_eligible
            || !decision.raw_blocking_gates.is_empty()
            || !decision.usd_blocking_gates.is_empty()
        {
            return Err(MetricError::InvalidScopeEvidence);
        }

        let mut limited_gates = BTreeSet::new();
        if decision.limitations.iter().any(|limitation| {
            !matches!(
                limitation.gate,
                HardGateId::DecoderRevisionCertification | HardGateId::TerminalState
            ) || limitation.underlying_status != GateStatus::Fail
                || limitation.detail_code.trim().is_empty()
                || !limited_gates.insert(limitation.gate)
        }) {
            return Err(MetricError::InvalidScopeEvidence);
        }

        match self.evidence_tier {
            ConfigEvidenceTier::AuditGrade
                if decision.raw_certification == CertificationStatus::Pass
                    && decision.usd_certification == CertificationStatus::Pass
                    && decision.limitations.is_empty() =>
            {
                Ok(())
            }
            ConfigEvidenceTier::DecisionGrade
                if (decision.limitations.is_empty()
                    && decision.raw_certification == CertificationStatus::Pass
                    && decision.usd_certification == CertificationStatus::Pass)
                    || (!decision.limitations.is_empty()
                        && decision.raw_certification == CertificationStatus::Limited
                        && decision.usd_certification == CertificationStatus::Limited) =>
            {
                Ok(())
            }
            _ => Err(MetricError::InvalidScopeEvidence),
        }
    }
}

fn selected_market_set_sha256(markets: &BTreeSet<String>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"v1-vs-dusk/included-markets/v1\0");
    for market in markets {
        hasher.update((market.len() as u64).to_le_bytes());
        hasher.update(market.as_bytes());
    }
    hex::encode(hasher.finalize())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResultManifestV1 {
    pub schema_version: u16,
    pub run_id: String,
    pub dataset_id: String,
    pub profile_freeze_sha256: String,
    pub dusk_source_sha256: String,
    pub cutoff_slot: u64,
    pub scope: ResultScopeV1,
    pub common_window_start_unix_seconds: i64,
    pub common_window_end_unix_seconds_exclusive: i64,
    pub holdout_start_unix_seconds: i64,
    pub holdout_end_unix_seconds_exclusive: i64,
    pub holdout_evidence_sha256: String,
    pub universe_window_sha256: String,
    pub source_artifact_sha256: BTreeMap<String, String>,
    pub coverage: HeadlineCoverageV1,
    pub headline_hlp_scenario: HlpScenario,
    pub no_hlp_control_included: bool,
    pub global_winner_profile_id: Option<String>,
    pub no_global_winner: bool,
    pub pareto_profile_ids: Vec<String>,
    pub per_market_calibration_qualification_sha256: String,
    pub per_market_holdout_qualification_sha256: String,
    pub per_market_opportunity_ceilings: Vec<MarketOpportunityCeilingV1>,
    pub sensitivity_evidence: Vec<SensitivityEvidenceSummaryV1>,
    pub markets: Vec<MarketMetricSetV1>,
    pub aggregate_metrics: Vec<NamedMetricV1>,
    pub bootstrap_intervals: Vec<BootstrapIntervalV1>,
    pub conservation_identities: Vec<ConservationIdentityV1>,
    pub accounting_verification: AccountingVerificationV1,
    pub limitations: Vec<String>,
    /// Every chart/table/report fact references one of these stable metric IDs.
    pub report_metric_ids: BTreeSet<String>,
    pub manifest_sha256: String,
}

impl ResultManifestV1 {
    pub fn seal(mut self) -> Result<Self, MetricError> {
        self.validate_contents()?;
        self.manifest_sha256.clear();
        let bytes = serde_json::to_vec(&self).map_err(|_| MetricError::Serialization)?;
        self.manifest_sha256 = hex::encode(Sha256::digest(bytes));
        Ok(self)
    }

    pub fn verify(&self) -> Result<(), MetricError> {
        self.validate_contents()?;
        let expected = self.clone().seal()?.manifest_sha256;
        if expected != self.manifest_sha256 {
            return Err(MetricError::ManifestChecksum);
        }
        Ok(())
    }

    fn validate_contents(&self) -> Result<(), MetricError> {
        if self.schema_version != METRICS_SCHEMA_VERSION {
            return Err(MetricError::SchemaVersion(self.schema_version));
        }
        if self.run_id.trim().is_empty()
            || !valid_sha256(&self.dataset_id)
            || !valid_sha256(&self.profile_freeze_sha256)
            || !valid_sha256(&self.dusk_source_sha256)
            || !valid_sha256(&self.holdout_evidence_sha256)
            || !valid_sha256(&self.universe_window_sha256)
            || !valid_sha256(&self.per_market_calibration_qualification_sha256)
            || !valid_sha256(&self.per_market_holdout_qualification_sha256)
            || self.source_artifact_sha256.is_empty()
            || self
                .source_artifact_sha256
                .iter()
                .any(|(name, hash)| name.trim().is_empty() || !valid_sha256(hash))
        {
            return Err(MetricError::InvalidIdentity);
        }
        self.scope.validate()?;
        if !self.coverage.passes() {
            return Err(MetricError::HeadlineCoverage);
        }
        if self.no_global_winner == self.global_winner_profile_id.is_some() {
            return Err(MetricError::WinnerState);
        }
        if self.headline_hlp_scenario != HlpScenario::MaximumFeasible {
            return Err(MetricError::WinnerState);
        }
        if self.common_window_start_unix_seconds >= self.common_window_end_unix_seconds_exclusive
            || self.holdout_start_unix_seconds < self.common_window_start_unix_seconds
            || self.holdout_end_unix_seconds_exclusive
                > self.common_window_end_unix_seconds_exclusive
            || self.holdout_start_unix_seconds >= self.holdout_end_unix_seconds_exclusive
        {
            return Err(MetricError::InvalidWindow);
        }
        if !self.accounting_verification.passes() {
            return Err(MetricError::AccountingVerification);
        }
        let mut opportunity_markets = BTreeSet::new();
        for claim in &self.per_market_opportunity_ceilings {
            if claim.market.trim().is_empty()
                || !opportunity_markets.insert(claim.market.as_str())
                || claim.claim_available != claim.selected_profile_id.is_some()
                || claim.claim_available == claim.unavailable_reason.is_some()
            {
                return Err(MetricError::WinnerState);
            }
        }
        let mut sensitivity_identities = BTreeSet::new();
        for evidence in &self.sensitivity_evidence {
            let ranking_bases: BTreeSet<_> = evidence
                .top_market_rankings
                .iter()
                .map(|ranking| ranking.basis)
                .collect();
            let exclusion_bases: BTreeSet<_> = evidence
                .top_market_exclusions
                .iter()
                .map(|exclusion| exclusion.basis)
                .collect();
            if evidence.profile_id.trim().is_empty()
                || !valid_sha256(&evidence.sensitivity_artifact_sha256)
                || !valid_sha256(&evidence.included_markets_sha256)
                || !valid_sha256(&evidence.sampled_block_sequence_sha256)
                || evidence.bootstrap_samples == 0
                || !sensitivity_identities.insert(format!(
                    "{}:{:?}:{:?}",
                    evidence.profile_id, evidence.replay_arm, evidence.hlp_scenario
                ))
                || (evidence.top_market_exclusions.is_empty()
                    == evidence.top_market_exclusion_unsupported_reason.is_none())
                || ranking_bases.len() != evidence.top_market_rankings.len()
                || exclusion_bases.len() != evidence.top_market_exclusions.len()
                || (!exclusion_bases.is_empty() && exclusion_bases != ranking_bases)
                || evidence.top_market_rankings.iter().any(|ranking| {
                    let mut markets = BTreeSet::new();
                    ranking.markets.iter().enumerate().any(|(index, market)| {
                        market.rank != u64::try_from(index + 1).unwrap_or(u64::MAX)
                            || !markets.insert(market.market.as_str())
                            || market.factual_value_usd_micro.denominator.as_str() == "0"
                    })
                })
                || evidence.top_market_exclusions.iter().any(|exclusion| {
                    exclusion.remaining_market_count == 0
                        || exclusion.excluded_market.trim().is_empty()
                        || exclusion
                            .excluded_factual_value_usd_micro
                            .denominator
                            .as_str()
                            == "0"
                        || exclusion
                            .estimate_without_top_market
                            .stakeholder_user_flow_revenue_delta_usd_micro
                            .exact
                            .denominator
                            .as_str()
                            == "0"
                })
            {
                return Err(MetricError::InvalidIdentity);
            }
        }
        for metric in self
            .aggregate_metrics
            .iter()
            .chain(
                self.markets
                    .iter()
                    .flat_map(|market| market.raw_metrics.iter()),
            )
            .chain(
                self.markets
                    .iter()
                    .flat_map(|market| market.usd_metrics.iter()),
            )
        {
            metric.v1.validate()?;
            metric.dusk.validate()?;
            metric.paired_delta.validate()?;
            if let Some(dimensions) = &metric.revenue_dimensions {
                if dimensions.accounting_class != dimensions.lane.accounting_class()
                    || dimensions
                        .v1_venue
                        .is_some_and(|venue| venue != RevenueVenueV1::V1)
                    || dimensions
                        .dusk_venue
                        .is_some_and(|venue| venue != RevenueVenueV1::Dusk)
                    || (dimensions.v1_venue.is_none() && dimensions.dusk_venue.is_none())
                    || (dimensions.origin != RevenueOriginV1::FactualUserFlow
                        && dimensions.v1_venue.is_some())
                    || (dimensions.v1_venue.is_none()
                        && metric.v1.availability == MetricAvailability::Available)
                    || (dimensions.dusk_venue.is_none()
                        && metric.dusk.availability == MetricAvailability::Available)
                {
                    return Err(MetricError::RevenueDimensions);
                }
            }
        }
        for identity in &self.conservation_identities {
            identity.verify()?;
        }
        let metric_count = self.aggregate_metrics.len()
            + self
                .markets
                .iter()
                .map(|market| market.raw_metrics.len() + market.usd_metrics.len())
                .sum::<usize>();
        let available_ids: BTreeSet<&str> = self
            .aggregate_metrics
            .iter()
            .chain(
                self.markets
                    .iter()
                    .flat_map(|market| market.raw_metrics.iter()),
            )
            .chain(
                self.markets
                    .iter()
                    .flat_map(|market| market.usd_metrics.iter()),
            )
            .map(|metric| metric.metric_id.as_str())
            .collect();
        if available_ids.len() != metric_count
            || self
                .report_metric_ids
                .iter()
                .any(|id| !available_ids.contains(id.as_str()))
        {
            return Err(MetricError::UnknownReportMetric);
        }
        Ok(())
    }
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum MetricError {
    #[error("unsupported metrics schema version {0}")]
    SchemaVersion(u16),
    #[error("integer field is not canonical")]
    InvalidInteger,
    #[error("metric denominator cannot be zero")]
    ZeroDenominator,
    #[error("metric availability and numeric fields disagree")]
    MalformedAvailability,
    #[error("coverage numerator/denominator is invalid")]
    InvalidCoverage,
    #[error("headline coverage or exact V1 reconciliation gate failed")]
    HeadlineCoverage,
    #[error("global winner and no-winner flags are inconsistent")]
    WinnerState,
    #[error("common or holdout result window is invalid")]
    InvalidWindow,
    #[error("transition conservation or revenue accounting verification failed")]
    AccountingVerification,
    #[error("bootstrap needs at least one block and one sample")]
    EmptyBootstrap,
    #[error("arithmetic overflow")]
    ArithmeticOverflow,
    #[error("conservation identity {identity} failed: {left} != {right}")]
    Conservation {
        identity: String,
        left: u128,
        right: u128,
    },
    #[error("result manifest checksum does not match its contents")]
    ManifestChecksum,
    #[error("result manifest source identity is incomplete or malformed")]
    InvalidIdentity,
    #[error("a report references a metric ID absent from the manifest")]
    UnknownReportMetric,
    #[error("result manifest serialization failed")]
    Serialization,
    #[error("revenue metric dimensions disagree with lane, origin, venue, or availability")]
    RevenueDimensions,
    #[error("result scope, cohort, or final certification evidence is inconsistent")]
    InvalidScopeEvidence,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(value: u128) -> TokenAmount {
        TokenAmount::new(value.to_string()).unwrap()
    }

    #[test]
    fn short_windows_are_never_annualized() {
        let presented = present_return(10, 100, 29, "raw_token").unwrap();
        assert!(matches!(presented, ReturnPresentationV1::Absolute { .. }));
        let presented = present_return(10, 100, 30, "raw_token").unwrap();
        assert!(matches!(
            presented,
            ReturnPresentationV1::AnnualizedLinearBps { .. }
        ));
    }

    #[test]
    fn paired_bootstrap_is_reproducible() {
        let observations = vec![
            PairedBlockObservation {
                seven_day_block: 0,
                v1_value: 10,
                dusk_value: 12,
            },
            PairedBlockObservation {
                seven_day_block: 1,
                v1_value: 20,
                dusk_value: 19,
            },
        ];
        assert_eq!(
            paired_block_bootstrap("m", "atom", &observations, 100, 7).unwrap(),
            paired_block_bootstrap("m", "atom", &observations, 100, 7).unwrap()
        );
    }

    #[test]
    fn conservation_is_exact() {
        ConservationIdentityV1 {
            identity_id: "token0".into(),
            initial: token(10),
            external_credits: token(5),
            external_debits: token(4),
            final_balance: token(11),
        }
        .verify()
        .unwrap();
    }

    #[test]
    fn no_lending_is_not_serialized_as_zero() {
        let value = MetricValueV1::unavailable(
            MetricAvailability::NotApplicableNoLendingActivity,
            "raw_token",
        );
        value.validate().unwrap();
        assert!(value.numerator.is_none());
    }

    #[test]
    fn decision_grade_scope_permits_only_typed_decoder_and_terminal_limits() {
        let cohort_sha256 = "a".repeat(64);
        let scope = ResultScopeV1::new(
            ScopeMode::HybridPanel,
            ConfigEvidenceTier::DecisionGrade,
            BTreeSet::from(["market-a".to_owned(), "market-b".to_owned()]),
            Some(cohort_sha256.clone()),
            Some(CohortRepresentativenessEvidenceV1 {
                artifact_sha256: "b".repeat(64),
                cohort_selection_sha256: cohort_sha256,
                swaps: activity(80, 100),
                debt_actions: activity(20, 40),
                liquidations: activity(3, 4),
            }),
            "c".repeat(64),
            limited_decision(vec![
                HardGateId::DecoderRevisionCertification,
                HardGateId::TerminalState,
            ]),
        )
        .unwrap();
        scope.validate().unwrap();

        let mut invalid = scope;
        invalid.final_phase_gate_decision = limited_decision(vec![HardGateId::EconomicReplay]);
        assert_eq!(invalid.validate(), Err(MetricError::InvalidScopeEvidence));
    }

    #[test]
    fn full_universe_and_audit_grade_fail_closed() {
        assert_eq!(
            ResultScopeV1::new(
                ScopeMode::FullUniverse,
                ConfigEvidenceTier::DecisionGrade,
                BTreeSet::from(["market".to_owned()]),
                None,
                None,
                "d".repeat(64),
                limited_decision(vec![HardGateId::TerminalState]),
            ),
            Err(MetricError::InvalidScopeEvidence)
        );

        let audit_limited = ResultScopeV1 {
            scope_mode: ScopeMode::FullUniverse,
            evidence_tier: ConfigEvidenceTier::AuditGrade,
            selected_markets: vec!["market".to_owned()],
            selected_market_set_sha256: selected_market_set_sha256(&BTreeSet::from([
                "market".to_owned()
            ])),
            cohort_selection_sha256: None,
            cohort_representativeness: None,
            final_phase_gate_sha256: "d".repeat(64),
            final_phase_gate_decision: PhaseGateDecisionV1 {
                evidence_tier: GateEvidenceTier::AuditGrade,
                ..limited_decision(vec![HardGateId::TerminalState])
            },
            legacy_global_profile_semantics: LEGACY_GLOBAL_PROFILE_SEMANTICS.to_owned(),
        };
        assert_eq!(
            audit_limited.validate(),
            Err(MetricError::InvalidScopeEvidence)
        );
    }

    fn activity(selected: u64, visible: u64) -> ScopeActivityCoverageV1 {
        ScopeActivityCoverageV1 {
            visible_universe_rows: visible,
            selected_panel_rows: selected,
            selected_panel_coverage_bps: u16::try_from(
                (u128::from(selected) * 10_000) / u128::from(visible),
            )
            .unwrap(),
        }
    }

    fn limited_decision(gates: Vec<HardGateId>) -> PhaseGateDecisionV1 {
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
            limitations: gates
                .into_iter()
                .map(|gate| crate::audit::GateLimitationV1 {
                    gate,
                    underlying_status: GateStatus::Fail,
                    detail_code: "typed_limitation".to_owned(),
                })
                .collect(),
        }
    }
}
