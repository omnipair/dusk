import * as anchorNamespace from "@coral-xyz/anchor";
import { type BN } from "@coral-xyz/anchor";

import { address, type AddressLike } from "./address.js";
import {
  GOVERNANCE_BPS_DENOMINATOR,
  MAX_PROPOSAL_DESCRIPTION_BYTES,
  MAX_PROPOSAL_DESCRIPTION_URI_BYTES,
  MAX_PROPOSAL_TITLE_BYTES,
  PARAMETER_PROPOSAL_SPONSOR_BPS,
  PARAMETER_PROPOSAL_SUPPORT_BPS,
  PROPOSAL_METADATA_VERSION,
} from "./constants.js";

export type GovernanceIntegerLike = bigint | number | string | BN | { toString(): string };

export const NAD = 1_000_000_000n;
export const MIN_PARAMETER_HALF_LIFE_MS = 60_000n;
export const MAX_PARAMETER_HALF_LIFE_MS = 12n * 60n * 60n * 1_000n;
export const MAX_PARAMETER_FEE_BPS = 5_000;
export const MIN_IRM_TARGET_UTILIZATION_BPS = 6_000;
export const MAX_IRM_TARGET_UTILIZATION_BPS = 7_500;
export const MIN_IRM_CURVE_STEEPNESS_NAD = 2n * NAD;
export const MAX_IRM_CURVE_STEEPNESS_NAD = 8n * NAD;
export const MIN_IRM_ADJUSTMENT_SPEED_PER_YEAR = 1n;
export const MAX_IRM_ADJUSTMENT_SPEED_PER_YEAR = 50n;
export const MAX_CONCENTRATION_AMPLIFICATION_NAD = 2_000n * NAD;
export const MAX_FEE_COEFFICIENT_NAD = 100n * NAD;
export const MAX_VOLATILITY_ACCUMULATOR_NAD = 10n * NAD;
export const MAX_DAILY_BORROW_BPS = 3_000;
export const MIN_CENTER_ADJUSTMENT_NAD = NAD / 1_000_000n;
export const MAX_CENTER_ADJUSTMENT_NAD = NAD / 10n;
export const MAX_CENTER_ADJUSTMENT_INTERVAL_SLOTS = 216_000n;
export const MAX_LAUNCH_FEE_DURATION_SECONDS = 30n * 24n * 60n * 60n;
export const LAUNCH_FEE_DECAY_DISABLED = 0;
export const LAUNCH_FEE_DECAY_LINEAR = 1;
export const LAUNCH_FEE_DECAY_EXPONENTIAL = 2;
export const LAUNCH_RATE_LIMIT_ASSET_DISABLED = 0;
export const LAUNCH_RATE_LIMIT_ASSET_BASE = 1;
export const LAUNCH_RATE_LIMIT_ASSET_QUOTE = 2;
export const SWAP_FEE_COLLECT_INPUT_ASSET = 0;
export const SWAP_FEE_COLLECT_BASE_ONLY = 1;
export const SWAP_FEE_COLLECT_QUOTE_ONLY = 2;
export const MAX_LAUNCH_MARKET_FEE_PERIODS = 64;
export const PARAMETER_PROPOSAL_DIGEST_DOMAIN = "DUSK_PARAMETER_PROPOSAL_V1";

const U64_MAX = 0xffff_ffff_ffff_ffffn;
const PROPOSAL_DIGEST_DOMAIN = new TextEncoder().encode(PARAMETER_PROPOSAL_DIGEST_DOMAIN);

// `@coral-xyz/anchor` ships both a CommonJS and an ESM build, and they disagree
// about how `BN` is reachable: bundlers resolve the ESM build, which has no
// default export, while Node's CJS interop exposes `BN` only through `default`.
// Reading both keeps this module importable from a browser bundle and from Node.
type AnchorBNConstructor = new (value: string) => BN;
const anchorExports = anchorNamespace as unknown as {
  BN?: AnchorBNConstructor;
  default?: { BN?: AnchorBNConstructor };
};
const AnchorBN = (anchorExports.BN ?? anchorExports.default?.BN) as AnchorBNConstructor;

export interface FeeProfileInput {
  baseFeeBps: number;
  divergenceFeeShareCapBps: number;
  volatilityFeeShareCapBps: number;
  divergenceFeeCoefficientNad: GovernanceIntegerLike;
  volatilityFeeCoefficientNad: GovernanceIntegerLike;
  volatilityHalfLifeMs: GovernanceIntegerLike;
  volatilityShockCapNad: GovernanceIntegerLike;
  volatilityAccumulatorCapNad: GovernanceIntegerLike;
  swapFeeCollectMode?: number;
  /** LP-owned swap-fee share compounded into reserve principal, in bps. */
  compoundingFeeBps?: number;
  launchFeeStartBps?: number;
  launchFeeDurationSeconds?: GovernanceIntegerLike;
  launchFeeDecayMode?: number;
  launchMarketPriceStepBps?: number;
  launchMarketNumberOfPeriods?: number;
  launchMarketReductionFactorBps?: number;
  launchRateLimitAsset?: number;
  launchRateLimitReferenceNad?: GovernanceIntegerLike;
  launchRateLimitIncrementBps?: number;
  launchRateLimitMaxFeeBps?: number;
  launchRateLimitDurationSeconds?: GovernanceIntegerLike;
}

export interface FeeProfile {
  baseFeeBps: number;
  divergenceFeeShareCapBps: number;
  volatilityFeeShareCapBps: number;
  divergenceFeeCoefficientNad: BN;
  volatilityFeeCoefficientNad: BN;
  volatilityHalfLifeMs: BN;
  volatilityShockCapNad: BN;
  volatilityAccumulatorCapNad: BN;
  swapFeeCollectMode: number;
  compoundingFeeBps: number;
  launchFeeStartBps: number;
  launchFeeDurationSeconds: BN;
  launchFeeDecayMode: number;
  launchMarketPriceStepBps: number;
  launchMarketNumberOfPeriods: number;
  launchMarketReductionFactorBps: number;
  launchRateLimitAsset: number;
  launchRateLimitReferenceNad: BN;
  launchRateLimitIncrementBps: number;
  launchRateLimitMaxFeeBps: number;
  launchRateLimitDurationSeconds: BN;
}

export interface IrmConfigInput {
  targetUtilizationBps: number;
  curveSteepnessNad: GovernanceIntegerLike;
  adjustmentSpeedPerYear: GovernanceIntegerLike;
}

export interface IrmConfig {
  targetUtilizationBps: number;
  curveSteepnessNad: BN;
  adjustmentSpeedPerYear: BN;
}

