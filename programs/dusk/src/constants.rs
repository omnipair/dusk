use anchor_lang::{prelude::*, solana_program::pubkey};

// GLOBAL CONSTANTS
/// NAD: Nine-decimal fixed point unit (1e9 scaling), similar to WAD (1e18) by Maker.
#[constant]
pub const NAD: u64 = 1_000_000_000;
#[constant]
pub const NAD_DECIMALS: u8 = 9;
/// Fee/yield growth indexes use a full unsigned share-width fractional scale.
/// Because LP supply is `u64`, every distributor remainder is strictly below
/// one raw token atom: `remainder < supply < 2^64`.
pub const YIELD_GROWTH_SCALE_Q64: u128 = 1_u128 << 64;
pub const YIELD_GROWTH_FRACTION_MASK_Q64: u128 = YIELD_GROWTH_SCALE_Q64 - 1;
#[constant]
pub const BPS_DENOMINATOR: u16 = 10_000;
#[constant]
pub const MAX_COLLATERAL_FACTOR_BPS: u16 = 8_500;
#[constant]
pub const LTV_BUFFER_BPS: u16 = 500;
/// Absolute cap shared by the three configurable swap-fee components.
/// Their configured component caps must also sum to no more than this value.
#[constant]
pub const MAX_PARAMETER_FEE_BPS: u16 = 5_000;
#[constant]
pub const MAX_REFERRAL_INTEREST_SHARE_BPS: u16 = BPS_DENOMINATOR;
#[constant]
pub const LIQUIDATION_CLOSE_FACTOR_BPS: u16 = 5_000;
#[constant]
pub const LIQUIDATION_INCENTIVE_BPS: u16 = 100;
#[constant]
pub const LIQUIDATION_MAX_INCENTIVE_BPS: u16 = 500;
#[constant]
pub const LIQUIDATION_INSURANCE_FUNDING_BPS: u16 = 200;
#[constant]
pub const LIQUIDATION_PENALTY_BPS: u16 = 300;
#[constant]
pub const MARKET_CREATION_FEE_LAMPORTS: u64 = 200_000_000; // 0.2 SOL
#[constant]
pub const TARGET_MS_PER_SLOT: u64 = 400;
/// Direct-yLP parameter governance thresholds and wall-clock lifecycle.
#[constant]
pub const PARAMETER_PROPOSAL_SPONSOR_BPS: u16 = 100; // 1%
#[constant]
pub const PARAMETER_PROPOSAL_SUPPORT_BPS: u16 = 5_000; // strict >50%
#[constant]
pub const PARAMETER_PROPOSAL_TIMELOCK_SECONDS: i64 = 7 * 24 * 60 * 60;
#[constant]
pub const PARAMETER_PROPOSAL_EXECUTION_WINDOW_SECONDS: i64 = 7 * 24 * 60 * 60;
#[constant]
pub const PARAMETER_EXECUTION_MAX_UTILIZATION_BPS: u64 = 8_000;

pub const PROPOSAL_METADATA_VERSION: u8 = 1;
pub const MAX_PROPOSAL_TITLE_BYTES: usize = 96;
pub const MAX_PROPOSAL_DESCRIPTION_URI_BYTES: usize = 200;
pub const MAX_PROPOSAL_DESCRIPTION_BYTES: u32 = 32_768;

pub const MIN_HALF_LIFE_MS: u64 = 60_000;
pub const MAX_HALF_LIFE_MS: u64 = 12 * 60 * 60 * 1_000;
pub const TAYLOR_TERMS: u64 = 5;
pub const NATURAL_LOG_OF_TWO_NAD: u64 = 693_147_180;
pub const MS_PER_DAY: u64 = 86_400_000;
pub const MS_PER_YEAR: u64 = 365 * MS_PER_DAY;
pub const MIN_LIQUIDITY: u64 = 1_000;

