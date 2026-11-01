//! Deterministic protocol-valid profile exploration and holdout sealing.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
};

use num_bigint::BigUint;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    canonical::{ChainOrder, TokenAmount},
    factual_replay::V1RevenueDeltaV1,
    replay::HlpScenario,
};
use dusk::{
    constants::{MAX_HALF_LIFE_MS, MIN_HALF_LIFE_MS, NAD},
    state::{
        AmmConfig, IrmConfig, MAX_AMM_ADJUSTMENT_INTERVAL_SLOTS, MAX_AMM_ADJUSTMENT_NAD,
        MAX_AMM_AMPLIFICATION_NAD, MAX_AMM_FEE_COEFFICIENT_NAD, MAX_AMM_VOLATILITY_NAD,
        MAX_IRM_ADJUSTMENT_SPEED_PER_YEAR, MAX_IRM_CURVE_STEEPNESS_NAD,
        MAX_IRM_TARGET_UTILIZATION_BPS, MIN_AMM_ADJUSTMENT_NAD, MIN_IRM_ADJUSTMENT_SPEED_PER_YEAR,
        MIN_IRM_CURVE_STEEPNESS_NAD, MIN_IRM_TARGET_UTILIZATION_BPS, SWAP_FEE_COLLECT_INPUT_ASSET,
    },
};

pub const PROFILE_SCHEMA_VERSION: u16 = 1;
pub const PROFILE_FREEZE_SCHEMA_VERSION: u16 = 2;
pub const PROTOCOL_INTEREST_SCHEDULE_SCHEMA_VERSION: u16 = 1;

const INTEREST_BPS_DENOMINATOR: u16 = 10_000;
const PROTOCOL_INTEREST_SCHEDULE_DOMAIN: &[u8] =
    b"omnipair:v1-vs-dusk:protocol-interest-schedule:v1";
const PROTOCOL_INTEREST_SAMPLES_DOMAIN: &[u8] = b"omnipair:v1-vs-dusk:protocol-interest-samples:v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuskProfileV1 {
    pub schema_version: u16,
    pub profile_id: String,
    pub peak_amplification_nad: u64,
    pub core_half_width_bps: u16,
    pub fade_width_bps: u16,
    pub center_ema_half_life_ms: u64,
    pub adjustment_threshold_nad: u64,
    pub adjustment_step_nad: u64,
    pub min_adjustment_interval_slots: u64,
    pub divergence_fee_share_cap_bps: u16,
    pub volatility_fee_share_cap_bps: u16,
    pub divergence_fee_coefficient_nad: u64,
    pub volatility_fee_coefficient_nad: u64,
    pub volatility_half_life_ms: u64,
    pub volatility_shock_cap_nad: u64,
    pub volatility_accumulator_cap_nad: u64,
    pub compounding_fee_bps: u16,
    pub target_utilization_bps: u16,
    pub irm_curve_steepness_nad: u64,
    pub irm_adjustment_speed_per_year: u64,
}

impl DuskProfileV1 {
    /// The prospective Dusk program's checked-in defaults. This is a named
    /// baseline, not a calibrated or deployed profile.
    pub fn program_defaults() -> Self {
        let amm = AmmConfig::default();
        let irm = IrmConfig::default();
        let mut profile = Self {
            schema_version: PROFILE_SCHEMA_VERSION,
            profile_id: String::new(),
            peak_amplification_nad: amm.peak_amplification_nad,
            core_half_width_bps: amm.core_half_width_bps,
            fade_width_bps: amm.fade_width_bps,
            center_ema_half_life_ms: amm.center_ema_half_life_ms,
            adjustment_threshold_nad: amm.adjustment_threshold_nad,
            adjustment_step_nad: amm.adjustment_step_nad,
            min_adjustment_interval_slots: amm.min_adjustment_interval_slots,
            divergence_fee_share_cap_bps: 0,
            volatility_fee_share_cap_bps: 0,
            divergence_fee_coefficient_nad: amm.divergence_fee_coefficient_nad,
            volatility_fee_coefficient_nad: amm.volatility_fee_coefficient_nad,
            volatility_half_life_ms: amm.volatility_half_life_ms,
            volatility_shock_cap_nad: amm.volatility_shock_cap_nad,
            volatility_accumulator_cap_nad: amm.volatility_cap_nad,
            compounding_fee_bps: amm.compounding_fee_bps,
            target_utilization_bps: irm.target_utilization_bps,
            irm_curve_steepness_nad: irm.curve_steepness_nad,
            irm_adjustment_speed_per_year: irm.adjustment_speed_per_year,
        };
        profile.recompute_id();
        profile
    }

    pub fn validate_for_base_fee(&self, base_fee_bps: u16) -> Result<(), CalibrationError> {
        if self.schema_version != PROFILE_SCHEMA_VERSION {
            return Err(CalibrationError::SchemaVersion(self.schema_version));
        }
        let mut amm = AmmConfig::default();
        amm.peak_amplification_nad = self.peak_amplification_nad;
        amm.core_half_width_bps = self.core_half_width_bps;
        amm.fade_width_bps = self.fade_width_bps;
        amm.center_ema_half_life_ms = self.center_ema_half_life_ms;
        amm.adjustment_threshold_nad = self.adjustment_threshold_nad;
        amm.adjustment_step_nad = self.adjustment_step_nad;
        amm.min_adjustment_interval_slots = self.min_adjustment_interval_slots;
        amm.volatility_half_life_ms = self.volatility_half_life_ms;
        amm.volatility_shock_cap_nad = self.volatility_shock_cap_nad;
        amm.volatility_cap_nad = self.volatility_accumulator_cap_nad;
        amm.divergence_fee_coefficient_nad = self.divergence_fee_coefficient_nad;
        amm.volatility_fee_coefficient_nad = self.volatility_fee_coefficient_nad;
        amm.swap_fee_collect_mode = SWAP_FEE_COLLECT_INPUT_ASSET;
        amm.compounding_fee_bps = self.compounding_fee_bps;
        amm.validate()
            .map_err(|_| CalibrationError::InvalidProfile)?;

        let total_fee_cap = u32::from(base_fee_bps)
            + u32::from(self.divergence_fee_share_cap_bps)
            + u32::from(self.volatility_fee_share_cap_bps);
        if total_fee_cap > 5_000 {
            return Err(CalibrationError::InvalidProfile);
        }
        let irm = IrmConfig {
            target_utilization_bps: self.target_utilization_bps,
            curve_steepness_nad: self.irm_curve_steepness_nad,
            adjustment_speed_per_year: self.irm_adjustment_speed_per_year,
        };
        irm.validate().map_err(|_| CalibrationError::InvalidProfile)
    }