export type ParameterUpdate =
  | { kind: "fee"; profile: FeeProfile }
  | {
      kind: "concentration";
      peakAmplificationNad: BN;
      coreHalfWidthBps: number;
      fadeWidthBps: number;
    }
  | { kind: "irm"; config: IrmConfig }
  | {
      kind: "emaHalfLives";
      priceMs: BN;
      directionalPriceMs: BN;
      curveDepthMs: BN;
      centerPriceMs: BN;
    }
  | { kind: "dailyBorrowLimit"; maxDailyBorrowBps: number }
  | {
      kind: "centerController";
      adjustmentThresholdNad: BN;
      adjustmentStepNad: BN;
      minAdjustmentIntervalSlots: BN;
    };

export type ParameterFamilyName = ParameterUpdate["kind"];

export interface ProposalMetadataV1 {
  version: number;
  title: string;
  descriptionUri: string;
  descriptionSha256: number[];
  descriptionLen: number;
}

export type ProposalMarkdown = string | Uint8Array;

export interface ProposalDescriptionUploadContext {
  readonly title: string;
  readonly contentType: "text/markdown; charset=utf-8";
}

export type ProposalDescriptionUploader = (
  exactMarkdownBytes: Uint8Array,
  context: ProposalDescriptionUploadContext
) => Promise<string | { uri: string }>;

export type ProposalFetch = (input: string | URL, init?: RequestInit) => Promise<Response>;
export type ProposalUriResolver = (uri: string) => string | URL | Promise<string | URL>;

export interface ProposalDescriptionFetchOptions {
  fetch?: ProposalFetch;
  resolveUri?: ProposalUriResolver;
  signal?: AbortSignal;
  ipfsGateway?: string;
  arweaveGateway?: string;
}

export interface VerifiedProposalDescription {
  verified: true;
  uri: string;
  markdown: string;
  bytes: Uint8Array;
}

export type ProposalDescriptionResult =
  | VerifiedProposalDescription
  | {
      verified: false;
      uri: string;
      warning: string;
      error: unknown;
    };

export interface CreateProposalMetadataInput extends ProposalDescriptionFetchOptions {
  title: string;
  markdown: ProposalMarkdown;
  descriptionUri: string;
}

export interface UploadProposalMetadataInput extends ProposalDescriptionFetchOptions {
  title: string;
  markdown: ProposalMarkdown;
  upload: ProposalDescriptionUploader;
}

export interface ParameterProposalDigestInput {
  programId: AddressLike;
  market: AddressLike;
  proposer: AddressLike;
  nonce: GovernanceIntegerLike;
  familyRevision: GovernanceIntegerLike;
  update: ParameterUpdate;
  metadata: ProposalMetadataV1;
}

export interface DecodedParameterProposalForDigest {
  market: AddressLike;
  proposer: AddressLike;
  nonce: GovernanceIntegerLike;
  familyRevision: GovernanceIntegerLike;
  update: unknown;
  metadata: ProposalMetadataV1;
  digest?: Uint8Array | number[];
}

