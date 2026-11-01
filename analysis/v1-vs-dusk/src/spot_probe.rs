//! Exact, immutable spot-capital-efficiency probes.
//!
//! Probe sizes are derived only from factual V1 history.  Native V1 CPMM
//! arithmetic is reproduced against the exact post-accrual/pre-swap reserves;
//! Dusk quotes are attached later by the counterfactual market on a fork.

use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufRead, BufReader},
    path::Path,
};

use anyhow::{bail, Context, Result};
use num_bigint::BigInt;
use num_rational::BigRational;
use num_traits::{Signed, ToPrimitive, Zero};
use omnipair::{ceil_div, constants::BPS_DENOMINATOR, CPCurve};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    canonical::{SignedTokenAmount, TokenAmount},
    replay::{CanonicalOperationV1, HlpScenario, OperationPayloadV1, ReplayArm},
    transfer_fee::ValidatedTransferFeeRegistryV1,
    v1_reconciliation::V1ReplayLedgerRowV1,
};

pub const SPOT_PROBE_SCHEMA_VERSION: u16 = 2;
pub const SPOT_PROBE_HASH_DOMAIN_V1: &[u8] = b"v1-vs-dusk/spot-probe-set/v2\0";
pub const SPOT_PROBE_SIZING_HASH_DOMAIN_V1: &[u8] = b"v1-vs-dusk/spot-probe-sizing/v2\0";
pub const PARTS_PER_BILLION: i64 = 1_000_000_000;
pub const FROZEN_STANDARDIZED_NAV_BPS: [u16; 3] = [10, 100, 500];

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpotProbeDirectionV1 {
    Token0In,
    Token1In,
}