    pub fn recompute_id(&mut self) {
        self.profile_id.clear();
        let bytes = serde_json::to_vec(self).expect("profile serialization cannot fail");
        self.profile_id = hex::encode(Sha256::digest(bytes));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchDomainV1 {
    pub maximum_dynamic_fee_budget_bps: u16,
}

impl Default for SearchDomainV1 {
    fn default() -> Self {
        Self {
            maximum_dynamic_fee_budget_bps: 4_900,
        }
    }
}

/// Deterministic low-discrepancy exploration over every governed parameter
/// family. Invalid cross-field combinations are repaired into the valid domain
/// and then checked by Dusk's native validators.
pub fn explore_profiles(count: usize, seed: u64, domain: SearchDomainV1) -> Vec<DuskProfileV1> {
    const PRIMES: [u32; 18] = [
        2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47, 53, 59, 61,
    ];
    let offset = seed.max(1);
    let mut profiles = Vec::with_capacity(count);
    let mut index = 0_u64;
    while profiles.len() < count {
        let point = offset.saturating_add(index);
        index = index.saturating_add(1);
        let q = |axis: usize, low: u64, high: u64| {
            scale_radical_inverse(point, PRIMES[axis], low, high)
        };

        let peak = q(0, NAD + 1, MAX_AMM_AMPLIFICATION_NAD);
        let core = q(1, 1, 30_000) as u16;
        let fade_max = u64::from(u16::MAX - core);
        let fade = q(2, 0, fade_max) as u16;
        let step = q(4, MIN_AMM_ADJUSTMENT_NAD, MAX_AMM_ADJUSTMENT_NAD);
        let threshold = q(5, step, MAX_AMM_ADJUSTMENT_NAD);
        let volatility_cap = q(11, 1, MAX_AMM_VOLATILITY_NAD);
        let shock_cap = q(10, 1, volatility_cap);
        let divergence_cap = q(7, 0, u64::from(domain.maximum_dynamic_fee_budget_bps)) as u16;
        let volatility_cap_bps = q(
            8,
            0,
            u64::from(domain.maximum_dynamic_fee_budget_bps - divergence_cap),
        ) as u16;

        let mut profile = DuskProfileV1 {
            schema_version: PROFILE_SCHEMA_VERSION,
            profile_id: String::new(),
            peak_amplification_nad: peak,
            core_half_width_bps: core,
            fade_width_bps: fade,
            center_ema_half_life_ms: q(3, MIN_HALF_LIFE_MS, MAX_HALF_LIFE_MS),
            adjustment_threshold_nad: threshold,
            adjustment_step_nad: step,
            min_adjustment_interval_slots: q(6, 1, MAX_AMM_ADJUSTMENT_INTERVAL_SLOTS),
            divergence_fee_share_cap_bps: divergence_cap,
            volatility_fee_share_cap_bps: volatility_cap_bps,
            divergence_fee_coefficient_nad: q(9, 0, MAX_AMM_FEE_COEFFICIENT_NAD),
            volatility_fee_coefficient_nad: q(12, 0, MAX_AMM_FEE_COEFFICIENT_NAD),
            volatility_half_life_ms: q(13, MIN_HALF_LIFE_MS, MAX_HALF_LIFE_MS),
            volatility_shock_cap_nad: shock_cap,
            volatility_accumulator_cap_nad: volatility_cap,
            compounding_fee_bps: q(14, 0, 10_000) as u16,
            target_utilization_bps: q(
                15,
                u64::from(MIN_IRM_TARGET_UTILIZATION_BPS),
                u64::from(MAX_IRM_TARGET_UTILIZATION_BPS),
            ) as u16,
            irm_curve_steepness_nad: q(
                16,
                MIN_IRM_CURVE_STEEPNESS_NAD,
                MAX_IRM_CURVE_STEEPNESS_NAD,
            ),
            irm_adjustment_speed_per_year: q(
                17,
                MIN_IRM_ADJUSTMENT_SPEED_PER_YEAR,
                MAX_IRM_ADJUSTMENT_SPEED_PER_YEAR,
            ),
        };
        profile.recompute_id();
        // Base fee is epoch-specific. Zero is the least restrictive valid
        // screen; each factual epoch is validated during evaluation.
        if profile.validate_for_base_fee(0).is_ok() {
            profiles.push(profile);
        }
    }
    profiles
}

/// Deterministic local refinement around a Pareto shortlist. Each round
/// perturbs every governed axis in both directions with a halving lattice;
/// cross-field invariants are repaired before native validation. Profile IDs
/// deduplicate coincident boundary points.
pub fn refine_profiles(
    shortlist: &[DuskProfileV1],
    rounds: usize,
    domain: SearchDomainV1,
) -> Vec<DuskProfileV1> {
    let mut refined = BTreeMap::<String, DuskProfileV1>::new();
    for round in 0..rounds {
        for seed in shortlist {
            for axis in 0..18 {
                for increase in [false, true] {
                    let mut candidate = seed.clone();
                    mutate_axis(&mut candidate, axis, round, increase, domain);
                    repair_cross_fields(&mut candidate, domain);
                    candidate.recompute_id();
                    if candidate.validate_for_base_fee(0).is_ok()
                        && candidate.profile_id != seed.profile_id
                    {
                        refined.insert(candidate.profile_id.clone(), candidate);
                    }
                }
            }
        }
    }
    refined.into_values().collect()
}

fn mutate_axis(
    profile: &mut DuskProfileV1,
    axis: usize,
    round: usize,
    increase: bool,
    domain: SearchDomainV1,
) {
    let p64 = |value: u64, low: u64, high: u64| perturb_u64(value, low, high, round, increase);
    let p16 = |value: u16, low: u16, high: u16| {
        u16::try_from(perturb_u64(
            u64::from(value),
            u64::from(low),
            u64::from(high),
            round,
            increase,
        ))
        .unwrap_or(high)
    };
    match axis {
        0 => {
            profile.peak_amplification_nad = p64(
                profile.peak_amplification_nad,
                NAD + 1,
                MAX_AMM_AMPLIFICATION_NAD,
            )
        }
        1 => profile.core_half_width_bps = p16(profile.core_half_width_bps, 1, 30_000),
        2 => profile.fade_width_bps = p16(profile.fade_width_bps, 0, u16::MAX),
        3 => {
            profile.center_ema_half_life_ms = p64(
                profile.center_ema_half_life_ms,
                MIN_HALF_LIFE_MS,
                MAX_HALF_LIFE_MS,
            )
        }
        4 => {
            profile.adjustment_step_nad = p64(
                profile.adjustment_step_nad,
                MIN_AMM_ADJUSTMENT_NAD,
                MAX_AMM_ADJUSTMENT_NAD,
            )
        }
        5 => {
            profile.adjustment_threshold_nad = p64(
                profile.adjustment_threshold_nad,
                MIN_AMM_ADJUSTMENT_NAD,
                MAX_AMM_ADJUSTMENT_NAD,
            )
        }
        6 => {
            profile.min_adjustment_interval_slots = p64(
                profile.min_adjustment_interval_slots,
                1,
                MAX_AMM_ADJUSTMENT_INTERVAL_SLOTS,
            )
        }
        7 => {
            profile.divergence_fee_share_cap_bps = p16(
                profile.divergence_fee_share_cap_bps,
                0,
                domain.maximum_dynamic_fee_budget_bps,
            )
        }
        8 => {
            profile.volatility_fee_share_cap_bps = p16(
                profile.volatility_fee_share_cap_bps,
                0,
                domain.maximum_dynamic_fee_budget_bps,
            )
        }
        9 => {
            profile.divergence_fee_coefficient_nad = p64(
                profile.divergence_fee_coefficient_nad,
                0,
                MAX_AMM_FEE_COEFFICIENT_NAD,
            )
        }
        10 => {
            profile.volatility_shock_cap_nad =
                p64(profile.volatility_shock_cap_nad, 1, MAX_AMM_VOLATILITY_NAD)
        }
        11 => {
            profile.volatility_accumulator_cap_nad = p64(
                profile.volatility_accumulator_cap_nad,
                1,
                MAX_AMM_VOLATILITY_NAD,
            )
        }
        12 => {
            profile.volatility_fee_coefficient_nad = p64(
                profile.volatility_fee_coefficient_nad,
                0,
                MAX_AMM_FEE_COEFFICIENT_NAD,
            )
        }
        13 => {
            profile.volatility_half_life_ms = p64(
                profile.volatility_half_life_ms,
                MIN_HALF_LIFE_MS,
                MAX_HALF_LIFE_MS,
            )
        }
        14 => profile.compounding_fee_bps = p16(profile.compounding_fee_bps, 0, 10_000),
        15 => {
            profile.target_utilization_bps = p16(
                profile.target_utilization_bps,
                MIN_IRM_TARGET_UTILIZATION_BPS,
                MAX_IRM_TARGET_UTILIZATION_BPS,
            )
        }
        16 => {
            profile.irm_curve_steepness_nad = p64(
                profile.irm_curve_steepness_nad,
                MIN_IRM_CURVE_STEEPNESS_NAD,
                MAX_IRM_CURVE_STEEPNESS_NAD,
            )
        }
        17 => {
            profile.irm_adjustment_speed_per_year = p64(
                profile.irm_adjustment_speed_per_year,
                MIN_IRM_ADJUSTMENT_SPEED_PER_YEAR,
                MAX_IRM_ADJUSTMENT_SPEED_PER_YEAR,
            )
        }
        _ => unreachable!("governed profile axis"),
    }
}

fn perturb_u64(value: u64, low: u64, high: u64, round: usize, increase: bool) -> u64 {
    if low >= high {
        return low;
    }
    let divisor_shift = u32::try_from(round.saturating_add(3)).unwrap_or(63).min(63);
    let step = ((high - low) >> divisor_shift).max(1);
    if increase {
        value.saturating_add(step).min(high)
    } else {
        value.saturating_sub(step).max(low)
    }
}

fn repair_cross_fields(profile: &mut DuskProfileV1, domain: SearchDomainV1) {
    profile.fade_width_bps = profile
        .fade_width_bps
        .min(u16::MAX.saturating_sub(profile.core_half_width_bps));
    profile.adjustment_threshold_nad = profile
        .adjustment_threshold_nad
        .max(profile.adjustment_step_nad);
    profile.volatility_shock_cap_nad = profile
        .volatility_shock_cap_nad
        .min(profile.volatility_accumulator_cap_nad);
    let remaining = domain
        .maximum_dynamic_fee_budget_bps
        .saturating_sub(profile.divergence_fee_share_cap_bps);
    profile.volatility_fee_share_cap_bps = profile.volatility_fee_share_cap_bps.min(remaining);
}

fn scale_radical_inverse(index: u64, base: u32, low: u64, high: u64) -> u64 {
    if low >= high {
        return low;
    }
    // Keep a fixed 2^48 fractional grid so exploration is reproducible without
    // floating point differences across platforms.
    const SCALE: u128 = 1_u128 << 48;
    let mut n = index;
    let mut denominator = u128::from(base);
    let mut fraction = 0_u128;
    while n > 0 && denominator <= SCALE.saturating_mul(u128::from(base)) {
        let digit = n % u64::from(base);
        fraction = fraction.saturating_add(
            u128::from(digit)
                .saturating_mul(SCALE)
                .checked_div(denominator)
                .unwrap_or(0),
        );
        n /= u64::from(base);
        denominator = denominator.saturating_mul(u128::from(base));
    }
    let width = u128::from(high - low);
    low.saturating_add(
        u64::try_from(
            width
                .saturating_mul(fraction)
                .checked_div(SCALE)
                .unwrap_or(0),
        )
        .unwrap_or(high - low),
    )
    .min(high)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateScoreV1 {
    pub profile_id: String,
    pub hlp_scenario: HlpScenario,
    /// Dusk minus V1; non-positive passes the safety gate.
    pub bad_debt_delta_usd_micro: i128,
    pub tail_liquidation_shortfall_delta_usd_micro: i128,
    pub volume_weighted_execution_cost_delta_ppb: i64,
    pub p95_slippage_delta_ppb: i64,
    pub capital_weighted_stakeholder_revenue_uplift_bps: i64,
    pub equal_market_stakeholder_revenue_uplift_bps: i64,
    pub quote_quality_uplift_ppb: i64,
    pub borrow_capacity_uplift_bps: i64,
}

impl CandidateScoreV1 {
    pub fn passes_gates(&self) -> bool {
        self.bad_debt_delta_usd_micro <= 0
            && self.tail_liquidation_shortfall_delta_usd_micro <= 0
            && self.volume_weighted_execution_cost_delta_ppb <= 0
            && self.p95_slippage_delta_ppb <= 0
    }

    fn objective_tuple(&self) -> (i64, i64, i64, &str) {
        (
            self.capital_weighted_stakeholder_revenue_uplift_bps
                .min(self.equal_market_stakeholder_revenue_uplift_bps),
            self.quote_quality_uplift_ppb,
            self.borrow_capacity_uplift_bps,
            &self.profile_id,
        )
    }
}

pub fn select_global_winner<'a>(scores: &'a [CandidateScoreV1]) -> Option<&'a CandidateScoreV1> {
    scores
        .iter()
        .filter(|score| score.passes_gates())
        .max_by(|left, right| compare_objective(left, right))
}

fn compare_objective(left: &CandidateScoreV1, right: &CandidateScoreV1) -> Ordering {
    let (l_min, l_quote, l_borrow, l_id) = left.objective_tuple();
    let (r_min, r_quote, r_borrow, r_id) = right.objective_tuple();
    l_min
        .cmp(&r_min)
        .then(l_quote.cmp(&r_quote))
        .then(l_borrow.cmp(&r_borrow))
        // A lexicographically smaller ID is the deterministic final tie break.
        .then_with(|| r_id.cmp(l_id))
}

/// Immutable identity of one market's active V1 protocol-revenue
/// configuration epoch. `config_epoch_id` is the canonical
/// `ConfigVersionV1.config_id`, not a checkpoint hash.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ProtocolInterestEpochKeyV1 {
    pub market: String,
    pub config_epoch_id: String,
}