/** Normalize and validate the complete mutable fee family. */
export function feeParameterUpdate(input: FeeProfileInput): ParameterUpdate {
  assertBps(input.baseFeeBps, "baseFeeBps", MAX_PARAMETER_FEE_BPS);
  assertBps(
    input.divergenceFeeShareCapBps,
    "divergenceFeeShareCapBps",
    MAX_PARAMETER_FEE_BPS
  );
  assertBps(
    input.volatilityFeeShareCapBps,
    "volatilityFeeShareCapBps",
    MAX_PARAMETER_FEE_BPS
  );
  if (
    input.baseFeeBps +
      input.divergenceFeeShareCapBps +
      input.volatilityFeeShareCapBps >
    MAX_PARAMETER_FEE_BPS
  ) {
    throw new Error(`fee component caps must sum to at most ${MAX_PARAMETER_FEE_BPS} bps`);
  }

  const divergenceFeeCoefficientNad = toU64BigInt(
    input.divergenceFeeCoefficientNad,
    "divergenceFeeCoefficientNad"
  );
  const volatilityFeeCoefficientNad = toU64BigInt(
    input.volatilityFeeCoefficientNad,
    "volatilityFeeCoefficientNad"
  );
  const volatilityHalfLifeMs = assertRange(
    toU64BigInt(input.volatilityHalfLifeMs, "volatilityHalfLifeMs"),
    MIN_PARAMETER_HALF_LIFE_MS,
    MAX_PARAMETER_HALF_LIFE_MS,
    "volatilityHalfLifeMs"
  );
  const volatilityShockCapNad = toU64BigInt(
    input.volatilityShockCapNad,
    "volatilityShockCapNad"
  );
  const volatilityAccumulatorCapNad = toU64BigInt(
    input.volatilityAccumulatorCapNad,
    "volatilityAccumulatorCapNad"
  );
  assertRange(
    divergenceFeeCoefficientNad,
    0n,
    MAX_FEE_COEFFICIENT_NAD,
    "divergenceFeeCoefficientNad"
  );
  assertRange(
    volatilityFeeCoefficientNad,
    0n,
    MAX_FEE_COEFFICIENT_NAD,
    "volatilityFeeCoefficientNad"
  );
  const signalDisabled = volatilityShockCapNad === 0n && volatilityAccumulatorCapNad === 0n;
  const signalValid =
    volatilityShockCapNad > 0n &&
    volatilityShockCapNad <= volatilityAccumulatorCapNad &&
    volatilityAccumulatorCapNad <= MAX_VOLATILITY_ACCUMULATOR_NAD;
  if (!signalDisabled && !signalValid) {
    throw new Error(
      "volatility caps must both be zero, or shock > 0, shock <= accumulator, and accumulator <= 10 NAD"
    );
  }
  if (volatilityFeeCoefficientNad !== 0n && !signalValid) {
    throw new Error("a nonzero volatility fee coefficient requires an enabled volatility signal");
  }

  const swapFeeCollectMode =
    input.swapFeeCollectMode ?? SWAP_FEE_COLLECT_INPUT_ASSET;
  if (
    ![
      SWAP_FEE_COLLECT_INPUT_ASSET,
      SWAP_FEE_COLLECT_BASE_ONLY,
      SWAP_FEE_COLLECT_QUOTE_ONLY,
    ].includes(swapFeeCollectMode)
  ) {
    throw new Error("invalid swap fee collection mode");
  }
  const compoundingFeeBps = input.compoundingFeeBps ?? 0;
  assertBps(
    compoundingFeeBps,
    "compoundingFeeBps",
    GOVERNANCE_BPS_DENOMINATOR
  );

  const launchFeeStartBps = input.launchFeeStartBps ?? 0;
  const launchFeeDurationSeconds = toU64BigInt(
    input.launchFeeDurationSeconds ?? 0,
    "launchFeeDurationSeconds"
  );
  const launchFeeDecayMode = input.launchFeeDecayMode ?? LAUNCH_FEE_DECAY_DISABLED;
  const launchMarketPriceStepBps = input.launchMarketPriceStepBps ?? 0;
  const launchMarketNumberOfPeriods = input.launchMarketNumberOfPeriods ?? 0;
  const launchMarketReductionFactorBps = input.launchMarketReductionFactorBps ?? 0;
  const marketScheduleDisabled =
    launchMarketPriceStepBps === 0 &&
    launchMarketNumberOfPeriods === 0 &&
    launchMarketReductionFactorBps === 0;
  const marketScheduleEnabled =
    launchMarketPriceStepBps > 0 &&
    launchMarketNumberOfPeriods > 0 &&
    launchMarketNumberOfPeriods <= MAX_LAUNCH_MARKET_FEE_PERIODS &&
    launchMarketReductionFactorBps > 0 &&
    launchMarketReductionFactorBps < GOVERNANCE_BPS_DENOMINATOR;
  const launchFeeDisabled =
    launchFeeStartBps === 0 &&
    launchFeeDurationSeconds === 0n &&
    launchFeeDecayMode === LAUNCH_FEE_DECAY_DISABLED &&
    marketScheduleDisabled;
  if (
    !launchFeeDisabled &&
    (launchFeeStartBps <= input.baseFeeBps ||
      launchFeeStartBps > MAX_PARAMETER_FEE_BPS ||
      launchFeeDurationSeconds < 1n ||
      launchFeeDurationSeconds > MAX_LAUNCH_FEE_DURATION_SECONDS ||
      ![LAUNCH_FEE_DECAY_LINEAR, LAUNCH_FEE_DECAY_EXPONENTIAL].includes(
        launchFeeDecayMode
      ))
  ) {
    throw new Error("invalid launch fee start, duration, or decay mode");
  }
  if (!launchFeeDisabled && !marketScheduleDisabled && !marketScheduleEnabled) {
    throw new Error("invalid launch market-cap fee schedule");
  }
  if (
    marketScheduleEnabled &&
    ![SWAP_FEE_COLLECT_BASE_ONLY, SWAP_FEE_COLLECT_QUOTE_ONLY].includes(
      swapFeeCollectMode
    )
  ) {
    throw new Error("a market-cap fee schedule requires a fixed fee asset");
  }
  if (
    !launchFeeDisabled &&
    launchFeeStartBps + input.divergenceFeeShareCapBps + input.volatilityFeeShareCapBps >
      MAX_PARAMETER_FEE_BPS
  ) {
    throw new Error(`launch fee component caps must sum to at most ${MAX_PARAMETER_FEE_BPS} bps`);
  }

  const launchRateLimitAsset =
    input.launchRateLimitAsset ?? LAUNCH_RATE_LIMIT_ASSET_DISABLED;
  const launchRateLimitReferenceNad = toU64BigInt(
    input.launchRateLimitReferenceNad ?? 0,
    "launchRateLimitReferenceNad"
  );
  const launchRateLimitIncrementBps = input.launchRateLimitIncrementBps ?? 0;
  const launchRateLimitMaxFeeBps = input.launchRateLimitMaxFeeBps ?? 0;
  const launchRateLimitDurationSeconds = toU64BigInt(
    input.launchRateLimitDurationSeconds ?? 0,
    "launchRateLimitDurationSeconds"
  );
  const rateLimitDisabled =
    launchRateLimitAsset === LAUNCH_RATE_LIMIT_ASSET_DISABLED &&
    launchRateLimitReferenceNad === 0n &&
    launchRateLimitIncrementBps === 0 &&
    launchRateLimitMaxFeeBps === 0 &&
    launchRateLimitDurationSeconds === 0n;
  const scheduledPeak = launchFeeDisabled ? input.baseFeeBps : launchFeeStartBps;
  if (
    !rateLimitDisabled &&
    (![LAUNCH_RATE_LIMIT_ASSET_BASE, LAUNCH_RATE_LIMIT_ASSET_QUOTE].includes(
      launchRateLimitAsset
    ) ||
      launchRateLimitReferenceNad === 0n ||
      launchRateLimitIncrementBps <= 0 ||
      launchRateLimitMaxFeeBps <= input.baseFeeBps ||
      launchRateLimitMaxFeeBps < scheduledPeak ||
      launchRateLimitDurationSeconds < 1n ||
      launchRateLimitDurationSeconds > MAX_LAUNCH_FEE_DURATION_SECONDS)
  ) {
    throw new Error("invalid launch buy-size limiter configuration");
  }
  if (!rateLimitDisabled) {
    assertBps(
      launchRateLimitIncrementBps,
      "launchRateLimitIncrementBps",
      MAX_PARAMETER_FEE_BPS,
      1
    );
    assertBps(
      launchRateLimitMaxFeeBps,
      "launchRateLimitMaxFeeBps",
      MAX_PARAMETER_FEE_BPS,
      1
    );
    if (
      launchRateLimitMaxFeeBps +
        input.divergenceFeeShareCapBps +
        input.volatilityFeeShareCapBps >
      MAX_PARAMETER_FEE_BPS
    ) {
      throw new Error(
        `rate-limit fee component caps must sum to at most ${MAX_PARAMETER_FEE_BPS} bps`
      );
    }
  }

  return {
    kind: "fee",
    profile: {
      baseFeeBps: input.baseFeeBps,
      divergenceFeeShareCapBps: input.divergenceFeeShareCapBps,
      volatilityFeeShareCapBps: input.volatilityFeeShareCapBps,
      divergenceFeeCoefficientNad: toBN(divergenceFeeCoefficientNad),
      volatilityFeeCoefficientNad: toBN(volatilityFeeCoefficientNad),
      volatilityHalfLifeMs: toBN(volatilityHalfLifeMs),
      volatilityShockCapNad: toBN(volatilityShockCapNad),
      volatilityAccumulatorCapNad: toBN(volatilityAccumulatorCapNad),
      swapFeeCollectMode,
      compoundingFeeBps,
      launchFeeStartBps,
      launchFeeDurationSeconds: toBN(launchFeeDurationSeconds),
      launchFeeDecayMode,
      launchMarketPriceStepBps,
      launchMarketNumberOfPeriods,
      launchMarketReductionFactorBps,
      launchRateLimitAsset,
      launchRateLimitReferenceNad: toBN(launchRateLimitReferenceNad),
      launchRateLimitIncrementBps,
      launchRateLimitMaxFeeBps,
      launchRateLimitDurationSeconds: toBN(launchRateLimitDurationSeconds),
    },
  };
}

