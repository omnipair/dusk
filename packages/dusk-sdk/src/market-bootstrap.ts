/**
 * Creating a market is a bootstrap, not an instruction.
 *
 * `initialize_market` needs three LP mints that already exist, each carrying a
 * transfer-hook extension pointing back at the Dusk program, and each owned by
 * a market PDA that does not exist yet. That ordering is the whole reason this
 * module exists: the mints are plain Token-2022 accounts that have to be in
 * place before the program instruction that adopts them.
 *
 * Two ways to create one. `createHookedLpMintInstructions` uses a fresh
 * keypair, which must sign. `createHookedLpMintWithSeedInstructions` derives
 * the address from the creator's key and a seed, so only the creator signs;
 * that is how a browser wallet or a multisig can launch a market, and it is
 * how the `yLP`/`hLP` address suffixes production builds require are produced
 * (see `lp-vanity.ts`). Both produce instructions rather than sending anything.
 */
import {
  ExtensionType,
  TOKEN_2022_PROGRAM_ID,
  createInitializeMintInstruction,
  createInitializeTransferHookInstruction,
  getMintLen,
} from "@solana/spl-token";
import {
  Keypair,
  PublicKey,
  SystemProgram,
  type Connection,
  type TransactionInstruction,
} from "@solana/web3.js";

import { address, type AddressLike } from "./address.js";
import { sha256 } from "./hash.js";

/** Every LP mint a market needs, in the order `initialize_market` takes them. */
export const MARKET_LP_MINT_KINDS = ["ylp", "baseHlp", "quoteHlp"] as const;

export type MarketLpMintKind = (typeof MARKET_LP_MINT_KINDS)[number];

/**
 * A market's identity is its parameter hash, so two markets over the same pair
 * are distinct only if their hashes are. The namespace keeps deployments from
 * colliding: the same label and pair on devnet and mainnet hash differently.
 */
export async function deriveMarketParamsHash(params: {
  namespace: string;
  label: string;
  baseMint: AddressLike;
  quoteMint: AddressLike;
}): Promise<Uint8Array> {
  const baseMint = address(params.baseMint).toBase58();
  const quoteMint = address(params.quoteMint).toBase58();
  const preimage = `${params.namespace}:${params.label}:${baseMint}:${quoteMint}`;
  return sha256(new TextEncoder().encode(preimage));
}

/**
 * The parameters a market launches with.
 *
 * Every field is required rather than defaulted, because a market's economics
 * are not something to inherit silently from a library version — see
 * `defaultMarketLaunchConfig` for a starting point that can be overridden
 * field by field.
 */
export interface MarketLaunchConfig {
  swapFeeBps: number;
  divergenceFeeShareCapBps: number;
  volatilityFeeShareCapBps: number;
  targetHlpLeverageBps: number;
  settlementDivergenceBps: number;
  emaHalfLifeMs: bigint | number | string;
  directionalEmaHalfLifeMs: bigint | number | string;
  curveDepthEmaHalfLifeMs: bigint | number | string;
  maxDailyBorrowBps: number;
  globalHealthContributionCapBps: number;
  borrowMarketHealthFloorBps: number;
  amm: Record<string, unknown>;
  irm: Record<string, unknown>;
  startTime: bigint | number | string;
}

/**
 * The configuration the devnet markets launched with, and a reasonable
 * starting point for a new one. The zeroed AMM fields switch off the
 * launch-fee ramp, the rate limiter and the dynamic fee coefficients: a market
 * that wants any of those should set them deliberately.
 */
export function defaultMarketLaunchConfig(
  overrides: Partial<MarketLaunchConfig> = {}
): MarketLaunchConfig {
  return {
    swapFeeBps: 30,
    divergenceFeeShareCapBps: 0,
    volatilityFeeShareCapBps: 0,
    targetHlpLeverageBps: 20_000,
    settlementDivergenceBps: 500,
    emaHalfLifeMs: 60_000,
    directionalEmaHalfLifeMs: 60_000,
    curveDepthEmaHalfLifeMs: 60_000,
    maxDailyBorrowBps: 2_000,
    globalHealthContributionCapBps: 15_000,
    borrowMarketHealthFloorBps: 11_000,
    amm: {
      peakAmplificationNad: 1_000_000_000,
      coreHalfWidthBps: 0,
      fadeWidthBps: 0,
      centerEmaHalfLifeMs: 60_000,
      volatilityHalfLifeMs: 60_000,
      adjustmentThresholdNad: 0,
      adjustmentStepNad: 0,
      minAdjustmentIntervalSlots: 0,
      volatilityShockCapNad: 0,
      volatilityCapNad: 0,
      divergenceFeeCoefficientNad: 0,
      volatilityFeeCoefficientNad: 0,
      swapFeeCollectMode: 0,
      compoundingFeeBps: 0,
      launchFeeStartBps: 0,
      launchFeeDurationSeconds: 0,
      launchFeeDecayMode: 0,
      launchMarketPriceStepBps: 0,
      launchMarketNumberOfPeriods: 0,
      launchMarketReductionFactorBps: 0,
      launchRateLimitAsset: 0,
      launchRateLimitReferenceNad: 0,
      launchRateLimitIncrementBps: 0,
      launchRateLimitMaxFeeBps: 0,
      launchRateLimitDurationSeconds: 0,
      reserved: [],
    },
    irm: {
      targetUtilizationBps: 7_000,
      curveSteepnessNad: 4_000_000_000,
      adjustmentSpeedPerYear: 20,
    },
    startTime: 0,
    ...overrides,
  };
}