/// Exact training-window provenance used to derive the schedule. The hashes
/// must identify the physically sealed training prefixes, never a full-history
/// file that includes validation or holdout rows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolInterestTrainingSourceV1 {
    pub operations_sha256: String,
    pub factual_ledger_sha256: String,
    pub training_start_unix_seconds: i64,
    pub training_end_unix_seconds_exclusive: i64,
}

/// Unitless importance multipliers applied only after each token side's error
/// is normalized by that side's own borrower-interest total. Raw token atoms
/// from different mints are never added together.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolInterestSideWeightsV1 {
    pub token0: u64,
    pub token1: u64,
}

impl Default for ProtocolInterestSideWeightsV1 {
    fn default() -> Self {
        Self {
            token0: 1,
            token1: 1,
        }
    }
}

/// One event-exact V1 interest observation. `lp_interest*` is the economic LP
/// entitlement after assigning any cash-uncovered nominal protocol surcharge
/// to LPs; borrower interest is therefore `lp_interest + cash_covered`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolInterestTrainingSampleV1 {
    pub operation_id: String,
    pub order: ChainOrder,
    pub block_time_unix_seconds: i64,
    pub lp_interest0: TokenAmount,
    pub lp_interest1: TokenAmount,
    pub nominal_protocol_interest0: TokenAmount,
    pub nominal_protocol_interest1: TokenAmount,
    pub cash_covered_protocol_interest0: TokenAmount,
    pub cash_covered_protocol_interest1: TokenAmount,
}

impl ProtocolInterestTrainingSampleV1 {
    /// Converts one reconciled factual delta into a fitting sample. Rows with
    /// no borrower interest on either side are intentionally omitted.
    pub fn from_revenue_delta(
        operation_id: String,
        order: ChainOrder,
        block_time_unix_seconds: i64,
        delta: &V1RevenueDeltaV1,
    ) -> Result<Option<Self>, CalibrationError> {
        if operation_id.is_empty() {
            return Err(CalibrationError::InvalidInterestSchedule(
                "training sample operation ID is empty",
            ));
        }
        if delta.protocol_interest0 != delta.cash_covered_protocol_interest0
            || delta.protocol_interest1 != delta.cash_covered_protocol_interest1
        {
            return Err(CalibrationError::InvalidInterestSchedule(
                "economic protocol interest differs from cash-covered source evidence",
            ));
        }
        let borrower0 = delta
            .lp_interest0
            .checked_add(delta.cash_covered_protocol_interest0)
            .ok_or(CalibrationError::InterestScheduleArithmetic(
                "token0 borrower interest",
            ))?;
        let borrower1 = delta
            .lp_interest1
            .checked_add(delta.cash_covered_protocol_interest1)
            .ok_or(CalibrationError::InterestScheduleArithmetic(
                "token1 borrower interest",
            ))?;
        if borrower0 == 0 && borrower1 == 0 {
            return Ok(None);
        }
        Ok(Some(Self {
            operation_id,
            order,
            block_time_unix_seconds,
            lp_interest0: token_amount(delta.lp_interest0),
            lp_interest1: token_amount(delta.lp_interest1),
            nominal_protocol_interest0: token_amount(delta.nominal_protocol_interest0),
            nominal_protocol_interest1: token_amount(delta.nominal_protocol_interest1),
            cash_covered_protocol_interest0: token_amount(delta.cash_covered_protocol_interest0),
            cash_covered_protocol_interest1: token_amount(delta.cash_covered_protocol_interest1),
        }))
    }
}

/// Training-only input for one exact market/configuration interval.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolInterestTrainingEpochV1 {
    pub key: ProtocolInterestEpochKeyV1,
    pub valid_from: ChainOrder,
    pub valid_to_exclusive: Option<ChainOrder>,
    pub v1_interest_surcharge_bps: u16,
    pub samples: Vec<ProtocolInterestTrainingSampleV1>,
}