/** Standard launch-token fee template: Base is sold and all swap revenue is Quote. */
export function standardLaunchFeeParameterUpdate(
  input: Omit<FeeProfileInput, "swapFeeCollectMode" | "launchRateLimitAsset">
): ParameterUpdate {
  return feeParameterUpdate({
    ...input,
    swapFeeCollectMode: SWAP_FEE_COLLECT_QUOTE_ONLY,
    launchRateLimitAsset: LAUNCH_RATE_LIMIT_ASSET_BASE,
  });
}

/** Build an atomic, protected concentration update. */
export function concentrationParameterUpdate(input: {
  peakAmplificationNad: GovernanceIntegerLike;
  coreHalfWidthBps: number;
  fadeWidthBps: number;
}): ParameterUpdate {
  const peakAmplificationNad = toU64BigInt(input.peakAmplificationNad, "peakAmplificationNad");
  assertBps(input.coreHalfWidthBps, "coreHalfWidthBps", 0xffff);
  assertBps(input.fadeWidthBps, "fadeWidthBps", 0xffff);
  if (input.coreHalfWidthBps + input.fadeWidthBps > 0xffff) {
    throw new Error("coreHalfWidthBps + fadeWidthBps must fit in u16");
  }
  if (peakAmplificationNad === NAD) {
    if (input.coreHalfWidthBps !== 0 || input.fadeWidthBps !== 0) {
      throw new Error("CPMM concentration must use zero core and fade widths");
    }
  } else {
    if (peakAmplificationNad <= NAD || peakAmplificationNad > MAX_CONCENTRATION_AMPLIFICATION_NAD) {
      throw new Error("peakAmplificationNad is outside the amplification policy");
    }
    if (input.coreHalfWidthBps === 0) {
      throw new Error("concentrated curves require a nonzero coreHalfWidthBps");
    }
  }

  return {
    kind: "concentration",
    peakAmplificationNad: toBN(peakAmplificationNad),
    coreHalfWidthBps: input.coreHalfWidthBps,
    fadeWidthBps: input.fadeWidthBps,
  };
}

export function irmParameterUpdate(input: IrmConfigInput): ParameterUpdate {
  assertBps(
    input.targetUtilizationBps,
    "targetUtilizationBps",
    MAX_IRM_TARGET_UTILIZATION_BPS,
    MIN_IRM_TARGET_UTILIZATION_BPS
  );
  const curveSteepnessNad = assertRange(
    toU64BigInt(input.curveSteepnessNad, "curveSteepnessNad"),
    MIN_IRM_CURVE_STEEPNESS_NAD,
    MAX_IRM_CURVE_STEEPNESS_NAD,
    "curveSteepnessNad"
  );
  const adjustmentSpeedPerYear = assertRange(
    toU64BigInt(input.adjustmentSpeedPerYear, "adjustmentSpeedPerYear"),
    MIN_IRM_ADJUSTMENT_SPEED_PER_YEAR,
    MAX_IRM_ADJUSTMENT_SPEED_PER_YEAR,
    "adjustmentSpeedPerYear"
  );
  return {
    kind: "irm",
    config: {
      targetUtilizationBps: input.targetUtilizationBps,
      curveSteepnessNad: toBN(curveSteepnessNad),
      adjustmentSpeedPerYear: toBN(adjustmentSpeedPerYear),
    },
  };
}

export function emaHalfLivesParameterUpdate(input: {
  priceMs: GovernanceIntegerLike;
  directionalPriceMs: GovernanceIntegerLike;
  curveDepthMs: GovernanceIntegerLike;
  centerPriceMs: GovernanceIntegerLike;
}): ParameterUpdate {
  const normalize = (value: GovernanceIntegerLike, label: string) =>
    toBN(
      assertRange(
        toU64BigInt(value, label),
        MIN_PARAMETER_HALF_LIFE_MS,
        MAX_PARAMETER_HALF_LIFE_MS,
        label
      )
    );
  return {
    kind: "emaHalfLives",
    priceMs: normalize(input.priceMs, "priceMs"),
    directionalPriceMs: normalize(input.directionalPriceMs, "directionalPriceMs"),
    curveDepthMs: normalize(input.curveDepthMs, "curveDepthMs"),
    centerPriceMs: normalize(input.centerPriceMs, "centerPriceMs"),
  };
}

export function dailyBorrowLimitParameterUpdate(maxDailyBorrowBps: number): ParameterUpdate {
  assertBps(maxDailyBorrowBps, "maxDailyBorrowBps", MAX_DAILY_BORROW_BPS);
  return { kind: "dailyBorrowLimit", maxDailyBorrowBps };
}

export function centerControllerParameterUpdate(input: {
  adjustmentThresholdNad: GovernanceIntegerLike;
  adjustmentStepNad: GovernanceIntegerLike;
  minAdjustmentIntervalSlots: GovernanceIntegerLike;
}): ParameterUpdate {
  const adjustmentThresholdNad = toU64BigInt(input.adjustmentThresholdNad, "adjustmentThresholdNad");
  const adjustmentStepNad = toU64BigInt(input.adjustmentStepNad, "adjustmentStepNad");
  const minAdjustmentIntervalSlots = toU64BigInt(
    input.minAdjustmentIntervalSlots,
    "minAdjustmentIntervalSlots"
  );
  const disabled =
    adjustmentThresholdNad === 0n && adjustmentStepNad === 0n && minAdjustmentIntervalSlots === 0n;
  if (
    !disabled &&
    (adjustmentStepNad < MIN_CENTER_ADJUSTMENT_NAD ||
      adjustmentStepNad > MAX_CENTER_ADJUSTMENT_NAD ||
      adjustmentThresholdNad < adjustmentStepNad ||
      adjustmentThresholdNad > MAX_CENTER_ADJUSTMENT_NAD ||
      minAdjustmentIntervalSlots < 1n ||
      minAdjustmentIntervalSlots > MAX_CENTER_ADJUSTMENT_INTERVAL_SLOTS)
  ) {
    throw new Error("invalid center-controller threshold, step, or interval");
  }
  return {
    kind: "centerController",
    adjustmentThresholdNad: toBN(adjustmentThresholdNad),
    adjustmentStepNad: toBN(adjustmentStepNad),
    minAdjustmentIntervalSlots: toBN(minAdjustmentIntervalSlots),
  };
}