impl SpotProbeDirectionV1 {
    pub const fn token0_in(self) -> bool {
        matches!(self, Self::Token0In)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpotProbeSizeKindV1 {
    FactualNav10Bps,
    FactualNav100Bps,
    FactualNav500Bps,
    FactualGrossInputP50,
    FactualGrossInputP90,
}

impl SpotProbeSizeKindV1 {
    pub const fn factual_nav_bps(self) -> Option<u16> {
        match self {
            Self::FactualNav10Bps => Some(10),
            Self::FactualNav100Bps => Some(100),
            Self::FactualNav500Bps => Some(500),
            Self::FactualGrossInputP50 | Self::FactualGrossInputP90 => None,
        }
    }

    pub fn from_factual_nav_bps(value: u16) -> Result<Self> {
        match value {
            10 => Ok(Self::FactualNav10Bps),
            100 => Ok(Self::FactualNav100Bps),
            500 => Ok(Self::FactualNav500Bps),
            _ => bail!("spot probe NAV bps is outside the frozen 10/100/500 campaign"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpotProbeUnsupportedReasonV1 {
    NoFactualGrossInputForDirection,
    RoundedGrossInputIsZero,
    CurveInputIsZero,
    InsufficientCash,
    NativePreviewRejected,
    HlpSettlementRejected,
    MissingHlpOwnedState,
    ZeroExternalLpNav,
    ExternalLpOwnershipNavUnavailable,
    NonpositiveMarginalImpact,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpotProbeExternalLpNavBasisV1 {
    /// V1 Pair reserves are its accounted LP inventory, valued at the factual
    /// pre-swap marginal price. Borrowed inventory remains represented in the
    /// reserve ledger even when it is not currently cash-withdrawable.
    FactualV1AccountedLpReserves,
    /// Exact current withdrawal claim of the aggregate external Dusk yLP/hLP
    /// owner, including separately claimable YieldAccount balances.
    DuskAggregateExternalOwnerClaim,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpotProbeSizingWindowV1 {
    pub start_unix_seconds: i64,
    pub end_unix_seconds_exclusive: i64,
    pub standardized_trade_nav_bps: Vec<u16>,
}

impl SpotProbeSizingWindowV1 {
    pub fn validate(&self) -> Result<()> {
        if self.start_unix_seconds >= self.end_unix_seconds_exclusive {
            bail!("spot probe sizing window is empty or reversed");
        }
        if self.standardized_trade_nav_bps != FROZEN_STANDARDIZED_NAV_BPS {
            bail!("spot probe sizes must be the ordered frozen 10/100/500 bps campaign");
        }
        Ok(())
    }

    pub const fn contains(&self, unix_seconds: i64) -> bool {
        unix_seconds >= self.start_unix_seconds && unix_seconds < self.end_unix_seconds_exclusive
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpotProbeExactRatioV1 {
    pub numerator: TokenAmount,
    pub denominator: TokenAmount,
}

impl SpotProbeExactRatioV1 {
    pub fn from_nonnegative(value: &BigRational) -> Result<Self> {
        if value.is_negative() || value.denom().is_zero() || value.denom().is_negative() {
            bail!("spot probe exact ratio is not non-negative with a positive denominator");
        }
        Ok(Self {
            numerator: token_big(value.numer()),
            denominator: token_big(value.denom()),
        })
    }

    pub fn to_rational(&self) -> Result<BigRational> {
        let numerator = self
            .numerator
            .as_str()
            .parse::<BigInt>()
            .context("spot probe ratio numerator is not an integer")?;
        let denominator = self
            .denominator
            .as_str()
            .parse::<BigInt>()
            .context("spot probe ratio denominator is not an integer")?;
        if denominator <= BigInt::zero() {
            bail!("spot probe ratio denominator is not positive");
        }
        Ok(BigRational::new(numerator, denominator))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpotProbeQuoteV1 {
    pub executable: bool,
    pub unsupported_reason: Option<SpotProbeUnsupportedReasonV1>,
    pub gross_input: TokenAmount,
    pub input_transfer_fee: TokenAmount,
    pub net_input: TokenAmount,
    /// Amount presented to the invariant after protocol swap fees.
    pub curve_input: TokenAmount,
    /// Output produced by the invariant before output-denominated protocol fees.
    pub curve_gross_output: TokenAmount,
    /// Gross token-program transfer debited from the output vault.
    pub gross_output: TokenAmount,
    pub output_transfer_fee: TokenAmount,
    pub net_output: TokenAmount,
    pub lp_swap_fee: TokenAmount,
    pub protocol_swap_fee: TokenAmount,
    pub dynamic_swap_fee: TokenAmount,
    /// This venue's own marginal price before the probe.
    pub start_price_nad: TokenAmount,
    pub end_price_nad: TokenAmount,
    /// Factual V1 pre-swap marginal price shared by both venues.
    pub common_reference_price_nad: TokenAmount,
    /// Wallet-to-wallet cost relative to the common factual V1 reference.
    pub execution_cost_ppb: SignedTokenAmount,
    /// Pure invariant slippage relative to this venue's own marginal price,
    /// excluding token transfers and protocol fee debits.
    pub slippage_ppb: SignedTokenAmount,
    /// CPMM-equivalent NAV divided by exact pre-state LP NAV. Both are valued
    /// in the input token, so the historical USD mark cancels exactly.
    pub cpmm_equivalent_depth_per_external_lp_value: Option<SpotProbeExactRatioV1>,
    pub external_lp_nav_in_input: Option<SpotProbeExactRatioV1>,
    pub external_lp_nav_basis: Option<SpotProbeExternalLpNavBasisV1>,
    pub depth_unsupported_reason: Option<SpotProbeUnsupportedReasonV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpotProbeObservationV1 {
    pub size_kind: SpotProbeSizeKindV1,
    pub direction: SpotProbeDirectionV1,
    pub factual_nav_bps: Option<u16>,
    pub gross_input: Option<TokenAmount>,
    pub size_unsupported_reason: Option<SpotProbeUnsupportedReasonV1>,
    pub v1: Option<SpotProbeQuoteV1>,
    pub dusk: Option<SpotProbeQuoteV1>,
    /// `(Dusk gross output - V1 gross output) / V1 gross output` in ppb.
    pub gross_quote_improvement_ppb: Option<SignedTokenAmount>,
    /// `(Dusk wallet-net output - V1 wallet-net output) / V1 wallet-net output` in ppb.
    pub net_quote_improvement_ppb: Option<SignedTokenAmount>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpotProbeSetV1 {
    pub schema_version: u16,
    pub profile_id: String,
    pub arm: ReplayArm,
    pub hlp_scenario: HlpScenario,
    pub operation_id: String,
    pub market: String,
    pub slot: u64,
    pub factual_record_sha256: String,
    pub factual_pre_state_sha256: String,
    pub dusk_pre_state_sha256: String,
    pub sizing_window: SpotProbeSizingWindowV1,
    pub sizing_source_sha256: String,
    pub observations: Vec<SpotProbeObservationV1>,
    pub probe_set_sha256: String,
}

impl SpotProbeSetV1 {
    pub fn seal(mut self) -> Result<Self> {
        self.probe_set_sha256.clear();
        self.probe_set_sha256 = spot_probe_set_sha256(&self)?;
        Ok(self)
    }

    pub fn validate_hash(&self) -> Result<()> {
        self.sizing_window.validate()?;
        if self.schema_version != SPOT_PROBE_SCHEMA_VERSION
            || self.probe_set_sha256.len() != 64
            || self.sizing_source_sha256.len() != 64
            || self.factual_record_sha256.len() != 64
            || self.factual_pre_state_sha256.len() != 64
            || self.dusk_pre_state_sha256.len() != 64
        {
            bail!("spot probe set has an unsupported schema or malformed hash");
        }
        let mut unhashed = self.clone();
        let expected = unhashed.probe_set_sha256.clone();
        unhashed.probe_set_sha256.clear();
        if spot_probe_set_sha256(&unhashed)? != expected {
            bail!("spot probe set hash differs from its typed observations");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct V1SpotPreStateV1 {
    pub market: String,
    pub token0: String,
    pub token1: String,
    pub token0_decimals: u8,
    pub token1_decimals: u8,
    pub reserve0: u64,
    pub reserve1: u64,
    pub cash_reserve0: u64,
    pub cash_reserve1: u64,
    pub swap_fee_bps: u16,
    pub protocol_swap_fee_bps: u16,
}

#[derive(Clone, Debug)]
pub struct SpotProbePlanEntryV1 {
    pub size_kind: SpotProbeSizeKindV1,
    pub direction: SpotProbeDirectionV1,
    pub factual_nav_bps: Option<u16>,
    pub gross_input: Option<u64>,
    pub size_unsupported_reason: Option<SpotProbeUnsupportedReasonV1>,
    pub v1: Option<SpotProbeQuoteV1>,
}

#[derive(Clone, Debug)]
pub struct SpotProbePlanV1 {
    pub factual: V1SpotPreStateV1,
    pub factual_reference_price_nad: u64,
    pub factual_record_sha256: String,
    pub factual_pre_state_sha256: String,
    pub sizing_window: SpotProbeSizingWindowV1,
    pub sizing_source_sha256: String,
    pub entries: Vec<SpotProbePlanEntryV1>,
}

#[derive(Clone, Debug)]
struct FactualGrossInputSampleV1 {
    operation_id: String,
    slot: u64,
    gross_input: u64,
}

#[derive(Clone, Debug)]
pub struct SpotProbeSizingIndexV1 {
    samples: BTreeMap<(String, SpotProbeDirectionV1), Vec<FactualGrossInputSampleV1>>,
    window: SpotProbeSizingWindowV1,
    source_sha256: String,
}

impl SpotProbeSizingIndexV1 {
    /// Precomputes factual sizes from exactly one scored chronological split
    /// before any profile evaluation. No row outside the split and no Dusk
    /// quote, status, fee, or policy outcome enters this index.
    pub fn from_canonical_jsonl(path: &Path, window: SpotProbeSizingWindowV1) -> Result<Self> {
        window.validate()?;
        let mut samples =
            BTreeMap::<(String, SpotProbeDirectionV1), Vec<FactualGrossInputSampleV1>>::new();
        for (index, line) in
            BufReader::new(File::open(path).with_context(|| format!("opening {}", path.display()))?)
                .lines()
                .enumerate()
        {
            let line = line.with_context(|| format!("reading operation line {}", index + 1))?;
            if line.trim().is_empty() {
                continue;
            }
            let operation: CanonicalOperationV1 = serde_json::from_str(&line)
                .with_context(|| format!("decoding operation line {}", index + 1))?;
            operation.validate()?;
            if !window.contains(operation.block_time_unix_seconds) {
                continue;
            }
            if let OperationPayloadV1::SwapExactIn {
                token0_in,
                gross_amount_in,
                ..
            } = &operation.payload
            {
                let gross_input = amount_u64("swap.gross_amount_in", gross_amount_in)?;
                samples
                    .entry((
                        operation.pair.clone(),
                        if *token0_in {
                            SpotProbeDirectionV1::Token0In
                        } else {
                            SpotProbeDirectionV1::Token1In
                        },
                    ))
                    .or_default()
                    .push(FactualGrossInputSampleV1 {
                        operation_id: operation.operation_id,
                        slot: operation.order.slot,
                        gross_input,
                    });
            }
        }
        let source_sha256 = sizing_source_sha256(&window, &samples)?;
        Ok(Self {
            samples,
            window,
            source_sha256,
        })
    }

    pub fn source_sha256(&self) -> &str {
        &self.source_sha256
    }

    pub fn window(&self) -> &SpotProbeSizingWindowV1 {
        &self.window
    }

    fn quantile(
        &self,
        market: &str,
        direction: SpotProbeDirectionV1,
        numerator: usize,
        denominator: usize,
    ) -> Option<u64> {
        if denominator == 0 || numerator > denominator {
            return None;
        }
        let mut values: Vec<u64> = self
            .samples
            .get(&(market.to_owned(), direction))?
            .iter()
            .map(|sample| sample.gross_input)
            .collect();
        values.sort_unstable();
        let index = (values.len() - 1).saturating_mul(numerator) / denominator;
        values.get(index).copied()
    }
}

pub fn build_spot_probe_plan(
    operation: &CanonicalOperationV1,
    factual: &V1ReplayLedgerRowV1,
    sizing: &SpotProbeSizingIndexV1,
    transfers: &ValidatedTransferFeeRegistryV1,
) -> Result<SpotProbePlanV1> {
    let pre = reconstruct_v1_pre_swap(operation, factual, transfers)?;
    let factual_reference_price_nad = v1_price0_nad(pre.reserve0, pre.reserve1)?;
    let factual_pre_state_sha256 = hex::encode(Sha256::digest(serde_json::to_vec(&pre)?));
    let mut entries = Vec::with_capacity(2 * (sizing.window.standardized_trade_nav_bps.len() + 2));
    for direction in [
        SpotProbeDirectionV1::Token0In,
        SpotProbeDirectionV1::Token1In,
    ] {
        let mut size_kinds = sizing
            .window
            .standardized_trade_nav_bps
            .iter()
            .copied()
            .map(SpotProbeSizeKindV1::from_factual_nav_bps)
            .collect::<Result<Vec<_>>>()?;
        size_kinds.extend([
            SpotProbeSizeKindV1::FactualGrossInputP50,
            SpotProbeSizeKindV1::FactualGrossInputP90,
        ]);
        for size_kind in size_kinds {
            let gross_input = match size_kind {
                SpotProbeSizeKindV1::FactualNav10Bps
                | SpotProbeSizeKindV1::FactualNav100Bps
                | SpotProbeSizeKindV1::FactualNav500Bps => Some(factual_nav_gross_input(
                    &pre,
                    direction,
                    size_kind.factual_nav_bps().expect("NAV size has bps"),
                )?),
                SpotProbeSizeKindV1::FactualGrossInputP50 => {
                    sizing.quantile(&operation.pair, direction, 50, 100)
                }
                SpotProbeSizeKindV1::FactualGrossInputP90 => {
                    sizing.quantile(&operation.pair, direction, 90, 100)
                }
            };
            let size_unsupported_reason = match gross_input {
                None => Some(SpotProbeUnsupportedReasonV1::NoFactualGrossInputForDirection),
                Some(0) => Some(SpotProbeUnsupportedReasonV1::RoundedGrossInputIsZero),
                Some(_) => None,
            };
            let v1 = gross_input
                .filter(|amount| *amount > 0)
                .map(|amount| quote_v1(&pre, direction, amount, operation.order.slot, transfers))
                .transpose()?;
            entries.push(SpotProbePlanEntryV1 {
                size_kind,
                direction,
                factual_nav_bps: size_kind.factual_nav_bps(),
                gross_input,
                size_unsupported_reason,
                v1,
            });
        }
    }
    Ok(SpotProbePlanV1 {
        factual: pre,
        factual_reference_price_nad,
        factual_record_sha256: factual.factual_record_sha256.clone(),
        factual_pre_state_sha256,
        sizing_window: sizing.window.clone(),
        sizing_source_sha256: sizing.source_sha256().to_owned(),
        entries,
    })
}

pub fn factual_nav_gross_input(
    state: &V1SpotPreStateV1,
    direction: SpotProbeDirectionV1,
    nav_bps: u16,
) -> Result<u64> {
    SpotProbeSizeKindV1::from_factual_nav_bps(nav_bps)?;
    let reserve_in = if direction.token0_in() {
        state.reserve0
    } else {
        state.reserve1
    };
    let numerator = u128::from(reserve_in)
        .checked_mul(2)
        .and_then(|value| value.checked_mul(u128::from(nav_bps)))
        .context("spot probe factual NAV size overflow")?;
    u64::try_from(numerator / u128::from(BPS_DENOMINATOR))
        .context("spot probe factual NAV size exceeds u64")
}

pub fn derive_quote_metrics(
    direction: SpotProbeDirectionV1,
    curve_input: u64,
    curve_gross_output: u64,
    gross_wallet_input: u64,
    net_wallet_output: u64,
    common_reference_price_nad: u64,
    venue_start_price_nad: u64,
    token0_decimals: u8,
    token1_decimals: u8,
    external_lp_nav_in_input: Option<BigRational>,
) -> Result<(
    SignedTokenAmount,
    SignedTokenAmount,
    Option<SpotProbeExactRatioV1>,
    Option<SpotProbeUnsupportedReasonV1>,
)> {
    let curve_ideal = ideal_output_at_price(
        direction,
        curve_input,
        venue_start_price_nad,
        token0_decimals,
        token1_decimals,
    )?;
    let wallet_ideal = ideal_output_at_price(
        direction,
        gross_wallet_input,
        common_reference_price_nad,
        token0_decimals,
        token1_decimals,
    )?;
    let slippage = scaled_cost_ppb(&curve_ideal, curve_gross_output)?;
    let execution_cost = scaled_cost_ppb(&wallet_ideal, net_wallet_output)?;
    let actual = BigRational::from_integer(BigInt::from(curve_gross_output));
    let impact = &curve_ideal - &actual;
    let (depth, depth_reason) = if curve_input == 0 || impact <= BigRational::zero() {
        (
            None,
            Some(SpotProbeUnsupportedReasonV1::NonpositiveMarginalImpact),
        )
    } else if external_lp_nav_in_input
        .as_ref()
        .is_some_and(|value| value <= &BigRational::zero())
    {
        (None, Some(SpotProbeUnsupportedReasonV1::ZeroExternalLpNav))
    } else if external_lp_nav_in_input.is_none() {
        (
            None,
            Some(SpotProbeUnsupportedReasonV1::ExternalLpOwnershipNavUnavailable),
        )
    } else {
        let equivalent_input_reserve =
            BigRational::from_integer(BigInt::from(curve_input)) * actual / impact;
        let equivalent_nav = equivalent_input_reserve * BigInt::from(2_u8);
        (
            Some(SpotProbeExactRatioV1::from_nonnegative(
                &(equivalent_nav / external_lp_nav_in_input.expect("checked above")),
            )?),
            None,
        )
    };
    Ok((
        signed(execution_cost),
        signed(slippage),
        depth,
        depth_reason,
    ))
}

pub fn ideal_output_at_price(
    direction: SpotProbeDirectionV1,
    input: u64,
    price_nad: u64,
    token0_decimals: u8,
    token1_decimals: u8,
) -> Result<BigRational> {
    if price_nad == 0 {
        bail!("spot probe marginal price is zero");
    }
    let input = BigInt::from(input);
    let price = BigInt::from(price_nad);
    let nad = BigInt::from(omnipair::constants::NAD);
    let decimal_scale = if token0_decimals >= token1_decimals {
        BigInt::from(10_u8).pow(u32::from(token0_decimals - token1_decimals))
    } else {
        BigInt::from(10_u8).pow(u32::from(token1_decimals - token0_decimals))
    };
    Ok(match (direction, token0_decimals.cmp(&token1_decimals)) {
        (SpotProbeDirectionV1::Token0In, std::cmp::Ordering::Greater) => {
            BigRational::new(input * price, nad * decimal_scale)
        }
        (SpotProbeDirectionV1::Token0In, std::cmp::Ordering::Equal) => {
            BigRational::new(input * price, nad)
        }
        (SpotProbeDirectionV1::Token0In, std::cmp::Ordering::Less) => {
            BigRational::new(input * price * decimal_scale, nad)
        }
        (SpotProbeDirectionV1::Token1In, std::cmp::Ordering::Greater) => {
            BigRational::new(input * nad * decimal_scale, price)
        }
        (SpotProbeDirectionV1::Token1In, std::cmp::Ordering::Equal) => {
            BigRational::new(input * nad, price)
        }
        (SpotProbeDirectionV1::Token1In, std::cmp::Ordering::Less) => {
            BigRational::new(input * nad, price * decimal_scale)
        }
    })
}

pub fn external_lp_nav_at_price(
    direction: SpotProbeDirectionV1,
    base_amount: u64,
    quote_amount: u64,
    price_nad: u64,
    token0_decimals: u8,
    token1_decimals: u8,
) -> Result<BigRational> {
    Ok(match direction {
        SpotProbeDirectionV1::Token0In => {
            BigRational::from_integer(BigInt::from(base_amount))
                + ideal_output_at_price(
                    SpotProbeDirectionV1::Token1In,
                    quote_amount,
                    price_nad,
                    token0_decimals,
                    token1_decimals,
                )?
        }
        SpotProbeDirectionV1::Token1In => {
            BigRational::from_integer(BigInt::from(quote_amount))
                + ideal_output_at_price(
                    SpotProbeDirectionV1::Token0In,
                    base_amount,
                    price_nad,
                    token0_decimals,
                    token1_decimals,
                )?
        }
    })
}

fn reconstruct_v1_pre_swap(
    operation: &CanonicalOperationV1,
    factual: &V1ReplayLedgerRowV1,
    transfers: &ValidatedTransferFeeRegistryV1,
) -> Result<V1SpotPreStateV1> {
    let OperationPayloadV1::SwapExactIn {
        token0_in,
        gross_amount_in,
        vault_credit_in,
        vault_debit_out,
        recipient_credit_out,
        ..
    } = &operation.payload
    else {
        bail!("spot probe pre-state reconstruction requires a factual swap");
    };
    if operation.pair != factual.market || operation.operation_id != factual.operation_id {
        bail!("spot probe factual row identity differs from the canonical swap");
    }
    let pair = &factual.compact_checkpoint.pair;
    let gross_input = amount_u64("swap.gross_amount_in", gross_amount_in)?;
    let observed_input_credit = amount_u64("swap.vault_credit_in", vault_credit_in)?;
    let amount_out = amount_u64("swap.vault_debit_out", vault_debit_out)?;
    let observed_recipient_credit = amount_u64("swap.recipient_credit_out", recipient_credit_out)?;
    let input_mint = if *token0_in {
        pair.token0.to_string()
    } else {
        pair.token1.to_string()
    };
    let output_mint = if *token0_in {
        pair.token1.to_string()
    } else {
        pair.token0.to_string()
    };
    transfers.assert_observed_credit(
        &input_mint,
        operation.order.slot,
        gross_input,
        observed_input_credit,
    )?;
    transfers.assert_observed_credit(
        &output_mint,
        operation.order.slot,
        amount_out,
        observed_recipient_credit,
    )?;
    let swap_fee = fee_ceil(gross_input, pair.swap_fee_bps)?;
    let protocol_fee = fee_ceil(swap_fee, factual.compact_checkpoint.authority.swap_bps)?;
    let amount_in_after_fee = gross_input
        .checked_sub(swap_fee)
        .context("V1 factual swap fee exceeds gross input")?;
    let reserve_input_credit = gross_input
        .checked_sub(protocol_fee)
        .context("V1 factual protocol fee exceeds gross input")?;
    let (reserve0, reserve1, cash_reserve0, cash_reserve1) = if *token0_in {
        (
            pair.reserve0
                .checked_sub(reserve_input_credit)
                .context("V1 token0 pre-swap reserve underflow")?,
            pair.reserve1
                .checked_add(amount_out)
                .context("V1 token1 pre-swap reserve overflow")?,
            pair.cash_reserve0
                .checked_sub(reserve_input_credit)
                .context("V1 token0 pre-swap cash underflow")?,
            pair.cash_reserve1
                .checked_add(amount_out)
                .context("V1 token1 pre-swap cash overflow")?,
        )
    } else {
        (
            pair.reserve0
                .checked_add(amount_out)
                .context("V1 token0 pre-swap reserve overflow")?,
            pair.reserve1
                .checked_sub(reserve_input_credit)
                .context("V1 token1 pre-swap reserve underflow")?,
            pair.cash_reserve0
                .checked_add(amount_out)
                .context("V1 token0 pre-swap cash overflow")?,
            pair.cash_reserve1
                .checked_sub(reserve_input_credit)
                .context("V1 token1 pre-swap cash underflow")?,
        )
    };
    let (reserve_in, reserve_out) = if *token0_in {
        (reserve0, reserve1)
    } else {
        (reserve1, reserve0)
    };
    let reproduced = CPCurve::calculate_amount_out(reserve_in, reserve_out, amount_in_after_fee)
        .map_err(|error| anyhow::anyhow!("V1 CPMM factual pre-state assertion failed: {error}"))?;
    if reproduced != amount_out {
        bail!(
            "V1 reconstructed pre-swap reserves quote {reproduced} raw units, factual output was {amount_out}"
        );
    }
    Ok(V1SpotPreStateV1 {
        market: factual.market.clone(),
        token0: pair.token0.to_string(),
        token1: pair.token1.to_string(),
        token0_decimals: pair.token0_decimals,
        token1_decimals: pair.token1_decimals,
        reserve0,
        reserve1,
        cash_reserve0,
        cash_reserve1,
        swap_fee_bps: pair.swap_fee_bps,
        protocol_swap_fee_bps: factual.compact_checkpoint.authority.swap_bps,
    })
}

fn quote_v1(
    state: &V1SpotPreStateV1,
    direction: SpotProbeDirectionV1,
    gross_input: u64,
    slot: u64,
    transfers: &ValidatedTransferFeeRegistryV1,
) -> Result<SpotProbeQuoteV1> {
    let (input_mint, output_mint, reserve_in, reserve_out, cash_out) = match direction {
        SpotProbeDirectionV1::Token0In => (
            state.token0.as_str(),
            state.token1.as_str(),
            state.reserve0,
            state.reserve1,
            state.cash_reserve1,
        ),
        SpotProbeDirectionV1::Token1In => (
            state.token1.as_str(),
            state.token0.as_str(),
            state.reserve1,
            state.reserve0,
            state.cash_reserve0,
        ),
    };
    let input_transfer = transfers.transfer(input_mint, slot, gross_input)?;
    let swap_fee = fee_ceil(gross_input, state.swap_fee_bps)?;
    let protocol_fee = fee_ceil(swap_fee, state.protocol_swap_fee_bps)?;
    let lp_fee = swap_fee
        .checked_sub(protocol_fee)
        .context("V1 probe protocol fee exceeds total swap fee")?;
    let curve_input = gross_input
        .checked_sub(swap_fee)
        .context("V1 probe swap fee exceeds gross input")?;
    if curve_input == 0 {
        return Ok(unsupported_quote(
            gross_input,
            input_transfer.fee_amount,
            input_transfer.net_amount,
            SpotProbeUnsupportedReasonV1::CurveInputIsZero,
        ));
    }
    let amount_out = CPCurve::calculate_amount_out(reserve_in, reserve_out, curve_input)
        .map_err(|error| anyhow::anyhow!("V1 CPMM probe quote failed: {error}"))?;
    let output_transfer = transfers.transfer(output_mint, slot, amount_out)?;
    let start_price_nad = v1_price0_nad(state.reserve0, state.reserve1)?;
    let reserve_input_credit = gross_input
        .checked_sub(protocol_fee)
        .context("V1 probe reserve input overflow")?;
    let (end_reserve0, end_reserve1) = match direction {
        SpotProbeDirectionV1::Token0In => (
            state
                .reserve0
                .checked_add(reserve_input_credit)
                .context("V1 probe token0 reserve overflow")?,
            state
                .reserve1
                .checked_sub(amount_out)
                .context("V1 probe token1 reserve underflow")?,
        ),
        SpotProbeDirectionV1::Token1In => (
            state
                .reserve0
                .checked_sub(amount_out)
                .context("V1 probe token0 reserve underflow")?,
            state
                .reserve1
                .checked_add(reserve_input_credit)
                .context("V1 probe token1 reserve overflow")?,
        ),
    };
    let external_nav = BigRational::from_integer(BigInt::from(reserve_in) * BigInt::from(2_u8));
    let (execution_cost, slippage, depth, depth_reason) = derive_quote_metrics(
        direction,
        curve_input,
        amount_out,
        gross_input,
        output_transfer.net_amount,
        start_price_nad,
        start_price_nad,
        state.token0_decimals,
        state.token1_decimals,
        Some(external_nav.clone()),
    )?;
    let executable = amount_out <= cash_out;
    Ok(SpotProbeQuoteV1 {
        executable,
        unsupported_reason: (!executable).then_some(SpotProbeUnsupportedReasonV1::InsufficientCash),
        gross_input: token(gross_input),
        input_transfer_fee: token(input_transfer.fee_amount),
        net_input: token(input_transfer.net_amount),
        curve_input: token(curve_input),
        curve_gross_output: token(amount_out),
        gross_output: token(amount_out),
        output_transfer_fee: token(output_transfer.fee_amount),
        net_output: token(output_transfer.net_amount),
        lp_swap_fee: token(lp_fee),
        protocol_swap_fee: token(protocol_fee),
        dynamic_swap_fee: token(0),
        start_price_nad: token(start_price_nad),
        end_price_nad: token(v1_price0_nad(end_reserve0, end_reserve1)?),
        common_reference_price_nad: token(start_price_nad),
        execution_cost_ppb: execution_cost,
        slippage_ppb: slippage,
        cpmm_equivalent_depth_per_external_lp_value: depth,
        external_lp_nav_in_input: Some(SpotProbeExactRatioV1::from_nonnegative(&external_nav)?),
        external_lp_nav_basis: Some(SpotProbeExternalLpNavBasisV1::FactualV1AccountedLpReserves),
        depth_unsupported_reason: depth_reason,
    })
}

pub fn unsupported_quote(
    gross_input: u64,
    input_transfer_fee: u64,
    net_input: u64,
    reason: SpotProbeUnsupportedReasonV1,
) -> SpotProbeQuoteV1 {
    SpotProbeQuoteV1 {
        executable: false,
        unsupported_reason: Some(reason),
        gross_input: token(gross_input),
        input_transfer_fee: token(input_transfer_fee),
        net_input: token(net_input),
        curve_input: token(0),
        curve_gross_output: token(0),
        gross_output: token(0),
        output_transfer_fee: token(0),
        net_output: token(0),
        lp_swap_fee: token(0),
        protocol_swap_fee: token(0),
        dynamic_swap_fee: token(0),
        start_price_nad: token(0),
        end_price_nad: token(0),
        common_reference_price_nad: token(0),
        execution_cost_ppb: signed(0),
        slippage_ppb: signed(0),
        cpmm_equivalent_depth_per_external_lp_value: None,
        external_lp_nav_in_input: None,
        external_lp_nav_basis: None,
        depth_unsupported_reason: Some(reason),
    }
}

pub fn quote_improvement_ppb(
    v1_output: &TokenAmount,
    dusk_output: &TokenAmount,
) -> Result<SignedTokenAmount> {
    let v1 = amount_u64("spot_probe.v1_output", v1_output)?;
    let dusk = amount_u64("spot_probe.dusk_output", dusk_output)?;
    if v1 == 0 {
        bail!("spot probe quote-improvement denominator is zero");
    }
    let delta = i128::from(dusk)
        .checked_sub(i128::from(v1))
        .and_then(|value| value.checked_mul(i128::from(PARTS_PER_BILLION)))
        .and_then(|value| value.checked_div(i128::from(v1)))
        .context("spot probe quote improvement overflow")?;
    Ok(signed(delta))
}

fn scaled_cost_ppb(ideal: &BigRational, actual: u64) -> Result<i128> {
    if ideal <= &BigRational::zero() {
        bail!("spot probe ideal output is not positive");
    }
    let value = (ideal - BigRational::from_integer(BigInt::from(actual)))
        * BigInt::from(PARTS_PER_BILLION)
        / ideal;
    (value.numer() / value.denom())
        .to_i128()
        .context("spot probe PPB metric exceeds i128")
}

fn fee_ceil(amount: u64, bps: u16) -> Result<u64> {
    if bps > BPS_DENOMINATOR {
        bail!("spot probe fee exceeds 10,000 bps");
    }
    u64::try_from(
        ceil_div(
            u128::from(amount)
                .checked_mul(u128::from(bps))
                .context("spot probe fee overflow")?,
            u128::from(BPS_DENOMINATOR),
        )
        .context("spot probe fee division failed")?,
    )
    .context("spot probe fee exceeds u64")
}

fn v1_price0_nad(reserve0: u64, reserve1: u64) -> Result<u64> {
    if reserve0 == 0 {
        bail!("V1 spot probe token0 reserve is zero");
    }
    u64::try_from(
        u128::from(reserve1)
            .checked_mul(u128::from(omnipair::constants::NAD))
            .context("V1 spot probe price overflow")?
            / u128::from(reserve0),
    )
    .context("V1 spot probe price exceeds u64")
}

fn sizing_source_sha256(
    window: &SpotProbeSizingWindowV1,
    samples: &BTreeMap<(String, SpotProbeDirectionV1), Vec<FactualGrossInputSampleV1>>,
) -> Result<String> {
    let mut digest = Sha256::new();
    digest.update(SPOT_PROBE_SIZING_HASH_DOMAIN_V1);
    let window_bytes = serde_json::to_vec(window)?;
    digest.update(u64::try_from(window_bytes.len())?.to_le_bytes());
    digest.update(window_bytes);
    for ((market, direction), rows) in samples {
        digest.update(u64::try_from(market.len())?.to_le_bytes());
        digest.update(market.as_bytes());
        digest.update([match direction {
            SpotProbeDirectionV1::Token0In => 0,
            SpotProbeDirectionV1::Token1In => 1,
        }]);
        for row in rows {
            digest.update(u64::try_from(row.operation_id.len())?.to_le_bytes());
            digest.update(row.operation_id.as_bytes());
            digest.update(row.slot.to_le_bytes());
            digest.update(row.gross_input.to_le_bytes());
        }
    }
    Ok(hex::encode(digest.finalize()))
}

fn spot_probe_set_sha256(set: &SpotProbeSetV1) -> Result<String> {
    let mut digest = Sha256::new();
    digest.update(SPOT_PROBE_HASH_DOMAIN_V1);
    digest.update(serde_json::to_vec(set)?);
    Ok(hex::encode(digest.finalize()))
}

fn amount_u64(field: &str, amount: &TokenAmount) -> Result<u64> {
    amount
        .as_str()
        .parse::<u64>()
        .with_context(|| format!("{field} exceeds u64"))
}

fn token(value: u64) -> TokenAmount {
    TokenAmount::new(value.to_string()).expect("u64 is a canonical token amount")
}

fn token_big(value: &BigInt) -> TokenAmount {
    TokenAmount::new(value.to_string()).expect("non-negative BigInt is a canonical token amount")
}

fn signed(value: i128) -> SignedTokenAmount {
    SignedTokenAmount::new(value.to_string()).expect("i128 is a canonical signed token amount")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfer_fee::{
        EpochScheduleV1, MintTransferFeeScheduleV1, TokenProgramKindV1, TransferFeeEpochV1,
        TransferFeeRegistryV1,
    };

    fn registry() -> ValidatedTransferFeeRegistryV1 {
        let schedule = |mint: &str, bps: u16| MintTransferFeeScheduleV1 {
            schema_version: 1,
            mint: mint.to_owned(),
            token_program: TokenProgramKindV1::Token2022,
            epochs: vec![TransferFeeEpochV1 {
                effective_epoch: 0,
                transfer_fee_basis_points: bps,
                maximum_fee: token(10_000),
                source_signature: "initialize".to_owned(),
                source_slot: 0,
                decoded_instruction_sha256: "b".repeat(64),
            }],
        };
        TransferFeeRegistryV1 {
            schema_version: 1,
            epoch_schedule: EpochScheduleV1 {
                slots_per_epoch: 432_000,
                leader_schedule_slot_offset: 432_000,
                warmup: false,
                first_normal_epoch: 0,
                first_normal_slot: 0,
                source_response_sha256: "a".repeat(64),
            },
            mints: vec![schedule("token0", 100), schedule("token1", 25)],
        }
        .validate()
        .unwrap()
    }

    #[test]
    fn frozen_nav_sizes_are_exact_for_10_100_and_500_bps() {
        let state = V1SpotPreStateV1 {
            market: "market".to_owned(),
            token0: "token0".to_owned(),
            token1: "token1".to_owned(),
            token0_decimals: 6,
            token1_decimals: 6,
            reserve0: 1_000_000,
            reserve1: 4_000_000,
            cash_reserve0: 1_000_000,
            cash_reserve1: 4_000_000,
            swap_fee_bps: 30,
            protocol_swap_fee_bps: 2_000,
        };
        assert_eq!(
            factual_nav_gross_input(&state, SpotProbeDirectionV1::Token0In, 10).unwrap(),
            2_000
        );
        assert_eq!(
            factual_nav_gross_input(&state, SpotProbeDirectionV1::Token0In, 100).unwrap(),
            20_000
        );
        assert_eq!(
            factual_nav_gross_input(&state, SpotProbeDirectionV1::Token0In, 500).unwrap(),
            100_000
        );
        assert_eq!(
            factual_nav_gross_input(&state, SpotProbeDirectionV1::Token1In, 10).unwrap(),
            8_000
        );
        assert_eq!(
            factual_nav_gross_input(&state, SpotProbeDirectionV1::Token1In, 100).unwrap(),
            80_000
        );
        assert_eq!(
            factual_nav_gross_input(&state, SpotProbeDirectionV1::Token1In, 500).unwrap(),
            400_000
        );
    }

    #[test]
    fn p50_and_p90_use_deterministic_floor_indices_with_ties() {
        let key = ("market".to_owned(), SpotProbeDirectionV1::Token0In);
        let samples = vec![10, 20, 20, 40, 100]
            .into_iter()
            .enumerate()
            .map(|(index, gross_input)| FactualGrossInputSampleV1 {
                operation_id: format!("op-{index}"),
                slot: u64::try_from(index).unwrap(),
                gross_input,
            })
            .collect();
        let index = SpotProbeSizingIndexV1 {
            samples: BTreeMap::from([(key, samples)]),
            window: SpotProbeSizingWindowV1 {
                start_unix_seconds: 0,
                end_unix_seconds_exclusive: 100,
                standardized_trade_nav_bps: vec![10, 100, 500],
            },
            source_sha256: "a".repeat(64),
        };
        assert_eq!(
            index.quantile("market", SpotProbeDirectionV1::Token0In, 50, 100),
            Some(20)
        );
        assert_eq!(
            index.quantile("market", SpotProbeDirectionV1::Token0In, 90, 100),
            Some(40)
        );
        assert_eq!(
            index.quantile("market", SpotProbeDirectionV1::Token1In, 50, 100),
            None
        );
    }

    #[test]
    fn sizing_window_and_config_are_committed_without_cross_split_reuse() {
        let samples = BTreeMap::from([(
            ("market".to_owned(), SpotProbeDirectionV1::Token0In),
            vec![FactualGrossInputSampleV1 {
                operation_id: "op".to_owned(),
                slot: 7,
                gross_input: 42,
            }],
        )]);
        let training = SpotProbeSizingWindowV1 {
            start_unix_seconds: 0,
            end_unix_seconds_exclusive: 60,
            standardized_trade_nav_bps: vec![10, 100, 500],
        };
        let validation = SpotProbeSizingWindowV1 {
            start_unix_seconds: 60,
            end_unix_seconds_exclusive: 80,
            standardized_trade_nav_bps: vec![10, 100, 500],
        };
        assert!(training.contains(59));
        assert!(!training.contains(60));
        assert_ne!(
            sizing_source_sha256(&training, &samples).unwrap(),
            sizing_source_sha256(&validation, &samples).unwrap()
        );
        let mut invalid = training;
        invalid.standardized_trade_nav_bps = vec![10, 500, 100];
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn price_conversion_is_symmetric_across_decimal_directions() {
        let price = 2_000_000_000;
        let quote =
            ideal_output_at_price(SpotProbeDirectionV1::Token0In, 1_000_000, price, 6, 9).unwrap();
        assert_eq!(
            quote,
            BigRational::from_integer(BigInt::from(2_000_000_000_u64))
        );
        let base =
            ideal_output_at_price(SpotProbeDirectionV1::Token1In, 2_000_000_000, price, 6, 9)
                .unwrap();
        assert_eq!(base, BigRational::from_integer(BigInt::from(1_000_000_u64)));
    }

    #[test]
    fn v1_probe_uses_native_cpmm_fee_math_and_historical_transfer_outcomes_both_directions() {
        let state = V1SpotPreStateV1 {
            market: "market".to_owned(),
            token0: "token0".to_owned(),
            token1: "token1".to_owned(),
            token0_decimals: 6,
            token1_decimals: 6,
            reserve0: 1_000_000,
            reserve1: 2_000_000,
            cash_reserve0: 1_000_000,
            cash_reserve1: 2_000_000,
            swap_fee_bps: 30,
            protocol_swap_fee_bps: 2_000,
        };
        let transfers = registry();
        for direction in [
            SpotProbeDirectionV1::Token0In,
            SpotProbeDirectionV1::Token1In,
        ] {
            let quote = quote_v1(&state, direction, 10_000, 1, &transfers).unwrap();
            let expected_curve_input = 9_970;
            let (reserve_in, reserve_out) = if direction.token0_in() {
                (state.reserve0, state.reserve1)
            } else {
                (state.reserve1, state.reserve0)
            };
            let expected_output =
                CPCurve::calculate_amount_out(reserve_in, reserve_out, expected_curve_input)
                    .unwrap();
            assert_eq!(quote.curve_input.as_str(), expected_curve_input.to_string());
            assert_eq!(
                quote.curve_gross_output.as_str(),
                expected_output.to_string()
            );
            assert_eq!(quote.gross_output.as_str(), expected_output.to_string());
            assert!(quote.executable);
            assert_eq!(quote.common_reference_price_nad, quote.start_price_nad);
            assert_eq!(
                quote.external_lp_nav_basis,
                Some(SpotProbeExternalLpNavBasisV1::FactualV1AccountedLpReserves)
            );
            assert!(quote.cpmm_equivalent_depth_per_external_lp_value.is_some());
            let input_mint = if direction.token0_in() {
                "token0"
            } else {
                "token1"
            };
            let output_mint = if direction.token0_in() {
                "token1"
            } else {
                "token0"
            };
            let input = transfers.transfer(input_mint, 1, 10_000).unwrap();
            let output = transfers.transfer(output_mint, 1, expected_output).unwrap();
            assert_eq!(
                quote.input_transfer_fee.as_str(),
                input.fee_amount.to_string()
            );
            assert_eq!(quote.net_input.as_str(), input.net_amount.to_string());
            assert_eq!(
                quote.output_transfer_fee.as_str(),
                output.fee_amount.to_string()
            );
            assert_eq!(quote.net_output.as_str(), output.net_amount.to_string());
        }
    }

    #[test]
    fn quote_improvement_is_signed_and_wallet_net_specific() {
        assert_eq!(
            quote_improvement_ppb(&token(100), &token(110))
                .unwrap()
                .as_str(),
            "100000000"
        );
        assert_eq!(
            quote_improvement_ppb(&token(100), &token(90))
                .unwrap()
                .as_str(),
            "-100000000"
        );
        assert!(quote_improvement_ppb(&token(0), &token(1)).is_err());
    }

    #[test]
    fn execution_cost_uses_common_factual_reference_while_slippage_uses_own_curve() {
        let (execution_cost, slippage, _, _) = derive_quote_metrics(
            SpotProbeDirectionV1::Token0In,
            100,
            90,
            100,
            90,
            2 * omnipair::constants::NAD,
            omnipair::constants::NAD,
            6,
            6,
            Some(BigRational::from_integer(BigInt::from(2_000_u16))),
        )
        .unwrap();
        assert_eq!(execution_cost.as_str(), "550000000");
        assert_eq!(slippage.as_str(), "100000000");
    }
}