/// Frozen result for one market/configuration epoch. Totals and fitting errors
/// remain separate by mint so the artifact never treats unlike atoms as one
/// quantity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolInterestShareEpochV1 {
    pub key: ProtocolInterestEpochKeyV1,
    pub valid_from: ChainOrder,
    pub valid_to_exclusive: Option<ChainOrder>,
    pub v1_interest_surcharge_bps: u16,
    pub dusk_protocol_interest_share_bps: u16,
    /// False means this epoch had no positive training accrual. Its Dusk share
    /// is the deterministic nominal conversion, not a cash-covered fit;
    /// evaluation must reject any positive accrual under such an epoch.
    pub training_fit_available: bool,
    pub training_observation_count: u64,
    pub training_samples_sha256: String,
    pub token0_borrower_interest: TokenAmount,
    pub token0_nominal_protocol_interest: TokenAmount,
    pub token0_cash_covered_protocol_interest: TokenAmount,
    pub token0_absolute_floor_error: TokenAmount,
    pub token1_borrower_interest: TokenAmount,
    pub token1_nominal_protocol_interest: TokenAmount,
    pub token1_cash_covered_protocol_interest: TokenAmount,
    pub token1_absolute_floor_error: TokenAmount,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolInterestShareScheduleV1 {
    pub schema_version: u16,
    pub source: ProtocolInterestTrainingSourceV1,
    pub side_weights: ProtocolInterestSideWeightsV1,
    pub epochs: Vec<ProtocolInterestShareEpochV1>,
    pub schedule_sha256: String,
}

impl ProtocolInterestShareScheduleV1 {
    pub fn seal(mut self) -> Result<Self, CalibrationError> {
        self.schedule_sha256.clear();
        self.validate_contents()?;
        self.schedule_sha256 = digest_serialized(PROTOCOL_INTEREST_SCHEDULE_DOMAIN, &self)?;
        Ok(self)
    }

    pub fn verify(&self) -> Result<(), CalibrationError> {
        self.validate_contents()?;
        if !is_canonical_sha256(&self.schedule_sha256) {
            return Err(CalibrationError::InterestScheduleChecksum);
        }
        let expected = self.clone().seal()?.schedule_sha256;
        if expected != self.schedule_sha256 {
            return Err(CalibrationError::InterestScheduleChecksum);
        }
        Ok(())
    }

    /// Resolves an exact market/configuration epoch. Unknown future epochs and
    /// orders outside the frozen interval fail instead of inheriting a previous
    /// fee share.
    pub fn resolve(
        &self,
        market: &str,
        config_epoch_id: &str,
        order: ChainOrder,
    ) -> Result<&ProtocolInterestShareEpochV1, CalibrationError> {
        let matches: Vec<_> = self
            .epochs
            .iter()
            .filter(|epoch| {
                epoch.key.market == market && epoch.key.config_epoch_id == config_epoch_id
            })
            .collect();
        if matches.len() != 1 {
            return Err(CalibrationError::MissingInterestEpoch {
                market: market.to_owned(),
                config_epoch_id: config_epoch_id.to_owned(),
            });
        }
        let epoch = matches[0];
        if order < epoch.valid_from
            || epoch
                .valid_to_exclusive
                .is_some_and(|exclusive| order >= exclusive)
        {
            return Err(CalibrationError::InterestEpochOutOfRange {
                market: market.to_owned(),
                config_epoch_id: config_epoch_id.to_owned(),
            });
        }
        Ok(epoch)
    }

    /// Resolves an evaluation row that carries canonical order and the factual
    /// V1 surcharge but not its configuration ID. Exactly one frozen interval
    /// must contain the order, and its factual surcharge is an assertion.
    pub fn resolve_at(
        &self,
        market: &str,
        order: ChainOrder,
        factual_v1_interest_surcharge_bps: u16,
    ) -> Result<&ProtocolInterestShareEpochV1, CalibrationError> {
        let mut matches = self.epochs.iter().filter(|epoch| {
            epoch.key.market == market
                && order >= epoch.valid_from
                && epoch
                    .valid_to_exclusive
                    .is_none_or(|exclusive| order < exclusive)
        });
        let epoch =
            matches
                .next()
                .ok_or_else(|| CalibrationError::MissingInterestEpochAtOrder {
                    market: market.to_owned(),
                    order,
                })?;
        if matches.next().is_some() {
            return Err(CalibrationError::AmbiguousInterestEpochAtOrder {
                market: market.to_owned(),
                order,
            });
        }
        if epoch.v1_interest_surcharge_bps != factual_v1_interest_surcharge_bps {
            return Err(CalibrationError::InterestEpochSurchargeMismatch {
                market: market.to_owned(),
                config_epoch_id: epoch.key.config_epoch_id.clone(),
                scheduled_bps: epoch.v1_interest_surcharge_bps,
                factual_bps: factual_v1_interest_surcharge_bps,
            });
        }
        Ok(epoch)
    }