export const parameterUpdate = {
  fee: feeParameterUpdate,
  concentration: concentrationParameterUpdate,
  irm: irmParameterUpdate,
  emaHalfLives: emaHalfLivesParameterUpdate,
  dailyBorrowLimit: dailyBorrowLimitParameterUpdate,
  centerController: centerControllerParameterUpdate,
} as const;

export function assertParameterUpdate(update: ParameterUpdate): void {
  switch (update.kind) {
    case "fee":
      feeParameterUpdate(update.profile);
      return;
    case "concentration":
      concentrationParameterUpdate(update);
      return;
    case "irm":
      irmParameterUpdate(update.config);
      return;
    case "emaHalfLives":
      emaHalfLivesParameterUpdate(update);
      return;
    case "dailyBorrowLimit":
      dailyBorrowLimitParameterUpdate(update.maxDailyBorrowBps);
      return;
    case "centerController":
      centerControllerParameterUpdate(update);
      return;
  }
}

export function parameterFamilyCode(update: ParameterUpdate): 0 | 1 | 2 | 3 | 4 | 5 {
  switch (update.kind) {
    case "fee":
      return 0;
    case "concentration":
      return 1;
    case "irm":
      return 2;
    case "emaHalfLives":
      return 3;
    case "dailyBorrowLimit":
      return 4;
    case "centerController":
      return 5;
  }
}

/** Convert the readable SDK union to Anchor's generated Rust-enum object. */
export function anchorParameterUpdate(update: ParameterUpdate): Record<string, unknown> {
  assertParameterUpdate(update);
  switch (update.kind) {
    case "fee":
      // Fee is a Rust tuple variant, so its one field is keyed by tuple index.
      return { fee: { 0: update.profile } };
    case "concentration":
      return {
        concentration: {
          peakAmplificationNad: update.peakAmplificationNad,
          coreHalfWidthBps: update.coreHalfWidthBps,
          fadeWidthBps: update.fadeWidthBps,
        },
      };
    case "irm":
      return { irm: { 0: update.config } };
    case "emaHalfLives":
      return {
        emaHalfLives: {
          priceMs: update.priceMs,
          directionalPriceMs: update.directionalPriceMs,
          curveDepthMs: update.curveDepthMs,
          centerPriceMs: update.centerPriceMs,
        },
      };
    case "dailyBorrowLimit":
      return { dailyBorrowLimit: { maxDailyBorrowBps: update.maxDailyBorrowBps } };
    case "centerController":
      return {
        centerController: {
          adjustmentThresholdNad: update.adjustmentThresholdNad,
          adjustmentStepNad: update.adjustmentStepNad,
          minAdjustmentIntervalSlots: update.minAdjustmentIntervalSlots,
        },
      };
  }
}

/** Convert a decoded Anchor enum (including tuple-field objects) to the readable SDK union. */
export function parameterUpdateFromAnchor(value: unknown): ParameterUpdate {
  const update = objectValue(value, "market parameter update");
  if (update.fee !== undefined) {
    const profile = tupleField(update.fee, "fee");
    return feeParameterUpdate(objectValue(profile, "fee profile") as unknown as FeeProfileInput);
  }
  if (update.concentration !== undefined) {
    const fields = objectValue(update.concentration, "concentration update");
    return concentrationParameterUpdate({
      peakAmplificationNad: integerField(fields, "peakAmplificationNad"),
      coreHalfWidthBps: numberField(fields, "coreHalfWidthBps"),
      fadeWidthBps: numberField(fields, "fadeWidthBps"),
    });
  }
  if (update.irm !== undefined) {
    const config = objectValue(tupleField(update.irm, "irm"), "IRM config");
    return irmParameterUpdate({
      targetUtilizationBps: numberField(config, "targetUtilizationBps"),
      curveSteepnessNad: integerField(config, "curveSteepnessNad"),
      adjustmentSpeedPerYear: integerField(config, "adjustmentSpeedPerYear"),
    });
  }
  if (update.emaHalfLives !== undefined) {
    const fields = objectValue(update.emaHalfLives, "EMA half-lives update");
    return emaHalfLivesParameterUpdate({
      priceMs: integerField(fields, "priceMs"),
      directionalPriceMs: integerField(fields, "directionalPriceMs"),
      curveDepthMs: integerField(fields, "curveDepthMs"),
      centerPriceMs: integerField(fields, "centerPriceMs"),
    });
  }
  if (update.dailyBorrowLimit !== undefined) {
    const fields = objectValue(update.dailyBorrowLimit, "daily borrow-limit update");
    return dailyBorrowLimitParameterUpdate(numberField(fields, "maxDailyBorrowBps"));
  }
  if (update.centerController !== undefined) {
    const fields = objectValue(update.centerController, "center-controller update");
    return centerControllerParameterUpdate({
      adjustmentThresholdNad: integerField(fields, "adjustmentThresholdNad"),
      adjustmentStepNad: integerField(fields, "adjustmentStepNad"),
      minAdjustmentIntervalSlots: integerField(fields, "minAdjustmentIntervalSlots"),
    });
  }
  throw new Error("unknown market parameter update variant");
}

/** Create metadata from already-uploaded bytes, then retrieve and verify the URI. */
export async function createProposalMetadata(
  input: CreateProposalMetadataInput
): Promise<ProposalMetadataV1> {
  const bytes = markdownBytes(input.markdown);
  const metadata: ProposalMetadataV1 = {
    version: PROPOSAL_METADATA_VERSION,
    title: input.title,
    descriptionUri: input.descriptionUri,
    descriptionSha256: [...(await sha256(bytes))],
    descriptionLen: bytes.byteLength,
  };
  assertProposalMetadata(metadata);
  await fetchAndVerifyProposalDescription(metadata, input);
  return metadata;
}

/** Upload the exact Markdown bytes, fetch the returned URI, and verify length + SHA-256. */
export async function uploadProposalMetadata(
  input: UploadProposalMetadataInput
): Promise<ProposalMetadataV1> {
  assertProposalTitle(input.title);
  const bytes = markdownBytes(input.markdown);
  assertDescriptionLength(bytes.byteLength);
  const uploaded = await input.upload(bytes.slice(), {
    title: input.title,
    contentType: "text/markdown; charset=utf-8",
  });
  const descriptionUri = typeof uploaded === "string" ? uploaded : uploaded.uri;
  return createProposalMetadata({ ...input, descriptionUri, markdown: bytes });
}