// ADAPTIVE-CURVE INTEREST RATE MODEL
// A fixed-shape curve anchored at the target utilization, multiplied by a
// per-market `rate_at_target` that drifts toward the target over time:
//
//   instantaneous_rate(u) = rate_at_target * curve(error(u))
//   error(u) in [-1, 1], 0 at target; curve in [1/steepness, steepness]
//   rate_at_target_next = rate_at_target * e^(speed * error * dt/year)  (clamped)
//
// The curve gives an immediate, graded response to utilization; the anchor
// makes the *level* market-driven (with a bounded ceiling), so the protocol
// never has to know the "right" rate in advance.
/// Lower/upper bounds and initial value for the adaptive anchor (APR in NAD).
pub const INTEREST_MIN_RATE_AT_TARGET_NAD: u128 = (NAD as u128) / 1_000; // 0.1% APR
pub const INTEREST_MAX_RATE_AT_TARGET_NAD: u128 = (NAD as u128) * 2; // 200% APR
pub const INTEREST_INITIAL_RATE_AT_TARGET_NAD: u128 = (NAD as u128) * 4 / 100; // 4% APR
/// Cap on the per-accrual exponent (NAD), bounding the anchor's move in a single
/// step so a stale market can't jump violently (clamped further by min/max).
pub const INTEREST_MAX_ADAPTATION_STEP_NAD: i128 = (NAD as i128) / 2;
/// Upper bound on the elapsed time charged in a single accrual, to bound
/// index growth (and therefore overflow / abuse) for very stale markets.
pub const MAX_INTEREST_ACCRUAL_MS: u64 = MS_PER_YEAR;

#[constant]
pub const MARKET_V2_SEED_PREFIX: &[u8] = b"market_v2";
#[constant]
pub const FUTARCHY_AUTHORITY_SEED_PREFIX: &[u8] = b"futarchy_authority";
#[constant]
pub const REFERRAL_PARTNER_SEED_PREFIX: &[u8] = b"referral_partner";
#[constant]
pub const REFERRAL_ACCRUAL_SEED_PREFIX: &[u8] = b"referral_accrual";
#[constant]
pub const MARKET_RESERVE_VAULT_SEED_PREFIX: &[u8] = b"market_reserve";
#[constant]
pub const MARKET_COLLATERAL_VAULT_SEED_PREFIX: &[u8] = b"market_collateral";
#[constant]
pub const MARKET_INTEREST_VAULT_SEED_PREFIX: &[u8] = b"market_interest";
#[constant]
pub const BORROW_POSITION_SEED_PREFIX: &[u8] = b"borrow_position_v2";
#[constant]
pub const YIELD_ACCOUNT_SEED_PREFIX: &[u8] = b"yield";
#[constant]
pub const PARAMETER_PROPOSAL_SEED_PREFIX: &[u8] = b"parameter_proposal";
#[constant]
pub const PROPOSAL_SUPPORT_SEED_PREFIX: &[u8] = b"proposal_support";
pub const TRANSFER_HOOK_EXTRA_ACCOUNT_METAS_SEED_PREFIX: &[u8] = b"extra-account-metas";
#[constant]
pub const HLP_YLP_VAULT_SEED_PREFIX: &[u8] = b"hlp_ylp_vault";
#[constant]
pub const METADATA_SEED_PREFIX: &[u8] = b"metadata";
#[constant]
pub const INSURANCE_SEED_PREFIX: &[u8] = b"insurance";
#[constant]
pub const LEVERAGE_POSITION_SEED_PREFIX: &[u8] = b"leverage_position_v2";
#[constant]
pub const LEVERAGE_DELEGATION_SEED_PREFIX: &[u8] = b"leverage_delegation_v2";
#[constant]
pub const LEVERAGE_COLLATERAL_VAULT_SEED_PREFIX: &[u8] = b"leverage_collateral";
#[constant]
pub const LEVERAGE_MAX_MULTIPLIER_BPS: u64 = 200_000; // 20x circuit breaker
#[constant]
pub const LEVERAGE_MAX_UNWIND_IMPACT_BPS: u16 = 200; // 2%
#[constant]
pub const LEVERAGE_INITIAL_MARGIN_BPS: u16 = 1_000; // 10%
#[constant]
pub const LEVERAGE_MAINTENANCE_BUFFER_BPS: u16 = 700; // 7%
/// Serialized `Market` account layout discriminator.
///
/// Dusk is still pre-launch, so CONCENTRATED ships in the first deployable layout.
/// Increment this only for an incompatible account-layout change after
/// deployment, never for ordinary feature work or product naming.
#[constant]
pub const MARKET_LAYOUT_VERSION: u8 = 1;

/// Emergency signer authorized to toggle reduce-only mode.
#[cfg(feature = "development")]
pub const REDUCE_ONLY_EMERGENCY_AUTHORITY: Pubkey = pubkey!("2iXtA8oeZqUU5pofxK971TCEvFGfems2AcDRaZHKD2pQ");
#[cfg(not(feature = "development"))]
pub const REDUCE_ONLY_EMERGENCY_AUTHORITY: Pubkey = pubkey!("3YL87sTCrHMB6DYKorE9CCN4dL45kZPahoREcMLDY6QV");