    fn validate_contents(&self) -> Result<(), CalibrationError> {
        if self.schema_version != PROTOCOL_INTEREST_SCHEDULE_SCHEMA_VERSION {
            return Err(CalibrationError::InterestScheduleSchemaVersion(
                self.schema_version,
            ));
        }
        validate_training_source(&self.source)?;
        if self.side_weights.token0 == 0 || self.side_weights.token1 == 0 {
            return Err(CalibrationError::InvalidInterestSchedule(
                "side weights must be positive",
            ));
        }
        if self.epochs.is_empty() {
            return Err(CalibrationError::InvalidInterestSchedule(
                "interest schedule has no epochs",
            ));
        }
        if self.epochs.windows(2).any(|epochs| {
            (
                &epochs[0].key.market,
                epochs[0].valid_from,
                &epochs[0].key.config_epoch_id,
            ) >= (
                &epochs[1].key.market,
                epochs[1].valid_from,
                &epochs[1].key.config_epoch_id,
            )
        }) {
            return Err(CalibrationError::InvalidInterestSchedule(
                "interest schedule epochs are not in canonical order",
            ));
        }
        let mut keys = BTreeSet::new();
        let mut previous_by_market = BTreeMap::<&str, &ProtocolInterestShareEpochV1>::new();
        for epoch in &self.epochs {
            validate_frozen_epoch(epoch)?;
            if !keys.insert((&epoch.key.market, &epoch.key.config_epoch_id)) {
                return Err(CalibrationError::InvalidInterestSchedule(
                    "duplicate market/config interest epoch",
                ));
            }
            if let Some(previous) = previous_by_market.insert(&epoch.key.market, epoch) {
                if previous.valid_from >= epoch.valid_from
                    || previous.valid_to_exclusive != Some(epoch.valid_from)
                {
                    return Err(CalibrationError::InvalidInterestSchedule(
                        "market interest epochs are not strictly ordered and contiguous",
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Fits every protocol-valid Dusk basis-point value and seals the unique
/// deterministic winner for every supplied training epoch.
pub fn derive_protocol_interest_share_schedule(
    source: ProtocolInterestTrainingSourceV1,
    side_weights: ProtocolInterestSideWeightsV1,
    mut epochs: Vec<ProtocolInterestTrainingEpochV1>,
) -> Result<ProtocolInterestShareScheduleV1, CalibrationError> {
    validate_training_source(&source)?;
    if side_weights.token0 == 0 || side_weights.token1 == 0 {
        return Err(CalibrationError::InvalidInterestSchedule(
            "side weights must be positive",
        ));
    }
    epochs.sort_by(|left, right| {
        left.key
            .market
            .cmp(&right.key.market)
            .then(left.valid_from.cmp(&right.valid_from))
            .then(left.key.config_epoch_id.cmp(&right.key.config_epoch_id))
    });
    let mut frozen = Vec::with_capacity(epochs.len());
    for epoch in epochs {
        frozen.push(fit_protocol_interest_epoch(&source, side_weights, epoch)?);
    }
    ProtocolInterestShareScheduleV1 {
        schema_version: PROTOCOL_INTEREST_SCHEDULE_SCHEMA_VERSION,
        source,
        side_weights,
        epochs: frozen,
        schedule_sha256: String::new(),
    }
    .seal()
}

#[derive(Clone, Copy)]
struct SideFitV1 {
    borrower_interest: u128,
    nominal_protocol_interest: u128,
    cash_covered_protocol_interest: u128,
    absolute_floor_error: u128,
}

#[derive(Clone, Copy)]
struct CandidateInterestFitV1 {
    bps: u16,
    token0: SideFitV1,
    token1: SideFitV1,
}

fn fit_protocol_interest_epoch(
    source: &ProtocolInterestTrainingSourceV1,
    weights: ProtocolInterestSideWeightsV1,
    mut epoch: ProtocolInterestTrainingEpochV1,
) -> Result<ProtocolInterestShareEpochV1, CalibrationError> {
    if epoch.key.market.is_empty() || !is_canonical_sha256(&epoch.key.config_epoch_id) {
        return Err(CalibrationError::InvalidInterestSchedule(
            "interest epoch identity is invalid",
        ));
    }
    if epoch.v1_interest_surcharge_bps > INTEREST_BPS_DENOMINATOR {
        return Err(CalibrationError::InvalidInterestSchedule(
            "V1 interest surcharge exceeds 10000 bps",
        ));
    }
    if epoch
        .valid_to_exclusive
        .is_some_and(|exclusive| epoch.valid_from >= exclusive)
    {
        return Err(CalibrationError::InvalidInterestSchedule(
            "interest epoch interval is empty or reversed",
        ));
    }
    epoch.samples.sort_by(|left, right| {
        left.order
            .cmp(&right.order)
            .then(left.operation_id.cmp(&right.operation_id))
    });
    let unique_operation_ids: BTreeSet<_> = epoch
        .samples
        .iter()
        .map(|sample| &sample.operation_id)
        .collect();
    if unique_operation_ids.len() != epoch.samples.len() {
        return Err(CalibrationError::InvalidInterestSchedule(
            "interest epoch contains duplicate operation IDs",
        ));
    }
    if epoch
        .samples
        .windows(2)
        .any(|rows| rows[0].order >= rows[1].order)
    {
        return Err(CalibrationError::InvalidInterestSchedule(
            "interest epoch samples are not in strict canonical order",
        ));
    }

    let mut parsed = Vec::with_capacity(epoch.samples.len());
    for sample in &epoch.samples {
        if sample.operation_id.is_empty()
            || sample.order < epoch.valid_from
            || epoch
                .valid_to_exclusive
                .is_some_and(|exclusive| sample.order >= exclusive)
            || sample.block_time_unix_seconds < source.training_start_unix_seconds
            || sample.block_time_unix_seconds >= source.training_end_unix_seconds_exclusive
        {
            return Err(CalibrationError::InvalidInterestSchedule(
                "training sample is outside its frozen epoch or training window",
            ));
        }
        parsed.push(parse_training_sample(
            sample,
            epoch.v1_interest_surcharge_bps,
        )?);
    }

    let training_fit_available = !parsed.is_empty();
    let winner = if training_fit_available {
        let mut winner = None::<CandidateInterestFitV1>;
        for bps in 0..=INTEREST_BPS_DENOMINATOR {
            let candidate = fit_candidate(bps, &parsed)?;
            if winner.as_ref().is_none_or(|current| {
                compare_interest_fit(&candidate, current, weights) == Ordering::Less
            }) {
                winner = Some(candidate);
            }
        }
        winner.expect("the exhaustive protocol-valid domain is non-empty")
    } else {
        CandidateInterestFitV1 {
            bps: nominal_conversion_bps(epoch.v1_interest_surcharge_bps),
            token0: empty_side_fit(),
            token1: empty_side_fit(),
        }
    };
    let samples_sha256 = digest_serialized(PROTOCOL_INTEREST_SAMPLES_DOMAIN, &epoch.samples)?;
    Ok(ProtocolInterestShareEpochV1 {
        key: epoch.key,
        valid_from: epoch.valid_from,
        valid_to_exclusive: epoch.valid_to_exclusive,
        v1_interest_surcharge_bps: epoch.v1_interest_surcharge_bps,
        dusk_protocol_interest_share_bps: winner.bps,
        training_fit_available,
        training_observation_count: u64::try_from(epoch.samples.len()).map_err(|_| {
            CalibrationError::InterestScheduleArithmetic("training observation count")
        })?,
        training_samples_sha256: samples_sha256,
        token0_borrower_interest: token_amount(winner.token0.borrower_interest),
        token0_nominal_protocol_interest: token_amount(winner.token0.nominal_protocol_interest),
        token0_cash_covered_protocol_interest: token_amount(
            winner.token0.cash_covered_protocol_interest,
        ),
        token0_absolute_floor_error: token_amount(winner.token0.absolute_floor_error),
        token1_borrower_interest: token_amount(winner.token1.borrower_interest),
        token1_nominal_protocol_interest: token_amount(winner.token1.nominal_protocol_interest),
        token1_cash_covered_protocol_interest: token_amount(
            winner.token1.cash_covered_protocol_interest,
        ),
        token1_absolute_floor_error: token_amount(winner.token1.absolute_floor_error),
    })
}

fn empty_side_fit() -> SideFitV1 {
    SideFitV1 {
        borrower_interest: 0,
        nominal_protocol_interest: 0,
        cash_covered_protocol_interest: 0,
        absolute_floor_error: 0,
    }
}

/// Converts V1's surcharge on native LP interest into Dusk's share of total
/// borrower interest. The exhaustive integer-domain comparison minimizes
/// `|bps / 10000 - surcharge / (10000 + surcharge)|` with lower-bps ties.
fn nominal_conversion_bps(v1_interest_surcharge_bps: u16) -> u16 {
    let surcharge = u64::from(v1_interest_surcharge_bps);
    let denominator = u64::from(INTEREST_BPS_DENOMINATOR) + surcharge;
    let target_cross = u64::from(INTEREST_BPS_DENOMINATOR) * surcharge;
    (0..=INTEREST_BPS_DENOMINATOR)
        .min_by_key(|bps| (u64::from(*bps) * denominator).abs_diff(target_cross))
        .expect("the exhaustive protocol-valid domain is non-empty")
}

#[derive(Clone, Copy)]
struct ParsedInterestSampleV1 {
    borrower0: u128,
    nominal0: u128,
    target0: u128,
    borrower1: u128,
    nominal1: u128,
    target1: u128,
}

fn parse_training_sample(
    sample: &ProtocolInterestTrainingSampleV1,
    v1_interest_surcharge_bps: u16,
) -> Result<ParsedInterestSampleV1, CalibrationError> {
    let lp0 = parse_token_amount(&sample.lp_interest0)?;
    let lp1 = parse_token_amount(&sample.lp_interest1)?;
    let nominal0 = parse_token_amount(&sample.nominal_protocol_interest0)?;
    let nominal1 = parse_token_amount(&sample.nominal_protocol_interest1)?;
    let target0 = parse_token_amount(&sample.cash_covered_protocol_interest0)?;
    let target1 = parse_token_amount(&sample.cash_covered_protocol_interest1)?;
    let borrower0 = validate_v1_interest_side(lp0, nominal0, target0, v1_interest_surcharge_bps)?;
    let borrower1 = validate_v1_interest_side(lp1, nominal1, target1, v1_interest_surcharge_bps)?;
    if borrower0 == 0 && borrower1 == 0 {
        return Err(CalibrationError::InvalidInterestSchedule(
            "training sample has no borrower interest",
        ));
    }
    Ok(ParsedInterestSampleV1 {
        borrower0,
        nominal0,
        target0,
        borrower1,
        nominal1,
        target1,
    })
}

fn validate_v1_interest_side(
    economic_lp_interest: u128,
    nominal_protocol_interest: u128,
    cash_covered_protocol_interest: u128,
    v1_interest_surcharge_bps: u16,
) -> Result<u128, CalibrationError> {
    let uncovered = nominal_protocol_interest
        .checked_sub(cash_covered_protocol_interest)
        .ok_or(CalibrationError::InvalidInterestSchedule(
            "cash-covered protocol interest exceeds nominal protocol interest",
        ))?;
    let native_lp_interest = economic_lp_interest.checked_sub(uncovered).ok_or(
        CalibrationError::InvalidInterestSchedule(
            "economic LP interest is below its cash-uncovered entitlement",
        ),
    )?;
    if floor_bps(native_lp_interest, v1_interest_surcharge_bps)? != nominal_protocol_interest {
        return Err(CalibrationError::InvalidInterestSchedule(
            "training raw interest does not match the V1 surcharge formula",
        ));
    }
    economic_lp_interest
        .checked_add(cash_covered_protocol_interest)
        .ok_or(CalibrationError::InterestScheduleArithmetic(
            "borrower interest",
        ))
}

fn fit_candidate(
    bps: u16,
    samples: &[ParsedInterestSampleV1],
) -> Result<CandidateInterestFitV1, CalibrationError> {
    let mut token0 = SideFitV1 {
        borrower_interest: 0,
        nominal_protocol_interest: 0,
        cash_covered_protocol_interest: 0,
        absolute_floor_error: 0,
    };
    let mut token1 = token0;
    for sample in samples {
        accumulate_side_fit(
            &mut token0,
            sample.borrower0,
            sample.nominal0,
            sample.target0,
            bps,
        )?;
        accumulate_side_fit(
            &mut token1,
            sample.borrower1,
            sample.nominal1,
            sample.target1,
            bps,
        )?;
    }
    if token0.borrower_interest == 0 && token1.borrower_interest == 0 {
        return Err(CalibrationError::InvalidInterestSchedule(
            "interest epoch has no borrower-interest denominator",
        ));
    }
    Ok(CandidateInterestFitV1 {
        bps,
        token0,
        token1,
    })
}

fn accumulate_side_fit(
    side: &mut SideFitV1,
    borrower_interest: u128,
    nominal_protocol_interest: u128,
    cash_covered_protocol_interest: u128,
    bps: u16,
) -> Result<(), CalibrationError> {
    if borrower_interest == 0 && cash_covered_protocol_interest != 0 {
        return Err(CalibrationError::InvalidInterestSchedule(
            "zero borrower-interest denominator has a nonzero protocol target",
        ));
    }
    let predicted = floor_bps(borrower_interest, bps)?;
    let error = predicted.abs_diff(cash_covered_protocol_interest);
    side.borrower_interest = checked_add_interest(side.borrower_interest, borrower_interest)?;
    side.nominal_protocol_interest =
        checked_add_interest(side.nominal_protocol_interest, nominal_protocol_interest)?;
    side.cash_covered_protocol_interest = checked_add_interest(
        side.cash_covered_protocol_interest,
        cash_covered_protocol_interest,
    )?;
    side.absolute_floor_error = checked_add_interest(side.absolute_floor_error, error)?;
    Ok(())
}

fn compare_interest_fit(
    left: &CandidateInterestFitV1,
    right: &CandidateInterestFitV1,
    weights: ProtocolInterestSideWeightsV1,
) -> Ordering {
    let (left_max, left_other) = ordered_side_ratios(left, weights);
    let (right_max, right_other) = ordered_side_ratios(right, weights);
    compare_ratio(left_max, right_max)
        .then_with(|| compare_ratio(left_other, right_other))
        .then(left.bps.cmp(&right.bps))
}

#[derive(Clone, Copy)]
struct SideErrorRatioV1 {
    error: u128,
    borrower_interest: u128,
    weight: u64,
}

fn ordered_side_ratios(
    fit: &CandidateInterestFitV1,
    weights: ProtocolInterestSideWeightsV1,
) -> (SideErrorRatioV1, SideErrorRatioV1) {
    let token0 = SideErrorRatioV1 {
        error: fit.token0.absolute_floor_error,
        borrower_interest: fit.token0.borrower_interest,
        weight: weights.token0,
    };
    let token1 = SideErrorRatioV1 {
        error: fit.token1.absolute_floor_error,
        borrower_interest: fit.token1.borrower_interest,
        weight: weights.token1,
    };
    match (token0.borrower_interest == 0, token1.borrower_interest == 0) {
        (false, false) if compare_ratio(token0, token1) == Ordering::Less => (token1, token0),
        (false, false) => (token0, token1),
        (false, true) => (token0, zero_ratio()),
        (true, false) => (token1, zero_ratio()),
        (true, true) => (zero_ratio(), zero_ratio()),
    }
}

fn zero_ratio() -> SideErrorRatioV1 {
    SideErrorRatioV1 {
        error: 0,
        borrower_interest: 1,
        weight: 1,
    }
}

fn compare_ratio(left: SideErrorRatioV1, right: SideErrorRatioV1) -> Ordering {
    if left.borrower_interest == 0 || right.borrower_interest == 0 {
        return left
            .borrower_interest
            .cmp(&right.borrower_interest)
            .reverse();
    }
    let left_cross = BigUint::from(left.error)
        * BigUint::from(left.weight)
        * BigUint::from(right.borrower_interest);
    let right_cross = BigUint::from(right.error)
        * BigUint::from(right.weight)
        * BigUint::from(left.borrower_interest);
    left_cross.cmp(&right_cross)
}

fn validate_training_source(
    source: &ProtocolInterestTrainingSourceV1,
) -> Result<(), CalibrationError> {
    if !is_canonical_sha256(&source.operations_sha256)
        || !is_canonical_sha256(&source.factual_ledger_sha256)
        || source.training_start_unix_seconds >= source.training_end_unix_seconds_exclusive
    {
        return Err(CalibrationError::InvalidInterestSchedule(
            "training source identity or window is invalid",
        ));
    }
    Ok(())
}

fn validate_frozen_epoch(epoch: &ProtocolInterestShareEpochV1) -> Result<(), CalibrationError> {
    if epoch.key.market.is_empty()
        || !is_canonical_sha256(&epoch.key.config_epoch_id)
        || !is_canonical_sha256(&epoch.training_samples_sha256)
        || epoch.v1_interest_surcharge_bps > INTEREST_BPS_DENOMINATOR
        || epoch.dusk_protocol_interest_share_bps > INTEREST_BPS_DENOMINATOR
        || epoch
            .valid_to_exclusive
            .is_some_and(|exclusive| epoch.valid_from >= exclusive)
    {
        return Err(CalibrationError::InvalidInterestSchedule(
            "frozen interest epoch metadata is invalid",
        ));
    }
    let borrower0 = parse_token_amount(&epoch.token0_borrower_interest)?;
    let borrower1 = parse_token_amount(&epoch.token1_borrower_interest)?;
    let target0 = parse_token_amount(&epoch.token0_cash_covered_protocol_interest)?;
    let target1 = parse_token_amount(&epoch.token1_cash_covered_protocol_interest)?;
    let error0 = parse_token_amount(&epoch.token0_absolute_floor_error)?;
    let error1 = parse_token_amount(&epoch.token1_absolute_floor_error)?;
    let nominal0 = parse_token_amount(&epoch.token0_nominal_protocol_interest)?;
    let nominal1 = parse_token_amount(&epoch.token1_nominal_protocol_interest)?;
    if (borrower0 == 0 && target0 != 0)
        || (borrower1 == 0 && target1 != 0)
        || target0 > borrower0
        || target1 > borrower1
        || nominal0 > borrower0
        || nominal1 > borrower1
        || error0 > borrower0
        || error1 > borrower1
    {
        return Err(CalibrationError::InvalidInterestSchedule(
            "frozen interest epoch proof totals are invalid",
        ));
    }
    if epoch.training_fit_available {
        if epoch.training_observation_count == 0 || (borrower0 == 0 && borrower1 == 0) {
            return Err(CalibrationError::InvalidInterestSchedule(
                "available interest fit has no training evidence",
            ));
        }
    } else {
        let empty_samples = Vec::<ProtocolInterestTrainingSampleV1>::new();
        let empty_samples_sha256 =
            digest_serialized(PROTOCOL_INTEREST_SAMPLES_DOMAIN, &empty_samples)?;
        if epoch.training_observation_count != 0
            || borrower0 != 0
            || borrower1 != 0
            || nominal0 != 0
            || nominal1 != 0
            || target0 != 0
            || target1 != 0
            || error0 != 0
            || error1 != 0
            || epoch.training_samples_sha256 != empty_samples_sha256
            || epoch.dusk_protocol_interest_share_bps
                != nominal_conversion_bps(epoch.v1_interest_surcharge_bps)
        {
            return Err(CalibrationError::InvalidInterestSchedule(
                "unavailable interest fit does not match its nominal zero-sample mapping",
            ));
        }
    }
    Ok(())
}

fn floor_bps(amount: u128, bps: u16) -> Result<u128, CalibrationError> {
    if bps > INTEREST_BPS_DENOMINATOR {
        return Err(CalibrationError::InvalidInterestSchedule(
            "basis points exceed 10000",
        ));
    }
    let denominator = u128::from(INTEREST_BPS_DENOMINATOR);
    let bps = u128::from(bps);
    let high = (amount / denominator).checked_mul(bps).ok_or(
        CalibrationError::InterestScheduleArithmetic("basis-point quotient"),
    )?;
    let low = (amount % denominator).checked_mul(bps).ok_or(
        CalibrationError::InterestScheduleArithmetic("basis-point remainder"),
    )? / denominator;
    high.checked_add(low)
        .ok_or(CalibrationError::InterestScheduleArithmetic(
            "basis-point result",
        ))
}

fn checked_add_interest(left: u128, right: u128) -> Result<u128, CalibrationError> {
    left.checked_add(right)
        .ok_or(CalibrationError::InterestScheduleArithmetic(
            "training interest total",
        ))
}

fn parse_token_amount(value: &TokenAmount) -> Result<u128, CalibrationError> {
    value
        .as_str()
        .parse::<u128>()
        .map_err(|_| CalibrationError::InvalidInterestSchedule("token amount exceeds u128"))
}

fn token_amount(value: u128) -> TokenAmount {
    TokenAmount::new(value.to_string()).expect("u128 formatting is canonical")
}

fn digest_serialized<T: Serialize>(domain: &[u8], value: &T) -> Result<String, CalibrationError> {
    let bytes = serde_json::to_vec(value).map_err(|_| CalibrationError::Serialization)?;
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
    Ok(hex::encode(digest.finalize()))
}

fn is_canonical_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileFreezeV1 {
    pub schema_version: u16,
    pub dataset_id: String,
    pub config_sha256: String,
    pub dusk_source_sha256: String,
    /// Required commitment to the sealed, training-only per-epoch protocol
    /// interest allocation schedule used by every evaluation arm.
    pub protocol_interest_schedule_sha256: String,
    pub hlp_scenario: HlpScenario,
    pub selected_profile_id: Option<String>,
    pub pareto_profile_ids: Vec<String>,
    pub training_metrics_sha256: String,
    pub validation_metrics_sha256: String,
    /// Commitment to the training/validation-only activity qualification used
    /// for the separately labeled per-market opportunity ceiling.
    pub per_market_qualification_sha256: String,
    pub per_market_training_metrics_sha256: String,
    pub per_market_validation_metrics_sha256: String,
    /// These profiles never substitute for the single global headline policy.
    pub per_market_selected_profile_ids: BTreeMap<String, String>,
    pub per_market_pareto_profile_ids: BTreeMap<String, Vec<String>>,
    pub holdout_read_before_freeze: bool,
    pub freeze_sha256: String,
}

impl ProfileFreezeV1 {
    pub fn seal(mut self) -> Result<Self, CalibrationError> {
        if self.schema_version != PROFILE_FREEZE_SCHEMA_VERSION {
            return Err(CalibrationError::FreezeSchemaVersion(self.schema_version));
        }
        if self.holdout_read_before_freeze {
            return Err(CalibrationError::HoldoutLeakage);
        }
        if !is_canonical_sha256(&self.protocol_interest_schedule_sha256) {
            return Err(CalibrationError::InvalidFreezeInterestScheduleHash);
        }
        self.freeze_sha256.clear();
        let bytes = serde_json::to_vec(&self).map_err(|_| CalibrationError::Serialization)?;
        self.freeze_sha256 = hex::encode(Sha256::digest(bytes));
        Ok(self)
    }

    pub fn verify(&self) -> Result<(), CalibrationError> {
        let expected = self.clone().seal()?.freeze_sha256;
        if expected != self.freeze_sha256 {
            return Err(CalibrationError::FreezeChecksum);
        }
        Ok(())
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CalibrationError {
    #[error("unsupported profile schema version {0}")]
    SchemaVersion(u16),
    #[error("candidate is outside Dusk's protocol-valid domain")]
    InvalidProfile,
    #[error("holdout data was read before the profile freeze")]
    HoldoutLeakage,
    #[error("profile freeze checksum does not match its contents")]
    FreezeChecksum,
    #[error("unsupported profile-freeze schema version {0}")]
    FreezeSchemaVersion(u16),
    #[error("profile freeze has an invalid protocol-interest schedule hash")]
    InvalidFreezeInterestScheduleHash,
    #[error("unsupported protocol-interest schedule schema version {0}")]
    InterestScheduleSchemaVersion(u16),
    #[error("invalid protocol-interest schedule: {0}")]
    InvalidInterestSchedule(&'static str),
    #[error("protocol-interest schedule checksum does not match its contents")]
    InterestScheduleChecksum,
    #[error("protocol-interest schedule arithmetic overflow: {0}")]
    InterestScheduleArithmetic(&'static str),
    #[error("no frozen protocol-interest epoch for market {market} and config {config_epoch_id}")]
    MissingInterestEpoch {
        market: String,
        config_epoch_id: String,
    },
    #[error("operation is outside the frozen protocol-interest epoch for market {market} and config {config_epoch_id}")]
    InterestEpochOutOfRange {
        market: String,
        config_epoch_id: String,
    },
    #[error("no frozen protocol-interest epoch contains order {order:?} for market {market}")]
    MissingInterestEpochAtOrder { market: String, order: ChainOrder },
    #[error(
        "multiple frozen protocol-interest epochs contain order {order:?} for market {market}"
    )]
    AmbiguousInterestEpochAtOrder { market: String, order: ChainOrder },
    #[error("factual V1 interest surcharge {factual_bps} bps does not match frozen {scheduled_bps} bps for market {market} and config {config_epoch_id}")]
    InterestEpochSurchargeMismatch {
        market: String,
        config_epoch_id: String,
        scheduled_bps: u16,
        factual_bps: u16,
    },
    #[error("profile freeze serialization failed")]
    Serialization,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(slot: u64) -> ChainOrder {
        ChainOrder {
            slot,
            transaction_index: 0,
            outer_instruction_index: 0,
            inner_execution_index: None,
            event_sequence: 0,
        }
    }

    fn repeated_hash(byte: char) -> String {
        byte.to_string().repeat(64)
    }

    fn training_source() -> ProtocolInterestTrainingSourceV1 {
        ProtocolInterestTrainingSourceV1 {
            operations_sha256: repeated_hash('a'),
            factual_ledger_sha256: repeated_hash('b'),
            training_start_unix_seconds: 100,
            training_end_unix_seconds_exclusive: 200,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn interest_delta(
        lp0: u128,
        nominal0: u128,
        cash0: u128,
        lp1: u128,
        nominal1: u128,
        cash1: u128,
    ) -> V1RevenueDeltaV1 {
        V1RevenueDeltaV1 {
            lp_interest0: lp0,
            lp_interest1: lp1,
            protocol_interest0: cash0,
            protocol_interest1: cash1,
            nominal_protocol_interest0: nominal0,
            nominal_protocol_interest1: nominal1,
            cash_covered_protocol_interest0: cash0,
            cash_covered_protocol_interest1: cash1,
            ..V1RevenueDeltaV1::default()
        }
    }

    fn training_sample(
        operation_id: &str,
        slot: u64,
        delta: V1RevenueDeltaV1,
    ) -> ProtocolInterestTrainingSampleV1 {
        ProtocolInterestTrainingSampleV1::from_revenue_delta(
            operation_id.to_owned(),
            order(slot),
            120,
            &delta,
        )
        .unwrap()
        .unwrap()
    }

    fn training_epoch(
        config_byte: char,
        surcharge_bps: u16,
        samples: Vec<ProtocolInterestTrainingSampleV1>,
    ) -> ProtocolInterestTrainingEpochV1 {
        ProtocolInterestTrainingEpochV1 {
            key: ProtocolInterestEpochKeyV1 {
                market: "market-a".into(),
                config_epoch_id: repeated_hash(config_byte),
            },
            valid_from: order(10),
            valid_to_exclusive: Some(order(20)),
            v1_interest_surcharge_bps: surcharge_bps,
            samples,
        }
    }

    #[test]
    fn exploration_is_deterministic_and_valid() {
        let left = explore_profiles(16, 42, SearchDomainV1::default());
        let right = explore_profiles(16, 42, SearchDomainV1::default());
        assert_eq!(left, right);
        assert!(left
            .iter()
            .all(|profile| profile.validate_for_base_fee(0).is_ok()));
    }

    #[test]
    fn local_refinement_is_deterministic_unique_and_protocol_valid() {
        let seeds = explore_profiles(3, 42, SearchDomainV1::default());
        let left = refine_profiles(&seeds, 2, SearchDomainV1::default());
        let right = refine_profiles(&seeds, 2, SearchDomainV1::default());
        assert_eq!(left, right);
        assert!(!left.is_empty());
        let ids: std::collections::BTreeSet<_> =
            left.iter().map(|profile| &profile.profile_id).collect();
        assert_eq!(ids.len(), left.len());
        assert!(left
            .iter()
            .all(|profile| profile.validate_for_base_fee(0).is_ok()));
    }

    #[test]
    fn infeasible_revenue_leader_cannot_win() {
        let unsafe_leader = CandidateScoreV1 {
            profile_id: "unsafe".into(),
            hlp_scenario: HlpScenario::MaximumFeasible,
            bad_debt_delta_usd_micro: 1,
            tail_liquidation_shortfall_delta_usd_micro: 0,
            volume_weighted_execution_cost_delta_ppb: 0,
            p95_slippage_delta_ppb: 0,
            capital_weighted_stakeholder_revenue_uplift_bps: 10_000,
            equal_market_stakeholder_revenue_uplift_bps: 10_000,
            quote_quality_uplift_ppb: 10_000,
            borrow_capacity_uplift_bps: 10_000,
        };
        let feasible = CandidateScoreV1 {
            profile_id: "feasible".into(),
            hlp_scenario: HlpScenario::MaximumFeasible,
            bad_debt_delta_usd_micro: 0,
            tail_liquidation_shortfall_delta_usd_micro: 0,
            volume_weighted_execution_cost_delta_ppb: 0,
            p95_slippage_delta_ppb: 0,
            capital_weighted_stakeholder_revenue_uplift_bps: 1,
            equal_market_stakeholder_revenue_uplift_bps: 1,
            quote_quality_uplift_ppb: 1,
            borrow_capacity_uplift_bps: 1,
        };
        assert_eq!(
            select_global_winner(&[unsafe_leader, feasible])
                .unwrap()
                .profile_id,
            "feasible"
        );
    }

    #[test]
    fn interest_schedule_exhaustively_fits_cash_covered_share() {
        // V1: native LP interest 1_000, nominal/cash-covered protocol 100.
        // Dusk allocates from total borrower interest 1_100, for which 910 is
        // the lowest bps producing exactly 100 raw units.
        let epoch = training_epoch(
            'c',
            1_000,
            vec![training_sample(
                "interest-1",
                11,
                interest_delta(1_000, 100, 100, 0, 0, 0),
            )],
        );
        let schedule = derive_protocol_interest_share_schedule(
            training_source(),
            ProtocolInterestSideWeightsV1::default(),
            vec![epoch],
        )
        .unwrap();

        schedule.verify().unwrap();
        let fitted = schedule
            .resolve("market-a", &repeated_hash('c'), order(11))
            .unwrap();
        assert_eq!(fitted.dusk_protocol_interest_share_bps, 910);
        assert_eq!(fitted.token0_borrower_interest.as_str(), "1100");
        assert_eq!(fitted.token0_nominal_protocol_interest.as_str(), "100");
        assert_eq!(fitted.token0_cash_covered_protocol_interest.as_str(), "100");
        assert_eq!(fitted.token0_absolute_floor_error.as_str(), "0");
        assert_eq!(
            schedule.resolve_at("market-a", order(11), 1_000).unwrap(),
            fitted
        );
    }

    #[test]
    fn interest_schedule_normalizes_sides_and_applies_explicit_weights() {
        // At bps 0 token0 is exact and token1 misses by one; at bps 1 the
        // inverse holds. Equal normalized errors choose lower bps. Giving
        // token1 twice the unitless weight deterministically chooses bps 1.
        let epoch = training_epoch(
            'c',
            1,
            vec![training_sample(
                "interest-1",
                11,
                interest_delta(10_001, 1, 0, 10_000, 1, 1),
            )],
        );
        let equal = derive_protocol_interest_share_schedule(
            training_source(),
            ProtocolInterestSideWeightsV1::default(),
            vec![epoch.clone()],
        )
        .unwrap();
        assert_eq!(equal.epochs[0].dusk_protocol_interest_share_bps, 0);
        assert_eq!(equal.epochs[0].token1_absolute_floor_error.as_str(), "1");

        let weighted = derive_protocol_interest_share_schedule(
            training_source(),
            ProtocolInterestSideWeightsV1 {
                token0: 1,
                token1: 2,
            },
            vec![epoch],
        )
        .unwrap();
        assert_eq!(weighted.epochs[0].dusk_protocol_interest_share_bps, 1);
        assert_eq!(weighted.epochs[0].token0_absolute_floor_error.as_str(), "1");
        assert_eq!(weighted.epochs[0].token1_absolute_floor_error.as_str(), "0");
    }

    #[test]
    fn interest_schedule_resolution_and_checksum_fail_closed() {
        let epoch = training_epoch(
            'c',
            1_000,
            vec![training_sample(
                "interest-1",
                11,
                interest_delta(1_000, 100, 100, 0, 0, 0),
            )],
        );
        let schedule = derive_protocol_interest_share_schedule(
            training_source(),
            ProtocolInterestSideWeightsV1::default(),
            vec![epoch],
        )
        .unwrap();

        assert!(matches!(
            schedule.resolve("market-a", &repeated_hash('d'), order(11)),
            Err(CalibrationError::MissingInterestEpoch { .. })
        ));
        assert!(matches!(
            schedule.resolve("market-a", &repeated_hash('c'), order(9)),
            Err(CalibrationError::InterestEpochOutOfRange { .. })
        ));
        assert!(matches!(
            schedule.resolve_at("market-a", order(9), 1_000),
            Err(CalibrationError::MissingInterestEpochAtOrder { .. })
        ));
        assert!(matches!(
            schedule.resolve_at("market-a", order(11), 999),
            Err(CalibrationError::InterestEpochSurchargeMismatch { .. })
        ));

        let mut tampered = schedule;
        tampered.epochs[0].dusk_protocol_interest_share_bps += 1;
        assert_eq!(
            tampered.verify(),
            Err(CalibrationError::InterestScheduleChecksum)
        );
    }

    #[test]
    fn zero_sample_interest_epoch_is_explicitly_unavailable() {
        let schedule = derive_protocol_interest_share_schedule(
            training_source(),
            ProtocolInterestSideWeightsV1::default(),
            vec![training_epoch('c', 1_000, Vec::new())],
        )
        .unwrap();
        let epoch = schedule.resolve_at("market-a", order(11), 1_000).unwrap();
        assert!(!epoch.training_fit_available);
        assert_eq!(epoch.training_observation_count, 0);
        assert_eq!(epoch.dusk_protocol_interest_share_bps, 909);
        assert_eq!(epoch.token0_borrower_interest.as_str(), "0");
        assert_eq!(epoch.token1_borrower_interest.as_str(), "0");
        schedule.verify().unwrap();
    }

    #[test]
    fn interest_schedule_rejects_inconsistent_raw_v1_evidence() {
        let epoch = training_epoch(
            'c',
            1_000,
            vec![training_sample(
                "interest-1",
                11,
                interest_delta(1_000, 99, 99, 0, 0, 0),
            )],
        );
        assert!(matches!(
            derive_protocol_interest_share_schedule(
                training_source(),
                ProtocolInterestSideWeightsV1::default(),
                vec![epoch],
            ),
            Err(CalibrationError::InvalidInterestSchedule(_))
        ));
    }

    #[test]
    fn freeze_rejects_holdout_leakage_and_tampering() {
        let base = ProfileFreezeV1 {
            schema_version: PROFILE_FREEZE_SCHEMA_VERSION,
            dataset_id: "data".into(),
            config_sha256: "config".into(),
            dusk_source_sha256: "source".into(),
            protocol_interest_schedule_sha256: repeated_hash('c'),
            hlp_scenario: HlpScenario::MaximumFeasible,
            selected_profile_id: None,
            pareto_profile_ids: vec!["p".into()],
            training_metrics_sha256: "train".into(),
            validation_metrics_sha256: "validate".into(),
            per_market_qualification_sha256: "qualification".into(),
            per_market_training_metrics_sha256: "market-train".into(),
            per_market_validation_metrics_sha256: "market-validate".into(),
            per_market_selected_profile_ids: BTreeMap::new(),
            per_market_pareto_profile_ids: BTreeMap::new(),
            holdout_read_before_freeze: false,
            freeze_sha256: String::new(),
        };
        let mut sealed = base.clone().seal().unwrap();
        sealed.verify().unwrap();
        sealed.dataset_id.push('x');
        assert_eq!(sealed.verify(), Err(CalibrationError::FreezeChecksum));
        let mut old_schema = base.clone();
        old_schema.schema_version = 1;
        assert_eq!(
            old_schema.seal(),
            Err(CalibrationError::FreezeSchemaVersion(1))
        );
        let mut missing_schedule = base.clone();
        missing_schedule.protocol_interest_schedule_sha256.clear();
        assert_eq!(
            missing_schedule.seal(),
            Err(CalibrationError::InvalidFreezeInterestScheduleHash)
        );
        let mut leaked = base;
        leaked.holdout_read_before_freeze = true;
        assert_eq!(leaked.seal(), Err(CalibrationError::HoldoutLeakage));
    }
}