export function assertProposalMetadata(metadata: ProposalMetadataV1): void {
  if (metadata.version !== PROPOSAL_METADATA_VERSION) {
    throw new Error(`proposal metadata version must be ${PROPOSAL_METADATA_VERSION}`);
  }
  assertProposalTitle(metadata.title);
  assertProposalUri(metadata.descriptionUri);
  assertDescriptionLength(metadata.descriptionLen);
  if (!Number.isInteger(metadata.descriptionLen)) {
    throw new Error("descriptionLen must be an integer byte length");
  }
  if (metadata.descriptionSha256.length !== 32) {
    throw new Error("descriptionSha256 must contain exactly 32 bytes");
  }
  for (const byte of metadata.descriptionSha256) {
    if (!Number.isInteger(byte) || byte < 0 || byte > 255) {
      throw new Error("descriptionSha256 must contain only bytes");
    }
  }
  if (metadata.descriptionSha256.every((byte) => byte === 0)) {
    throw new Error("descriptionSha256 must be nonzero");
  }
}

export function assertProposalTitle(title: string): void {
  const byteLength = new TextEncoder().encode(title).byteLength;
  if (
    byteLength < 1 ||
    byteLength > MAX_PROPOSAL_TITLE_BYTES ||
    title !== title.trim() ||
    /\p{Cc}/u.test(title)
  ) {
    throw new Error(
      `title must be trimmed, control-free UTF-8 occupying 1-${MAX_PROPOSAL_TITLE_BYTES} bytes`
    );
  }
}

export function assertProposalUri(uri: string): void {
  const ascii = /^[\x00-\x7f]*$/.test(uri);
  const validScheme = ["ipfs://", "ar://", "https://"].some(
    (prefix) => uri.startsWith(prefix) && uri.length > prefix.length
  );
  if (
    uri.length < 1 ||
    uri.length > MAX_PROPOSAL_DESCRIPTION_URI_BYTES ||
    !ascii ||
    /[\x00-\x20\x7f]/.test(uri) ||
    !validScheme
  ) {
    throw new Error(
      `description URI must be a whitespace-free ipfs://, ar://, or https:// URI of at most ${MAX_PROPOSAL_DESCRIPTION_URI_BYTES} ASCII bytes`
    );
  }
}

/** Fetch no unbounded body, verify the exact byte count/hash, then decode strict UTF-8. */
export async function fetchAndVerifyProposalDescription(
  metadata: ProposalMetadataV1,
  options: ProposalDescriptionFetchOptions = {}
): Promise<VerifiedProposalDescription> {
  assertProposalMetadata(metadata);
  const fetchImpl = options.fetch ?? globalThis.fetch?.bind(globalThis);
  if (!fetchImpl) throw new Error("fetch is unavailable; supply ProposalDescriptionFetchOptions.fetch");
  const resolved = options.resolveUri
    ? await options.resolveUri(metadata.descriptionUri)
    : resolveProposalDescriptionUri(metadata.descriptionUri, options);
  const response = await fetchImpl(resolved, {
    signal: options.signal,
    headers: {
      accept: "text/markdown, text/plain;q=0.9, application/octet-stream;q=0.8",
      range: `bytes=0-${metadata.descriptionLen - 1}`,
    },
  });
  if (!response.ok) {
    throw new Error(`proposal description fetch failed with HTTP ${response.status}`);
  }
  const bytes = await readExactBody(response, metadata.descriptionLen);
  const actualHash = await sha256(bytes);
  if (!equalBytes(actualHash, metadata.descriptionSha256)) {
    throw new Error("proposal description SHA-256 does not match its on-chain metadata");
  }
  let markdown: string;
  try {
    markdown = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    throw new Error("verified proposal description is not valid UTF-8");
  }
  return { verified: true, uri: metadata.descriptionUri, markdown, bytes };
}

/** UI-friendly verification: failure is data, so callers can show title + typed diff. */
export async function tryFetchProposalDescription(
  metadata: ProposalMetadataV1,
  options: ProposalDescriptionFetchOptions = {}
): Promise<ProposalDescriptionResult> {
  try {
    return await fetchAndVerifyProposalDescription(metadata, options);
  } catch (error) {
    return {
      verified: false,
      uri: metadata.descriptionUri,
      warning: "Proposal rationale is unavailable or failed on-chain length/hash verification.",
      error,
    };
  }
}