export interface HookedLpMint {
  /** Must sign the transaction carrying these instructions. */
  readonly keypair: Keypair;
  readonly mint: PublicKey;
  readonly instructions: readonly TransactionInstruction[];
}

/**
 * Create one transfer-hooked LP mint.
 *
 * The extension has to be initialized between allocating the account and
 * initializing the mint, and the account has to be sized for it up front —
 * a mint initialized without the space cannot gain the extension afterwards.
 * The hook authority is left unset (`PublicKey.default`) so nothing can
 * repoint it; the program is the hook and that is fixed at creation.
 */
export function createHookedLpMintInstructions(params: {
  payer: AddressLike;
  decimals: number;
  /** The market PDA, which owns every LP mint it issues. */
  mintAuthority: AddressLike;
  transferHookProgramId: AddressLike;
  /** From `getMinimumBalanceForRentExemption(lpMintLen())`. */
  lamports: number;
  /** Supply one to reproduce a known mint; otherwise a fresh key. */
  keypair?: Keypair;
}): HookedLpMint {
  const keypair = params.keypair ?? Keypair.generate();
  const payer = address(params.payer);
  return {
    keypair,
    mint: keypair.publicKey,
    instructions: [
      SystemProgram.createAccount({
        fromPubkey: payer,
        newAccountPubkey: keypair.publicKey,
        lamports: params.lamports,
        space: lpMintLen(),
        programId: TOKEN_2022_PROGRAM_ID,
      }),
      createInitializeTransferHookInstruction(
        keypair.publicKey,
        PublicKey.default,
        address(params.transferHookProgramId),
        TOKEN_2022_PROGRAM_ID
      ),
      createInitializeMintInstruction(
        keypair.publicKey,
        params.decimals,
        address(params.mintAuthority),
        null,
        TOKEN_2022_PROGRAM_ID
      ),
    ],
  };
}

export interface SeededLpMint {
  /** Signs `createAccountWithSeed`: the wallet the seed was ground for. */
  readonly base: PublicKey;
  readonly seed: string;
  readonly mint: PublicKey;
  readonly instructions: readonly TransactionInstruction[];
}

/**
 * Create one transfer-hooked LP mint at a `create_with_seed` address.
 *
 * The mint address is `sha256(base ‖ seed ‖ Token-2022 program)`, so the
 * creator can grind seeds until the address ends in the suffix
 * `initialize_market` demands in production builds, and no second keypair
 * ever exists: only `base` and the payer sign. The owner program is part of
 * the derivation, which is why a seed ground for the SPL Token program does
 * not produce the same address here.
 */
export async function createHookedLpMintWithSeedInstructions(params: {
  payer: AddressLike;
  /** The key the seed was ground against. Must sign. */
  base: AddressLike;
  seed: string;
  decimals: number;
  /** The market PDA, which owns every LP mint it issues. */
  mintAuthority: AddressLike;
  transferHookProgramId: AddressLike;
  /** From `getMinimumBalanceForRentExemption(lpMintLen())`. */
  lamports: number;
  /** The address the seed was ground for; rejected unless it derives from `base` and `seed`. */
  mint?: AddressLike;
}): Promise<SeededLpMint> {
  const base = address(params.base);
  const mint = await PublicKey.createWithSeed(base, params.seed, TOKEN_2022_PROGRAM_ID);
  if (params.mint !== undefined && !address(params.mint).equals(mint)) {
    throw new Error(
      `LP mint ${address(params.mint).toBase58()} does not derive from base ${base.toBase58()} and seed "${params.seed}" under Token-2022; got ${mint.toBase58()}`
    );
  }
  return {
    base,
    seed: params.seed,
    mint,
    instructions: [
      SystemProgram.createAccountWithSeed({
        fromPubkey: address(params.payer),
        newAccountPubkey: mint,
        basePubkey: base,
        seed: params.seed,
        lamports: params.lamports,
        space: lpMintLen(),
        programId: TOKEN_2022_PROGRAM_ID,
      }),
      createInitializeTransferHookInstruction(
        mint,
        PublicKey.default,
        address(params.transferHookProgramId),
        TOKEN_2022_PROGRAM_ID
      ),
      createInitializeMintInstruction(
        mint,
        params.decimals,
        address(params.mintAuthority),
        null,
        TOKEN_2022_PROGRAM_ID
      ),
    ],
  };
}

/** Account size of an LP mint, which is a Token-2022 mint plus the hook. */
export function lpMintLen(): number {
  return getMintLen([ExtensionType.TransferHook]);
}

/** Rent for one LP mint, to pass to `createHookedLpMintInstructions`. */
export function lpMintRent(connection: Connection): Promise<number> {
  return connection.getMinimumBalanceForRentExemption(lpMintLen());
}