export function resolveProposalDescriptionUri(
  uri: string,
  options: Pick<ProposalDescriptionFetchOptions, "ipfsGateway" | "arweaveGateway"> = {}
): string {
  assertProposalUri(uri);
  if (uri.startsWith("https://")) return uri;
  if (uri.startsWith("ipfs://")) {
    const payload = uri.slice("ipfs://".length).replace(/^ipfs\//, "");
    const gateway = (options.ipfsGateway ?? "https://ipfs.io/ipfs/").replace(/\/*$/, "/");
    return `${gateway}${payload}`;
  }
  const gateway = (options.arweaveGateway ?? "https://arweave.net/").replace(/\/*$/, "/");
  return `${gateway}${uri.slice("ar://".length)}`;
}

/** Reproduce the immutable on-chain proposal digest over canonical Borsh bytes. */
export async function computeParameterProposalDigest(
  input: ParameterProposalDigestInput
): Promise<Uint8Array> {
  assertProposalMetadata(input.metadata);
  return sha256(
    concatBytes(
      PROPOSAL_DIGEST_DOMAIN,
      address(input.programId).toBytes(),
      address(input.market).toBytes(),
      address(input.proposer).toBytes(),
      encodeU64(input.nonce, "nonce"),
      encodeU64(input.familyRevision, "familyRevision"),
      encodeParameterUpdate(input.update),
      encodeProposalMetadata(input.metadata)
    )
  );
}

export async function verifyParameterProposalDigest(
  expected: Uint8Array | number[],
  input: ParameterProposalDigestInput
): Promise<boolean> {
  return equalBytes(await computeParameterProposalDigest(input), expected);
}

export function computeDecodedParameterProposalDigest(
  programId: AddressLike,
  proposal: DecodedParameterProposalForDigest
): Promise<Uint8Array> {
  return computeParameterProposalDigest({
    programId,
    market: proposal.market,
    proposer: proposal.proposer,
    nonce: proposal.nonce,
    familyRevision: proposal.familyRevision,
    update: parameterUpdateFromAnchor(proposal.update),
    metadata: proposal.metadata,
  });
}

export async function verifyDecodedParameterProposalDigest(
  programId: AddressLike,
  proposal: DecodedParameterProposalForDigest
): Promise<boolean> {
  if (!proposal.digest) throw new Error("decoded proposal is missing its digest");
  return equalBytes(await computeDecodedParameterProposalDigest(programId, proposal), proposal.digest);
}

export function proposalSponsorshipFloor(eligibleYlp: GovernanceIntegerLike): bigint {
  const eligible = toU64BigInt(eligibleYlp, "eligibleYlp");
  if (eligible === 0n) throw new Error("eligibleYlp must be nonzero");
  return (
    eligible * BigInt(PARAMETER_PROPOSAL_SPONSOR_BPS) +
    BigInt(GOVERNANCE_BPS_DENOMINATOR - 1)
  ) / BigInt(GOVERNANCE_BPS_DENOMINATOR);
}

export function hasStrictProposalMajority(
  lockedSupport: GovernanceIntegerLike,
  eligibleYlp: GovernanceIntegerLike
): boolean {
  const locked = toU64BigInt(lockedSupport, "lockedSupport");
  const eligible = toU64BigInt(eligibleYlp, "eligibleYlp");
  return (
    locked * BigInt(GOVERNANCE_BPS_DENOMINATOR) >
    eligible * BigInt(PARAMETER_PROPOSAL_SUPPORT_BPS)
  );
}

export function minimumProposalQueueSupport(eligibleYlp: GovernanceIntegerLike): bigint {
  const eligible = toU64BigInt(eligibleYlp, "eligibleYlp");
  if (eligible === 0n) throw new Error("eligibleYlp must be nonzero");
  return eligible / 2n + 1n;
}

export function eligibleDirectYlp(input: {
  mintSupply: GovernanceIntegerLike;
  governanceLockedYlp: GovernanceIntegerLike;
  baseHlpYlpVaultAmount: GovernanceIntegerLike;
  quoteHlpYlpVaultAmount: GovernanceIntegerLike;
}): bigint {
  const totalOwnership =
    toU64BigInt(input.mintSupply, "mintSupply") +
    toU64BigInt(input.governanceLockedYlp, "governanceLockedYlp");
  if (totalOwnership > U64_MAX) throw new Error("total yLP ownership exceeded u64");
  const afterBase =
    totalOwnership - toU64BigInt(input.baseHlpYlpVaultAmount, "baseHlpYlpVaultAmount");
  if (afterBase < 0n) throw new Error("base hLP yLP vault exceeds total yLP ownership");
  const eligible =
    afterBase - toU64BigInt(input.quoteHlpYlpVaultAmount, "quoteHlpYlpVaultAmount");
  if (eligible < 0n) throw new Error("quote hLP yLP vault exceeds remaining yLP ownership");
  return eligible;
}

export function governanceIntegerBN(value: GovernanceIntegerLike, label = "value"): BN {
  return toBN(toU64BigInt(value, label));
}

function markdownBytes(markdown: ProposalMarkdown): Uint8Array {
  const bytes = typeof markdown === "string" ? new TextEncoder().encode(markdown) : markdown.slice();
  assertDescriptionLength(bytes.byteLength);
  try {
    new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    throw new Error("proposal Markdown must contain valid UTF-8 bytes");
  }
  return bytes;
}

function assertDescriptionLength(length: number): void {
  if (!Number.isInteger(length) || length < 1 || length > MAX_PROPOSAL_DESCRIPTION_BYTES) {
    throw new Error(`proposal Markdown must occupy 1-${MAX_PROPOSAL_DESCRIPTION_BYTES} bytes`);
  }
}

function objectValue(value: unknown, label: string): Record<string, unknown> {
  if (value === null || typeof value !== "object") throw new Error(`${label} must be an object`);
  return value as Record<string, unknown>;
}

function tupleField(value: unknown, label: string): unknown {
  const tuple = objectValue(value, `${label} tuple`);
  if (!("0" in tuple)) throw new Error(`${label} tuple is missing field 0`);
  return tuple["0"];
}

function integerField(value: Record<string, unknown>, field: string): GovernanceIntegerLike {
  const candidate = value[field];
  if (
    typeof candidate !== "bigint" &&
    typeof candidate !== "number" &&
    typeof candidate !== "string" &&
    (candidate === null || typeof candidate !== "object" || !("toString" in candidate))
  ) {
    throw new Error(`${field} is missing or is not integer-like`);
  }
  return candidate as GovernanceIntegerLike;
}

function numberField(value: Record<string, unknown>, field: string): number {
  const candidate = value[field];
  if (typeof candidate !== "number") throw new Error(`${field} is missing or is not a number`);
  return candidate;
}

function assertBps(value: number, label: string, max: number, min = 0): void {
  if (!Number.isInteger(value) || value < min || value > max) {
    throw new Error(`${label} must be an integer from ${min} to ${max} bps`);
  }
}

function assertRange(value: bigint, min: bigint, max: bigint, label: string): bigint {
  if (value < min || value > max) {
    throw new Error(`${label} must be between ${min} and ${max}`);
  }
  return value;
}

function toU64BigInt(value: GovernanceIntegerLike, label: string): bigint {
  if (typeof value === "number" && !Number.isSafeInteger(value)) {
    throw new Error(`${label} must be a safe integer when supplied as a number`);
  }
  let normalized: bigint;
  try {
    normalized = BigInt(value.toString());
  } catch {
    throw new Error(`${label} must be an unsigned 64-bit integer`);
  }
  if (normalized < 0n || normalized > U64_MAX) {
    throw new Error(`${label} must be an unsigned 64-bit integer`);
  }
  return normalized;
}

function toBN(value: bigint): BN {
  return new AnchorBN(value.toString());
}

function encodeParameterUpdate(update: ParameterUpdate): Uint8Array {
  assertParameterUpdate(update);
  switch (update.kind) {
    case "fee":
      return concatBytes(
        Uint8Array.of(0),
        encodeU16(update.profile.baseFeeBps),
        encodeU16(update.profile.divergenceFeeShareCapBps),
        encodeU16(update.profile.volatilityFeeShareCapBps),
        encodeU64(update.profile.divergenceFeeCoefficientNad, "divergenceFeeCoefficientNad"),
        encodeU64(update.profile.volatilityFeeCoefficientNad, "volatilityFeeCoefficientNad"),
        encodeU64(update.profile.volatilityHalfLifeMs, "volatilityHalfLifeMs"),
        encodeU64(update.profile.volatilityShockCapNad, "volatilityShockCapNad"),
        encodeU64(update.profile.volatilityAccumulatorCapNad, "volatilityAccumulatorCapNad"),
        Uint8Array.of(update.profile.swapFeeCollectMode),
        encodeU16(update.profile.compoundingFeeBps),
        encodeU16(update.profile.launchFeeStartBps),
        encodeU64(update.profile.launchFeeDurationSeconds, "launchFeeDurationSeconds"),
        Uint8Array.of(update.profile.launchFeeDecayMode),
        encodeU16(update.profile.launchMarketPriceStepBps),
        encodeU16(update.profile.launchMarketNumberOfPeriods),
        encodeU16(update.profile.launchMarketReductionFactorBps),
        Uint8Array.of(update.profile.launchRateLimitAsset),
        encodeU64(update.profile.launchRateLimitReferenceNad, "launchRateLimitReferenceNad"),
        encodeU16(update.profile.launchRateLimitIncrementBps),
        encodeU16(update.profile.launchRateLimitMaxFeeBps),
        encodeU64(
          update.profile.launchRateLimitDurationSeconds,
          "launchRateLimitDurationSeconds"
        )
      );
    case "concentration":
      return concatBytes(
        Uint8Array.of(1),
        encodeU64(update.peakAmplificationNad, "peakAmplificationNad"),
        encodeU16(update.coreHalfWidthBps),
        encodeU16(update.fadeWidthBps)
      );
    case "irm":
      return concatBytes(
        Uint8Array.of(2),
        encodeU16(update.config.targetUtilizationBps),
        encodeU64(update.config.curveSteepnessNad, "curveSteepnessNad"),
        encodeU64(update.config.adjustmentSpeedPerYear, "adjustmentSpeedPerYear")
      );
    case "emaHalfLives":
      return concatBytes(
        Uint8Array.of(3),
        encodeU64(update.priceMs, "priceMs"),
        encodeU64(update.directionalPriceMs, "directionalPriceMs"),
        encodeU64(update.curveDepthMs, "curveDepthMs"),
        encodeU64(update.centerPriceMs, "centerPriceMs")
      );
    case "dailyBorrowLimit":
      return concatBytes(Uint8Array.of(4), encodeU16(update.maxDailyBorrowBps));
    case "centerController":
      return concatBytes(
        Uint8Array.of(5),
        encodeU64(update.adjustmentThresholdNad, "adjustmentThresholdNad"),
        encodeU64(update.adjustmentStepNad, "adjustmentStepNad"),
        encodeU64(update.minAdjustmentIntervalSlots, "minAdjustmentIntervalSlots")
      );
  }
}

function encodeProposalMetadata(metadata: ProposalMetadataV1): Uint8Array {
  return concatBytes(
    Uint8Array.of(metadata.version),
    encodeBorshString(metadata.title),
    encodeBorshString(metadata.descriptionUri),
    Uint8Array.from(metadata.descriptionSha256),
    encodeU32(metadata.descriptionLen)
  );
}

function encodeBorshString(value: string): Uint8Array {
  const bytes = new TextEncoder().encode(value);
  return concatBytes(encodeU32(bytes.byteLength), bytes);
}

function encodeU16(value: number): Uint8Array {
  if (!Number.isInteger(value) || value < 0 || value > 0xffff) {
    throw new Error("value must fit in u16");
  }
  const bytes = new Uint8Array(2);
  new DataView(bytes.buffer).setUint16(0, value, true);
  return bytes;
}

function encodeU32(value: number): Uint8Array {
  if (!Number.isInteger(value) || value < 0 || value > 0xffff_ffff) {
    throw new Error("value must fit in u32");
  }
  const bytes = new Uint8Array(4);
  new DataView(bytes.buffer).setUint32(0, value, true);
  return bytes;
}

function encodeU64(value: GovernanceIntegerLike, label: string): Uint8Array {
  const normalized = toU64BigInt(value, label);
  const bytes = new Uint8Array(8);
  new DataView(bytes.buffer).setBigUint64(0, normalized, true);
  return bytes;
}

function concatBytes(...parts: readonly Uint8Array[]): Uint8Array {
  const total = parts.reduce((sum, part) => sum + part.byteLength, 0);
  const output = new Uint8Array(total);
  let offset = 0;
  for (const part of parts) {
    output.set(part, offset);
    offset += part.byteLength;
  }
  return output;
}

async function sha256(bytes: Uint8Array): Promise<Uint8Array> {
  if (globalThis.crypto?.subtle) {
    const stableBytes = Uint8Array.from(bytes);
    return new Uint8Array(await globalThis.crypto.subtle.digest("SHA-256", stableBytes));
  }
  const { createHash } = await import("node:crypto");
  return Uint8Array.from(createHash("sha256").update(bytes).digest());
}

function equalBytes(left: Uint8Array | readonly number[], right: Uint8Array | readonly number[]): boolean {
  if (left.length !== right.length) return false;
  let difference = 0;
  for (let index = 0; index < left.length; index += 1) {
    difference |= left[index]! ^ right[index]!;
  }
  return difference === 0;
}

async function readExactBody(response: Response, expectedLength: number): Promise<Uint8Array> {
  const contentRange = response.headers.get("content-range");
  if (response.status === 206) {
    const match = /^bytes\s+(\d+)-(\d+)\/(\d+|\*)$/i.exec(contentRange ?? "");
    if (!match) throw new Error("partial proposal response is missing a valid Content-Range");
    const start = Number(match[1]);
    const end = Number(match[2]);
    const total = match[3] === "*" ? null : Number(match[3]);
    if (start !== 0 || end + 1 !== expectedLength || total !== expectedLength) {
      throw new Error("proposal description response does not have the exact on-chain byte length");
    }
  } else {
    const contentLength = response.headers.get("content-length");
    const contentEncoding = response.headers.get("content-encoding");
    if (contentLength !== null && !contentEncoding && Number(contentLength) !== expectedLength) {
      throw new Error("proposal description Content-Length does not match on-chain metadata");
    }
  }

  if (!response.body) {
    const bytes = new Uint8Array(await response.arrayBuffer());
    if (bytes.byteLength !== expectedLength) {
      throw new Error("proposal description byte length does not match on-chain metadata");
    }
    return bytes;
  }

  const output = new Uint8Array(expectedLength);
  const reader = response.body.getReader();
  let offset = 0;
  while (true) {
    const { value, done } = await reader.read();
    if (done) break;
    if (!value) continue;
    if (offset + value.byteLength > expectedLength) {
      await reader.cancel();
      throw new Error("proposal description exceeds its on-chain byte length");
    }
    output.set(value, offset);
    offset += value.byteLength;
  }
  if (offset !== expectedLength) {
    throw new Error("proposal description byte length does not match on-chain metadata");
  }
  return output;
}
