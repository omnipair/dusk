import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import http from "node:http";
import { createHash } from "node:crypto";
import { dirname, resolve } from "node:path";
import anchor from "@coral-xyz/anchor";
import BN from "bn.js";
import {
  ASSOCIATED_TOKEN_PROGRAM_ID,
  ExtensionType,
  NATIVE_MINT,
  TOKEN_2022_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  createAssociatedTokenAccountInstruction,
  createInitializeMintInstruction,
  createInitializeTransferFeeConfigInstruction,
  createInitializeTransferHookInstruction,
  createMintToCheckedInstruction,
  createTransferCheckedWithTransferHookInstruction,
  getAccount,
  getAssociatedTokenAddressSync,
  getMint,
  getMintLen,
} from "@solana/spl-token";
import {
  ComputeBudgetProgram,
  Connection,
  Keypair,
  LAMPORTS_PER_SOL,
  PublicKey,
  SystemProgram,
  SYSVAR_INSTRUCTIONS_PUBKEY,
  Transaction,
  TransactionInstruction,
} from "@solana/web3.js";
import { SCENARIO_CATALOG } from "../protocol-tests/catalog.js";

const DEFAULT_PROGRAM_ID = "JA8Zxxm4t4zopBL8e3dQQXWfQ3a5pBUPY9Sp9RnybV2X";
const LEVERAGE_DELEGATE_PROGRAM_ID = new PublicKey("AXNfmZt5e1UM4daeTzW3H7zNo4boobBcnFm8RzJYxvAv");
const DEFAULT_META_MINT = "METAwkXcqyXKy1AtsSgJ8JiUHwGCafnZL38n3vYmeta";
const DEFAULT_USDC_MINT = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const BPF_LOADER_UPGRADEABLE_ID = new PublicKey("BPFLoaderUpgradeab1e11111111111111111111111");
const TOKEN_METADATA_PROGRAM_ID = new PublicKey("metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s");
const SYSVAR_CLOCK_PUBKEY = new PublicKey("SysvarC1ock11111111111111111111111111111111");
const NAD = 1_000_000_000n;

function duskEnv(name: string): string | undefined;
function duskEnv(name: string, fallback: string): string;
function duskEnv(name: string, fallback?: string): string | undefined {
  const suffix = name.replace(/^DUSK_/, "");
  return process.env[`DUSK_${suffix}`] ?? fallback;
}

const SURFPOOL_RPC_URL = process.env.SURFPOOL_RPC_URL ?? "http://127.0.0.1:8899";
const PUBLIC_RPC_URL = normalizePublicUrl(
  process.env.PUBLIC_SURFPOOL_RPC_URL ?? process.env.SURFPOOL_RPC_PROXY_URL ?? SURFPOOL_RPC_URL
);
const PROGRAM_ID = new PublicKey(duskEnv("PROGRAM_ID", DEFAULT_PROGRAM_ID));
const DEFAULT_SOL_FUNDING = Number(process.env.FORK_DEFAULT_SOL_FUNDING ?? "10");
const DEFAULT_TOKEN_FUNDING_UI = process.env.FORK_DEFAULT_TOKEN_FUNDING ?? "10000";
const MAX_SOL_FUNDING = Number(process.env.FORK_MAX_SOL_FUNDING ?? "100");
const MAX_TOKEN_FUNDING_UI = process.env.FORK_MAX_TOKEN_FUNDING ?? "1000000";
const DEFAULT_SEED_BASE_UI = duskEnv("BASE_LIQUIDITY") ?? "100000";
const DEFAULT_SEED_QUOTE_UI = duskEnv("QUOTE_LIQUIDITY") ?? "100000";
const ALLOW_PUBLIC_FUNDING = process.env.FORK_ALLOW_PUBLIC_FUNDING !== "false";

type MarketAsset = "base" | "quote";
type YieldTokenKind = "ylp" | "hlp";
type ProtocolAuctionLane = "fee" | "buyback";
type ProtocolRevenueSource = "swap" | "interest";
type ForkMarketFixture = "mainnet" | "token2022-fees" | "mixed-decimals";

type StoredMarket = {
  label: string;
  programId: string;
  market: string;
  paramsHash: string;
  baseMint: string;
  quoteMint: string;
  baseDecimals: number;
  quoteDecimals: number;
  baseTokenProgram: string;
  quoteTokenProgram: string;
  ylpMint: string;
  baseHlpMint: string;
  quoteHlpMint: string;
  ylpTokenMetadata: string;
  baseHlpTokenMetadata: string;
  quoteHlpTokenMetadata: string;
  baseReserveVault: string;
  quoteReserveVault: string;
  baseCollateralVault: string;
  quoteCollateralVault: string;
  baseInsuranceVault: string;
  quoteInsuranceVault: string;
  baseInterestVault: string;
  quoteInterestVault: string;
  baseHlpYlpVault: string;
  quoteHlpYlpVault: string;
  eventAuthority: string;
  seededLiquidity: boolean;
  transferHookValidationAccounts: Record<string, string>;
};

type ForkState = {
  markets: Record<string, StoredMarket>;
};

type BootstrapTransactionEvidence = {
  label: string;
  signature: string;
  instructions: string[];
};

let runtime:
  | {
      payer: Keypair;
      connection: Connection;
      provider: anchor.AnchorProvider;
      program: any;
      idl: anchor.Idl;
      accountCoder: anchor.BorshAccountsCoder;
    }
  | undefined;
let runtimeError: string | null = null;
let bootstrapPromise: Promise<StoredMarket> | undefined;
let leverageDelegateProgram: any;
let bootstrapTransactionEvidence: BootstrapTransactionEvidence[] = [];

function recordBootstrapTransaction(label: string, signature: string, instructions: string[]) {
  bootstrapTransactionEvidence.push({ label, signature, instructions });
}

function stateDir(): string {
  return resolve(process.env.FORK_LAB_STATE_DIR ?? ".v2-fork-lab");
}

function statePath(): string {
  return resolve(process.env.FORK_LAB_STATE_PATH ?? `${stateDir()}/state.json`);
}

function protocolTestRunsDir(): string {
  return resolve(process.env.PROTOCOL_TEST_OUTPUT_DIR ?? ".protocol-test-lab/runs");
}

function protocolTestRunPath(runId: string): string {
  if (!/^[a-zA-Z0-9._-]+$/.test(runId)) throw new Error("Invalid protocol test run id");
  return resolve(protocolTestRunsDir(), runId, "report.json");
}

function readProtocolTestRun(path: string): any {
  if (!existsSync(path)) return null;
  return JSON.parse(readFileSync(path, "utf8"));
}

function listProtocolTestRuns(): any[] {
  const directory = protocolTestRunsDir();
  if (!existsSync(directory)) return [];
  return readdirSync(directory, { withFileTypes: true })
    .filter((entry) => entry.isDirectory())
    .map((entry) => readProtocolTestRun(protocolTestRunPath(entry.name)))
    .filter(Boolean)
    .sort((left, right) => String(right.startedAt).localeCompare(String(left.startedAt)));
}

function ensureStateDir() {
  mkdirSync(stateDir(), { recursive: true, mode: 0o700 });
}

function readState(): ForkState {
  ensureStateDir();
  if (!existsSync(statePath())) return { markets: {} };
  return JSON.parse(readFileSync(statePath(), "utf8")) as ForkState;
}

function writeState(state: ForkState) {
  ensureStateDir();
  writeFileSync(statePath(), `${JSON.stringify(state, null, 2)}\n`, { mode: 0o600 });
}

function normalizePublicUrl(value: string): string {
  if (/^https?:\/\//i.test(value)) return value.replace(/\/$/, "");
  if (value.includes("localhost") || value.includes("127.0.0.1")) return `http://${value}`;
  return `https://${value}`;
}

function loadIdl(): anchor.Idl {
  const candidates = [
    duskEnv("IDL_PATH"),
    "target/idl/dusk.json",
    "packages/dusk-sdk/src/idl_v2.json",
  ].filter(Boolean) as string[];

  for (const candidate of candidates) {
    const path = resolve(candidate);
    if (existsSync(path)) {
      const idl = JSON.parse(readFileSync(path, "utf8"));
      return { ...idl, address: PROGRAM_ID.toBase58() } as anchor.Idl;
    }
  }

  throw new Error(
    `Dusk IDL not found. Tried ${candidates.map((path) => resolve(path)).join(", ")}`
  );
}

function getLeverageDelegateProgram() {
  if (leverageDelegateProgram) return leverageDelegateProgram;
  const { provider } = initializeRuntime();
  const idl = JSON.parse(readFileSync(resolve("target/idl/leverage_delegate.json"), "utf8"));
  leverageDelegateProgram = new anchor.Program(
    { ...idl, address: LEVERAGE_DELEGATE_PROGRAM_ID.toBase58() } as anchor.Idl,
    provider
  );
  return leverageDelegateProgram;
}

function readKeypairFile(path: string): Keypair {
  return Keypair.fromSecretKey(Uint8Array.from(JSON.parse(readFileSync(path, "utf8"))));
}

function parseKeypairSecret(value: string): Keypair {
  const trimmed = value.trim();
  const json = trimmed.startsWith("[")
    ? trimmed
    : Buffer.from(trimmed, "base64").toString("utf8");
  return Keypair.fromSecretKey(Uint8Array.from(JSON.parse(json) as number[]));
}

function loadOrCreateKeypair(label: string): { keypair: Keypair; path: string; created: boolean } {
  ensureStateDir();
  const safeLabel = label.replace(/[^a-zA-Z0-9_-]/g, "-");
  const path = resolve(stateDir(), `${safeLabel}.json`);
  if (existsSync(path)) {
    return { keypair: readKeypairFile(path), path, created: false };
  }
  const keypair = Keypair.generate();
  writeFileSync(path, JSON.stringify(Array.from(keypair.secretKey)), { mode: 0o600 });
  return { keypair, path, created: true };
}

function loadPayer(): Keypair {
  const inline =
    process.env.FORK_LAB_PAYER_KEYPAIR_JSON ?? process.env.FORK_LAB_PAYER_KEYPAIR_BASE64;
  const materializedPath = resolve(
    process.env.FORK_LAB_MATERIALIZED_PAYER_PATH ?? `${stateDir()}/payer.json`
  );

  if (inline) {
    const payer = parseKeypairSecret(inline);
    mkdirSync(dirname(materializedPath), { recursive: true, mode: 0o700 });
    writeFileSync(materializedPath, JSON.stringify(Array.from(payer.secretKey)), { mode: 0o600 });
    return payer;
  }

  const keypairPath =
    process.env.FORK_LAB_PAYER_KEYPAIR ?? process.env.ANCHOR_WALLET ?? "deployer-keypair.json";
  const resolved = resolve(keypairPath);
  if (existsSync(resolved)) return readKeypairFile(resolved);

  return loadOrCreateKeypair("payer").keypair;
}

function initializeRuntime() {
  if (runtime) return runtime;

  try {
    const payer = loadPayer();
    const connection = new Connection(SURFPOOL_RPC_URL, "confirmed");
    const provider = new anchor.AnchorProvider(connection, new anchor.Wallet(payer), {
      commitment: "confirmed",
      preflightCommitment: "confirmed",
      skipPreflight: false,
    });
    const idl = loadIdl();
    const program = new anchor.Program({ ...idl, address: PROGRAM_ID.toBase58() } as any, provider);
    const accountCoder = new anchor.BorshAccountsCoder(idl);
    anchor.setProvider(provider);
    runtime = { payer, connection, provider, program, idl, accountCoder };
    runtimeError = null;
    return runtime;
  } catch (error) {
    runtimeError = error instanceof Error ? error.message : String(error);
    console.error("Dusk fork API runtime initialization failed:", runtimeError);
    throw error;
  }
}

function seed(value: string): Buffer {
  return Buffer.from(value);
}

function pda(...seeds: Buffer[]): PublicKey {
  return PublicKey.findProgramAddressSync(seeds, PROGRAM_ID)[0];
}

function tokenMetadataPda(mint: PublicKey): PublicKey {
  return PublicKey.findProgramAddressSync(
    [seed("metadata"), TOKEN_METADATA_PROGRAM_ID.toBuffer(), mint.toBuffer()],
    TOKEN_METADATA_PROGRAM_ID
  )[0];
}

function marketPda(baseMint: PublicKey, quoteMint: PublicKey, paramsHash: Buffer): PublicKey {
  return pda(seed("market_v2"), baseMint.toBuffer(), quoteMint.toBuffer(), paramsHash);
}

function deriveMarketAddresses(baseMint: PublicKey, quoteMint: PublicKey, paramsHash: Buffer) {
  const market = marketPda(baseMint, quoteMint, paramsHash);
  return {
    market,
    futarchyAuthority: pda(seed("futarchy_authority")),
    eventAuthority: pda(seed("__event_authority")),
    baseReserveVault: pda(seed("market_reserve"), market.toBuffer(), baseMint.toBuffer()),
    quoteReserveVault: pda(seed("market_reserve"), market.toBuffer(), quoteMint.toBuffer()),
    baseCollateralVault: pda(seed("market_collateral"), market.toBuffer(), baseMint.toBuffer()),
    quoteCollateralVault: pda(seed("market_collateral"), market.toBuffer(), quoteMint.toBuffer()),
    baseInsuranceVault: pda(seed("insurance"), market.toBuffer(), baseMint.toBuffer()),
    quoteInsuranceVault: pda(seed("insurance"), market.toBuffer(), quoteMint.toBuffer()),
    baseInterestVault: pda(seed("market_interest"), market.toBuffer(), baseMint.toBuffer()),
    quoteInterestVault: pda(seed("market_interest"), market.toBuffer(), quoteMint.toBuffer()),
  };
}

function deriveBorrowPosition(market: PublicKey, positionId: PublicKey): PublicKey {
  return pda(seed("borrow_position_v2"), market.toBuffer(), positionId.toBuffer());
}

function deriveLeveragePosition(market: PublicKey, positionId: PublicKey): PublicKey {
  return pda(seed("leverage_position_v2"), market.toBuffer(), positionId.toBuffer());
}

function deriveLeverageCollateralVault(market: PublicKey, collateralMint: PublicKey): PublicKey {
  return pda(seed("leverage_collateral"), market.toBuffer(), collateralMint.toBuffer());
}

function deriveLeverageDelegation(leveragePosition: PublicKey): PublicKey {
  return pda(seed("leverage_delegation_v2"), leveragePosition.toBuffer());
}

function u64Le(value: bigint): Buffer {
  const buffer = Buffer.alloc(8);
  buffer.writeBigUInt64LE(value);
  return buffer;
}

function deriveLeverageOrder(
  leveragePosition: PublicKey,
  positionOwner: PublicKey,
  orderId: bigint
): PublicKey {
  return PublicKey.findProgramAddressSync(
    [seed("leverage_order"), leveragePosition.toBuffer(), positionOwner.toBuffer(), u64Le(orderId)],
    LEVERAGE_DELEGATE_PROGRAM_ID
  )[0];
}

function deriveReferralPartner(authority: PublicKey): PublicKey {
  return pda(seed("referral_partner"), authority.toBuffer());
}

function deriveReferralAccrual(
  referralPartner: PublicKey,
  market: PublicKey,
  assetMint: PublicKey
): PublicKey {
  return pda(
    seed("referral_accrual"),
    referralPartner.toBuffer(),
    market.toBuffer(),
    assetMint.toBuffer()
  );
}

function optionalReferralAccounts(
  value: unknown,
  market: PublicKey,
  assetMint: PublicKey
): { referralPartner: PublicKey | null; referralAccrual: PublicKey | null } {
  if (value == null) return { referralPartner: null, referralAccrual: null };
  const referralPartner = value instanceof PublicKey
    ? value
    : new PublicKey(String(value));
  if (referralPartner.equals(PublicKey.default)) {
    return { referralPartner: null, referralAccrual: null };
  }
  return {
    referralPartner,
    referralAccrual: deriveReferralAccrual(referralPartner, market, assetMint),
  };
}

async function borrowPositionReferralAccounts(
  market: PublicKey,
  positionId: PublicKey,
  debtAsset: MarketAsset,
  assetMint: PublicKey
) {
  const { program } = initializeRuntime();
  const position = await program.account.borrowPosition.fetchNullable(
    deriveBorrowPosition(market, positionId)
  );
  if (!position) return { referralPartner: null, referralAccrual: null };
  return optionalReferralAccounts(
    debtAsset === "base"
      ? field(position, "baseReferralPartner", "base_referral_partner")
      : field(position, "quoteReferralPartner", "quote_referral_partner"),
    market,
    assetMint
  );
}

async function leveragePositionReferralAccounts(
  market: PublicKey,
  positionId: PublicKey,
  assetMint: PublicKey
) {
  const { program } = initializeRuntime();
  const position = await program.account.leveragePosition.fetch(
    deriveLeveragePosition(market, positionId)
  );
  return optionalReferralAccounts(
    field(position, "referralPartner", "referral_partner"),
    market,
    assetMint
  );
}

function optionalPublicKey(value: unknown): PublicKey | null {
  if (value == null || value === "") return null;
  return new PublicKey(String(value));
}

function requiredPositionId(body: Record<string, unknown>): PublicKey {
  const positionId = optionalPublicKey(body.positionId ?? body.borrowPositionId ?? body.position_id);
  if (!positionId) {
    throw new Error("positionId is required for this borrow position action");
  }
  return positionId;
}

function deriveYieldAccount(
  market: PublicKey,
  owner: PublicKey,
  lpMint: PublicKey,
  assetMint: PublicKey,
  tokenKind: YieldTokenKind
): PublicKey {
  return pda(
    seed("yield"),
    market.toBuffer(),
    owner.toBuffer(),
    lpMint.toBuffer(),
    assetMint.toBuffer(),
    Buffer.from([tokenKind === "ylp" ? 0 : 1])
  );
}

function deriveParameterProposal(
  market: PublicKey,
  proposer: PublicKey,
  nonce: bigint
): PublicKey {
  return pda(
    seed("parameter_proposal"),
    market.toBuffer(),
    proposer.toBuffer(),
    u64Le(nonce)
  );
}

function deriveProposalSupport(proposal: PublicKey, supporter: PublicKey): PublicKey {
  return pda(seed("proposal_support"), proposal.toBuffer(), supporter.toBuffer());
}

function deriveHlpYlpVault(
  market: PublicKey,
  targetHlpMint: PublicKey,
  ylpMint: PublicKey
): PublicKey {
  return pda(
    seed("hlp_ylp_vault"),
    market.toBuffer(),
    targetHlpMint.toBuffer(),
    ylpMint.toBuffer()
  );
}

function deriveProgramDataAddress(): PublicKey {
  return PublicKey.findProgramAddressSync([PROGRAM_ID.toBuffer()], BPF_LOADER_UPGRADEABLE_ID)[0];
}

function orderedMints(mintA: PublicKey, mintB: PublicKey): [PublicKey, PublicKey] {
  return Buffer.compare(mintA.toBuffer(), mintB.toBuffer()) < 0 ? [mintA, mintB] : [mintB, mintA];
}

function paramsHashForMarket(label: string, baseMint: PublicKey, quoteMint: PublicKey): Buffer {
  const override =
    duskEnv("FORK_PARAMS_HASH") ?? duskEnv("MARKET_PARAMS_HASH");
  if (override) {
    const bytes = Buffer.from(override.replace(/^0x/, ""), "hex");
    if (bytes.length !== 32) throw new Error("DUSK_FORK_PARAMS_HASH must be 32 bytes");
    return bytes;
  }
  return createHash("sha256")
    .update(`dusk-mainnet-fork:${label}:${baseMint.toBase58()}:${quoteMint.toBase58()}`)
    .digest();
}

function paramsHashForCustomMarket(label: string, baseMint: PublicKey, quoteMint: PublicKey): Buffer {
  return createHash("sha256")
    .update(`dusk-protocol-lab:${label}:${baseMint.toBase58()}:${quoteMint.toBase58()}`)
    .digest();
}

function toBN(value: bigint | number | string): BN {
  return new BN(value.toString());
}

function toBigInt(value: BN | bigint | number | string | null | undefined): bigint {
  if (value == null) return 0n;
  if (typeof value === "bigint") return value;
  if (typeof value === "number") return BigInt(value);
  if (typeof value === "string") return BigInt(value);
  return BigInt(value.toString());
}

function stringValue(value: unknown): string {
  if (value == null) return "0";
  if (typeof value === "bigint") return value.toString();
  if (typeof value === "number") return String(value);
  if (typeof value === "string") return value;
  if (value instanceof PublicKey) return value.toBase58();
  if (value instanceof BN) return value.toString();
  if (typeof value === "object" && value.constructor?.name === "BN") {
    return (value as { toString(): string }).toString();
  }
  return String(value);
}

function field<T = unknown>(obj: any, camel: string, snake?: string): T {
  if (!obj) return undefined as T;
  if (obj[camel] !== undefined) return obj[camel] as T;
  if (snake && obj[snake] !== undefined) return obj[snake] as T;
  return undefined as T;
}

function parseUnits(value: string | number | bigint | undefined, decimals: number): bigint {
  if (typeof value === "bigint") return value;
  const raw = String(value ?? "0").trim();
  if (!/^\d+(\.\d+)?$/.test(raw)) throw new Error(`Invalid decimal amount: ${raw}`);
  const [whole, fraction = ""] = raw.split(".");
  const normalizedFraction = fraction.padEnd(decimals, "0").slice(0, decimals);
  return BigInt(whole) * 10n ** BigInt(decimals) + BigInt(normalizedFraction || "0");
}

async function rpcRequest(method: string, params: unknown[]) {
  const response = await fetch(SURFPOOL_RPC_URL, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }),
  });
  const payload = (await response.json()) as { result?: unknown; error?: unknown };
  if (payload.error) throw new Error(`${method} failed: ${JSON.stringify(payload.error)}`);
  return payload.result;
}

async function timeTravel(seconds: number, slots: number) {
  if (!Number.isSafeInteger(seconds) || seconds < 0) {
    throw new Error("time travel seconds must be a nonnegative safe integer");
  }
  if (!Number.isSafeInteger(slots) || slots < 0) {
    throw new Error("time travel slots must be a nonnegative safe integer");
  }
  const { connection } = initializeRuntime();
  let absoluteTimestamp: number | null = null;
  let timestampResult: unknown = null;
  if (seconds > 0) {
    const currentSlot = await connection.getSlot("confirmed");
    const blockTime = await connection.getBlockTime(currentSlot);
    absoluteTimestamp = (blockTime ?? Math.floor(Date.now() / 1000)) * 1_000 + seconds * 1_000;
    timestampResult = await rpcRequest("surfnet_timeTravel", [{ absoluteTimestamp }]);
  }
  // Surfpool writes an epoch-relative Clock.slot during timestamp travel. Apply
  // absolute-slot travel last so programs observe the same slot returned by RPC.
  const slot = await connection.getSlot("confirmed");
  const absoluteSlot = slot + slots;
  const slotResult = slots > 0
    ? await rpcRequest("surfnet_timeTravel", [{ absoluteSlot }])
    : null;
  const clockBeforeNormalization = await connection.getAccountInfo(SYSVAR_CLOCK_PUBKEY, "confirmed");
  const clockSlotBeforeNormalization = clockBeforeNormalization
    ? clockBeforeNormalization.data.readBigUInt64LE(0).toString()
    : null;
  let normalizationSignature: string | null = null;
  if (seconds === 0 && slots > 0) {
    const { provider, payer } = initializeRuntime();
    normalizationSignature = await provider.sendAndConfirm(
      new Transaction().add(ComputeBudgetProgram.setComputeUnitLimit({ units: 200_000 })),
      [payer]
    );
  }
  const clockAfterNormalization = await connection.getAccountInfo(SYSVAR_CLOCK_PUBKEY, "confirmed");
  const clockSlotAfterNormalization = clockAfterNormalization
    ? clockAfterNormalization.data.readBigUInt64LE(0).toString()
    : null;
  return {
    seconds,
    slots,
    absoluteTimestamp,
    absoluteSlot,
    slotResult,
    timestampResult,
    clockSlotBeforeNormalization,
    clockSlotAfterNormalization,
    normalizationSignature,
  };
}

async function setLamports(pubkey: PublicKey, sol: number) {
  const { connection } = initializeRuntime();
  try {
    const signature = await connection.requestAirdrop(pubkey, sol * LAMPORTS_PER_SOL);
    await connection.confirmTransaction(signature, "confirmed");
  } catch {
    await rpcRequest("surfnet_setAccount", [
      pubkey.toBase58(),
      {
        lamports: sol * LAMPORTS_PER_SOL,
        owner: SystemProgram.programId.toBase58(),
      },
    ]);
  }
}

async function setTokenBalance(
  owner: PublicKey,
  mint: PublicKey,
  amount: bigint,
  tokenProgram: PublicKey
) {
  if (amount > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw new Error(`surfnet_setTokenAccount amount is above JSON safe integer range: ${amount}`);
  }
  if (forkMarketFixture() !== "mainnet" && tokenProgram.equals(TOKEN_2022_PROGRAM_ID)) {
    const { connection, payer, provider } = initializeRuntime();
    const tokenAccount = await createAtaIfMissing({
      payer,
      owner,
      mint,
      tokenProgram,
    });
    const current = (await getAccount(connection, tokenAccount, "confirmed", tokenProgram)).amount;
    if (current > amount) {
      throw new Error(`Fixture Token-2022 balance cannot be reduced from ${current} to ${amount}`);
    }
    const mintAmount = amount - current;
    if (mintAmount > 0n) {
      const decimals = (await getMint(connection, mint, "confirmed", tokenProgram)).decimals;
      await provider.sendAndConfirm(
        new Transaction().add(
          createMintToCheckedInstruction(
            mint,
            tokenAccount,
            payer.publicKey,
            mintAmount,
            decimals,
            [],
            tokenProgram
          )
        ),
        [payer]
      );
    }
    return;
  }
  await rpcRequest("surfnet_setTokenAccount", [
    owner.toBase58(),
    mint.toBase58(),
    {
      amount: Number(amount),
      state: "initialized",
    },
    tokenProgram.toBase58(),
  ]);
}

async function setRawAccount(params: {
  pubkey: PublicKey;
  owner: PublicKey;
  lamports: number;
  data: Buffer;
}) {
  const account = {
    lamports: params.lamports,
    owner: params.owner.toBase58(),
    executable: false,
    data: params.data.toString("hex"),
  };
  await rpcRequest("surfnet_setAccount", [params.pubkey.toBase58(), account]);
}

async function tokenProgramForMint(mint: PublicKey): Promise<PublicKey> {
  const { connection } = initializeRuntime();
  const account = await connection.getAccountInfo(mint, "confirmed");
  if (!account) throw new Error(`Mint account not found in fork: ${mint.toBase58()}`);
  return account.owner.equals(TOKEN_2022_PROGRAM_ID) ? TOKEN_2022_PROGRAM_ID : TOKEN_PROGRAM_ID;
}

async function mintDecimals(mint: PublicKey, tokenProgram?: PublicKey): Promise<number> {
  const { connection } = initializeRuntime();
  const programId = tokenProgram ?? (await tokenProgramForMint(mint));
  return (await getMint(connection, mint, "confirmed", programId)).decimals;
}

async function tokenAccountAmount(tokenAccount: PublicKey, tokenProgram: PublicKey): Promise<bigint> {
  const { connection } = initializeRuntime();
  try {
    return (await getAccount(connection, tokenAccount, "confirmed", tokenProgram)).amount;
  } catch {
    return 0n;
  }
}

async function ataInstructionIfMissing(params: {
  payer: PublicKey;
  owner: PublicKey;
  mint: PublicKey;
  tokenProgram: PublicKey;
  allowOwnerOffCurve?: boolean;
}): Promise<{ address: PublicKey; instruction?: TransactionInstruction }> {
  const { connection } = initializeRuntime();
  const address = getAssociatedTokenAddressSync(
    params.mint,
    params.owner,
    params.allowOwnerOffCurve ?? false,
    params.tokenProgram,
    ASSOCIATED_TOKEN_PROGRAM_ID
  );
  const existing = await connection.getAccountInfo(address, "confirmed");
  if (existing) return { address };
  return {
    address,
    instruction: createAssociatedTokenAccountInstruction(
      params.payer,
      address,
      params.owner,
      params.mint,
      params.tokenProgram,
      ASSOCIATED_TOKEN_PROGRAM_ID
    ),
  };
}

async function createAtaIfMissing(params: {
  payer: Keypair;
  owner: PublicKey;
  mint: PublicKey;
  tokenProgram: PublicKey;
  allowOwnerOffCurve?: boolean;
}): Promise<PublicKey> {
  const { provider } = initializeRuntime();
  const ata = await ataInstructionIfMissing({
    payer: params.payer.publicKey,
    owner: params.owner,
    mint: params.mint,
    tokenProgram: params.tokenProgram,
    allowOwnerOffCurve: params.allowOwnerOffCurve,
  });
  if (ata.instruction) {
    await provider.sendAndConfirm(new Transaction().add(ata.instruction), [params.payer]);
  }
  return ata.address;
}

function defaultMarketConfig() {
  return {
    swapFeeBps: Number(duskEnv("SWAP_FEE_BPS") ?? "30"),
    divergenceFeeShareCapBps: Number(
      duskEnv("DIVERGENCE_FEE_SHARE_CAP_BPS") ?? "0"
    ),
    volatilityFeeShareCapBps: Number(
      duskEnv("VOLATILITY_FEE_SHARE_CAP_BPS") ?? "0"
    ),
    targetHlpLeverageBps: Number(duskEnv("TARGET_HLP_LEVERAGE_BPS") ?? "20000"),
    settlementDivergenceBps: Number(duskEnv("SETTLEMENT_DIVERGENCE_BPS") ?? "500"),
    emaHalfLifeMs: toBN(duskEnv("EMA_HALF_LIFE_MS") ?? "60000"),
    directionalEmaHalfLifeMs: toBN(
      duskEnv("DIRECTIONAL_EMA_HALF_LIFE_MS") ?? "60000"
    ),
    curveDepthEmaHalfLifeMs: toBN(duskEnv("CURVE_DEPTH_EMA_HALF_LIFE_MS") ?? "60000"),
    maxDailyBorrowBps: Number(duskEnv("MAX_DAILY_BORROW_BPS") ?? "2000"),
    globalHealthContributionCapBps: Number(
      duskEnv("GLOBAL_HEALTH_CONTRIBUTION_CAP_BPS") ?? "15000"
    ),
    borrowMarketHealthFloorBps: Number(
      duskEnv("BORROW_MARKET_HEALTH_FLOOR_BPS") ?? "11000"
    ),
    amm: defaultAmmConfig(),
    irm: {
      targetUtilizationBps: Number(
        duskEnv("IRM_TARGET_UTILIZATION_BPS") ?? "7000"
      ),
      curveSteepnessNad: toBN(
        duskEnv("IRM_CURVE_STEEPNESS_NAD") ?? "4000000000"
      ),
      adjustmentSpeedPerYear: toBN(
        duskEnv("IRM_ADJUSTMENT_SPEED_PER_YEAR") ?? "20"
      ),
    },
    startTime: toBN(duskEnv("MARKET_START_TIME") ?? "0"),
  };
}

function defaultAmmConfig() {
  return {
    peakAmplificationNad: toBN(duskEnv("AMM_PEAK_AMPLIFICATION_NAD") ?? "1000000000"),
    coreHalfWidthBps: Number(duskEnv("AMM_CORE_HALF_WIDTH_BPS") ?? "0"),
    fadeWidthBps: Number(duskEnv("AMM_FADE_WIDTH_BPS") ?? "0"),
    centerEmaHalfLifeMs: toBN(duskEnv("AMM_CENTER_EMA_HALF_LIFE_MS") ?? "60000"),
    volatilityHalfLifeMs: toBN(duskEnv("AMM_VOLATILITY_HALF_LIFE_MS") ?? "60000"),
    adjustmentThresholdNad: toBN(duskEnv("AMM_ADJUSTMENT_THRESHOLD_NAD") ?? "0"),
    adjustmentStepNad: toBN(duskEnv("AMM_ADJUSTMENT_STEP_NAD") ?? "0"),
    minAdjustmentIntervalSlots: toBN(duskEnv("AMM_MIN_ADJUSTMENT_INTERVAL_SLOTS") ?? "0"),
    volatilityShockCapNad: toBN(duskEnv("AMM_VOLATILITY_SHOCK_CAP_NAD") ?? "0"),
    volatilityCapNad: toBN(duskEnv("AMM_VOLATILITY_CAP_NAD") ?? "0"),
    divergenceFeeCoefficientNad: toBN(
      duskEnv("AMM_DIVERGENCE_FEE_COEFFICIENT_NAD") ?? "0"
    ),
    volatilityFeeCoefficientNad: toBN(
      duskEnv("AMM_VOLATILITY_FEE_COEFFICIENT_NAD") ?? "0"
    ),
    swapFeeCollectMode: Number(duskEnv("AMM_SWAP_FEE_COLLECT_MODE") ?? "0"),
    compoundingFeeBps: Number(duskEnv("AMM_COMPOUNDING_FEE_BPS") ?? "0"),
    launchFeeStartBps: Number(duskEnv("AMM_LAUNCH_FEE_START_BPS") ?? "0"),
    launchFeeDurationSeconds: toBN(duskEnv("AMM_LAUNCH_FEE_DURATION_SECONDS") ?? "0"),
    launchFeeDecayMode: Number(duskEnv("AMM_LAUNCH_FEE_DECAY_MODE") ?? "0"),
    launchMarketPriceStepBps: Number(duskEnv("AMM_LAUNCH_MARKET_PRICE_STEP_BPS") ?? "0"),
    launchMarketNumberOfPeriods: Number(
      duskEnv("AMM_LAUNCH_MARKET_NUMBER_OF_PERIODS") ?? "0"
    ),
    launchMarketReductionFactorBps: Number(
      duskEnv("AMM_LAUNCH_MARKET_REDUCTION_FACTOR_BPS") ?? "0"
    ),
    launchRateLimitAsset: Number(duskEnv("AMM_LAUNCH_RATE_LIMIT_ASSET") ?? "0"),
    launchRateLimitReferenceNad: toBN(
      duskEnv("AMM_LAUNCH_RATE_LIMIT_REFERENCE_NAD") ?? "0"
    ),
    launchRateLimitIncrementBps: Number(
      duskEnv("AMM_LAUNCH_RATE_LIMIT_INCREMENT_BPS") ?? "0"
    ),
    launchRateLimitMaxFeeBps: Number(
      duskEnv("AMM_LAUNCH_RATE_LIMIT_MAX_FEE_BPS") ?? "0"
    ),
    launchRateLimitDurationSeconds: toBN(
      duskEnv("AMM_LAUNCH_RATE_LIMIT_DURATION_SECONDS") ?? "0"
    ),
    reserved: [],
  };
}

function defaultLpMetadata(kind: "ylp" | "baseHlp" | "quoteHlp") {
  const suffix =
    kind === "ylp"
      ? "YLP"
      : kind === "baseHlp"
        ? "BASE_HLP"
        : "QUOTE_HLP";
  const defaults = {
    ylp: {
      name: "Omnipair V2 (Dusk) yLP",
      symbol: "yLP",
      uri: "https://omnipair.fi/metadata/dusk/ylp.json",
    },
    baseHlp: {
      name: "Omnipair V2 (Dusk) Base hLP",
      symbol: "hLP",
      uri: "https://omnipair.fi/metadata/dusk/base-hlp.json",
    },
    quoteHlp: {
      name: "Omnipair V2 (Dusk) Quote hLP",
      symbol: "hLP",
      uri: "https://omnipair.fi/metadata/dusk/quote-hlp.json",
    },
  }[kind];
  return {
    name: duskEnv(`${suffix}_NAME`, defaults.name),
    symbol: duskEnv(`${suffix}_SYMBOL`, defaults.symbol),
    uri: duskEnv(`${suffix}_URI`, defaults.uri),
  };
}

async function ensureFutarchyAuthority(futarchyAuthority: PublicKey) {
  const { program, payer, accountCoder, connection } = initializeRuntime();
  const existing = await program.account.futarchyAuthority.fetchNullable(futarchyAuthority);
  if (existing) return existing;

  await setLamports(payer.publicKey, DEFAULT_SOL_FUNDING);

  try {
    const signature = await program.methods
      .initFutarchyAuthority({
        authority: payer.publicKey,
        swapBps: Number(duskEnv("PROTOCOL_SWAP_BPS") ?? "0"),
        interestBps: Number(duskEnv("PROTOCOL_INTEREST_BPS") ?? "0"),
        maxReferralInterestShareBps: Number(duskEnv("MAX_REFERRAL_INTEREST_SHARE_BPS") ?? "5000"),
        futarchyTreasury: payer.publicKey,
        futarchyTreasuryBps: 0,
        buybacksVault: payer.publicKey,
        buybacksVaultBps: 0,
        teamTreasury: payer.publicKey,
        teamTreasuryBps: 10_000,
        stakingVault: payer.publicKey,
        feeAuctionAcceptedMint: NATIVE_MINT,
        buybackAuctionAcceptedMint: NATIVE_MINT,
      })
      .accounts({
        deployer: payer.publicKey,
        futarchyAuthority,
        programData: deriveProgramDataAddress(),
        systemProgram: SystemProgram.programId,
      })
      .rpc();
    console.log(`Dusk futarchy authority initialized: ${signature}`);
    recordBootstrapTransaction("initialize futarchy authority", signature, ["init_futarchy_authority"]);
    return await program.account.futarchyAuthority.fetch(futarchyAuthority);
  } catch (error) {
    console.warn(
      `initFutarchyAuthority failed; seeding authority account through Surfpool: ${
        error instanceof Error ? error.message : String(error)
      }`
    );
  }

  const [, bump] = PublicKey.findProgramAddressSync([seed("futarchy_authority")], PROGRAM_ID);
  const data = await accountCoder.encode("FutarchyAuthority", {
    version: 3,
    authority: payer.publicKey,
    recipients: {
      futarchy_treasury: payer.publicKey,
      buybacks_vault: payer.publicKey,
      team_treasury: payer.publicKey,
    },
    revenue_share: {
      swap_bps: Number(duskEnv("PROTOCOL_SWAP_BPS") ?? "0"),
      interest_bps: Number(duskEnv("PROTOCOL_INTEREST_BPS") ?? "0"),
    },
    max_referral_interest_share_bps: Number(duskEnv("MAX_REFERRAL_INTEREST_SHARE_BPS") ?? "5000"),
    revenue_distribution: {
      futarchy_treasury_bps: 0,
      buybacks_vault_bps: 0,
      team_treasury_bps: 10_000,
    },
    global_reduce_only: false,
    bump,
  });
  await setRawAccount({
    pubkey: futarchyAuthority,
    owner: PROGRAM_ID,
    lamports: await connection.getMinimumBalanceForRentExemption(data.length),
    data,
  });
  return await program.account.futarchyAuthority.fetch(futarchyAuthority);
}

async function createHookedLpMintIfMissing(params: {
  label: string;
  decimals: number;
  mintAuthority: PublicKey;
}) {
  const { connection, payer } = initializeRuntime();
  const { keypair, path } = loadOrCreateKeypair(`mint-${params.label}`);
  const existing = await connection.getAccountInfo(keypair.publicKey, "confirmed");
  if (!existing) {
    await setLamports(payer.publicKey, DEFAULT_SOL_FUNDING);
    const mintLen = getMintLen([ExtensionType.TransferHook]);
    const lamports = await connection.getMinimumBalanceForRentExemption(mintLen);
    const transaction = new Transaction().add(
      SystemProgram.createAccount({
        fromPubkey: payer.publicKey,
        newAccountPubkey: keypair.publicKey,
        lamports,
        space: mintLen,
        programId: TOKEN_2022_PROGRAM_ID,
      }),
      createInitializeTransferHookInstruction(
        keypair.publicKey,
        PublicKey.default,
        PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID
      ),
      createInitializeMintInstruction(
        keypair.publicKey,
        params.decimals,
        params.mintAuthority,
        null,
        TOKEN_2022_PROGRAM_ID
      )
    );
    transaction.feePayer = payer.publicKey;
    await anchor.web3.sendAndConfirmTransaction(connection, transaction, [payer, keypair], {
      commitment: "confirmed",
    });
  }
  return {
    mint: keypair.publicKey,
    keypairPath: path,
  };
}

async function createFixtureAssetMintIfMissing(params: {
  label: string;
  decimals: number;
  tokenProgram: PublicKey;
  transferFeeBps?: number;
}) {
  const { connection, payer } = initializeRuntime();
  const { keypair } = loadOrCreateKeypair(`asset-mint-${params.label}`);
  if (await connection.getAccountInfo(keypair.publicKey, "confirmed")) return keypair.publicKey;

  await setLamports(payer.publicKey, DEFAULT_SOL_FUNDING);
  const hasTransferFee = params.tokenProgram.equals(TOKEN_2022_PROGRAM_ID) &&
    (params.transferFeeBps ?? 0) > 0;
  const mintLen = getMintLen(hasTransferFee ? [ExtensionType.TransferFeeConfig] : []);
  const lamports = await connection.getMinimumBalanceForRentExemption(mintLen);
  const transaction = new Transaction().add(
    SystemProgram.createAccount({
      fromPubkey: payer.publicKey,
      newAccountPubkey: keypair.publicKey,
      lamports,
      space: mintLen,
      programId: params.tokenProgram,
    })
  );
  if (hasTransferFee) {
    transaction.add(
      createInitializeTransferFeeConfigInstruction(
        keypair.publicKey,
        payer.publicKey,
        payer.publicKey,
        params.transferFeeBps ?? 0,
        1_000_000_000n,
        TOKEN_2022_PROGRAM_ID
      )
    );
  }
  transaction.add(
    createInitializeMintInstruction(
      keypair.publicKey,
      params.decimals,
      payer.publicKey,
      null,
      params.tokenProgram
    )
  );
  transaction.feePayer = payer.publicKey;
  await anchor.web3.sendAndConfirmTransaction(connection, transaction, [payer, keypair], {
    commitment: "confirmed",
  });
  return keypair.publicKey;
}

function forkMarketFixture(): ForkMarketFixture {
  const fixture = process.env.FORK_MARKET_FIXTURE ?? "mainnet";
  if (fixture === "mainnet" || fixture === "token2022-fees" || fixture === "mixed-decimals") {
    return fixture;
  }
  throw new Error(`Unsupported FORK_MARKET_FIXTURE: ${fixture}`);
}

async function fixtureAssetMints(fixture: ForkMarketFixture): Promise<[PublicKey, PublicKey]> {
  if (fixture === "mainnet") {
    return [
      new PublicKey(duskEnv("BASE_MINT") ?? DEFAULT_META_MINT),
      new PublicKey(duskEnv("QUOTE_MINT") ?? DEFAULT_USDC_MINT),
    ];
  }
  if (fixture === "token2022-fees") {
    return Promise.all([
      createFixtureAssetMintIfMissing({
        label: `${fixture}-base`,
        decimals: 6,
        tokenProgram: TOKEN_2022_PROGRAM_ID,
        transferFeeBps: 100,
      }),
      createFixtureAssetMintIfMissing({
        label: `${fixture}-quote`,
        decimals: 6,
        tokenProgram: TOKEN_2022_PROGRAM_ID,
        transferFeeBps: 50,
      }),
    ]);
  }
  return Promise.all([
    createFixtureAssetMintIfMissing({
      label: `${fixture}-zero`,
      decimals: 0,
      tokenProgram: TOKEN_PROGRAM_ID,
    }),
    createFixtureAssetMintIfMissing({
      label: `${fixture}-nine`,
      decimals: 9,
      tokenProgram: TOKEN_PROGRAM_ID,
    }),
  ]);
}

function deriveTransferHookValidationAddress(lpMint: PublicKey): PublicKey {
  return pda(seed("extra-account-metas"), lpMint.toBuffer());
}

async function ensureLpTransferHook(params: {
  lpMint: PublicKey;
  market: PublicKey;
}) {
  const { program, payer } = initializeRuntime();
  const validationAccount = deriveTransferHookValidationAddress(params.lpMint);
  const signature = await program.methods
    .initializeLpTransferHook()
    .accounts({
      payer: payer.publicKey,
      market: params.market,
      lpMint: params.lpMint,
      validationAccount,
      systemProgram: SystemProgram.programId,
    })
    .rpc();
  recordBootstrapTransaction("initialize LP transfer hook", signature, ["initialize_lp_transfer_hook"]);
  return validationAccount;
}

async function ensureLpMetadata(params: {
  market: PublicKey;
  lpMint: PublicKey;
  lpTokenMetadata: PublicKey;
  metadata: { name: string; symbol: string; uri: string };
}) {
  const { connection, program, payer } = initializeRuntime();
  if (await connection.getAccountInfo(params.lpTokenMetadata, "confirmed")) return;

  const signature = await program.methods
    .initializeLpMetadata(params.metadata)
    .accounts({
      payer: payer.publicKey,
      market: params.market,
      lpMint: params.lpMint,
      lpTokenMetadata: params.lpTokenMetadata,
      systemProgram: SystemProgram.programId,
      sysvarInstructions: SYSVAR_INSTRUCTIONS_PUBKEY,
      token2022Program: TOKEN_2022_PROGRAM_ID,
      tokenMetadataProgram: TOKEN_METADATA_PROGRAM_ID,
    })
    .preInstructions([ComputeBudgetProgram.setComputeUnitLimit({ units: 250_000 })])
    .rpc();
  console.log(`Dusk fork LP metadata initialized: ${signature}`);
  recordBootstrapTransaction(`initialize ${params.metadata.symbol} metadata`, signature, ["initialize_lp_metadata"]);
}

async function buildInitFutarchyAuthorityDuplicateTx() {
  const { program, payer } = initializeRuntime();
  const instruction = await program.methods
    .initFutarchyAuthority({
      authority: payer.publicKey,
      swapBps: 0,
      interestBps: 0,
      maxReferralInterestShareBps: 5_000,
      futarchyTreasury: payer.publicKey,
      futarchyTreasuryBps: 0,
      buybacksVault: payer.publicKey,
      buybacksVaultBps: 0,
      teamTreasury: payer.publicKey,
      teamTreasuryBps: 10_000,
      stakingVault: payer.publicKey,
      feeAuctionAcceptedMint: NATIVE_MINT,
      buybackAuctionAcceptedMint: NATIVE_MINT,
    })
    .accounts({
      deployer: payer.publicKey,
      futarchyAuthority: pda(seed("futarchy_authority")),
      programData: deriveProgramDataAddress(),
      systemProgram: SystemProgram.programId,
    })
    .instruction();
  return serializeBootstrapTransaction([instruction]);
}

async function buildInitializeMarketTx(params: {
  stored: StoredMarket;
  addresses: ReturnType<typeof deriveMarketAddresses>;
  paramsHash: Buffer;
  ylpMint: PublicKey;
  baseHlpMint: PublicKey;
  quoteHlpMint: PublicKey;
  config: ReturnType<typeof defaultMarketConfig>;
}) {
  const { program, payer } = initializeRuntime();
  const futarchy = await program.account.futarchyAuthority.fetch(params.addresses.futarchyAuthority);
  const teamTreasury =
    field<PublicKey>(field(futarchy, "recipients"), "teamTreasury", "team_treasury") ?? payer.publicKey;
  const teamTreasuryWsolAccount = getAssociatedTokenAddressSync(
    NATIVE_MINT,
    teamTreasury,
    true,
    TOKEN_PROGRAM_ID
  );
  const instruction = await program.methods
    .initializeMarket({
      config: params.config,
      paramsHash: Array.from(params.paramsHash),
      bootstrapPriceNad: toBN(0),
      launchFeeProgressOffset: 0,
    })
    .accounts({
      payer: payer.publicKey,
      baseMint: new PublicKey(params.stored.baseMint),
      quoteMint: new PublicKey(params.stored.quoteMint),
      market: params.addresses.market,
      futarchyAuthority: params.addresses.futarchyAuthority,
      ylpMint: params.ylpMint,
      baseHlpMint: params.baseHlpMint,
      quoteHlpMint: params.quoteHlpMint,
      baseReserveVault: params.addresses.baseReserveVault,
      quoteReserveVault: params.addresses.quoteReserveVault,
      baseCollateralVault: params.addresses.baseCollateralVault,
      quoteCollateralVault: params.addresses.quoteCollateralVault,
      baseInsuranceVault: params.addresses.baseInsuranceVault,
      quoteInsuranceVault: params.addresses.quoteInsuranceVault,
      baseInterestVault: params.addresses.baseInterestVault,
      quoteInterestVault: params.addresses.quoteInterestVault,
      teamTreasury,
      teamTreasuryWsolAccount,
      systemProgram: SystemProgram.programId,
      tokenProgram: TOKEN_PROGRAM_ID,
      token2022Program: TOKEN_2022_PROGRAM_ID,
      eventAuthority: params.addresses.eventAuthority,
      program: PROGRAM_ID,
    })
    .preInstructions([ComputeBudgetProgram.setComputeUnitLimit({ units: 600_000 })])
    .instruction();
  return serializeBootstrapTransaction([instruction]);
}

function storedMarketDefinition(params: {
  label: string;
  addresses: ReturnType<typeof deriveMarketAddresses>;
  paramsHash: Buffer;
  baseMint: PublicKey;
  quoteMint: PublicKey;
  baseDecimals: number;
  quoteDecimals: number;
  baseTokenProgram: PublicKey;
  quoteTokenProgram: PublicKey;
  ylpMint: PublicKey;
  baseHlpMint: PublicKey;
  quoteHlpMint: PublicKey;
  seededLiquidity?: boolean;
}): StoredMarket {
  return {
    label: params.label,
    programId: PROGRAM_ID.toBase58(),
    market: params.addresses.market.toBase58(),
    paramsHash: params.paramsHash.toString("hex"),
    baseMint: params.baseMint.toBase58(),
    quoteMint: params.quoteMint.toBase58(),
    baseDecimals: params.baseDecimals,
    quoteDecimals: params.quoteDecimals,
    baseTokenProgram: params.baseTokenProgram.toBase58(),
    quoteTokenProgram: params.quoteTokenProgram.toBase58(),
    ylpMint: params.ylpMint.toBase58(),
    baseHlpMint: params.baseHlpMint.toBase58(),
    quoteHlpMint: params.quoteHlpMint.toBase58(),
    ylpTokenMetadata: tokenMetadataPda(params.ylpMint).toBase58(),
    baseHlpTokenMetadata: tokenMetadataPda(params.baseHlpMint).toBase58(),
    quoteHlpTokenMetadata: tokenMetadataPda(params.quoteHlpMint).toBase58(),
    baseReserveVault: params.addresses.baseReserveVault.toBase58(),
    quoteReserveVault: params.addresses.quoteReserveVault.toBase58(),
    baseCollateralVault: params.addresses.baseCollateralVault.toBase58(),
    quoteCollateralVault: params.addresses.quoteCollateralVault.toBase58(),
    baseInsuranceVault: params.addresses.baseInsuranceVault.toBase58(),
    quoteInsuranceVault: params.addresses.quoteInsuranceVault.toBase58(),
    baseInterestVault: params.addresses.baseInterestVault.toBase58(),
    quoteInterestVault: params.addresses.quoteInterestVault.toBase58(),
    baseHlpYlpVault: deriveHlpYlpVault(params.addresses.market, params.baseHlpMint, params.ylpMint).toBase58(),
    quoteHlpYlpVault: deriveHlpYlpVault(params.addresses.market, params.quoteHlpMint, params.ylpMint).toBase58(),
    eventAuthority: params.addresses.eventAuthority.toBase58(),
    seededLiquidity: params.seededLiquidity ?? false,
    transferHookValidationAccounts: {
      ylp: deriveTransferHookValidationAddress(params.ylpMint).toBase58(),
      baseHlp: deriveTransferHookValidationAddress(params.baseHlpMint).toBase58(),
      quoteHlp: deriveTransferHookValidationAddress(params.quoteHlpMint).toBase58(),
    },
  };
}

async function prepareCreateMarketTx(params: {
  owner: PublicKey;
  label: string;
  mintA: PublicKey;
  mintB: PublicKey;
  config: Record<string, unknown>;
}) {
  const { connection, program, payer } = initializeRuntime();
  if (!params.label.trim()) throw new Error("Market name is required");
  if (params.mintA.equals(params.mintB)) throw new Error("Choose two different token mints");

  const [baseMint, quoteMint] = orderedMints(params.mintA, params.mintB);
  const paramsHash = paramsHashForCustomMarket(params.label.trim(), baseMint, quoteMint);
  const addresses = deriveMarketAddresses(baseMint, quoteMint, paramsHash);
  if (await connection.getAccountInfo(addresses.market, "confirmed")) {
    throw new Error(`Market already exists at ${addresses.market.toBase58()}`);
  }

  const [baseTokenProgram, quoteTokenProgram] = await Promise.all([
    tokenProgramForMint(baseMint),
    tokenProgramForMint(quoteMint),
  ]);
  const [baseDecimals, quoteDecimals] = await Promise.all([
    mintDecimals(baseMint, baseTokenProgram),
    mintDecimals(quoteMint, quoteTokenProgram),
  ]);
  const mintLabel = `${params.label}-${paramsHash.toString("hex").slice(0, 10)}`;
  const [ylp, baseHlp, quoteHlp] = await Promise.all([
    createHookedLpMintIfMissing({
      label: `${mintLabel}-ylp`,
      decimals: baseDecimals,
      mintAuthority: addresses.market,
    }),
    createHookedLpMintIfMissing({
      label: `${mintLabel}-base-hlp`,
      decimals: baseDecimals,
      mintAuthority: addresses.market,
    }),
    createHookedLpMintIfMissing({
      label: `${mintLabel}-quote-hlp`,
      decimals: quoteDecimals,
      mintAuthority: addresses.market,
    }),
  ]);

  const futarchy = await ensureFutarchyAuthority(addresses.futarchyAuthority);
  const teamTreasury =
    field<PublicKey>(field(futarchy, "recipients"), "teamTreasury", "team_treasury") ?? payer.publicKey;
  const teamTreasuryWsolAccount = await createAtaIfMissing({
    payer,
    owner: teamTreasury,
    mint: NATIVE_MINT,
    tokenProgram: TOKEN_PROGRAM_ID,
    allowOwnerOffCurve: true,
  });
  const config = marketConfigFromBody(params.config);
  const instruction = await program.methods
    .initializeMarket({
      config,
      paramsHash: Array.from(paramsHash),
      bootstrapPriceNad: toBN(0),
      launchFeeProgressOffset: 0,
    })
    .accounts({
      payer: params.owner,
      baseMint,
      quoteMint,
      market: addresses.market,
      futarchyAuthority: addresses.futarchyAuthority,
      ylpMint: ylp.mint,
      baseHlpMint: baseHlp.mint,
      quoteHlpMint: quoteHlp.mint,
      baseReserveVault: addresses.baseReserveVault,
      quoteReserveVault: addresses.quoteReserveVault,
      baseCollateralVault: addresses.baseCollateralVault,
      quoteCollateralVault: addresses.quoteCollateralVault,
      baseInsuranceVault: addresses.baseInsuranceVault,
      quoteInsuranceVault: addresses.quoteInsuranceVault,
      baseInterestVault: addresses.baseInterestVault,
      quoteInterestVault: addresses.quoteInterestVault,
      teamTreasury,
      teamTreasuryWsolAccount,
      systemProgram: SystemProgram.programId,
      tokenProgram: TOKEN_PROGRAM_ID,
      token2022Program: TOKEN_2022_PROGRAM_ID,
      eventAuthority: addresses.eventAuthority,
      program: PROGRAM_ID,
    })
    .instruction();
  const stored = storedMarketDefinition({
    label: params.label.trim(),
    addresses,
    paramsHash,
    baseMint,
    quoteMint,
    baseDecimals,
    quoteDecimals,
    baseTokenProgram,
    quoteTokenProgram,
    ylpMint: ylp.mint,
    baseHlpMint: baseHlp.mint,
    quoteHlpMint: quoteHlp.mint,
  });
  return {
    stored,
    config: marketConfigPayload({ config }),
    transaction: await serializeOwnerTransaction(params.owner, [instruction]),
  };
}

async function buildFinalizeMarketTx(owner: PublicKey, stored: StoredMarket) {
  const { program } = initializeRuntime();
  const instructions = await Promise.all(
    [stored.ylpMint, stored.baseHlpMint, stored.quoteHlpMint].map(async (mint) => {
      const lpMint = new PublicKey(mint);
      return program.methods
        .initializeLpTransferHook()
        .accounts({
          payer: owner,
          market: new PublicKey(stored.market),
          lpMint,
          validationAccount: deriveTransferHookValidationAddress(lpMint),
          systemProgram: SystemProgram.programId,
        })
        .instruction();
    })
  );
  return serializeOwnerTransaction(owner, instructions);
}

async function buildDuplicateMarketTx(stored: StoredMarket) {
  const paramsHash = Buffer.from(stored.paramsHash, "hex");
  const addresses = deriveMarketAddresses(
    new PublicKey(stored.baseMint),
    new PublicKey(stored.quoteMint),
    paramsHash
  );
  return buildInitializeMarketTx({
    stored,
    addresses,
    paramsHash,
    ylpMint: new PublicKey(stored.ylpMint),
    baseHlpMint: new PublicKey(stored.baseHlpMint),
    quoteHlpMint: new PublicKey(stored.quoteHlpMint),
    config: defaultMarketConfig(),
  });
}

async function buildInvalidConfigMarketTx(stored: StoredMarket) {
  const marketLabel = `${stored.label}-invalid-config-fixture`;
  const baseMint = new PublicKey(stored.baseMint);
  const quoteMint = new PublicKey(stored.quoteMint);
  const paramsHash = paramsHashForMarket(marketLabel, baseMint, quoteMint);
  const addresses = deriveMarketAddresses(baseMint, quoteMint, paramsHash);
  const [ylp, baseHlp, quoteHlp] = await Promise.all([
    createHookedLpMintIfMissing({
      label: `${marketLabel}-ylp`,
      decimals: stored.baseDecimals,
      mintAuthority: addresses.market,
    }),
    createHookedLpMintIfMissing({
      label: `${marketLabel}-base-hlp`,
      decimals: stored.baseDecimals,
      mintAuthority: addresses.market,
    }),
    createHookedLpMintIfMissing({
      label: `${marketLabel}-quote-hlp`,
      decimals: stored.quoteDecimals,
      mintAuthority: addresses.market,
    }),
  ]);
  return buildInitializeMarketTx({
    stored,
    addresses,
    paramsHash,
    ylpMint: ylp.mint,
    baseHlpMint: baseHlp.mint,
    quoteHlpMint: quoteHlp.mint,
    config: { ...defaultMarketConfig(), swapFeeBps: 10_001 },
  });
}

async function buildInitializeLpMetadataTx(params: {
  stored: StoredMarket;
  lpMint: PublicKey;
  metadata: { name: string; symbol: string; uri: string };
}) {
  const { program, payer } = initializeRuntime();
  const instruction = await program.methods
    .initializeLpMetadata(params.metadata)
    .accounts({
      payer: payer.publicKey,
      market: new PublicKey(params.stored.market),
      lpMint: params.lpMint,
      lpTokenMetadata: tokenMetadataPda(params.lpMint),
      systemProgram: SystemProgram.programId,
      sysvarInstructions: SYSVAR_INSTRUCTIONS_PUBKEY,
      token2022Program: TOKEN_2022_PROGRAM_ID,
      tokenMetadataProgram: TOKEN_METADATA_PROGRAM_ID,
    })
    .preInstructions([ComputeBudgetProgram.setComputeUnitLimit({ units: 250_000 })])
    .instruction();
  return serializeBootstrapTransaction([instruction]);
}

async function bootstrap(): Promise<StoredMarket> {
  initializeRuntime();
  bootstrapPromise ??= bootstrapUncached().catch((error) => {
    bootstrapPromise = undefined;
    throw error;
  });
  return bootstrapPromise;
}

async function bootstrapUncached(): Promise<StoredMarket> {
  bootstrapTransactionEvidence = [];
  const { connection, payer, program } = initializeRuntime();
  const state = readState();
  const fixture = forkMarketFixture();
  const marketLabel = duskEnv("MARKET_LABEL") ??
    (fixture === "mainnet" ? "meta-usdc-mainnet-fork" : `dusk-${fixture}-fixture`);

  await setLamports(payer.publicKey, DEFAULT_SOL_FUNDING);
  const [defaultBase, defaultQuote] = await fixtureAssetMints(fixture);
  const [baseMint, quoteMint] = orderedMints(defaultBase, defaultQuote);
  const paramsHash = paramsHashForMarket(marketLabel, baseMint, quoteMint);
  const addresses = deriveMarketAddresses(baseMint, quoteMint, paramsHash);

  const [baseTokenProgram, quoteTokenProgram] = await Promise.all([
    tokenProgramForMint(baseMint),
    tokenProgramForMint(quoteMint),
  ]);
  const [baseDecimals, quoteDecimals] = await Promise.all([
    mintDecimals(baseMint, baseTokenProgram),
    mintDecimals(quoteMint, quoteTokenProgram),
  ]);

  const futarchy = await ensureFutarchyAuthority(addresses.futarchyAuthority);
  const teamTreasury =
    field<PublicKey>(field(futarchy, "recipients"), "teamTreasury", "team_treasury") ??
    payer.publicKey;
  const teamTreasuryWsolAccount = await createAtaIfMissing({
    payer,
    owner: teamTreasury,
    mint: NATIVE_MINT,
    tokenProgram: TOKEN_PROGRAM_ID,
    allowOwnerOffCurve: true,
  });

  const lpLabels = {
    ylp: `${marketLabel}-ylp`,
    baseHlp: `${marketLabel}-base-hlp`,
    quoteHlp: `${marketLabel}-quote-hlp`,
  };

  const existingMarketAccount = await program.account.market.fetchNullable(addresses.market);
  let ylpMint = field<PublicKey>(existingMarketAccount, "ylpMint", "ylp_mint");
  const existingBaseSide = field<any>(existingMarketAccount, "baseSide", "base_side");
  const existingQuoteSide = field<any>(existingMarketAccount, "quoteSide", "quote_side");
  let baseHlpMint = field<PublicKey>(existingBaseSide, "hlpMint", "hlp_mint");
  let quoteHlpMint = field<PublicKey>(existingQuoteSide, "hlpMint", "hlp_mint");

  if (!existingMarketAccount) {
    const [ylp, baseHlp, quoteHlp] = await Promise.all([
      createHookedLpMintIfMissing({
        label: lpLabels.ylp,
        decimals: baseDecimals,
        mintAuthority: addresses.market,
      }),
      createHookedLpMintIfMissing({
        label: lpLabels.baseHlp,
        decimals: baseDecimals,
        mintAuthority: addresses.market,
      }),
      createHookedLpMintIfMissing({
        label: lpLabels.quoteHlp,
        decimals: quoteDecimals,
        mintAuthority: addresses.market,
      }),
    ]);
    ylpMint = ylp.mint;
    baseHlpMint = baseHlp.mint;
    quoteHlpMint = quoteHlp.mint;
  }

  if (!ylpMint || !baseHlpMint || !quoteHlpMint) {
    throw new Error(`Unable to resolve Dusk LP mints for market ${addresses.market.toBase58()}`);
  }

  const ylpTokenMetadata = tokenMetadataPda(ylpMint);
  const baseHlpTokenMetadata = tokenMetadataPda(baseHlpMint);
  const quoteHlpTokenMetadata = tokenMetadataPda(quoteHlpMint);
  const baseHlpYlpVault = deriveHlpYlpVault(addresses.market, baseHlpMint, ylpMint);
  const quoteHlpYlpVault = deriveHlpYlpVault(addresses.market, quoteHlpMint, ylpMint);

  if (!existingMarketAccount) {
    const signature = await program.methods
      .initializeMarket({
        config: defaultMarketConfig(),
        paramsHash: Array.from(paramsHash),
        bootstrapPriceNad: toBN(0),
        launchFeeProgressOffset: 0,
      })
      .accounts({
        payer: payer.publicKey,
        baseMint,
        quoteMint,
        market: addresses.market,
        futarchyAuthority: addresses.futarchyAuthority,
        ylpMint,
        baseHlpMint,
        quoteHlpMint,
        baseReserveVault: addresses.baseReserveVault,
        quoteReserveVault: addresses.quoteReserveVault,
        baseCollateralVault: addresses.baseCollateralVault,
        quoteCollateralVault: addresses.quoteCollateralVault,
        baseInsuranceVault: addresses.baseInsuranceVault,
        quoteInsuranceVault: addresses.quoteInsuranceVault,
        baseInterestVault: addresses.baseInterestVault,
        quoteInterestVault: addresses.quoteInterestVault,
        teamTreasury,
        teamTreasuryWsolAccount,
        systemProgram: SystemProgram.programId,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: addresses.eventAuthority,
        program: PROGRAM_ID,
      })
      .preInstructions([ComputeBudgetProgram.setComputeUnitLimit({ units: 600_000 })])
      .rpc();
    console.log(`Dusk fork market initialized: ${signature}`);
    recordBootstrapTransaction("initialize market", signature, ["initialize_market"]);
  }

  const [ylpHookValidation, baseHlpHookValidation, quoteHlpHookValidation] = await Promise.all([
    ensureLpTransferHook({ market: addresses.market, lpMint: ylpMint }),
    ensureLpTransferHook({ market: addresses.market, lpMint: baseHlpMint }),
    ensureLpTransferHook({ market: addresses.market, lpMint: quoteHlpMint }),
  ]);
  const transferHookValidationAccounts = {
    ylp: ylpHookValidation.toBase58(),
    baseHlp: baseHlpHookValidation.toBase58(),
    quoteHlp: quoteHlpHookValidation.toBase58(),
  };

  await ensureLpMetadata({
    market: addresses.market,
    lpMint: ylpMint,
    lpTokenMetadata: ylpTokenMetadata,
    metadata: defaultLpMetadata("ylp"),
  });
  await ensureLpMetadata({
    market: addresses.market,
    lpMint: baseHlpMint,
    lpTokenMetadata: baseHlpTokenMetadata,
    metadata: defaultLpMetadata("baseHlp"),
  });
  await ensureLpMetadata({
    market: addresses.market,
    lpMint: quoteHlpMint,
    lpTokenMetadata: quoteHlpTokenMetadata,
    metadata: defaultLpMetadata("quoteHlp"),
  });

  const previous = state.markets[marketLabel];
  const stored: StoredMarket = {
    label: marketLabel,
    programId: PROGRAM_ID.toBase58(),
    market: addresses.market.toBase58(),
    paramsHash: paramsHash.toString("hex"),
    baseMint: baseMint.toBase58(),
    quoteMint: quoteMint.toBase58(),
    baseDecimals,
    quoteDecimals,
    baseTokenProgram: baseTokenProgram.toBase58(),
    quoteTokenProgram: quoteTokenProgram.toBase58(),
    ylpMint: ylpMint.toBase58(),
    baseHlpMint: baseHlpMint.toBase58(),
    quoteHlpMint: quoteHlpMint.toBase58(),
    ylpTokenMetadata: ylpTokenMetadata.toBase58(),
    baseHlpTokenMetadata: baseHlpTokenMetadata.toBase58(),
    quoteHlpTokenMetadata: quoteHlpTokenMetadata.toBase58(),
    baseReserveVault: addresses.baseReserveVault.toBase58(),
    quoteReserveVault: addresses.quoteReserveVault.toBase58(),
    baseCollateralVault: addresses.baseCollateralVault.toBase58(),
    quoteCollateralVault: addresses.quoteCollateralVault.toBase58(),
    baseInsuranceVault: addresses.baseInsuranceVault.toBase58(),
    quoteInsuranceVault: addresses.quoteInsuranceVault.toBase58(),
    baseInterestVault: addresses.baseInterestVault.toBase58(),
    quoteInterestVault: addresses.quoteInterestVault.toBase58(),
    baseHlpYlpVault: baseHlpYlpVault.toBase58(),
    quoteHlpYlpVault: quoteHlpYlpVault.toBase58(),
    eventAuthority: addresses.eventAuthority.toBase58(),
    seededLiquidity:
      Boolean(existingMarketAccount) &&
      previous?.market === addresses.market.toBase58() &&
      previous.seededLiquidity,
    transferHookValidationAccounts,
  };

  state.markets[marketLabel] = stored;
  writeState(state);

  if (duskEnv("SEED_LIQUIDITY") !== "0" && !stored.seededLiquidity) {
    await seedInitialLiquidity(stored);
    stored.seededLiquidity = true;
    state.markets[marketLabel] = stored;
    writeState(state);
  }

  return stored;
}

async function seedInitialLiquidity(market: StoredMarket) {
  const { provider, payer } = initializeRuntime();
  const baseMint = new PublicKey(market.baseMint);
  const quoteMint = new PublicKey(market.quoteMint);
  const baseAmount = parseUnits(DEFAULT_SEED_BASE_UI, market.baseDecimals);
  const quoteAmount = parseUnits(DEFAULT_SEED_QUOTE_UI, market.quoteDecimals);
  const baseProgram = new PublicKey(market.baseTokenProgram);
  const quoteProgram = new PublicKey(market.quoteTokenProgram);

  await setTokenBalance(payer.publicKey, baseMint, baseAmount, baseProgram);
  await setTokenBalance(payer.publicKey, quoteMint, quoteAmount, quoteProgram);
  const tx = await buildAddLiquidityTx({
    owner: payer.publicKey,
    market,
    baseDepositAmount: baseAmount,
    quoteDepositAmount: quoteAmount,
    minYlpAmount: 0n,
    payerCanSign: true,
  });
  tx.sign(payer);
  const signature = await provider.connection.sendRawTransaction(tx.serialize());
  await provider.connection.confirmTransaction(signature, "confirmed");
  console.log(`Dusk fork market seeded with initial liquidity: ${signature}`);
}

function marketConfigPayload(marketAccount: any) {
  const config = field<any>(marketAccount, "config");
  const amm = field<any>(config, "amm") ?? defaultAmmConfig();
  const irm = field<any>(config, "irm") ?? defaultMarketConfig().irm;
  return {
    targetHlpLeverageBps: Number(field(config, "targetHlpLeverageBps", "target_hlp_leverage_bps") ?? 0),
    swapFeeBps: Number(field(config, "swapFeeBps", "swap_fee_bps") ?? 0),
    divergenceFeeShareCapBps: Number(
      field(config, "divergenceFeeShareCapBps", "divergence_fee_share_cap_bps") ?? 0
    ),
    volatilityFeeShareCapBps: Number(
      field(config, "volatilityFeeShareCapBps", "volatility_fee_share_cap_bps") ?? 0
    ),
    settlementDivergenceBps: Number(
      field(config, "settlementDivergenceBps", "settlement_divergence_bps") ?? 0
    ),
    emaHalfLifeMs: stringValue(field(config, "emaHalfLifeMs", "ema_half_life_ms")),
    directionalEmaHalfLifeMs: stringValue(
      field(config, "directionalEmaHalfLifeMs", "directional_ema_half_life_ms")
    ),
    curveDepthEmaHalfLifeMs: stringValue(
      field(config, "curveDepthEmaHalfLifeMs", "curve_depth_ema_half_life_ms")
    ),
    maxDailyBorrowBps: Number(field(config, "maxDailyBorrowBps", "max_daily_borrow_bps") ?? 0),
    globalHealthContributionCapBps: Number(
      field(config, "globalHealthContributionCapBps", "global_health_contribution_cap_bps") ?? 0
    ),
    borrowMarketHealthFloorBps: Number(
      field(config, "borrowMarketHealthFloorBps", "borrow_market_health_floor_bps") ?? 0
    ),
    amm: {
      peakAmplificationNad: stringValue(
        field(amm, "peakAmplificationNad", "peak_amplification_nad")
      ),
      coreHalfWidthBps: Number(field(amm, "coreHalfWidthBps", "core_half_width_bps") ?? 0),
      fadeWidthBps: Number(field(amm, "fadeWidthBps", "fade_width_bps") ?? 0),
      centerEmaHalfLifeMs: stringValue(field(amm, "centerEmaHalfLifeMs", "center_ema_half_life_ms")),
      volatilityHalfLifeMs: stringValue(
        field(amm, "volatilityHalfLifeMs", "volatility_half_life_ms")
      ),
      adjustmentThresholdNad: stringValue(
        field(amm, "adjustmentThresholdNad", "adjustment_threshold_nad")
      ),
      adjustmentStepNad: stringValue(field(amm, "adjustmentStepNad", "adjustment_step_nad")),
      minAdjustmentIntervalSlots: stringValue(
        field(amm, "minAdjustmentIntervalSlots", "min_adjustment_interval_slots")
      ),
      volatilityShockCapNad: stringValue(
        field(amm, "volatilityShockCapNad", "volatility_shock_cap_nad")
      ),
      volatilityCapNad: stringValue(field(amm, "volatilityCapNad", "volatility_cap_nad")),
      divergenceFeeCoefficientNad: stringValue(
        field(amm, "divergenceFeeCoefficientNad", "divergence_fee_coefficient_nad")
      ),
      volatilityFeeCoefficientNad: stringValue(
        field(amm, "volatilityFeeCoefficientNad", "volatility_fee_coefficient_nad")
      ),
      reserved: Array.from(field<number[]>(amm, "reserved") ?? []),
    },
    irm: {
      targetUtilizationBps: Number(
        field(irm, "targetUtilizationBps", "target_utilization_bps") ?? 0
      ),
      curveSteepnessNad: stringValue(
        field(irm, "curveSteepnessNad", "curve_steepness_nad")
      ),
      adjustmentSpeedPerYear: stringValue(
        field(irm, "adjustmentSpeedPerYear", "adjustment_speed_per_year")
      ),
    },
    startTime: stringValue(field(config, "startTime", "start_time")),
  };
}

function protocolAuctionPayload(auction: any) {
  const recipients = field<any>(auction, "recipients");
  const params = field<any>(auction, "params");
  return {
    acceptedMint: stringValue(field(auction, "acceptedMint", "accepted_mint")),
    recipients: {
      treasury: stringValue(field(recipients, "treasury")),
      stakingVault: stringValue(field(recipients, "stakingVault", "staking_vault")),
      treasuryBps: Number(field(recipients, "treasuryBps", "treasury_bps") ?? 0),
      stakingVaultBps: Number(field(recipients, "stakingVaultBps", "staking_vault_bps") ?? 0),
    },
    params: {
      startMultiplierBps: Number(field(params, "startMultiplierBps", "start_multiplier_bps") ?? 0),
      floorMultiplierBps: Number(field(params, "floorMultiplierBps", "floor_multiplier_bps") ?? 0),
      durationSlots: stringValue(field(params, "durationSlots", "duration_slots")),
      maxReferenceAgeSlots: stringValue(
        field(params, "maxReferenceAgeSlots", "max_reference_age_slots")
      ),
    },
  };
}

async function futarchyPayload() {
  const { program } = initializeRuntime();
  const address = pda(seed("futarchy_authority"));
  const account = await program.account.futarchyAuthority.fetch(address);
  const recipients = field<any>(account, "recipients");
  const revenueShare = field<any>(account, "revenueShare", "revenue_share");
  const distribution = field<any>(account, "revenueDistribution", "revenue_distribution");
  const auctionSplit = field<any>(account, "protocolAuctionSplit", "protocol_auction_split");
  return {
    address: address.toBase58(),
    version: Number(field(account, "version") ?? 0),
    authority: stringValue(field(account, "authority")),
    globalReduceOnly: Boolean(field(account, "globalReduceOnly", "global_reduce_only") ?? false),
    maxReferralInterestShareBps: Number(
      field(account, "maxReferralInterestShareBps", "max_referral_interest_share_bps") ?? 0
    ),
    revenueShare: {
      swapBps: Number(field(revenueShare, "swapBps", "swap_bps") ?? 0),
      interestBps: Number(field(revenueShare, "interestBps", "interest_bps") ?? 0),
    },
    recipients: {
      futarchyTreasury: stringValue(field(recipients, "futarchyTreasury", "futarchy_treasury")),
      buybacksVault: stringValue(field(recipients, "buybacksVault", "buybacks_vault")),
      teamTreasury: stringValue(field(recipients, "teamTreasury", "team_treasury")),
    },
    revenueDistribution: {
      futarchyTreasuryBps: Number(
        field(distribution, "futarchyTreasuryBps", "futarchy_treasury_bps") ?? 0
      ),
      buybacksVaultBps: Number(field(distribution, "buybacksVaultBps", "buybacks_vault_bps") ?? 0),
      teamTreasuryBps: Number(field(distribution, "teamTreasuryBps", "team_treasury_bps") ?? 0),
    },
    protocolAuctionSplit: {
      feeAuctionBps: Number(field(auctionSplit, "feeAuctionBps", "fee_auction_bps") ?? 0),
      buybackAuctionBps: Number(field(auctionSplit, "buybackAuctionBps", "buyback_auction_bps") ?? 0),
    },
    feeAuction: protocolAuctionPayload(field(account, "feeAuction", "fee_auction")),
    buybackAuction: protocolAuctionPayload(field(account, "buybackAuction", "buyback_auction")),
  };
}

async function yieldAccountPayload(
  stored: StoredMarket,
  owner: PublicKey,
  lpMint: PublicKey,
  asset: MarketAsset,
  tokenKind: YieldTokenKind
) {
  const { program } = initializeRuntime();
  const m = marketFromStored(stored);
  const assetMint = asset === "base" ? m.baseMint : m.quoteMint;
  const address = deriveYieldAccount(m.market, owner, lpMint, assetMint, tokenKind);
  const account = await program.account.yieldAccount.fetchNullable(address);
  if (!account) return null;
  return {
    address: address.toBase58(),
    owner: stringValue(field(account, "owner")),
    market: stringValue(field(account, "market")),
    assetMint: stringValue(field(account, "assetMint", "asset_mint")),
    tokenKind: Number(field(account, "tokenKind", "token_kind")),
    recipient: stringValue(field(account, "recipient")),
    swapFeeCheckpointQ64: stringValue(
      field(account, "swapFeeCheckpointQ64", "swap_fee_checkpoint_q64")
    ),
    interestCheckpointQ64: stringValue(
      field(account, "interestCheckpointQ64", "interest_checkpoint_q64")
    ),
    accruedSwapFeeAmount: stringValue(
      field(account, "accruedSwapFeeAmount", "accrued_swap_fee_amount")
    ),
    accruedInterestAmount: stringValue(
      field(account, "accruedInterestAmount", "accrued_interest_amount")
    ),
    bump: Number(field(account, "bump")),
  };
}

async function currentMarketHealth(market: PublicKey) {
  const { connection, payer, program } = initializeRuntime();
  const instruction = await program.methods.previewMarket().accounts({ market }).instruction();
  const transaction = new Transaction().add(instruction);
  transaction.feePayer = payer.publicKey;
  transaction.recentBlockhash = (await connection.getLatestBlockhash("confirmed")).blockhash;
  const simulation = await connection.simulateTransaction(transaction);
  if (simulation.value.err) {
    throw new Error(`preview_market health simulation failed: ${JSON.stringify(simulation.value.err)}`);
  }
  const returnData = simulation.value.returnData;
  if (!returnData || returnData.programId !== program.programId.toBase58()) {
    throw new Error("preview_market health simulation returned no Dusk data");
  }
  const preview = program.coder.types.decode(
    "MarketPreview",
    Buffer.from(returnData.data[0], returnData.data[1])
  );
  return field<any>(preview, "health");
}

async function marketPayload(stored: StoredMarket) {
  const { program } = initializeRuntime();
  const marketAccount = await program.account.market.fetch(new PublicKey(stored.market));
  const config = marketConfigPayload(marketAccount);
  const baseSide = field<any>(marketAccount, "baseSide", "base_side");
  const quoteSide = field<any>(marketAccount, "quoteSide", "quote_side");
  const baseReserves = field<any>(baseSide, "reserves");
  const quoteReserves = field<any>(quoteSide, "reserves");
  const baseFees = field<any>(baseSide, "fees");
  const quoteFees = field<any>(quoteSide, "fees");
  const baseDailyBorrowBucket = field<any>(baseSide, "dailyBorrowBucket", "daily_borrow_bucket");
  const quoteDailyBorrowBucket = field<any>(quoteSide, "dailyBorrowBucket", "daily_borrow_bucket");
  const debt = field<any>(marketAccount, "debt");
  const health = await currentMarketHealth(new PublicKey(stored.market));
  const insurance = field<any>(marketAccount, "insurance");
  const fixedBaseShares = toBigInt(field(debt, "fixedBaseShares", "fixed_base_shares"));
  const fixedQuoteShares = toBigInt(field(debt, "fixedQuoteShares", "fixed_quote_shares"));
  const baseBorrowIndexNad = toBigInt(field(debt, "baseBorrowIndexNad", "base_borrow_index_nad"));
  const quoteBorrowIndexNad = toBigInt(field(debt, "quoteBorrowIndexNad", "quote_borrow_index_nad"));
  const now = new Date().toISOString();

  return {
    marketAddress: stored.market,
    baseMint: stored.baseMint,
    quoteMint: stored.quoteMint,
    baseDecimals: stored.baseDecimals,
    quoteDecimals: stored.quoteDecimals,
    ylpMint: stored.ylpMint,
    baseHlpMint: stored.baseHlpMint,
    quoteHlpMint: stored.quoteHlpMint,
    baseReserveVault: stored.baseReserveVault,
    quoteReserveVault: stored.quoteReserveVault,
    baseCollateralVault: stored.baseCollateralVault,
    quoteCollateralVault: stored.quoteCollateralVault,
    baseInsuranceVault: stored.baseInsuranceVault,
    quoteInsuranceVault: stored.quoteInsuranceVault,
    baseInterestVault: stored.baseInterestVault,
    quoteInterestVault: stored.quoteInterestVault,
    baseHlpYlpVault: stored.baseHlpYlpVault,
    quoteHlpYlpVault: stored.quoteHlpYlpVault,
    targetHlpLeverageBps: config.targetHlpLeverageBps,
    swapFeeBps: config.swapFeeBps,
    config,
    governanceLockedYlp: stringValue(
      field(marketAccount, "governanceLockedYlp", "governance_locked_ylp")
    ),
    parameterRevisions: Array.from(
      field<Array<BN | bigint | number | string>>(
        marketAccount,
        "parameterRevisions",
        "parameter_revisions"
      ) ?? []
    ).map(stringValue),
    paramsHash: stored.paramsHash,
    version: Number(field(marketAccount, "version") ?? 1),
    reduceOnly: Boolean(field(marketAccount, "reduceOnly", "reduce_only") ?? false),
    createdTxSig: null,
    createdSlot: null,
    createdAt: now,
    updatedAt: now,
    swapCount: 0,
    lastSwapAt: null,
    state: {
      baseLiveReserve: stringValue(field(baseReserves, "liveReserve", "live_reserve")),
      quoteLiveReserve: stringValue(field(quoteReserves, "liveReserve", "live_reserve")),
      baseCashReserve: stringValue(field(baseReserves, "cashReserve", "cash_reserve")),
      quoteCashReserve: stringValue(field(quoteReserves, "cashReserve", "cash_reserve")),
      baseSideYlpSupply: stringValue(field(field(baseSide, "shares"), "ylpSupply", "ylp_supply")),
      quoteSideYlpSupply: stringValue(field(field(quoteSide, "shares"), "ylpSupply", "ylp_supply")),
      fixedBaseShares: fixedBaseShares.toString(),
      fixedQuoteShares: fixedQuoteShares.toString(),
      fixedBaseDebt: ((fixedBaseShares * baseBorrowIndexNad) / NAD).toString(),
      fixedQuoteDebt: ((fixedQuoteShares * quoteBorrowIndexNad) / NAD).toString(),
      fixedBasePrincipal: stringValue(field(debt, "fixedBasePrincipal", "fixed_base_principal")),
      fixedQuotePrincipal: stringValue(field(debt, "fixedQuotePrincipal", "fixed_quote_principal")),
      baseBorrowIndexNad: baseBorrowIndexNad.toString(),
      quoteBorrowIndexNad: quoteBorrowIndexNad.toString(),
      isolatedBaseShares: stringValue(field(debt, "isolatedBaseShares", "isolated_base_shares")),
      isolatedQuoteShares: stringValue(field(debt, "isolatedQuoteShares", "isolated_quote_shares")),
      isolatedBasePrincipal: stringValue(field(debt, "isolatedBasePrincipal", "isolated_base_principal")),
      isolatedQuotePrincipal: stringValue(field(debt, "isolatedQuotePrincipal", "isolated_quote_principal")),
      baseInsuranceAvailable: stringValue(field(insurance, "baseAvailable", "base_available")),
      quoteInsuranceAvailable: stringValue(field(insurance, "quoteAvailable", "quote_available")),
      baseSwapFeeCustodyBalance: stringValue(
        field(baseFees, "swapFeeCustodyBalance", "swap_fee_custody_balance")
      ),
      quoteSwapFeeCustodyBalance: stringValue(
        field(quoteFees, "swapFeeCustodyBalance", "swap_fee_custody_balance")
      ),
      baseSwapProtocolFeeLiability: stringValue(
        field(baseFees, "swapProtocolFeeLiability", "swap_protocol_fee_liability")
      ),
      quoteSwapProtocolFeeLiability: stringValue(
        field(quoteFees, "swapProtocolFeeLiability", "swap_protocol_fee_liability")
      ),
      baseInterestProtocolFeeLiability: stringValue(
        field(baseFees, "interestProtocolFeeLiability", "interest_protocol_fee_liability")
      ),
      quoteInterestProtocolFeeLiability: stringValue(
        field(quoteFees, "interestProtocolFeeLiability", "interest_protocol_fee_liability")
      ),
      baseSwapBuybackFeeLiability: stringValue(
        field(baseFees, "swapBuybackFeeLiability", "swap_buyback_fee_liability")
      ),
      quoteSwapBuybackFeeLiability: stringValue(
        field(quoteFees, "swapBuybackFeeLiability", "swap_buyback_fee_liability")
      ),
      baseInterestBuybackFeeLiability: stringValue(
        field(baseFees, "interestBuybackFeeLiability", "interest_buyback_fee_liability")
      ),
      quoteInterestBuybackFeeLiability: stringValue(
        field(quoteFees, "interestBuybackFeeLiability", "interest_buyback_fee_liability")
      ),
      baseLpSwapFeeLiability: stringValue(field(baseFees, "swapFeeLiability", "swap_fee_liability")),
      quoteLpSwapFeeLiability: stringValue(field(quoteFees, "swapFeeLiability", "swap_fee_liability")),
      baseLpInterestFeeLiability: stringValue(field(baseFees, "interestLiability", "interest_liability")),
      quoteLpInterestFeeLiability: stringValue(field(quoteFees, "interestLiability", "interest_liability")),
      baseUnallocatedSwapFeeLiability: stringValue(
        field(baseFees, "unallocatedSwapFeeLiability", "unallocated_swap_fee_liability")
      ),
      quoteUnallocatedSwapFeeLiability: stringValue(
        field(quoteFees, "unallocatedSwapFeeLiability", "unallocated_swap_fee_liability")
      ),
      baseDailyBorrowedBucket: stringValue(
        field(baseDailyBorrowBucket, "borrowedBucket", "borrowed_bucket")
      ),
      quoteDailyBorrowedBucket: stringValue(
        field(quoteDailyBorrowBucket, "borrowedBucket", "borrowed_bucket")
      ),
      baseDailyLastDecaySlot: stringValue(
        field(baseDailyBorrowBucket, "lastDecaySlot", "last_decay_slot")
      ),
      quoteDailyLastDecaySlot: stringValue(
        field(quoteDailyBorrowBucket, "lastDecaySlot", "last_decay_slot")
      ),
      baseDailyDecayRemainderMs: stringValue(
        field(baseDailyBorrowBucket, "decayRemainderMs", "decay_remainder_ms")
      ),
      quoteDailyDecayRemainderMs: stringValue(
        field(quoteDailyBorrowBucket, "decayRemainderMs", "decay_remainder_ms")
      ),
      globalHealthBaseContributionForQuoteDebt: stringValue(
        field(
          debt,
          "globalHealthBaseContributionForQuoteDebt",
          "global_health_base_contribution_for_quote_debt"
        )
      ),
      globalHealthQuoteContributionForBaseDebt: stringValue(
        field(
          debt,
          "globalHealthQuoteContributionForBaseDebt",
          "global_health_quote_contribution_for_base_debt"
        )
      ),
      effectiveBaseDebtNad: stringValue(field(health, "effectiveBaseDebtNad", "effective_base_debt_nad")),
      effectiveQuoteDebtNad: stringValue(
        field(health, "effectiveQuoteDebtNad", "effective_quote_debt_nad")
      ),
      baseDebtHealthBps: stringValue(field(health, "baseDebtHealthBps", "base_debt_health_bps")),
      quoteDebtHealthBps: stringValue(field(health, "quoteDebtHealthBps", "quote_debt_health_bps")),
      sourceTxSig: null,
      sourceSlot: null,
      updatedAt: now,
    },
  };
}

function forkConfigPayload(stored: StoredMarket) {
  return {
    rpcUrl: PUBLIC_RPC_URL,
    privateRpcUrl: SURFPOOL_RPC_URL,
    programId: PROGRAM_ID.toBase58(),
    payer: initializeRuntime().payer.publicKey.toBase58(),
    market: stored.market,
    label: stored.label,
    fixtureMode: forkMarketFixture(),
    baseMint: stored.baseMint,
    quoteMint: stored.quoteMint,
    baseDecimals: stored.baseDecimals,
    quoteDecimals: stored.quoteDecimals,
    baseTokenProgram: stored.baseTokenProgram,
    quoteTokenProgram: stored.quoteTokenProgram,
    ylpMint: stored.ylpMint,
    baseHlpMint: stored.baseHlpMint,
    quoteHlpMint: stored.quoteHlpMint,
    parameterTimelockSeconds: 7 * 24 * 60 * 60,
    parameterExecutionWindowSeconds: 7 * 24 * 60 * 60,
    seededLiquidity: stored.seededLiquidity,
    transferHookValidationAccounts: stored.transferHookValidationAccounts,
  };
}

function marketFromStored(stored: StoredMarket) {
  return {
    market: new PublicKey(stored.market),
    futarchyAuthority: pda(seed("futarchy_authority")),
    eventAuthority: new PublicKey(stored.eventAuthority),
    baseMint: new PublicKey(stored.baseMint),
    quoteMint: new PublicKey(stored.quoteMint),
    baseTokenProgram: new PublicKey(stored.baseTokenProgram),
    quoteTokenProgram: new PublicKey(stored.quoteTokenProgram),
    ylpMint: new PublicKey(stored.ylpMint),
    baseHlpMint: new PublicKey(stored.baseHlpMint),
    quoteHlpMint: new PublicKey(stored.quoteHlpMint),
    baseReserveVault: new PublicKey(stored.baseReserveVault),
    quoteReserveVault: new PublicKey(stored.quoteReserveVault),
    baseCollateralVault: new PublicKey(stored.baseCollateralVault),
    quoteCollateralVault: new PublicKey(stored.quoteCollateralVault),
    baseInsuranceVault: new PublicKey(stored.baseInsuranceVault),
    quoteInsuranceVault: new PublicKey(stored.quoteInsuranceVault),
    baseInterestVault: new PublicKey(stored.baseInterestVault),
    quoteInterestVault: new PublicKey(stored.quoteInterestVault),
    baseHlpYlpVault: new PublicKey(stored.baseHlpYlpVault),
    quoteHlpYlpVault: new PublicKey(stored.quoteHlpYlpVault),
  };
}

async function activeHlpSwapAccounts(m: ReturnType<typeof marketFromStored>) {
  const { program } = initializeRuntime();
  const market = await program.account.market.fetch(m.market);
  const baseHlpVault = field(market, "baseHlpVault", "base_hlp_vault");
  const quoteHlpVault = field(market, "quoteHlpVault", "quote_hlp_vault");
  const active =
    toBigInt(field(baseHlpVault, "hlpSupply", "hlp_supply")) > 0n ||
    toBigInt(field(quoteHlpVault, "hlpSupply", "hlp_supply")) > 0n ||
    toBigInt(field(baseHlpVault, "residualExposure", "residual_exposure")) !== 0n ||
    toBigInt(field(quoteHlpVault, "residualExposure", "residual_exposure")) !== 0n;
  if (!active) return [];
  return [
    { pubkey: m.ylpMint, isWritable: true, isSigner: false },
    { pubkey: m.baseHlpYlpVault, isWritable: true, isSigner: false },
    { pubkey: m.quoteHlpYlpVault, isWritable: true, isSigner: false },
    { pubkey: m.baseInterestVault, isWritable: true, isSigner: false },
    { pubkey: m.quoteInterestVault, isWritable: true, isSigner: false },
  ];
}

async function resolveStoredMarket(marketAddress: string, fallback: StoredMarket): Promise<StoredMarket> {
  if (!marketAddress || marketAddress === fallback.market) return fallback;
  const state = readState();
  const saved = Object.values(state.markets).find((market) => market.market === marketAddress);
  if (saved) return saved;

  const { program } = initializeRuntime();
  const market = new PublicKey(marketAddress);
  const account = await program.account.market.fetch(market);
  const baseSide = field<any>(account, "baseSide", "base_side");
  const quoteSide = field<any>(account, "quoteSide", "quote_side");
  const insurance = field<any>(account, "insurance");
  const baseMint = field<PublicKey>(baseSide, "assetMint", "asset_mint");
  const quoteMint = field<PublicKey>(quoteSide, "assetMint", "asset_mint");
  const ylpMint = field<PublicKey>(account, "ylpMint", "ylp_mint");
  const baseHlpMint = field<PublicKey>(baseSide, "hlpMint", "hlp_mint");
  const quoteHlpMint = field<PublicKey>(quoteSide, "hlpMint", "hlp_mint");
  const paramsHashValue = field<number[] | Uint8Array>(account, "paramsHash", "params_hash");
  if (!baseMint || !quoteMint || !ylpMint || !baseHlpMint || !quoteHlpMint || !paramsHashValue) {
    throw new Error(`Unable to reconstruct market ${marketAddress} from RPC`);
  }
  const paramsHash = Buffer.from(paramsHashValue);
  const addresses = deriveMarketAddresses(baseMint, quoteMint, paramsHash);
  if (!addresses.market.equals(market)) throw new Error(`Market PDA does not match ${marketAddress}`);
  const [baseTokenProgram, quoteTokenProgram] = await Promise.all([
    tokenProgramForMint(baseMint),
    tokenProgramForMint(quoteMint),
  ]);
  const stored = storedMarketDefinition({
    label: `market-${market.toBase58().slice(0, 8)}`,
    addresses: {
      ...addresses,
      baseReserveVault: field<PublicKey>(baseSide, "reserveVault", "reserve_vault") ?? addresses.baseReserveVault,
      quoteReserveVault: field<PublicKey>(quoteSide, "reserveVault", "reserve_vault") ?? addresses.quoteReserveVault,
      baseCollateralVault:
        field<PublicKey>(baseSide, "collateralVault", "collateral_vault") ?? addresses.baseCollateralVault,
      quoteCollateralVault:
        field<PublicKey>(quoteSide, "collateralVault", "collateral_vault") ?? addresses.quoteCollateralVault,
      baseInsuranceVault: field<PublicKey>(insurance, "baseVault", "base_vault") ?? addresses.baseInsuranceVault,
      quoteInsuranceVault: field<PublicKey>(insurance, "quoteVault", "quote_vault") ?? addresses.quoteInsuranceVault,
      baseInterestVault: field<PublicKey>(baseSide, "interestVault", "interest_vault") ?? addresses.baseInterestVault,
      quoteInterestVault: field<PublicKey>(quoteSide, "interestVault", "interest_vault") ?? addresses.quoteInterestVault,
    },
    paramsHash,
    baseMint,
    quoteMint,
    baseDecimals: Number(field(baseSide, "assetDecimals", "asset_decimals") ?? 0),
    quoteDecimals: Number(field(quoteSide, "assetDecimals", "asset_decimals") ?? 0),
    baseTokenProgram,
    quoteTokenProgram,
    ylpMint,
    baseHlpMint,
    quoteHlpMint,
    seededLiquidity:
      toBigInt(field(field(baseSide, "reserves"), "liveReserve", "live_reserve")) > 0n ||
      toBigInt(field(field(quoteSide, "reserves"), "liveReserve", "live_reserve")) > 0n,
  });
  state.markets[stored.label] = stored;
  writeState(state);
  return stored;
}

async function ownerTransaction(
  owner: PublicKey,
  instructions: TransactionInstruction[],
  payerCanSign = false
): Promise<Transaction> {
  const { connection, payer } = initializeRuntime();
  const tx = new Transaction();
  tx.add(
    ComputeBudgetProgram.requestHeapFrame({ bytes: 256 * 1024 }),
    ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
    ...instructions
  );
  tx.feePayer = payerCanSign ? payer.publicKey : owner;
  tx.recentBlockhash = (await connection.getLatestBlockhash("confirmed")).blockhash;
  return tx;
}

async function serializeOwnerTransaction(owner: PublicKey, instructions: TransactionInstruction[]) {
  const tx = await ownerTransaction(owner, instructions);
  return tx.serialize({ requireAllSignatures: false, verifySignatures: false }).toString("base64");
}

async function serializeBootstrapTransaction(instructions: TransactionInstruction[]) {
  const { payer } = initializeRuntime();
  const tx = await ownerTransaction(payer.publicKey, instructions, true);
  tx.sign(payer);
  return tx.serialize().toString("base64");
}

function rawAmount(body: Record<string, unknown>, keys: string[], decimals: number, fallback: string) {
  for (const key of keys) {
    const value = body[key];
    if (value != null && value !== "") return parseUnits(value as any, decimals);
  }
  return parseUnits(fallback, decimals);
}

function assetFromBody(value: unknown, fallback: MarketAsset): MarketAsset {
  if (value === "base" || value === "quote") return value;
  return fallback;
}

function yieldTokenKindFromBody(value: unknown, fallback: YieldTokenKind): YieldTokenKind {
  if (value === "ylp" || value === "hlp") return value;
  return fallback;
}

function protocolAuctionLaneFromBody(value: unknown, fallback: ProtocolAuctionLane): ProtocolAuctionLane {
  if (value === "fee" || value === "buyback") return value;
  return fallback;
}

function protocolAuctionLaneArg(lane: ProtocolAuctionLane) {
  return lane === "fee" ? { fee: {} } : { buyback: {} };
}

function protocolRevenueSourceFromBody(value: unknown): ProtocolRevenueSource {
  if (value === "swap" || value === "interest") return value;
  throw new Error('Protocol auction source must be explicitly set to "swap" or "interest"');
}

function protocolRevenueSourceArg(source: ProtocolRevenueSource) {
  return source === "swap" ? { swap: {} } : { interest: {} };
}

function protocolRevenueVault(
  market: ReturnType<typeof marketFromStored>,
  source: ProtocolRevenueSource,
  soldIsBase: boolean
): PublicKey {
  if (source === "swap") {
    return soldIsBase ? market.baseReserveVault : market.quoteReserveVault;
  }
  return soldIsBase ? market.baseInterestVault : market.quoteInterestVault;
}

async function maybeAddAta(
  instructions: TransactionInstruction[],
  owner: PublicKey,
  mint: PublicKey,
  tokenProgram: PublicKey
) {
  const ata = await ataInstructionIfMissing({ payer: owner, owner, mint, tokenProgram });
  if (ata.instruction) instructions.push(ata.instruction);
  return ata.address;
}

async function buildPreviewMarketTx(owner: PublicKey, market: StoredMarket) {
  const { program } = initializeRuntime();
  const m = marketFromStored(market);
  const instruction = await program.methods.previewMarket().accounts({ market: m.market }).instruction();
  return serializeOwnerTransaction(owner, [instruction]);
}

async function buildPreviewAddLiquidityTx(params: {
  owner: PublicKey;
  market: StoredMarket;
  baseDepositAmount: bigint;
  quoteDepositAmount: bigint;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const instruction = await program.methods
    .previewAddLiquidity({
      baseDepositAmount: toBN(params.baseDepositAmount),
      quoteDepositAmount: toBN(params.quoteDepositAmount),
    })
    .accounts({ market: m.market, baseMint: m.baseMint, quoteMint: m.quoteMint })
    .instruction();
  return serializeOwnerTransaction(params.owner, [instruction]);
}

async function buildPreviewSwapTx(params: {
  owner: PublicKey;
  market: StoredMarket;
  assetIn: MarketAsset;
  exactAssetIn: bigint;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const inIsBase = params.assetIn === "base";
  const instruction = await program.methods
    .previewSwap({ exactAssetIn: toBN(params.exactAssetIn) })
    .accounts({
      market: m.market,
      assetInMint: inIsBase ? m.baseMint : m.quoteMint,
      assetOutMint: inIsBase ? m.quoteMint : m.baseMint,
    })
    .instruction();
  return serializeOwnerTransaction(params.owner, [instruction]);
}

async function buildPreviewBorrowCapacityTx(params: {
  owner: PublicKey;
  market: StoredMarket;
  collateralAsset: MarketAsset;
  collateralAmount: bigint;
  projectedBorrowAmount: bigint | null;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const collateralIsBase = params.collateralAsset === "base";
  const instruction = await program.methods
    .previewBorrowCapacity({
      collateralAmount: toBN(params.collateralAmount),
      projectedBorrowAmount: params.projectedBorrowAmount === null ? null : toBN(params.projectedBorrowAmount),
    })
    .accounts({
      market: m.market,
      collateralAssetMint: collateralIsBase ? m.baseMint : m.quoteMint,
      debtAssetMint: collateralIsBase ? m.quoteMint : m.baseMint,
    })
    .instruction();
  return serializeOwnerTransaction(params.owner, [instruction]);
}

async function buildPreviewBorrowPositionTx(params: {
  owner: PublicKey;
  market: StoredMarket;
  positionId: PublicKey;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const instruction = await program.methods
    .previewBorrowPosition()
    .accounts({
      market: m.market,
      borrowPosition: deriveBorrowPosition(m.market, params.positionId),
    })
    .instruction();
  return serializeOwnerTransaction(params.owner, [instruction]);
}

async function buildAddLiquidityTx(params: {
  owner: PublicKey;
  market: StoredMarket;
  baseDepositAmount: bigint;
  quoteDepositAmount: bigint;
  minYlpAmount: bigint;
  payerCanSign?: boolean;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const instructions: TransactionInstruction[] = [];
  const ownerBase = await maybeAddAta(instructions, params.owner, m.baseMint, m.baseTokenProgram);
  const ownerQuote = await maybeAddAta(instructions, params.owner, m.quoteMint, m.quoteTokenProgram);
  const ownerYlp = await maybeAddAta(instructions, params.owner, m.ylpMint, TOKEN_2022_PROGRAM_ID);

  const ix = await program.methods
    .addLiquidity({
      baseDepositAmount: toBN(params.baseDepositAmount),
      quoteDepositAmount: toBN(params.quoteDepositAmount),
      minYlpAmount: toBN(params.minYlpAmount),
    })
    .accounts({
      market: m.market,
      futarchyAuthority: m.futarchyAuthority,
      owner: params.owner,
      baseMint: m.baseMint,
      quoteMint: m.quoteMint,
      ylpMint: m.ylpMint,
      baseReserveVault: m.baseReserveVault,
      quoteReserveVault: m.quoteReserveVault,
      ownerBaseAccount: ownerBase,
      ownerQuoteAccount: ownerQuote,
      ownerYlpAccount: ownerYlp,
      baseYieldAccount: deriveYieldAccount(m.market, params.owner, m.ylpMint, m.baseMint, "ylp"),
      quoteYieldAccount: deriveYieldAccount(m.market, params.owner, m.ylpMint, m.quoteMint, "ylp"),
      tokenProgram: TOKEN_PROGRAM_ID,
      token2022Program: TOKEN_2022_PROGRAM_ID,
      systemProgram: SystemProgram.programId,
      eventAuthority: m.eventAuthority,
      program: PROGRAM_ID,
    })
    .instruction();
  instructions.push(ix);
  return ownerTransaction(params.owner, instructions, params.payerCanSign);
}

async function buildRemoveLiquidityTx(params: {
  owner: PublicKey;
  market: StoredMarket;
  ylpAmount: bigint;
  minBaseAmountOut: bigint;
  minQuoteAmountOut: bigint;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const instructions: TransactionInstruction[] = [];
  const ownerBase = await maybeAddAta(instructions, params.owner, m.baseMint, m.baseTokenProgram);
  const ownerQuote = await maybeAddAta(instructions, params.owner, m.quoteMint, m.quoteTokenProgram);
  const ownerYlp = await maybeAddAta(instructions, params.owner, m.ylpMint, TOKEN_2022_PROGRAM_ID);

  instructions.push(
    await program.methods
      .removeLiquidity({
        ylpAmount: toBN(params.ylpAmount),
        minBaseAmountOut: toBN(params.minBaseAmountOut),
        minQuoteAmountOut: toBN(params.minQuoteAmountOut),
      })
      .accounts({
        market: m.market,
        owner: params.owner,
        baseMint: m.baseMint,
        quoteMint: m.quoteMint,
        ylpMint: m.ylpMint,
        baseReserveVault: m.baseReserveVault,
        quoteReserveVault: m.quoteReserveVault,
        ownerBaseAccount: ownerBase,
        ownerQuoteAccount: ownerQuote,
        ownerYlpAccount: ownerYlp,
        baseYieldAccount: deriveYieldAccount(m.market, params.owner, m.ylpMint, m.baseMint, "ylp"),
        quoteYieldAccount: deriveYieldAccount(m.market, params.owner, m.ylpMint, m.quoteMint, "ylp"),
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: m.eventAuthority,
        program: PROGRAM_ID,
      })
      .instruction()
  );
  return serializeOwnerTransaction(params.owner, instructions);
}

async function buildSwapTx(params: {
  owner: PublicKey;
  market: StoredMarket;
  assetIn: MarketAsset;
  exactAssetIn: bigint;
  minAssetOut: bigint;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const inIsBase = params.assetIn === "base";
  const instructions: TransactionInstruction[] = [];
  const ownerIn = await maybeAddAta(
    instructions,
    params.owner,
    inIsBase ? m.baseMint : m.quoteMint,
    inIsBase ? m.baseTokenProgram : m.quoteTokenProgram
  );
  const ownerOut = await maybeAddAta(
    instructions,
    params.owner,
    inIsBase ? m.quoteMint : m.baseMint,
    inIsBase ? m.quoteTokenProgram : m.baseTokenProgram
  );

  let builder = program.methods
    .swap({
      exactAssetIn: toBN(params.exactAssetIn),
      minAssetOut: toBN(params.minAssetOut),
    })
    .accounts({
      market: m.market,
      futarchyAuthority: m.futarchyAuthority,
      trader: params.owner,
      assetInMint: inIsBase ? m.baseMint : m.quoteMint,
      assetOutMint: inIsBase ? m.quoteMint : m.baseMint,
      reserveInVault: inIsBase ? m.baseReserveVault : m.quoteReserveVault,
      reserveOutVault: inIsBase ? m.quoteReserveVault : m.baseReserveVault,
      traderAssetInAccount: ownerIn,
      traderAssetOutAccount: ownerOut,
      tokenProgram: TOKEN_PROGRAM_ID,
      token2022Program: TOKEN_2022_PROGRAM_ID,
      eventAuthority: m.eventAuthority,
      program: PROGRAM_ID,
    });

  const remainingAccounts = await activeHlpSwapAccounts(m);
  if (remainingAccounts.length > 0) builder = builder.remainingAccounts(remainingAccounts);
  instructions.push(await builder.instruction());
  return serializeOwnerTransaction(params.owner, instructions);
}

async function buildDepositCollateralTx(params: {
  owner: PublicKey;
  market: StoredMarket;
  positionId: PublicKey;
  marketAsset: MarketAsset;
  depositAmount: bigint;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const isBase = params.marketAsset === "base";
  const instructions: TransactionInstruction[] = [];
  const ownerAsset = await maybeAddAta(
    instructions,
    params.owner,
    isBase ? m.baseMint : m.quoteMint,
    isBase ? m.baseTokenProgram : m.quoteTokenProgram
  );
  instructions.push(
    await program.methods
      .depositCollateral({
        positionId: params.positionId,
        depositAmount: toBN(params.depositAmount),
      })
      .accounts({
        market: m.market,
        owner: params.owner,
        assetMint: isBase ? m.baseMint : m.quoteMint,
        collateralVault: isBase ? m.baseCollateralVault : m.quoteCollateralVault,
        ownerAssetAccount: ownerAsset,
        borrowPosition: deriveBorrowPosition(m.market, params.positionId),
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
        eventAuthority: m.eventAuthority,
        program: program.programId,
      })
      .instruction()
  );
  return serializeOwnerTransaction(params.owner, instructions);
}

/**
 * Donate into a market's insurance fund.
 *
 * `fortify_market` credits the vault for one side and is permissionless — the
 * donor is any signer, not the market authority — so the only accounts beyond
 * the transfer pair are the market and the side's insurance vault. The
 * instruction is `#[event_cpi]`, hence the event authority.
 */
async function buildFortifyMarketTx(params: {
  owner: PublicKey;
  market: StoredMarket;
  marketAsset: MarketAsset;
  amount: bigint;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const isBase = params.marketAsset === "base";
  const instructions: TransactionInstruction[] = [];
  const donorAsset = await maybeAddAta(
    instructions,
    params.owner,
    isBase ? m.baseMint : m.quoteMint,
    isBase ? m.baseTokenProgram : m.quoteTokenProgram
  );
  instructions.push(
    await program.methods
      .fortifyMarket({
        asset: isBase ? 0 : 1,
        amount: toBN(params.amount),
      })
      .accounts({
        market: m.market,
        donor: params.owner,
        assetMint: isBase ? m.baseMint : m.quoteMint,
        donorAssetAccount: donorAsset,
        insuranceVault: isBase ? m.baseInsuranceVault : m.quoteInsuranceVault,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: m.eventAuthority,
        program: program.programId,
      })
      .instruction()
  );
  return serializeOwnerTransaction(params.owner, instructions);
}

async function buildWithdrawCollateralTx(params: {
  owner: PublicKey;
  market: StoredMarket;
  positionId: PublicKey;
  marketAsset: MarketAsset;
  withdrawAmount: bigint;
  minAssetAmountOut: bigint;
  minLiquidationCfBps: number;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const isBase = params.marketAsset === "base";
  const instructions: TransactionInstruction[] = [];
  const ownerAsset = await maybeAddAta(
    instructions,
    params.owner,
    isBase ? m.baseMint : m.quoteMint,
    isBase ? m.baseTokenProgram : m.quoteTokenProgram
  );
  instructions.push(
    await program.methods
      .withdrawCollateral({
        withdrawAmount: toBN(params.withdrawAmount),
        minAssetAmountOut: toBN(params.minAssetAmountOut),
        minLiquidationCfBps: params.minLiquidationCfBps,
      })
      .accounts({
        market: m.market,
        futarchyAuthority: m.futarchyAuthority,
        owner: params.owner,
        assetMint: isBase ? m.baseMint : m.quoteMint,
        collateralVault: isBase ? m.baseCollateralVault : m.quoteCollateralVault,
        ownerAssetAccount: ownerAsset,
        borrowPosition: deriveBorrowPosition(m.market, params.positionId),
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: m.eventAuthority,
        program: PROGRAM_ID,
      })
      .instruction()
  );
  return serializeOwnerTransaction(params.owner, instructions);
}

async function buildBorrowTx(params: {
  owner: PublicKey;
  market: StoredMarket;
  positionId: PublicKey;
  borrowAsset: MarketAsset;
  borrowAmount: bigint;
  minDebtAmountOut: bigint;
  minLiquidationCfBps: number;
  referrer: PublicKey | null;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const isBase = params.borrowAsset === "base";
  const instructions: TransactionInstruction[] = [];
  const ownerDebt = await maybeAddAta(
    instructions,
    params.owner,
    isBase ? m.baseMint : m.quoteMint,
    isBase ? m.baseTokenProgram : m.quoteTokenProgram
  );
  const debtMint = isBase ? m.baseMint : m.quoteMint;
  const boundReferral = await borrowPositionReferralAccounts(
    m.market,
    params.positionId,
    params.borrowAsset,
    debtMint
  );
  let referralPartner = boundReferral.referralPartner;
  let referralAccrual = boundReferral.referralAccrual;
  if (!referralPartner && params.referrer) {
    referralPartner = deriveReferralPartner(params.referrer);
    const initialized = await buildInitializeReferralAccrualInstruction({
      payer: params.owner,
      market: m.market,
      assetMint: debtMint,
      referralPartner,
    });
    referralAccrual = initialized.referralAccrual;
    instructions.push(initialized.instruction);
  }
  instructions.push(
    await program.methods
      .borrow({
        borrowAmount: toBN(params.borrowAmount),
        minDebtAmountOut: toBN(params.minDebtAmountOut),
        minLiquidationCfBps: params.minLiquidationCfBps,
        referrer: params.referrer,
      })
      .accounts({
        market: m.market,
        futarchyAuthority: m.futarchyAuthority,
        owner: params.owner,
        debtAssetMint: debtMint,
        collateralAssetMint: isBase ? m.quoteMint : m.baseMint,
        reserveVault: isBase ? m.baseReserveVault : m.quoteReserveVault,
        ownerDebtAccount: ownerDebt,
        borrowPosition: deriveBorrowPosition(m.market, params.positionId),
        referralPartner,
        referralAccrual,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: m.eventAuthority,
        program: PROGRAM_ID,
      })
      .instruction()
  );
  return serializeOwnerTransaction(params.owner, instructions);
}

async function buildSetReferralRecipientTx(params: {
  authority: PublicKey;
  recipient: PublicKey;
}) {
  const { program } = initializeRuntime();
  const referralPartner = deriveReferralPartner(params.authority);
  const instruction = await program.methods
    .setReferralRecipient({ recipient: params.recipient })
    .accounts({
      authority: params.authority,
      referralPartner,
      eventAuthority: pda(seed("__event_authority")),
      program: program.programId,
    })
    .instruction();
  return {
    transaction: await serializeOwnerTransaction(params.authority, [instruction]),
    referralPartner,
  };
}

async function buildConfigureReferralPartnerTx(params: {
  authority: PublicKey;
  referrer: PublicKey;
  interestShareBps: number;
  active: boolean;
}) {
  const { program } = initializeRuntime();
  const referralPartner = deriveReferralPartner(params.referrer);
  const instruction = await program.methods
    .configureReferralPartner({
      referrer: params.referrer,
      interestShareBps: params.interestShareBps,
      active: params.active,
    })
    .accounts({
      authoritySigner: params.authority,
      futarchyAuthority: pda(seed("futarchy_authority")),
      referralPartner,
      systemProgram: SystemProgram.programId,
      eventAuthority: pda(seed("__event_authority")),
      program: program.programId,
    })
    .instruction();
  return {
    transaction: await serializeOwnerTransaction(params.authority, [instruction]),
    referralPartner,
  };
}

async function buildInitializeReferralAccrualInstruction(params: {
  payer: PublicKey;
  market: PublicKey;
  assetMint: PublicKey;
  referralPartner: PublicKey;
}) {
  const { program } = initializeRuntime();
  const referralAccrual = deriveReferralAccrual(
    params.referralPartner,
    params.market,
    params.assetMint
  );
  return {
    referralAccrual,
    instruction: await program.methods
      .initializeReferralAccrual()
      .accounts({
        payer: params.payer,
        referralPartner: params.referralPartner,
        market: params.market,
        assetMint: params.assetMint,
        referralAccrual,
        systemProgram: SystemProgram.programId,
      })
      .instruction(),
  };
}

async function buildClaimReferralInterestTx(params: {
  authority: PublicKey;
  market: StoredMarket;
  assetMint: PublicKey;
  tokenProgram: PublicKey;
}) {
  const { connection, program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const referralPartner = deriveReferralPartner(params.authority);
  const partner = await program.account.referralPartner.fetch(referralPartner);
  const recipient = new PublicKey(
    stringValue(field(partner, "recipient"))
  );
  const referralAccrual = deriveReferralAccrual(
    referralPartner,
    m.market,
    params.assetMint
  );
  const interestVault = params.assetMint.equals(m.baseMint)
    ? m.baseInterestVault
    : m.quoteInterestVault;
  const instructions: TransactionInstruction[] = [];
  const recipientAccountResult = await ataInstructionIfMissing({
    payer: params.authority,
    owner: recipient,
    mint: params.assetMint,
    tokenProgram: params.tokenProgram,
  });
  if (recipientAccountResult.instruction) instructions.push(recipientAccountResult.instruction);
  const recipientAccount = recipientAccountResult.address;
  let builder = program.methods
    .claimReferralInterest()
    .accounts({
      market: m.market,
      authority: params.authority,
      referralPartner,
      assetMint: params.assetMint,
      referralAccrual,
      interestVault,
      recipientTokenAccount: recipientAccount,
      tokenProgram: TOKEN_PROGRAM_ID,
      token2022Program: TOKEN_2022_PROGRAM_ID,
      eventAuthority: m.eventAuthority,
      program: program.programId,
    });
  if (params.tokenProgram.equals(TOKEN_2022_PROGRAM_ID)) {
    const accrual = await program.account.referralAccrual.fetch(referralAccrual);
    const mint = await getMint(connection, params.assetMint, "confirmed", params.tokenProgram);
    const hookTransfer = await createTransferCheckedWithTransferHookInstruction(
      connection,
      interestVault,
      params.assetMint,
      recipientAccount,
      m.market,
      toBigInt(field(accrual, "amount")),
      mint.decimals,
      [],
      "confirmed",
      params.tokenProgram
    );
    builder = builder.remainingAccounts(hookTransfer.keys.slice(4));
  }
  instructions.push(await builder.instruction());
  return {
    transaction: await serializeOwnerTransaction(params.authority, instructions),
    referralPartner,
    referralAccrual,
    recipient,
    recipientTokenAccount: recipientAccount,
  };
}

async function buildSetYieldRecipientTx(params: {
  owner: PublicKey;
  market: StoredMarket;
  asset: MarketAsset;
  tokenKind: YieldTokenKind;
  recipient: PublicKey;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const isBase = params.asset === "base";
  const assetMint = isBase ? m.baseMint : m.quoteMint;
  const lpMint = params.tokenKind === "ylp"
    ? m.ylpMint
    : isBase
      ? m.baseHlpMint
      : m.quoteHlpMint;
  const instruction = await program.methods
    .setYieldRecipient({
      tokenKind: params.tokenKind === "ylp" ? { ylp: {} } : { hlp: {} },
      recipient: params.recipient,
    })
    .accounts({
      market: m.market,
      owner: params.owner,
      assetMint,
      lpMint,
      yieldAccount: deriveYieldAccount(m.market, params.owner, lpMint, assetMint, params.tokenKind),
      eventAuthority: m.eventAuthority,
      program: PROGRAM_ID,
    })
    .instruction();
  return serializeOwnerTransaction(params.owner, [instruction]);
}

async function buildHarvestTx(params: {
  owner: PublicKey;
  market: StoredMarket;
  asset: MarketAsset;
  tokenKind: YieldTokenKind;
  recipient: PublicKey;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const isBase = params.asset === "base";
  const assetMint = isBase ? m.baseMint : m.quoteMint;
  const assetTokenProgram = isBase ? m.baseTokenProgram : m.quoteTokenProgram;
  const lpMint = params.tokenKind === "ylp"
    ? m.ylpMint
    : isBase
      ? m.baseHlpMint
      : m.quoteHlpMint;
  const instructions: TransactionInstruction[] = [];
  const ownerLpAccount = await maybeAddAta(
    instructions,
    params.owner,
    lpMint,
    TOKEN_2022_PROGRAM_ID
  );
  const recipientAssetAccount = await maybeAddAta(
    instructions,
    params.recipient,
    assetMint,
    assetTokenProgram
  );
  instructions.push(
    await program.methods
      .harvest({ tokenKind: params.tokenKind === "ylp" ? { ylp: {} } : { hlp: {} } })
      .accounts({
        market: m.market,
        owner: params.owner,
        assetMint,
        lpMint,
        ownerLpAccount,
        reserveVault: isBase ? m.baseReserveVault : m.quoteReserveVault,
        interestVault: isBase ? m.baseInterestVault : m.quoteInterestVault,
        recipientAssetAccount,
        yieldAccount: deriveYieldAccount(m.market, params.owner, lpMint, assetMint, params.tokenKind),
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: m.eventAuthority,
        program: PROGRAM_ID,
      })
      .instruction()
  );
  return serializeOwnerTransaction(params.owner, instructions);
}

function parameterUpdateFromBody(update: Record<string, unknown>) {
  const recognizedFamilies = [
    "fee",
    "concentration",
    "irm",
    "emaHalfLives",
    "dailyBorrowLimit",
  ].filter((family) => update[family] !== undefined);
  if (recognizedFamilies.length !== 1) {
    throw new Error(
      "update must contain exactly one of fee, concentration, irm, emaHalfLives, or dailyBorrowLimit"
    );
  }
  if (update.fee && typeof update.fee === "object") {
    const fee = update.fee as Record<string, unknown>;
    return {
      fee: {
        0: {
          baseFeeBps: Number(fee.baseFeeBps),
          divergenceFeeShareCapBps: Number(fee.divergenceFeeShareCapBps),
          volatilityFeeShareCapBps: Number(fee.volatilityFeeShareCapBps),
          divergenceFeeCoefficientNad: toBN(String(fee.divergenceFeeCoefficientNad)),
          volatilityFeeCoefficientNad: toBN(String(fee.volatilityFeeCoefficientNad)),
          volatilityHalfLifeMs: toBN(String(fee.volatilityHalfLifeMs)),
          volatilityShockCapNad: toBN(String(fee.volatilityShockCapNad)),
          volatilityAccumulatorCapNad: toBN(String(fee.volatilityAccumulatorCapNad)),
        },
      },
    };
  }
  if (update.concentration && typeof update.concentration === "object") {
    const concentration = update.concentration as Record<string, unknown>;
    return {
      concentration: {
        peakAmplificationNad: toBN(String(concentration.peakAmplificationNad)),
        coreHalfWidthBps: Number(concentration.coreHalfWidthBps),
        fadeWidthBps: Number(concentration.fadeWidthBps),
      },
    };
  }
  if (update.irm && typeof update.irm === "object") {
    const irm = update.irm as Record<string, unknown>;
    return {
      irm: {
        0: {
          targetUtilizationBps: Number(irm.targetUtilizationBps),
          curveSteepnessNad: toBN(String(irm.curveSteepnessNad)),
          adjustmentSpeedPerYear: toBN(String(irm.adjustmentSpeedPerYear)),
        },
      },
    };
  }
  if (update.emaHalfLives && typeof update.emaHalfLives === "object") {
    const ema = update.emaHalfLives as Record<string, unknown>;
    return {
      emaHalfLives: {
        priceMs: toBN(String(ema.priceMs)),
        directionalPriceMs: toBN(String(ema.directionalPriceMs)),
        curveDepthMs: toBN(String(ema.curveDepthMs)),
        centerPriceMs: toBN(String(ema.centerPriceMs)),
      },
    };
  }
  if (update.dailyBorrowLimit && typeof update.dailyBorrowLimit === "object") {
    const daily = update.dailyBorrowLimit as Record<string, unknown>;
    return {
      dailyBorrowLimit: {
        maxDailyBorrowBps: Number(daily.maxDailyBorrowBps),
      },
    };
  }
  throw new Error(
    "update must contain exactly one of fee, concentration, irm, emaHalfLives, or dailyBorrowLimit"
  );
}

function proposalMetadataFromBody(metadata: Record<string, unknown> | undefined) {
  const title = String(metadata?.title ?? "Dusk parameter update").trim();
  const descriptionUri = String(
    metadata?.descriptionUri ?? "ipfs://dusk-parameter-proposal"
  );
  const description = String(metadata?.description ?? title);
  const suppliedHash = metadata?.descriptionSha256;
  let descriptionSha256: number[];
  if (Array.isArray(suppliedHash)) {
    descriptionSha256 = suppliedHash.map(Number);
  } else if (typeof suppliedHash === "string") {
    descriptionSha256 = Array.from(
      Buffer.from(suppliedHash.replace(/^0x/, ""), "hex")
    );
  } else {
    descriptionSha256 = Array.from(createHash("sha256").update(description).digest());
  }
  if (descriptionSha256.length !== 32) {
    throw new Error("descriptionSha256 must contain exactly 32 bytes");
  }
  return {
    version: Number(metadata?.version ?? 1),
    title,
    descriptionUri,
    descriptionSha256,
    descriptionLen: Number(
      metadata?.descriptionLen ?? Buffer.byteLength(description, "utf8")
    ),
  };
}

async function buildCreateParameterProposalTx(params: {
  proposer: PublicKey;
  market: StoredMarket;
  nonce: bigint;
  update: Record<string, unknown>;
  metadata?: Record<string, unknown>;
  initialSupport: bigint;
  bootstrapSigned: boolean;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const proposal = deriveParameterProposal(m.market, params.proposer, params.nonce);
  const proposalSupport = deriveProposalSupport(proposal, params.proposer);
  const proposerYlpAccount = getAssociatedTokenAddressSync(
    m.ylpMint,
    params.proposer,
    false,
    TOKEN_2022_PROGRAM_ID
  );
  const instruction = await program.methods
    .createParameterProposal({
      nonce: toBN(params.nonce),
      update: parameterUpdateFromBody(params.update),
      metadata: proposalMetadataFromBody(params.metadata),
      initialSupport: toBN(params.initialSupport),
    })
    .accounts({
      proposer: params.proposer,
      market: m.market,
      proposal,
      proposalSupport,
      ylpMint: m.ylpMint,
      proposerYlpAccount,
      baseYieldAccount: deriveYieldAccount(
        m.market,
        params.proposer,
        m.ylpMint,
        m.baseMint,
        "ylp"
      ),
      quoteYieldAccount: deriveYieldAccount(
        m.market,
        params.proposer,
        m.ylpMint,
        m.quoteMint,
        "ylp"
      ),
      baseHlpYlpVault: new PublicKey(params.market.baseHlpYlpVault),
      quoteHlpYlpVault: new PublicKey(params.market.quoteHlpYlpVault),
      token2022Program: TOKEN_2022_PROGRAM_ID,
      systemProgram: SystemProgram.programId,
      eventAuthority: m.eventAuthority,
      program: PROGRAM_ID,
    })
    .instruction();
  return {
    proposal,
    proposalSupport,
    transaction: params.bootstrapSigned
      ? await serializeBootstrapTransaction([instruction])
      : await serializeOwnerTransaction(params.proposer, [instruction]),
  };
}

async function buildSupportParameterProposalTx(params: {
  supporter: PublicKey;
  market: StoredMarket;
  proposal: PublicKey;
  amount: bigint;
  bootstrapSigned: boolean;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const proposalSupport = deriveProposalSupport(params.proposal, params.supporter);
  const supporterYlpAccount = getAssociatedTokenAddressSync(
    m.ylpMint,
    params.supporter,
    false,
    TOKEN_2022_PROGRAM_ID
  );
  const instruction = await program.methods
    .supportParameterProposal({ amount: toBN(params.amount) })
    .accounts({
      supporter: params.supporter,
      market: m.market,
      proposal: params.proposal,
      proposalSupport,
      ylpMint: m.ylpMint,
      supporterYlpAccount,
      baseYieldAccount: deriveYieldAccount(
        m.market,
        params.supporter,
        m.ylpMint,
        m.baseMint,
        "ylp"
      ),
      quoteYieldAccount: deriveYieldAccount(
        m.market,
        params.supporter,
        m.ylpMint,
        m.quoteMint,
        "ylp"
      ),
      baseHlpYlpVault: new PublicKey(params.market.baseHlpYlpVault),
      quoteHlpYlpVault: new PublicKey(params.market.quoteHlpYlpVault),
      token2022Program: TOKEN_2022_PROGRAM_ID,
      systemProgram: SystemProgram.programId,
      eventAuthority: m.eventAuthority,
      program: PROGRAM_ID,
    })
    .instruction();
  return {
    proposalSupport,
    transaction: params.bootstrapSigned
      ? await serializeBootstrapTransaction([instruction])
      : await serializeOwnerTransaction(params.supporter, [instruction]),
  };
}

async function buildQueueParameterProposalTx(params: {
  caller: PublicKey;
  market: StoredMarket;
  proposal: PublicKey;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const instruction = await program.methods
    .queueParameterProposal()
    .accounts({
      market: m.market,
      proposal: params.proposal,
      ylpMint: m.ylpMint,
      baseHlpYlpVault: new PublicKey(params.market.baseHlpYlpVault),
      quoteHlpYlpVault: new PublicKey(params.market.quoteHlpYlpVault),
      eventAuthority: m.eventAuthority,
      program: PROGRAM_ID,
    })
    .instruction();
  return serializeOwnerTransaction(params.caller, [instruction]);
}

async function buildExecuteParameterProposalTx(params: {
  caller: PublicKey;
  market: StoredMarket;
  proposal: PublicKey;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const instruction = await program.methods
    .executeParameterProposal()
    .accounts({
      market: m.market,
      proposal: params.proposal,
      eventAuthority: m.eventAuthority,
      program: PROGRAM_ID,
    })
    .instruction();
  return serializeOwnerTransaction(params.caller, [instruction]);
}

async function buildWithdrawParameterSupportTx(params: {
  supporter: PublicKey;
  market: StoredMarket;
  proposal: PublicKey;
  bootstrapSigned: boolean;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const proposalSupport = deriveProposalSupport(params.proposal, params.supporter);
  const supporterYlpAccount = getAssociatedTokenAddressSync(
    m.ylpMint,
    params.supporter,
    false,
    TOKEN_2022_PROGRAM_ID
  );
  const instruction = await program.methods
    .withdrawParameterSupport()
    .accounts({
      supporter: params.supporter,
      market: m.market,
      proposal: params.proposal,
      proposalSupport,
      ylpMint: m.ylpMint,
      supporterYlpAccount,
      baseYieldAccount: deriveYieldAccount(
        m.market,
        params.supporter,
        m.ylpMint,
        m.baseMint,
        "ylp"
      ),
      quoteYieldAccount: deriveYieldAccount(
        m.market,
        params.supporter,
        m.ylpMint,
        m.quoteMint,
        "ylp"
      ),
      token2022Program: TOKEN_2022_PROGRAM_ID,
      eventAuthority: m.eventAuthority,
      program: PROGRAM_ID,
    })
    .instruction();
  return {
    proposalSupport,
    transaction: params.bootstrapSigned
      ? await serializeBootstrapTransaction([instruction])
      : await serializeOwnerTransaction(params.supporter, [instruction]),
  };
}

async function buildTransferLpTx(params: {
  owner: PublicKey;
  recipient: PublicKey;
  market: StoredMarket;
  tokenKind: YieldTokenKind;
  asset: MarketAsset;
  amount: bigint;
}) {
  const { connection } = initializeRuntime();
  const m = marketFromStored(params.market);
  const lpMint = params.tokenKind === "ylp"
    ? m.ylpMint
    : params.asset === "base"
      ? m.baseHlpMint
      : m.quoteHlpMint;
  const mint = await getMint(connection, lpMint, "confirmed", TOKEN_2022_PROGRAM_ID);
  const instructions: TransactionInstruction[] = [];
  const source = await maybeAddAta(
    instructions,
    params.owner,
    lpMint,
    TOKEN_2022_PROGRAM_ID
  );
  const destination = await ataInstructionIfMissing({
    payer: params.owner,
    owner: params.recipient,
    mint: lpMint,
    tokenProgram: TOKEN_2022_PROGRAM_ID,
  });
  if (destination.instruction) instructions.push(destination.instruction);
  instructions.push(
    await createTransferCheckedWithTransferHookInstruction(
      connection,
      source,
      lpMint,
      destination.address,
      params.owner,
      params.amount,
      mint.decimals,
      [],
      "confirmed",
      TOKEN_2022_PROGRAM_ID
    )
  );
  return serializeOwnerTransaction(params.owner, instructions);
}

async function buildSetGlobalReduceOnlyTx(params: {
  authority: PublicKey;
  reduceOnly: boolean;
}) {
  const { program } = initializeRuntime();
  const instruction = await program.methods
    .setGlobalReduceOnly({ reduceOnly: params.reduceOnly })
    .accounts({
      authoritySigner: params.authority,
      futarchyAuthority: pda(seed("futarchy_authority")),
    })
    .instruction();
  return serializeOwnerTransaction(params.authority, [instruction]);
}

async function buildSetMarketReduceOnlyTx(params: {
  authority: PublicKey;
  market: StoredMarket;
  reduceOnly: boolean;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const instruction = await program.methods
    .setMarketReduceOnly({ reduceOnly: params.reduceOnly })
    .accounts({
      market: m.market,
      authoritySigner: params.authority,
      eventAuthority: m.eventAuthority,
      program: PROGRAM_ID,
    })
    .instruction();
  return serializeOwnerTransaction(params.authority, [instruction]);
}

async function buildUpdateFutarchyAuthorityTx(params: {
  authority: PublicKey;
  newAuthority: PublicKey;
  bootstrapSigned: boolean;
}) {
  const { program } = initializeRuntime();
  const instruction = await program.methods
    .updateFutarchyAuthority({ newAuthority: params.newAuthority })
    .accounts({
      authoritySigner: params.authority,
      futarchyAuthority: pda(seed("futarchy_authority")),
    })
    .instruction();
  return params.bootstrapSigned
    ? serializeBootstrapTransaction([instruction])
    : serializeOwnerTransaction(params.authority, [instruction]);
}

async function buildUpdateProtocolRevenueTx(params: {
  authority: PublicKey;
  swapBps: number | null;
  interestBps: number | null;
  maxReferralInterestShareBps: number | null;
  revenueDistribution: {
    futarchyTreasuryBps: number;
    buybacksVaultBps: number;
    teamTreasuryBps: number;
  } | null;
  protocolAuctionSplit: {
    feeAuctionBps: number;
    buybackAuctionBps: number;
  } | null;
}) {
  const { program } = initializeRuntime();
  const instruction = await program.methods
    .updateProtocolRevenue({
      swapBps: params.swapBps,
      interestBps: params.interestBps,
      maxReferralInterestShareBps: params.maxReferralInterestShareBps,
      revenueDistribution: params.revenueDistribution,
      protocolAuctionSplit: params.protocolAuctionSplit,
    })
    .accounts({
      authoritySigner: params.authority,
      futarchyAuthority: pda(seed("futarchy_authority")),
      eventAuthority: pda(seed("__event_authority")),
      program: program.programId,
    })
    .instruction();
  return serializeOwnerTransaction(params.authority, [instruction]);
}

async function buildUpdateRevenueRecipientsTx(params: {
  authority: PublicKey;
  futarchyTreasury: PublicKey | null;
  buybacksVault: PublicKey | null;
  teamTreasury: PublicKey | null;
}) {
  const { program } = initializeRuntime();
  const instruction = await program.methods
    .updateRevenueRecipients({
      futarchyTreasury: params.futarchyTreasury,
      buybacksVault: params.buybacksVault,
      teamTreasury: params.teamTreasury,
    })
    .accounts({
      authoritySigner: params.authority,
      futarchyAuthority: pda(seed("futarchy_authority")),
    })
    .instruction();
  return serializeOwnerTransaction(params.authority, [instruction]);
}

async function buildUpdateProtocolAuctionConfigTx(params: {
  authority: PublicKey;
  lane: ProtocolAuctionLane;
  acceptedMint: PublicKey | null;
  auctionParams: {
    startMultiplierBps: number;
    floorMultiplierBps: number;
    durationSlots: bigint;
    maxReferenceAgeSlots: bigint;
  } | null;
}) {
  const { program } = initializeRuntime();
  const instruction = await program.methods
    .updateProtocolAuctionConfig({
      lane: protocolAuctionLaneArg(params.lane),
      acceptedMint: params.acceptedMint,
      params: params.auctionParams == null
        ? null
        : {
            startMultiplierBps: params.auctionParams.startMultiplierBps,
            floorMultiplierBps: params.auctionParams.floorMultiplierBps,
            durationSlots: toBN(params.auctionParams.durationSlots),
            maxReferenceAgeSlots: toBN(params.auctionParams.maxReferenceAgeSlots),
          },
    })
    .accounts({
      authoritySigner: params.authority,
      futarchyAuthority: pda(seed("futarchy_authority")),
      eventAuthority: pda(seed("__event_authority")),
      program: program.programId,
    })
    .instruction();
  return serializeOwnerTransaction(params.authority, [instruction]);
}

async function buildUpdateProtocolAuctionRecipientsTx(params: {
  authority: PublicKey;
  lane: ProtocolAuctionLane;
  treasury: PublicKey | null;
  stakingVault: PublicKey | null;
  treasuryBps: number | null;
  stakingVaultBps: number | null;
}) {
  const { program } = initializeRuntime();
  const instruction = await program.methods
    .updateProtocolAuctionRecipients({
      lane: protocolAuctionLaneArg(params.lane),
      treasury: params.treasury,
      stakingVault: params.stakingVault,
      treasuryBps: params.treasuryBps,
      stakingVaultBps: params.stakingVaultBps,
    })
    .accounts({
      authoritySigner: params.authority,
      futarchyAuthority: pda(seed("futarchy_authority")),
      eventAuthority: pda(seed("__event_authority")),
      program: program.programId,
    })
    .instruction();
  return serializeOwnerTransaction(params.authority, [instruction]);
}

async function buildUpdateProtocolAuctionRouteTx(params: {
  authority: PublicKey;
  market: StoredMarket;
  lane: ProtocolAuctionLane;
  soldAsset: MarketAsset;
  referenceMarket: PublicKey;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const soldMint = params.soldAsset === "base" ? m.baseMint : m.quoteMint;
  const instruction = await program.methods
    .updateProtocolAuctionRoute({
      lane: protocolAuctionLaneArg(params.lane),
      soldMint,
      referenceMarket: params.referenceMarket,
    })
    .accounts({
      authoritySigner: params.authority,
      futarchyAuthority: m.futarchyAuthority,
      market: m.market,
      eventAuthority: m.eventAuthority,
      program: program.programId,
    })
    .instruction();
  return serializeOwnerTransaction(params.authority, [instruction]);
}

async function buildSettleProtocolAuctionTx(params: {
  bidder: PublicKey;
  market: StoredMarket;
  lane: ProtocolAuctionLane;
  source: ProtocolRevenueSource;
  soldAsset: MarketAsset;
  acceptedMint: PublicKey;
  acceptedTokenProgram: PublicKey;
  recipients: { treasury: PublicKey; stakingVault: PublicKey };
  referenceMarket: PublicKey;
  soldAmount: bigint;
  maxPaymentAmount: bigint;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const soldIsBase = params.soldAsset === "base";
  const soldMint = soldIsBase ? m.baseMint : m.quoteMint;
  const soldTokenProgram = soldIsBase ? m.baseTokenProgram : m.quoteTokenProgram;
  const instructions: TransactionInstruction[] = [];
  const bidderPayment = await ataInstructionIfMissing({
    payer: params.bidder,
    owner: params.bidder,
    mint: params.acceptedMint,
    tokenProgram: params.acceptedTokenProgram,
  });
  if (bidderPayment.instruction) instructions.push(bidderPayment.instruction);
  const bidderReceive = await ataInstructionIfMissing({
    payer: params.bidder,
    owner: params.bidder,
    mint: soldMint,
    tokenProgram: soldTokenProgram,
  });
  if (bidderReceive.instruction) instructions.push(bidderReceive.instruction);
  const treasuryPayment = await ataInstructionIfMissing({
    payer: params.bidder,
    owner: params.recipients.treasury,
    mint: params.acceptedMint,
    tokenProgram: params.acceptedTokenProgram,
  });
  if (treasuryPayment.instruction) instructions.push(treasuryPayment.instruction);
  const stakingPayment = await ataInstructionIfMissing({
    payer: params.bidder,
    owner: params.recipients.stakingVault,
    mint: params.acceptedMint,
    tokenProgram: params.acceptedTokenProgram,
  });
  if (stakingPayment.instruction) instructions.push(stakingPayment.instruction);
  instructions.push(
    await program.methods
      .settleProtocolAuction({
        lane: protocolAuctionLaneArg(params.lane),
        source: protocolRevenueSourceArg(params.source),
        soldAmount: toBN(params.soldAmount),
        maxPaymentAmount: toBN(params.maxPaymentAmount),
      })
      .accounts({
        bidder: params.bidder,
        market: m.market,
        futarchyAuthority: m.futarchyAuthority,
        soldMint,
        acceptedMint: params.acceptedMint,
        soldVault: protocolRevenueVault(m, params.source, soldIsBase),
        bidderPaymentAccount: bidderPayment.address,
        bidderReceiveAccount: bidderReceive.address,
        treasuryPaymentAccount: treasuryPayment.address,
        stakingVaultPaymentAccount: stakingPayment.address,
        referenceMarket: params.referenceMarket,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: m.eventAuthority,
        program: PROGRAM_ID,
      })
      .instruction()
  );
  return serializeOwnerTransaction(params.bidder, instructions);
}

function marketConfigFromBody(config: Record<string, unknown>) {
  const amm = (config.amm as Record<string, unknown> | undefined) ?? defaultAmmConfig();
  const defaultIrm = defaultMarketConfig().irm;
  const irm = (config.irm as Record<string, unknown> | undefined) ?? defaultIrm;
  return {
    swapFeeBps: Number(config.swapFeeBps),
    divergenceFeeShareCapBps: Number(config.divergenceFeeShareCapBps ?? 0),
    volatilityFeeShareCapBps: Number(config.volatilityFeeShareCapBps ?? 0),
    targetHlpLeverageBps: Number(config.targetHlpLeverageBps),
    settlementDivergenceBps: Number(config.settlementDivergenceBps),
    emaHalfLifeMs: toBN(String(config.emaHalfLifeMs)),
    directionalEmaHalfLifeMs: toBN(String(config.directionalEmaHalfLifeMs)),
    curveDepthEmaHalfLifeMs: toBN(String(config.curveDepthEmaHalfLifeMs)),
    maxDailyBorrowBps: Number(config.maxDailyBorrowBps),
    globalHealthContributionCapBps: Number(config.globalHealthContributionCapBps),
    borrowMarketHealthFloorBps: Number(config.borrowMarketHealthFloorBps),
    amm: {
      peakAmplificationNad: toBN(String(amm.peakAmplificationNad)),
      coreHalfWidthBps: Number(amm.coreHalfWidthBps),
      fadeWidthBps: Number(amm.fadeWidthBps),
      centerEmaHalfLifeMs: toBN(String(amm.centerEmaHalfLifeMs)),
      volatilityHalfLifeMs: toBN(String(amm.volatilityHalfLifeMs)),
      adjustmentThresholdNad: toBN(String(amm.adjustmentThresholdNad)),
      adjustmentStepNad: toBN(String(amm.adjustmentStepNad)),
      minAdjustmentIntervalSlots: toBN(String(amm.minAdjustmentIntervalSlots)),
      volatilityShockCapNad: toBN(String(amm.volatilityShockCapNad)),
      volatilityCapNad: toBN(String(amm.volatilityCapNad)),
      divergenceFeeCoefficientNad: toBN(String(amm.divergenceFeeCoefficientNad)),
      volatilityFeeCoefficientNad: toBN(String(amm.volatilityFeeCoefficientNad)),
      reserved: Array.isArray(amm.reserved) ? amm.reserved : Array(33).fill(0),
    },
    irm: {
      targetUtilizationBps: Number(
        irm.targetUtilizationBps ?? defaultIrm.targetUtilizationBps
      ),
      curveSteepnessNad: toBN(
        String(irm.curveSteepnessNad ?? defaultIrm.curveSteepnessNad)
      ),
      adjustmentSpeedPerYear: toBN(
        String(irm.adjustmentSpeedPerYear ?? defaultIrm.adjustmentSpeedPerYear)
      ),
    },
    startTime: toBN(String(config.startTime)),
  };
}

async function buildRepayTx(params: {
  owner: PublicKey;
  market: StoredMarket;
  positionId: PublicKey;
  repayAsset: MarketAsset;
  repayAmount: bigint;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const isBase = params.repayAsset === "base";
  const debtMint = isBase ? m.baseMint : m.quoteMint;
  const referral = await borrowPositionReferralAccounts(
    m.market,
    params.positionId,
    params.repayAsset,
    debtMint
  );
  const instructions: TransactionInstruction[] = [];
  const ownerDebt = await maybeAddAta(
    instructions,
    params.owner,
    debtMint,
    isBase ? m.baseTokenProgram : m.quoteTokenProgram
  );
  instructions.push(
    await program.methods
      .repay({
        repayAmount: toBN(params.repayAmount),
      })
      .accounts({
        market: m.market,
        futarchyAuthority: m.futarchyAuthority,
        owner: params.owner,
        debtAssetMint: debtMint,
        reserveVault: isBase ? m.baseReserveVault : m.quoteReserveVault,
        interestVault: isBase ? m.baseInterestVault : m.quoteInterestVault,
        ownerDebtAccount: ownerDebt,
        borrowPosition: deriveBorrowPosition(m.market, params.positionId),
        referralPartner: referral.referralPartner,
        referralAccrual: referral.referralAccrual,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: m.eventAuthority,
        program: PROGRAM_ID,
      })
      .instruction()
  );
  return serializeOwnerTransaction(params.owner, instructions);
}

async function buildOpenLeverageTx(params: {
  owner: PublicKey;
  market: StoredMarket;
  positionId: PublicKey;
  debtAsset: MarketAsset;
  marginAmount: bigint;
  multiplierBps: bigint;
  minCollateralOut: bigint;
  referrer: PublicKey | null;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const debtIsBase = params.debtAsset === "base";
  const debtMint = debtIsBase ? m.baseMint : m.quoteMint;
  const collateralMint = debtIsBase ? m.quoteMint : m.baseMint;
  const debtTokenProgram = debtIsBase ? m.baseTokenProgram : m.quoteTokenProgram;
  const instructions: TransactionInstruction[] = [];
  const ownerDebtAccount = await maybeAddAta(instructions, params.owner, debtMint, debtTokenProgram);
  const referralPartner = params.referrer ? deriveReferralPartner(params.referrer) : null;
  let referralAccrual: PublicKey | null = null;
  if (referralPartner) {
    const initialized = await buildInitializeReferralAccrualInstruction({
      payer: params.owner,
      market: m.market,
      assetMint: debtMint,
      referralPartner,
    });
    referralAccrual = initialized.referralAccrual;
    instructions.push(initialized.instruction);
  }
  const leveragePosition = deriveLeveragePosition(m.market, params.positionId);
  const leverageCollateralVault = deriveLeverageCollateralVault(m.market, collateralMint);
  instructions.push(
    await program.methods
      .openLeverage({
        positionId: params.positionId,
        debtAsset: debtIsBase ? 0 : 1,
        marginAmount: toBN(params.marginAmount),
        multiplierBps: toBN(params.multiplierBps),
        minCollateralOut: toBN(params.minCollateralOut),
        referrer: params.referrer,
        positionOwner: null,
        limitPriceNad: new BN(0),
      })
      .accounts({
        market: m.market,
        futarchyAuthority: m.futarchyAuthority,
        owner: params.owner,
        payer: params.owner,
        leveragePosition,
        debtMint,
        collateralMint,
        debtReserveVault: debtIsBase ? m.baseReserveVault : m.quoteReserveVault,
        collateralReserveVault: debtIsBase ? m.quoteReserveVault : m.baseReserveVault,
        leverageCollateralVault,
        ownerDebtAccount,
        referralPartner,
        referralAccrual,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
        eventAuthority: m.eventAuthority,
        program: PROGRAM_ID,
      })
      .instruction()
  );
  return {
    transaction: await serializeOwnerTransaction(params.owner, instructions),
    leveragePosition,
    leverageCollateralVault,
  };
}

async function buildIncreaseLeverageTx(params: {
  owner: PublicKey;
  market: StoredMarket;
  positionId: PublicKey;
  debtAsset: MarketAsset;
  debtAmount: bigint;
  minCollateralOut: bigint;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const debtIsBase = params.debtAsset === "base";
  const debtMint = debtIsBase ? m.baseMint : m.quoteMint;
  const collateralMint = debtIsBase ? m.quoteMint : m.baseMint;
  const leveragePosition = deriveLeveragePosition(m.market, params.positionId);
  const leverageCollateralVault = deriveLeverageCollateralVault(m.market, collateralMint);
  const instruction = await program.methods
      .increaseLeverage({
        debtAsset: debtIsBase ? 0 : 1,
        debtAmount: toBN(params.debtAmount),
        minCollateralOut: toBN(params.minCollateralOut),
      })
      .accounts({
        market: m.market,
        futarchyAuthority: m.futarchyAuthority,
        positionOwner: params.owner,
        leveragePosition,
        debtMint,
        collateralMint,
        debtReserveVault: debtIsBase ? m.baseReserveVault : m.quoteReserveVault,
        collateralReserveVault: debtIsBase ? m.quoteReserveVault : m.baseReserveVault,
        leverageCollateralVault,
        owner: params.owner,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: m.eventAuthority,
        program: PROGRAM_ID,
      })
      .instruction();
  return serializeOwnerTransaction(params.owner, [instruction]);
}

async function buildDecreaseLeverageTx(params: {
  owner: PublicKey;
  market: StoredMarket;
  positionId: PublicKey;
  debtAsset: MarketAsset;
  collateralAmount: bigint;
  minRepayOut: bigint;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const debtIsBase = params.debtAsset === "base";
  const debtMint = debtIsBase ? m.baseMint : m.quoteMint;
  const collateralMint = debtIsBase ? m.quoteMint : m.baseMint;
  const leveragePosition = deriveLeveragePosition(m.market, params.positionId);
  const referral = await leveragePositionReferralAccounts(
    m.market,
    params.positionId,
    debtMint
  );
  const leverageCollateralVault = deriveLeverageCollateralVault(m.market, collateralMint);
  const instruction = await program.methods
    .decreaseLeverage({
      debtAsset: debtIsBase ? 0 : 1,
      collateralAmount: toBN(params.collateralAmount),
      minRepayOut: toBN(params.minRepayOut),
    })
    .accounts({
      market: m.market,
      futarchyAuthority: m.futarchyAuthority,
      positionOwner: params.owner,
      leveragePosition,
      debtMint,
      collateralMint,
      debtReserveVault: debtIsBase ? m.baseReserveVault : m.quoteReserveVault,
      collateralReserveVault: debtIsBase ? m.quoteReserveVault : m.baseReserveVault,
      debtInterestVault: debtIsBase ? m.baseInterestVault : m.quoteInterestVault,
      leverageCollateralVault,
      referralPartner: referral.referralPartner,
      referralAccrual: referral.referralAccrual,
      owner: params.owner,
      tokenProgram: TOKEN_PROGRAM_ID,
      token2022Program: TOKEN_2022_PROGRAM_ID,
      eventAuthority: m.eventAuthority,
      program: PROGRAM_ID,
    })
    .instruction();
  return serializeOwnerTransaction(params.owner, [instruction]);
}

async function buildAddLeverageMarginTx(params: {
  owner: PublicKey;
  market: StoredMarket;
  positionId: PublicKey;
  debtAsset: MarketAsset;
  amount: bigint;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const debtIsBase = params.debtAsset === "base";
  const debtMint = debtIsBase ? m.baseMint : m.quoteMint;
  const debtTokenProgram = debtIsBase ? m.baseTokenProgram : m.quoteTokenProgram;
  const instructions: TransactionInstruction[] = [];
  const referral = await leveragePositionReferralAccounts(
    m.market,
    params.positionId,
    debtMint
  );
  const ownerDebtAccount = await maybeAddAta(instructions, params.owner, debtMint, debtTokenProgram);
  instructions.push(
    await program.methods
      .addLeverageMargin({ debtAsset: debtIsBase ? 0 : 1, amount: toBN(params.amount) })
      .accounts({
        market: m.market,
        futarchyAuthority: m.futarchyAuthority,
        positionOwner: params.owner,
        leveragePosition: deriveLeveragePosition(m.market, params.positionId),
        debtMint,
        debtReserveVault: debtIsBase ? m.baseReserveVault : m.quoteReserveVault,
        debtInterestVault: debtIsBase ? m.baseInterestVault : m.quoteInterestVault,
        ownerDebtAccount,
        referralPartner: referral.referralPartner,
        referralAccrual: referral.referralAccrual,
        owner: params.owner,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: m.eventAuthority,
        program: PROGRAM_ID,
      })
      .instruction()
  );
  return serializeOwnerTransaction(params.owner, instructions);
}

async function buildRemoveLeverageMarginTx(params: {
  owner: PublicKey;
  market: StoredMarket;
  positionId: PublicKey;
  debtAsset: MarketAsset;
  amount: bigint;
  minAmountOut: bigint;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const debtIsBase = params.debtAsset === "base";
  const debtMint = debtIsBase ? m.baseMint : m.quoteMint;
  const collateralMint = debtIsBase ? m.quoteMint : m.baseMint;
  const debtTokenProgram = debtIsBase ? m.baseTokenProgram : m.quoteTokenProgram;
  const instructions: TransactionInstruction[] = [];
  const ownerDebtAccount = await maybeAddAta(instructions, params.owner, debtMint, debtTokenProgram);
  instructions.push(
    await program.methods
      .removeLeverageMargin({
        debtAsset: debtIsBase ? 0 : 1,
        amount: toBN(params.amount),
        minAmountOut: toBN(params.minAmountOut),
      })
      .accounts({
        market: m.market,
        futarchyAuthority: m.futarchyAuthority,
        positionOwner: params.owner,
        leveragePosition: deriveLeveragePosition(m.market, params.positionId),
        debtMint,
        collateralMint,
        debtReserveVault: debtIsBase ? m.baseReserveVault : m.quoteReserveVault,
        ownerDebtAccount,
        owner: params.owner,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: m.eventAuthority,
        program: PROGRAM_ID,
      })
      .instruction()
  );
  return serializeOwnerTransaction(params.owner, instructions);
}

async function buildCloseLeverageTx(params: {
  owner: PublicKey;
  market: StoredMarket;
  positionId: PublicKey;
  debtAsset: MarketAsset;
  minAmountOut: bigint;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const debtIsBase = params.debtAsset === "base";
  const debtMint = debtIsBase ? m.baseMint : m.quoteMint;
  const collateralMint = debtIsBase ? m.quoteMint : m.baseMint;
  const debtTokenProgram = debtIsBase ? m.baseTokenProgram : m.quoteTokenProgram;
  const instructions: TransactionInstruction[] = [];
  const ownerDebtAccount = await maybeAddAta(instructions, params.owner, debtMint, debtTokenProgram);
  const leveragePosition = deriveLeveragePosition(m.market, params.positionId);
  const referral = await leveragePositionReferralAccounts(
    m.market,
    params.positionId,
    debtMint
  );
  instructions.push(
    await program.methods
      .closeLeverage({ debtAsset: debtIsBase ? 0 : 1, minAmountOut: toBN(params.minAmountOut) })
      .accounts({
        market: m.market,
        futarchyAuthority: m.futarchyAuthority,
        positionOwner: params.owner,
        leveragePosition,
        debtMint,
        collateralMint,
        debtReserveVault: debtIsBase ? m.baseReserveVault : m.quoteReserveVault,
        collateralReserveVault: debtIsBase ? m.quoteReserveVault : m.baseReserveVault,
        debtInterestVault: debtIsBase ? m.baseInterestVault : m.quoteInterestVault,
        leverageCollateralVault: deriveLeverageCollateralVault(m.market, collateralMint),
        ownerDebtAccount,
        referralPartner: referral.referralPartner,
        referralAccrual: referral.referralAccrual,
        leverageDelegation: null,
        delegatedProgram: null,
        authority: params.owner,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: m.eventAuthority,
        program: PROGRAM_ID,
      })
      .instruction()
  );
  return serializeOwnerTransaction(params.owner, instructions);
}

async function buildCreateLeverageDelegationTx(params: {
  owner: PublicKey;
  market: StoredMarket;
  positionId: PublicKey;
  debtAsset: MarketAsset;
  delegatedProgram: PublicKey;
  approvedActions: number;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const leveragePosition = deriveLeveragePosition(m.market, params.positionId);
  const leverageDelegation = deriveLeverageDelegation(leveragePosition);
  const instruction = await program.methods
    .createLeverageDelegation({
      debtAsset: params.debtAsset === "base" ? 0 : 1,
      delegatedProgram: params.delegatedProgram,
      approvedActions: params.approvedActions,
    })
    .accounts({
      market: m.market,
      leveragePosition,
      leverageDelegation,
      owner: params.owner,
      systemProgram: SystemProgram.programId,
      eventAuthority: m.eventAuthority,
      program: PROGRAM_ID,
    })
    .instruction();
  return { transaction: await serializeOwnerTransaction(params.owner, [instruction]), leverageDelegation };
}

async function buildUpdateLeverageDelegationTx(params: {
  owner: PublicKey;
  market: StoredMarket;
  positionId: PublicKey;
  debtAsset: MarketAsset;
  delegatedProgram: PublicKey;
  approvedActions: number;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const leveragePosition = deriveLeveragePosition(m.market, params.positionId);
  const leverageDelegation = deriveLeverageDelegation(leveragePosition);
  const instruction = await program.methods
    .updateLeverageDelegation({
      debtAsset: params.debtAsset === "base" ? 0 : 1,
      delegatedProgram: params.delegatedProgram,
      approvedActions: params.approvedActions,
    })
    .accounts({
      market: m.market,
      leveragePosition,
      leverageDelegation,
      owner: params.owner,
      eventAuthority: m.eventAuthority,
      program: PROGRAM_ID,
    })
    .instruction();
  return { transaction: await serializeOwnerTransaction(params.owner, [instruction]), leverageDelegation };
}

async function buildCloseLeverageDelegationTx(params: {
  owner: PublicKey;
  market: StoredMarket;
  positionId: PublicKey;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const leveragePosition = deriveLeveragePosition(m.market, params.positionId);
  const leverageDelegation = deriveLeverageDelegation(leveragePosition);
  const instruction = await program.methods
    .closeLeverageDelegation({ position: leveragePosition })
    .accounts({ leverageDelegation, owner: params.owner })
    .instruction();
  return { transaction: await serializeOwnerTransaction(params.owner, [instruction]), leverageDelegation };
}

async function buildCreateLeverageOrderTx(params: {
  owner: PublicKey;
  market: StoredMarket;
  positionId: PublicKey;
  orderId: bigint;
  kind: number;
  triggerCloseoutPriceNad: bigint;
}) {
  const delegateProgram = getLeverageDelegateProgram();
  const m = marketFromStored(params.market);
  const leveragePosition = deriveLeveragePosition(m.market, params.positionId);
  const order = deriveLeverageOrder(leveragePosition, params.owner, params.orderId);
  const instruction = await delegateProgram.methods
    .createLeverageOrder({
      orderId: toBN(params.orderId),
      kind: params.kind,
      triggerCloseoutPriceNad: toBN(params.triggerCloseoutPriceNad),
    })
    .accounts({
      market: m.market,
      leveragePosition,
      order,
      owner: params.owner,
      systemProgram: SystemProgram.programId,
    })
    .instruction();
  return { transaction: await serializeOwnerTransaction(params.owner, [instruction]), order };
}

async function buildUpdateLeverageOrderTx(params: {
  owner: PublicKey;
  market: StoredMarket;
  positionId: PublicKey;
  orderId: bigint;
  kind: number;
  triggerCloseoutPriceNad: bigint;
}) {
  const delegateProgram = getLeverageDelegateProgram();
  const m = marketFromStored(params.market);
  const leveragePosition = deriveLeveragePosition(m.market, params.positionId);
  const order = deriveLeverageOrder(leveragePosition, params.owner, params.orderId);
  const instruction = await delegateProgram.methods
    .updateLeverageOrder({
      orderId: toBN(params.orderId),
      kind: params.kind,
      triggerCloseoutPriceNad: toBN(params.triggerCloseoutPriceNad),
    })
    .accounts({ market: m.market, leveragePosition, order, owner: params.owner })
    .instruction();
  return { transaction: await serializeOwnerTransaction(params.owner, [instruction]), order };
}

async function buildDelegatedCloseLeverageTx(params: {
  executor: PublicKey;
  positionOwner: PublicKey;
  market: StoredMarket;
  positionId: PublicKey;
  debtAsset: MarketAsset;
  orderId: bigint;
  minAmountOut: bigint;
}) {
  const { program } = initializeRuntime();
  const delegateProgram = getLeverageDelegateProgram();
  const m = marketFromStored(params.market);
  const debtIsBase = params.debtAsset === "base";
  const debtMint = debtIsBase ? m.baseMint : m.quoteMint;
  const collateralMint = debtIsBase ? m.quoteMint : m.baseMint;
  const debtTokenProgram = debtIsBase ? m.baseTokenProgram : m.quoteTokenProgram;
  const leveragePosition = deriveLeveragePosition(m.market, params.positionId);
  const referral = await leveragePositionReferralAccounts(
    m.market,
    params.positionId,
    debtMint
  );
  const leverageDelegation = deriveLeverageDelegation(leveragePosition);
  const order = deriveLeverageOrder(leveragePosition, params.positionOwner, params.orderId);
  const instructions: TransactionInstruction[] = [];

  const custodyAccount = await ataInstructionIfMissing({
    payer: params.executor,
    owner: order,
    mint: debtMint,
    tokenProgram: debtTokenProgram,
    allowOwnerOffCurve: true,
  });
  if (custodyAccount.instruction) instructions.push(custodyAccount.instruction);
  const executorAccount = await ataInstructionIfMissing({
    payer: params.executor,
    owner: params.executor,
    mint: debtMint,
    tokenProgram: debtTokenProgram,
  });
  if (executorAccount.instruction) instructions.push(executorAccount.instruction);
  const ownerAccount = await ataInstructionIfMissing({
    payer: params.executor,
    owner: params.positionOwner,
    mint: debtMint,
    tokenProgram: debtTokenProgram,
  });
  if (ownerAccount.instruction) instructions.push(ownerAccount.instruction);

  const beforeInstruction = await delegateProgram.methods
    .beforeTakeProfit({ orderId: toBN(params.orderId) })
    .accounts({
      order,
      market: m.market,
      leveragePosition,
      leverageDelegation,
      custodyTokenAccount: custodyAccount.address,
      tokenMint: debtMint,
      executor: params.executor,
    })
    .instruction();
  const afterInstruction = await delegateProgram.methods
    .afterCloseOrder({ orderId: toBN(params.orderId) })
    .accounts({
      order,
      owner: params.positionOwner,
      leveragePosition,
      leverageDelegation,
      custodyTokenAccount: custodyAccount.address,
      executorTokenAccount: executorAccount.address,
      ownerTokenAccount: ownerAccount.address,
      tokenMint: debtMint,
      executor: params.executor,
      tokenProgram: TOKEN_PROGRAM_ID,
      token2022Program: TOKEN_2022_PROGRAM_ID,
    })
    .instruction();

  instructions.push(
    await program.methods
      .delegatedCloseLeverage({
        debtAsset: debtIsBase ? 0 : 1,
        minAmountOut: toBN(params.minAmountOut),
        delegated: {
          beforeIxData: Buffer.from(beforeInstruction.data),
          afterIxData: Buffer.from(afterInstruction.data),
          beforeAccountsLen: beforeInstruction.keys.length,
        },
      })
      .accounts({
        market: m.market,
        futarchyAuthority: m.futarchyAuthority,
        positionOwner: params.positionOwner,
        leveragePosition,
        debtMint,
        collateralMint,
        debtReserveVault: debtIsBase ? m.baseReserveVault : m.quoteReserveVault,
        collateralReserveVault: debtIsBase ? m.quoteReserveVault : m.baseReserveVault,
        debtInterestVault: debtIsBase ? m.baseInterestVault : m.quoteInterestVault,
        leverageCollateralVault: deriveLeverageCollateralVault(m.market, collateralMint),
        ownerDebtAccount: custodyAccount.address,
        referralPartner: referral.referralPartner,
        referralAccrual: referral.referralAccrual,
        leverageDelegation,
        delegatedProgram: LEVERAGE_DELEGATE_PROGRAM_ID,
        authority: params.executor,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: m.eventAuthority,
        program: PROGRAM_ID,
      })
      .remainingAccounts([...beforeInstruction.keys, ...afterInstruction.keys])
      .instruction()
  );

  return {
    transaction: await serializeOwnerTransaction(params.executor, instructions),
    leveragePosition,
    leverageDelegation,
    order,
    custodyTokenAccount: custodyAccount.address,
    executorTokenAccount: executorAccount.address,
    ownerTokenAccount: ownerAccount.address,
  };
}

async function buildLiquidateLeveragePositionTx(params: {
  liquidator: PublicKey;
  positionOwner: PublicKey;
  market: StoredMarket;
  positionId: PublicKey;
  debtAsset: MarketAsset;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const debtIsBase = params.debtAsset === "base";
  const debtMint = debtIsBase ? m.baseMint : m.quoteMint;
  const collateralMint = debtIsBase ? m.quoteMint : m.baseMint;
  const debtTokenProgram = debtIsBase ? m.baseTokenProgram : m.quoteTokenProgram;
  const instructions: TransactionInstruction[] = [];
  const liquidatorDebtAccount = await maybeAddAta(
    instructions,
    params.liquidator,
    debtMint,
    debtTokenProgram
  );
  const ownerDebtAccountResult = await ataInstructionIfMissing({
    payer: params.liquidator,
    owner: params.positionOwner,
    mint: debtMint,
    tokenProgram: debtTokenProgram,
  });
  if (ownerDebtAccountResult.instruction) instructions.push(ownerDebtAccountResult.instruction);
  const leveragePosition = deriveLeveragePosition(m.market, params.positionId);
  const referral = await leveragePositionReferralAccounts(
    m.market,
    params.positionId,
    debtMint
  );
  instructions.push(
    await program.methods
      .liquidateLeveragePosition({ debtAsset: debtIsBase ? 0 : 1 })
      .accounts({
        market: m.market,
        futarchyAuthority: m.futarchyAuthority,
        positionOwner: params.positionOwner,
        leveragePosition,
        debtMint,
        collateralMint,
        debtReserveVault: debtIsBase ? m.baseReserveVault : m.quoteReserveVault,
        collateralReserveVault: debtIsBase ? m.quoteReserveVault : m.baseReserveVault,
        debtInterestVault: debtIsBase ? m.baseInterestVault : m.quoteInterestVault,
        leverageCollateralVault: deriveLeverageCollateralVault(m.market, collateralMint),
        liquidatorDebtAccount,
        ownerDebtAccount: ownerDebtAccountResult.address,
        referralPartner: referral.referralPartner,
        referralAccrual: referral.referralAccrual,
        liquidator: params.liquidator,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: m.eventAuthority,
        program: PROGRAM_ID,
      })
      .instruction()
  );
  return {
    transaction: await serializeOwnerTransaction(params.liquidator, instructions),
    leveragePosition,
    liquidatorDebtAccount,
    ownerDebtAccount: ownerDebtAccountResult.address,
  };
}

async function buildStartLiquidationAuctionTx(params: {
  payer: PublicKey;
  market: StoredMarket;
  positionId: PublicKey;
  debtAsset: MarketAsset;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const instruction = await program.methods
    .startLiquidationAuction()
    .accounts({
      market: m.market,
      borrowPosition: deriveBorrowPosition(m.market, params.positionId),
      debtAssetMint: params.debtAsset === "base" ? m.baseMint : m.quoteMint,
    })
    .instruction();
  return serializeOwnerTransaction(params.payer, [instruction]);
}

async function buildFillLiquidationAuctionTx(params: {
  liquidator: PublicKey;
  market: StoredMarket;
  positionId: PublicKey;
  debtAsset: MarketAsset;
  repayAmount: bigint;
  minCollateralOut: bigint;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const debtIsBase = params.debtAsset === "base";
  const debtMint = debtIsBase ? m.baseMint : m.quoteMint;
  const collateralMint = debtIsBase ? m.quoteMint : m.baseMint;
  const borrowPosition = deriveBorrowPosition(m.market, params.positionId);
  const position = await program.account.borrowPosition.fetch(borrowPosition);
  const positionOwner = field<PublicKey>(position, "owner");
  const referral = await borrowPositionReferralAccounts(
    m.market,
    params.positionId,
    params.debtAsset,
    debtMint
  );
  const instructions: TransactionInstruction[] = [];
  const liquidatorDebtAccount = await maybeAddAta(
    instructions,
    params.liquidator,
    debtMint,
    debtIsBase ? m.baseTokenProgram : m.quoteTokenProgram
  );
  const liquidatorCollateralAccount = await maybeAddAta(
    instructions,
    params.liquidator,
    collateralMint,
    debtIsBase ? m.quoteTokenProgram : m.baseTokenProgram
  );
  instructions.push(
    await program.methods
      .fillLiquidationAuction({
        repayAmount: toBN(params.repayAmount),
        minCollateralOut: toBN(params.minCollateralOut),
      })
      .accounts({
        market: m.market,
        futarchyAuthority: m.futarchyAuthority,
        positionOwner,
        liquidator: params.liquidator,
        debtAssetMint: debtMint,
        collateralAssetMint: collateralMint,
        reserveVault: debtIsBase ? m.baseReserveVault : m.quoteReserveVault,
        interestVault: debtIsBase ? m.baseInterestVault : m.quoteInterestVault,
        collateralVault: debtIsBase ? m.quoteCollateralVault : m.baseCollateralVault,
        insuranceVault: debtIsBase ? m.baseInsuranceVault : m.quoteInsuranceVault,
        collateralInsuranceVault: debtIsBase ? m.quoteInsuranceVault : m.baseInsuranceVault,
        liquidatorDebtAccount,
        liquidatorCollateralAccount,
        borrowPosition,
        referralPartner: referral.referralPartner,
        referralAccrual: referral.referralAccrual,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: m.eventAuthority,
        program: PROGRAM_ID,
      })
      .instruction()
  );
  return serializeOwnerTransaction(params.liquidator, instructions);
}

async function buildBackstopLiquidationAuctionTx(params: {
  liquidator: PublicKey;
  market: StoredMarket;
  positionId: PublicKey;
  debtAsset: MarketAsset;
  minCallerBountyOut: bigint;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const debtIsBase = params.debtAsset === "base";
  const debtMint = debtIsBase ? m.baseMint : m.quoteMint;
  const collateralMint = debtIsBase ? m.quoteMint : m.baseMint;
  const borrowPosition = deriveBorrowPosition(m.market, params.positionId);
  const position = await program.account.borrowPosition.fetch(borrowPosition);
  const positionOwner = field<PublicKey>(position, "owner");
  const referral = await borrowPositionReferralAccounts(
    m.market,
    params.positionId,
    params.debtAsset,
    debtMint
  );
  const instructions: TransactionInstruction[] = [];
  const liquidatorCollateralAccount = await maybeAddAta(
    instructions,
    params.liquidator,
    collateralMint,
    debtIsBase ? m.quoteTokenProgram : m.baseTokenProgram
  );
  const ownerDebtAta = await ataInstructionIfMissing({
    payer: params.liquidator,
    owner: positionOwner,
    mint: debtMint,
    tokenProgram: debtIsBase ? m.baseTokenProgram : m.quoteTokenProgram,
  });
  if (ownerDebtAta.instruction) instructions.push(ownerDebtAta.instruction);
  let builder = program.methods
      .backstopLiquidationAuction({ minCallerBountyOut: toBN(params.minCallerBountyOut) })
      .accounts({
        market: m.market,
        futarchyAuthority: m.futarchyAuthority,
        positionOwner,
        liquidator: params.liquidator,
        debtAssetMint: debtMint,
        collateralAssetMint: collateralMint,
        debtReserveVault: debtIsBase ? m.baseReserveVault : m.quoteReserveVault,
        collateralReserveVault: debtIsBase ? m.quoteReserveVault : m.baseReserveVault,
        interestVault: debtIsBase ? m.baseInterestVault : m.quoteInterestVault,
        collateralVault: debtIsBase ? m.quoteCollateralVault : m.baseCollateralVault,
        insuranceVault: debtIsBase ? m.baseInsuranceVault : m.quoteInsuranceVault,
        liquidatorCollateralAccount,
        ownerDebtAccount: ownerDebtAta.address,
        borrowPosition,
        referralPartner: referral.referralPartner,
        referralAccrual: referral.referralAccrual,
        instructionsSysvar: SYSVAR_INSTRUCTIONS_PUBKEY,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: m.eventAuthority,
        program: PROGRAM_ID,
      });
  const remainingAccounts = await activeHlpSwapAccounts(m);
  if (remainingAccounts.length > 0) builder = builder.remainingAccounts(remainingAccounts);
  instructions.push(await builder.instruction());
  return serializeOwnerTransaction(params.liquidator, instructions);
}

async function buildDepositSingleSidedTx(params: {
  owner: PublicKey;
  market: StoredMarket;
  targetAsset: MarketAsset;
  depositAmount: bigint;
  minHlpAmount: bigint;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const isBase = params.targetAsset === "base";
  const instructions: TransactionInstruction[] = [];
  const ownerTarget = await maybeAddAta(
    instructions,
    params.owner,
    isBase ? m.baseMint : m.quoteMint,
    isBase ? m.baseTokenProgram : m.quoteTokenProgram
  );
  const ownerHlp = await maybeAddAta(
    instructions,
    params.owner,
    isBase ? m.baseHlpMint : m.quoteHlpMint,
    TOKEN_2022_PROGRAM_ID
  );
  const targetHlpMint = isBase ? m.baseHlpMint : m.quoteHlpMint;
  const baseYieldAccount = deriveYieldAccount(m.market, params.owner, targetHlpMint, m.baseMint, "hlp");
  const quoteYieldAccount = deriveYieldAccount(m.market, params.owner, targetHlpMint, m.quoteMint, "hlp");
  instructions.push(
    await program.methods
      .initializeYieldAccounts({ owner: params.owner, tokenKind: { hlp: {} } })
      .accounts({
        payer: params.owner,
        market: m.market,
        lpMint: targetHlpMint,
        baseMint: m.baseMint,
        quoteMint: m.quoteMint,
        baseYieldAccount,
        quoteYieldAccount,
        systemProgram: SystemProgram.programId,
      })
      .instruction()
  );
  instructions.push(
    await program.methods
      .depositSingleSided({
        depositAmount: toBN(params.depositAmount),
        minHlpAmount: toBN(params.minHlpAmount),
      })
      .accounts({
        market: m.market,
        futarchyAuthority: m.futarchyAuthority,
        owner: params.owner,
        baseMint: m.baseMint,
        quoteMint: m.quoteMint,
        ylpMint: m.ylpMint,
        targetHlpMint,
        baseReserveVault: m.baseReserveVault,
        quoteReserveVault: m.quoteReserveVault,
        ownerTargetAccount: ownerTarget,
        ownerHlpAccount: ownerHlp,
        hlpYlpAccount: isBase ? m.baseHlpYlpVault : m.quoteHlpYlpVault,
        baseYieldAccount,
        quoteYieldAccount,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
        eventAuthority: m.eventAuthority,
        program: PROGRAM_ID,
      })
      .instruction()
  );
  return serializeOwnerTransaction(params.owner, instructions);
}

async function buildWithdrawSingleSidedTx(params: {
  owner: PublicKey;
  market: StoredMarket;
  targetAsset: MarketAsset;
  hlpAmount: bigint;
  minTargetAmountOut: bigint;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const isBase = params.targetAsset === "base";
  const instructions: TransactionInstruction[] = [];
  const ownerTarget = await maybeAddAta(
    instructions,
    params.owner,
    isBase ? m.baseMint : m.quoteMint,
    isBase ? m.baseTokenProgram : m.quoteTokenProgram
  );
  const ownerHlp = await maybeAddAta(
    instructions,
    params.owner,
    isBase ? m.baseHlpMint : m.quoteHlpMint,
    TOKEN_2022_PROGRAM_ID
  );
  const targetHlpMint = isBase ? m.baseHlpMint : m.quoteHlpMint;
  const baseYieldAccount = deriveYieldAccount(m.market, params.owner, targetHlpMint, m.baseMint, "hlp");
  const quoteYieldAccount = deriveYieldAccount(m.market, params.owner, targetHlpMint, m.quoteMint, "hlp");
  instructions.push(
    await program.methods
      .initializeYieldAccounts({ owner: params.owner, tokenKind: { hlp: {} } })
      .accounts({
        payer: params.owner,
        market: m.market,
        lpMint: targetHlpMint,
        baseMint: m.baseMint,
        quoteMint: m.quoteMint,
        baseYieldAccount,
        quoteYieldAccount,
        systemProgram: SystemProgram.programId,
      })
      .instruction()
  );
  instructions.push(
    await program.methods
      .withdrawSingleSided({
        hlpAmount: toBN(params.hlpAmount),
        minTargetAmountOut: toBN(params.minTargetAmountOut),
      })
      .accounts({
        market: m.market,
        futarchyAuthority: m.futarchyAuthority,
        owner: params.owner,
        baseMint: m.baseMint,
        quoteMint: m.quoteMint,
        ylpMint: m.ylpMint,
        targetHlpMint,
        baseReserveVault: m.baseReserveVault,
        quoteReserveVault: m.quoteReserveVault,
        borrowedInterestVault: isBase ? m.quoteInterestVault : m.baseInterestVault,
        ownerTargetAccount: ownerTarget,
        ownerHlpAccount: ownerHlp,
        hlpYlpAccount: isBase ? m.baseHlpYlpVault : m.quoteHlpYlpVault,
        baseYieldAccount,
        quoteYieldAccount,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: m.eventAuthority,
        program: PROGRAM_ID,
      })
      .instruction()
  );
  return serializeOwnerTransaction(params.owner, instructions);
}

async function userPositionsPayload(
  wallet: PublicKey,
  stored: StoredMarket,
  positionId: PublicKey | null = null
) {
  const { program } = initializeRuntime();
  const market = new PublicKey(stored.market);
  const now = new Date().toISOString();
  const positions = [];
  if (!positionId) return positions;

  const borrowPositionAddress = deriveBorrowPosition(market, positionId);
  const borrowPosition = await program.account.borrowPosition.fetchNullable(borrowPositionAddress);
  if (borrowPosition) {
    const auctionDebtAssetCode = Number(field(borrowPosition, "auctionDebtAsset", "auction_debt_asset"));
    positions.push({
      id: 1,
      eventType: "borrow_position",
      market: stored.market,
      owner: wallet.toBase58(),
      assetMint: null,
      txSig: "",
      slot: 0,
      instructionIndex: 0,
      instructionPath: "fork-state",
      timestamp: now,
      payload: {
        positionId: positionId.toBase58(),
        address: borrowPositionAddress.toBase58(),
        baseCollateral: stringValue(field(borrowPosition, "baseCollateral", "base_collateral")),
        quoteCollateral: stringValue(field(borrowPosition, "quoteCollateral", "quote_collateral")),
        fixedBaseShares: stringValue(field(borrowPosition, "fixedBaseShares", "fixed_base_shares")),
        fixedQuoteShares: stringValue(field(borrowPosition, "fixedQuoteShares", "fixed_quote_shares")),
        baseLiquidationCfBps: Number(field(borrowPosition, "baseLiquidationCfBps", "base_liquidation_cf_bps")),
        quoteLiquidationCfBps: Number(field(borrowPosition, "quoteLiquidationCfBps", "quote_liquidation_cf_bps")),
        auctionDebtAsset: auctionDebtAssetCode === 0 ? "base" : auctionDebtAssetCode === 1 ? "quote" : null,
        auctionStartTime: stringValue(field(borrowPosition, "auctionStartTime", "auction_start_time")),
        auctionStartPriceNad: stringValue(field(borrowPosition, "auctionStartPriceNad", "auction_start_price_nad")),
        auctionFloorPriceNad: stringValue(field(borrowPosition, "auctionFloorPriceNad", "auction_floor_price_nad")),
      },
    });
  }

  const leveragePositionAddress = deriveLeveragePosition(market, positionId);
  const leveragePosition = await program.account.leveragePosition.fetchNullable(leveragePositionAddress);
  if (leveragePosition) {
    positions.push({
      id: 2,
      eventType: "leverage_position",
      market: stored.market,
      owner: stringValue(field(leveragePosition, "owner")),
      assetMint: null,
      txSig: "",
      slot: 0,
      instructionIndex: 0,
      instructionPath: "fork-state",
      timestamp: now,
      payload: {
        positionId: positionId.toBase58(),
        address: leveragePositionAddress.toBase58(),
        debtAsset: Number(field(leveragePosition, "debtAsset", "debt_asset")),
        collateralAmount: stringValue(field(leveragePosition, "collateralAmount", "collateral_amount")),
        marginAmount: stringValue(field(leveragePosition, "marginAmount", "margin_amount")),
        openNotional: stringValue(field(leveragePosition, "openNotional", "open_notional")),
        debtPrincipal: stringValue(field(leveragePosition, "debtPrincipal", "debt_principal")),
        debtShares: stringValue(field(leveragePosition, "debtShares", "debt_shares")),
        multiplierBps: stringValue(field(leveragePosition, "multiplierBps", "multiplier_bps")),
      },
    });

    const leverageDelegationAddress = deriveLeverageDelegation(leveragePositionAddress);
    const leverageDelegation = await program.account.leverageDelegation.fetchNullable(leverageDelegationAddress);
    if (leverageDelegation) {
      positions.push({
        id: 3,
        eventType: "leverage_delegation",
        market: stored.market,
        owner: stringValue(field(leverageDelegation, "owner")),
        assetMint: null,
        txSig: "",
        slot: 0,
        instructionIndex: 0,
        instructionPath: "fork-state",
        timestamp: now,
        payload: {
          address: leverageDelegationAddress.toBase58(),
          position: leveragePositionAddress.toBase58(),
          debtAsset: Number(field(leverageDelegation, "debtAsset", "debt_asset")),
          delegatedProgram: stringValue(field(leverageDelegation, "delegatedProgram", "delegated_program")),
          approvedActions: Number(field(leverageDelegation, "approvedActions", "approved_actions")),
        },
      });
    }
  }
  return positions;
}

async function fundWallet(body: Record<string, unknown>, stored: StoredMarket) {
  if (!ALLOW_PUBLIC_FUNDING) throw new Error("Public fork wallet funding is disabled");
  const owner = new PublicKey(String(body.wallet ?? body.owner ?? body.publicKey ?? ""));
  const sol = Number(body.sol ?? DEFAULT_SOL_FUNDING);
  const baseAmount = rawAmount(body, ["baseAmount", "baseUiAmount", "tokenAmount"], stored.baseDecimals, DEFAULT_TOKEN_FUNDING_UI);
  const quoteAmount = rawAmount(
    body,
    ["quoteAmount", "quoteUiAmount", "tokenAmount"],
    stored.quoteDecimals,
    DEFAULT_TOKEN_FUNDING_UI
  );
  const maxBaseAmount = parseUnits(MAX_TOKEN_FUNDING_UI, stored.baseDecimals);
  const maxQuoteAmount = parseUnits(MAX_TOKEN_FUNDING_UI, stored.quoteDecimals);
  if (!Number.isFinite(sol) || sol < 0 || sol > MAX_SOL_FUNDING) {
    throw new Error(`Fork SOL funding must be between 0 and ${MAX_SOL_FUNDING}`);
  }
  if (baseAmount > maxBaseAmount || quoteAmount > maxQuoteAmount) {
    throw new Error(`Fork token funding is capped at ${MAX_TOKEN_FUNDING_UI} UI units`);
  }
  await setLamports(owner, sol);
  await setTokenBalance(owner, new PublicKey(stored.baseMint), baseAmount, new PublicKey(stored.baseTokenProgram));
  await setTokenBalance(owner, new PublicKey(stored.quoteMint), quoteAmount, new PublicKey(stored.quoteTokenProgram));
  return {
    wallet: owner.toBase58(),
    sol,
    baseAmount: baseAmount.toString(),
    quoteAmount: quoteAmount.toString(),
    baseMint: stored.baseMint,
    quoteMint: stored.quoteMint,
  };
}

async function txResponse(
  name: string,
  owner: PublicKey,
  stored: StoredMarket,
  transaction: string,
  extra: Record<string, unknown> = {}
) {
  return {
    success: true,
    data: {
      action: name,
      owner: owner.toBase58(),
      market: stored.market,
      rpcUrl: PUBLIC_RPC_URL,
      transaction,
      ...extra,
    },
  };
}

export async function route(req: http.IncomingMessage, body: Record<string, unknown>) {
  const url = new URL(req.url ?? "/", "http://localhost");
  const path = url.pathname.replace(/\/$/, "") || "/";

  if (req.method === "GET" && path === "/health") {
    return {
      ok: true,
      rpcUrl: SURFPOOL_RPC_URL,
      publicRpcUrl: PUBLIC_RPC_URL,
      runtimeInitialized: Boolean(runtime),
      runtimeError,
    };
  }

  if (req.method === "GET" && path === "/api/v2/fork/test-catalog") {
    return { success: true, data: { scenarios: SCENARIO_CATALOG } };
  }

  if (req.method === "GET" && path === "/api/v2/fork/test-runs") {
    return { success: true, data: { runs: listProtocolTestRuns() } };
  }

  if (req.method === "GET" && path === "/api/v2/fork/test-runs/latest") {
    const report = readProtocolTestRun(resolve(protocolTestRunsDir(), "latest.json"));
    return { success: true, data: { run: report } };
  }

  const protocolTestRunMatch = path.match(/^\/api\/v2\/fork\/test-runs\/([a-zA-Z0-9._-]+)$/);
  if (req.method === "GET" && protocolTestRunMatch) {
    return {
      success: true,
      data: { run: readProtocolTestRun(protocolTestRunPath(protocolTestRunMatch[1])) },
    };
  }

  let stored = await bootstrap();

  if (req.method === "GET" && path === "/api/v2/fork/config") {
    return { success: true, data: forkConfigPayload(stored) };
  }

  if (req.method === "GET" && path === "/api/v2/fork/bootstrap-evidence") {
    return { success: true, data: { transactions: bootstrapTransactionEvidence } };
  }

  if (req.method === "GET" && path === "/api/v2/fork/futarchy") {
    return { success: true, data: await futarchyPayload() };
  }

  if (req.method === "GET" && path === "/api/v2/markets") {
    return {
      success: true,
      data: {
        markets: [await marketPayload(stored)],
        pagination: { limit: 1, offset: 0, total: 1 },
      },
    };
  }

  if (req.method === "GET" && path === `/api/v2/markets/${stored.market}`) {
    return { success: true, data: await marketPayload(stored) };
  }

  if (req.method === "GET" && path === `/api/v2/markets/${stored.market}/swaps`) {
    return {
      success: true,
      data: { swaps: [], pagination: { limit: 100, offset: 0, total: 0 } },
    };
  }

  const userPositionsMatch = path.match(/^\/api\/v2\/users\/([^/]+)\/positions$/);
  if (req.method === "GET" && userPositionsMatch) {
    const wallet = new PublicKey(userPositionsMatch[1]);
    return {
      success: true,
      data: {
        positions: await userPositionsPayload(
          wallet,
          stored,
          optionalPublicKey(url.searchParams.get("positionId"))
        ),
      },
    };
  }

  const userActivityMatch = path.match(/^\/api\/v2\/users\/([^/]+)\/activity$/);
  if (req.method === "GET" && userActivityMatch) {
    return {
      success: true,
      data: { activity: [], pagination: { limit: 100, offset: 0, total: 0 } },
    };
  }

  if (req.method === "GET" && path === "/api/v2/fork/yield-account") {
    const owner = new PublicKey(String(url.searchParams.get("owner") ?? ""));
    const asset = assetFromBody(url.searchParams.get("asset"), "base");
    const tokenKind = yieldTokenKindFromBody(url.searchParams.get("tokenKind"), "ylp");
    const market = marketFromStored(stored);
    const lpMint = optionalPublicKey(url.searchParams.get("lpMint")) ??
      (tokenKind === "ylp" ? market.ylpMint : asset === "base" ? market.baseHlpMint : market.quoteHlpMint);
    return {
      success: true,
      data: { yieldAccount: await yieldAccountPayload(stored, owner, lpMint, asset, tokenKind) },
    };
  }

  if (req.method !== "POST") {
    throw new Error(`Unsupported route: ${req.method} ${path}`);
  }

  if (typeof body.market === "string" && body.market) {
    stored = await resolveStoredMarket(body.market, stored);
  }

  if (path === "/api/v2/fork/fund-wallet") {
    return { success: true, data: await fundWallet(body, stored) };
  }

  if (path === "/api/v2/fork/admin/time-travel") {
    return {
      success: true,
      data: await timeTravel(Number(body.seconds ?? 30), Number(body.slots ?? 0)),
    };
  }

  const owner = new PublicKey(String(body.owner ?? body.wallet ?? body.publicKey ?? ""));

  if (path === "/api/v2/fork/tx/create-market") {
    const config = body.config as Record<string, unknown> | undefined;
    if (!config) throw new Error("config is required");
    const prepared = await prepareCreateMarketTx({
      owner,
      label: String(body.label ?? "").trim(),
      mintA: new PublicKey(String(body.mintA ?? body.baseMint ?? "")),
      mintB: new PublicKey(String(body.mintB ?? body.quoteMint ?? "")),
      config,
    });
    return txResponse("create-market", owner, prepared.stored, prepared.transaction, {
      label: prepared.stored.label,
      config: prepared.config,
      baseMint: prepared.stored.baseMint,
      quoteMint: prepared.stored.quoteMint,
      ylpMint: prepared.stored.ylpMint,
      baseHlpMint: prepared.stored.baseHlpMint,
      quoteHlpMint: prepared.stored.quoteHlpMint,
    });
  }

  if (path === "/api/v2/fork/tx/finalize-market") {
    const transaction = await buildFinalizeMarketTx(owner, stored);
    return txResponse("activate-market", owner, stored, transaction, {
      transferHookValidationAccounts: stored.transferHookValidationAccounts,
    });
  }

  if (path === "/api/v2/fork/tx/bootstrap-rejection") {
    const { payer } = initializeRuntime();
    const kind = String(body.kind ?? "");
    let transaction: string;
    if (kind === "futarchy-duplicate") {
      transaction = await buildInitFutarchyAuthorityDuplicateTx();
    } else if (kind === "market-duplicate") {
      transaction = await buildDuplicateMarketTx(stored);
    } else if (kind === "market-invalid-config") {
      transaction = await buildInvalidConfigMarketTx(stored);
    } else if (kind === "metadata-duplicate") {
      transaction = await buildInitializeLpMetadataTx({
        stored,
        lpMint: new PublicKey(stored.ylpMint),
        metadata: defaultLpMetadata("ylp"),
      });
    } else if (kind === "metadata-invalid-name") {
      transaction = await buildInitializeLpMetadataTx({
        stored,
        lpMint: new PublicKey(stored.ylpMint),
        metadata: { ...defaultLpMetadata("ylp"), name: "x".repeat(33) },
      });
    } else if (kind === "metadata-mismatched-mint") {
      transaction = await buildInitializeLpMetadataTx({
        stored,
        lpMint: new PublicKey(stored.baseMint),
        metadata: defaultLpMetadata("ylp"),
      });
    } else {
      throw new Error(`Unsupported bootstrap rejection kind: ${kind}`);
    }
    return txResponse("bootstrap-rejection", payer.publicKey, stored, transaction, { kind });
  }

  if (path === "/api/v2/fork/tx/set-global-reduce-only") {
    const reduceOnly = Boolean(body.reduceOnly);
    const transaction = await buildSetGlobalReduceOnlyTx({ authority: owner, reduceOnly });
    return txResponse("set-global-reduce-only", owner, stored, transaction, { reduceOnly });
  }

  if (path === "/api/v2/fork/tx/set-reduce-only") {
    const reduceOnly = Boolean(body.reduceOnly);
    const transaction = await buildSetMarketReduceOnlyTx({ authority: owner, market: stored, reduceOnly });
    return txResponse("set-reduce-only", owner, stored, transaction, { reduceOnly });
  }

  if (path === "/api/v2/fork/tx/update-futarchy-authority") {
    const bootstrapSigned = Boolean(body.bootstrapSigned ?? false);
    const authority = bootstrapSigned ? initializeRuntime().payer.publicKey : owner;
    const newAuthority = new PublicKey(String(body.newAuthority ?? ""));
    const transaction = await buildUpdateFutarchyAuthorityTx({
      authority,
      newAuthority,
      bootstrapSigned,
    });
    return txResponse("update-futarchy-authority", authority, stored, transaction, {
      newAuthority: newAuthority.toBase58(),
      bootstrapSigned,
    });
  }

  if (path === "/api/v2/fork/tx/update-protocol-revenue") {
    const revenueDistribution = body.revenueDistribution as Record<string, unknown> | null | undefined;
    const protocolAuctionSplit = body.protocolAuctionSplit as Record<string, unknown> | null | undefined;
    const transaction = await buildUpdateProtocolRevenueTx({
      authority: owner,
      swapBps: body.swapBps == null ? null : Number(body.swapBps),
      interestBps: body.interestBps == null ? null : Number(body.interestBps),
      maxReferralInterestShareBps:
        body.maxReferralInterestShareBps == null ? null : Number(body.maxReferralInterestShareBps),
      revenueDistribution: revenueDistribution == null
        ? null
        : {
            futarchyTreasuryBps: Number(revenueDistribution.futarchyTreasuryBps),
            buybacksVaultBps: Number(revenueDistribution.buybacksVaultBps),
            teamTreasuryBps: Number(revenueDistribution.teamTreasuryBps),
          },
      protocolAuctionSplit: protocolAuctionSplit == null
        ? null
        : {
            feeAuctionBps: Number(protocolAuctionSplit.feeAuctionBps),
            buybackAuctionBps: Number(protocolAuctionSplit.buybackAuctionBps),
          },
    });
    return txResponse("update-protocol-revenue", owner, stored, transaction);
  }

  if (path === "/api/v2/fork/tx/update-revenue-recipients") {
    const transaction = await buildUpdateRevenueRecipientsTx({
      authority: owner,
      futarchyTreasury: optionalPublicKey(body.futarchyTreasury),
      buybacksVault: optionalPublicKey(body.buybacksVault),
      teamTreasury: optionalPublicKey(body.teamTreasury),
    });
    return txResponse("update-revenue-recipients", owner, stored, transaction);
  }

  if (path === "/api/v2/fork/tx/update-protocol-auction-config") {
    const lane = protocolAuctionLaneFromBody(body.lane, "fee");
    const auctionParamsBody = body.params as Record<string, unknown> | null | undefined;
    const transaction = await buildUpdateProtocolAuctionConfigTx({
      authority: owner,
      lane,
      acceptedMint: optionalPublicKey(body.acceptedMint),
      auctionParams: auctionParamsBody == null
        ? null
        : {
            startMultiplierBps: Number(auctionParamsBody.startMultiplierBps),
            floorMultiplierBps: Number(auctionParamsBody.floorMultiplierBps),
            durationSlots: BigInt(String(auctionParamsBody.durationSlots)),
            maxReferenceAgeSlots: BigInt(String(auctionParamsBody.maxReferenceAgeSlots)),
          },
    });
    return txResponse("update-protocol-auction-config", owner, stored, transaction, { lane });
  }

  if (path === "/api/v2/fork/tx/update-protocol-auction-recipients") {
    const lane = protocolAuctionLaneFromBody(body.lane, "fee");
    const transaction = await buildUpdateProtocolAuctionRecipientsTx({
      authority: owner,
      lane,
      treasury: optionalPublicKey(body.treasury),
      stakingVault: optionalPublicKey(body.stakingVault),
      treasuryBps: body.treasuryBps == null ? null : Number(body.treasuryBps),
      stakingVaultBps: body.stakingVaultBps == null ? null : Number(body.stakingVaultBps),
    });
    return txResponse("update-protocol-auction-recipients", owner, stored, transaction, { lane });
  }

  if (path === "/api/v2/fork/tx/update-protocol-auction-route") {
    const lane = protocolAuctionLaneFromBody(body.lane, "fee");
    const soldAsset = assetFromBody(body.soldAsset, "base");
    const transaction = await buildUpdateProtocolAuctionRouteTx({
      authority: owner,
      market: stored,
      lane,
      soldAsset,
      referenceMarket: optionalPublicKey(body.referenceMarket) ?? PublicKey.default,
    });
    return txResponse("update-protocol-auction-route", owner, stored, transaction, {
      lane,
      soldAsset,
    });
  }

  if (path === "/api/v2/fork/tx/settle-protocol-auction") {
    const lane = protocolAuctionLaneFromBody(body.lane, "fee");
    const source = protocolRevenueSourceFromBody(body.source);
    const soldAsset = assetFromBody(body.soldAsset, "base");
    const authority = await futarchyPayload();
    const auction = lane === "fee" ? authority.feeAuction : authority.buybackAuction;
    const acceptedMint = new PublicKey(auction.acceptedMint);
    const { connection } = initializeRuntime();
    const acceptedMintInfo = await connection.getAccountInfo(acceptedMint, "confirmed");
    if (!acceptedMintInfo) throw new Error(`Accepted mint ${acceptedMint.toBase58()} does not exist`);
    const acceptedTokenProgram = acceptedMintInfo.owner;
    if (!acceptedTokenProgram.equals(TOKEN_PROGRAM_ID) && !acceptedTokenProgram.equals(TOKEN_2022_PROGRAM_ID)) {
      throw new Error(`Accepted mint ${acceptedMint.toBase58()} has an unsupported token program`);
    }
    const acceptedMintAccount = await getMint(connection, acceptedMint, "confirmed", acceptedTokenProgram);
    const soldDecimals = soldAsset === "base" ? stored.baseDecimals : stored.quoteDecimals;
    const transaction = await buildSettleProtocolAuctionTx({
      bidder: owner,
      market: stored,
      lane,
      source,
      soldAsset,
      acceptedMint,
      acceptedTokenProgram,
      recipients: {
        treasury: new PublicKey(auction.recipients.treasury),
        stakingVault: new PublicKey(auction.recipients.stakingVault),
      },
      referenceMarket: new PublicKey(String(body.referenceMarket ?? stored.market)),
      soldAmount: parseUnits(String(body.soldAmount ?? "0"), soldDecimals),
      maxPaymentAmount: parseUnits(
        String(body.maxPaymentAmount ?? "0"),
        acceptedMintAccount.decimals
      ),
    });
    return txResponse("settle-protocol-auction", owner, stored, transaction, {
      lane,
      source,
      soldAsset,
      acceptedMint: acceptedMint.toBase58(),
    });
  }

  if (path === "/api/v2/fork/tx/create-parameter-proposal") {
    const bootstrapSigned = Boolean(body.bootstrapSigned ?? false);
    const proposer = bootstrapSigned ? initializeRuntime().payer.publicKey : owner;
    const nonce = BigInt(String(body.nonce ?? Date.now()));
    const update = body.update as Record<string, unknown> | undefined;
    if (!update) throw new Error("update is required");
    const decimals = await mintDecimals(
      new PublicKey(stored.ylpMint),
      TOKEN_2022_PROGRAM_ID
    );
    const built = await buildCreateParameterProposalTx({
      proposer,
      market: stored,
      nonce,
      update,
      metadata: body.metadata as Record<string, unknown> | undefined,
      initialSupport: parseUnits(String(body.initialSupport ?? "0"), decimals),
      bootstrapSigned,
    });
    return txResponse(
      "create-parameter-proposal",
      proposer,
      stored,
      built.transaction,
      {
        proposal: built.proposal.toBase58(),
        proposalSupport: built.proposalSupport.toBase58(),
        nonce: nonce.toString(),
        bootstrapSigned,
      }
    );
  }

  if (path === "/api/v2/fork/tx/support-parameter-proposal") {
    const bootstrapSigned = Boolean(body.bootstrapSigned ?? false);
    const supporter = bootstrapSigned ? initializeRuntime().payer.publicKey : owner;
    const proposal = new PublicKey(String(body.proposal ?? ""));
    const decimals = await mintDecimals(
      new PublicKey(stored.ylpMint),
      TOKEN_2022_PROGRAM_ID
    );
    const built = await buildSupportParameterProposalTx({
      supporter,
      market: stored,
      proposal,
      amount: parseUnits(String(body.amount ?? "0"), decimals),
      bootstrapSigned,
    });
    return txResponse(
      "support-parameter-proposal",
      supporter,
      stored,
      built.transaction,
      {
        proposal: proposal.toBase58(),
        proposalSupport: built.proposalSupport.toBase58(),
        bootstrapSigned,
      }
    );
  }

  if (path === "/api/v2/fork/tx/queue-parameter-proposal") {
    const proposal = new PublicKey(String(body.proposal ?? ""));
    const transaction = await buildQueueParameterProposalTx({
      caller: owner,
      market: stored,
      proposal,
    });
    return txResponse("queue-parameter-proposal", owner, stored, transaction, {
      proposal: proposal.toBase58(),
    });
  }

  if (path === "/api/v2/fork/tx/execute-parameter-proposal") {
    const proposal = new PublicKey(String(body.proposal ?? ""));
    const transaction = await buildExecuteParameterProposalTx({
      caller: owner,
      market: stored,
      proposal,
    });
    return txResponse("execute-parameter-proposal", owner, stored, transaction, {
      proposal: proposal.toBase58(),
    });
  }

  if (path === "/api/v2/fork/tx/withdraw-parameter-support") {
    const bootstrapSigned = Boolean(body.bootstrapSigned ?? false);
    const supporter = bootstrapSigned ? initializeRuntime().payer.publicKey : owner;
    const proposal = new PublicKey(String(body.proposal ?? ""));
    const built = await buildWithdrawParameterSupportTx({
      supporter,
      market: stored,
      proposal,
      bootstrapSigned,
    });
    return txResponse(
      "withdraw-parameter-support",
      supporter,
      stored,
      built.transaction,
      {
        proposal: proposal.toBase58(),
        proposalSupport: built.proposalSupport.toBase58(),
        bootstrapSigned,
      }
    );
  }

  if (path === "/api/v2/fork/tx/configure-referral-partner") {
    const referrer = new PublicKey(String(body.referrer ?? ""));
    const interestShareBps = Number(body.interestShareBps ?? 0);
    const active = Boolean(body.active ?? true);
    const built = await buildConfigureReferralPartnerTx({
      authority: owner,
      referrer,
      interestShareBps,
      active,
    });
    return txResponse("configure-referral-partner", owner, stored, built.transaction, {
      referrer: referrer.toBase58(),
      interestShareBps,
      active,
      referralPartner: built.referralPartner.toBase58(),
    });
  }

  if (path === "/api/v2/fork/tx/set-referral-recipient") {
    const recipient = new PublicKey(String(body.recipient ?? ""));
    const built = await buildSetReferralRecipientTx({ authority: owner, recipient });
    return txResponse("set-referral-recipient", owner, stored, built.transaction, {
      recipient: recipient.toBase58(),
      referralPartner: built.referralPartner.toBase58(),
    });
  }

  if (path === "/api/v2/fork/tx/claim-referral-interest") {
    const asset = assetFromBody(body.asset ?? body.claimAsset, "quote");
    const assetMint = new PublicKey(asset === "base" ? stored.baseMint : stored.quoteMint);
    const tokenProgram = new PublicKey(asset === "base" ? stored.baseTokenProgram : stored.quoteTokenProgram);
    const built = await buildClaimReferralInterestTx({
      authority: owner,
      market: stored,
      assetMint,
      tokenProgram,
    });
    return txResponse("claim-referral-interest", owner, stored, built.transaction, {
      asset,
      recipient: built.recipient.toBase58(),
      referralPartner: built.referralPartner.toBase58(),
      referralAccrual: built.referralAccrual.toBase58(),
      recipientTokenAccount: built.recipientTokenAccount.toBase58(),
    });
  }

  if (path === "/api/v2/fork/tx/set-yield-recipient") {
    const asset = assetFromBody(body.asset, "base");
    const tokenKind = yieldTokenKindFromBody(body.tokenKind, "ylp");
    const recipient = new PublicKey(String(body.recipient ?? owner.toBase58()));
    const transaction = await buildSetYieldRecipientTx({ owner, market: stored, asset, tokenKind, recipient });
    return txResponse("set-yield-recipient", owner, stored, transaction, {
      asset,
      tokenKind,
      recipient: recipient.toBase58(),
    });
  }

  if (path === "/api/v2/fork/tx/claim-yield") {
    const asset = assetFromBody(body.asset, "base");
    const tokenKind = yieldTokenKindFromBody(body.tokenKind, "ylp");
    const recipient = new PublicKey(String(body.recipient ?? owner.toBase58()));
    const transaction = await buildHarvestTx({ owner, market: stored, asset, tokenKind, recipient });
    return txResponse("claim-yield", owner, stored, transaction, {
      asset,
      tokenKind,
      recipient: recipient.toBase58(),
    });
  }

  if (path === "/api/v2/fork/tx/transfer-lp") {
    const recipient = new PublicKey(String(body.recipient ?? ""));
    const asset = assetFromBody(body.asset, "base");
    const tokenKind = yieldTokenKindFromBody(body.tokenKind, "ylp");
    const m = marketFromStored(stored);
    const lpMint = tokenKind === "ylp"
      ? m.ylpMint
      : asset === "base"
        ? m.baseHlpMint
        : m.quoteHlpMint;
    const mint = await getMint(
      initializeRuntime().connection,
      lpMint,
      "confirmed",
      TOKEN_2022_PROGRAM_ID
    );
    const amount = parseUnits(String(body.amount ?? "0"), mint.decimals);
    const transaction = await buildTransferLpTx({
      owner,
      recipient,
      market: stored,
      tokenKind,
      asset,
      amount,
    });
    return txResponse("transfer-lp", owner, stored, transaction, {
      recipient: recipient.toBase58(),
      asset,
      tokenKind,
      lpMint: lpMint.toBase58(),
      amount: amount.toString(),
    });
  }

  if (path === "/api/v2/fork/tx/preview-market") {
    return txResponse("preview-market", owner, stored, await buildPreviewMarketTx(owner, stored));
  }

  if (path === "/api/v2/fork/tx/preview-add-liquidity") {
    const transaction = await buildPreviewAddLiquidityTx({
      owner,
      market: stored,
      baseDepositAmount: rawAmount(body, ["baseDepositAmount", "baseAmount"], stored.baseDecimals, "1"),
      quoteDepositAmount: rawAmount(body, ["quoteDepositAmount", "quoteAmount"], stored.quoteDecimals, "1"),
    });
    return txResponse("preview-add-liquidity", owner, stored, transaction);
  }

  if (path === "/api/v2/fork/tx/preview-swap") {
    const assetIn = assetFromBody(body.assetIn, "base");
    const transaction = await buildPreviewSwapTx({
      owner,
      market: stored,
      assetIn,
      exactAssetIn: rawAmount(
        body,
        ["exactAssetIn", "amountIn", "amount"],
        assetIn === "base" ? stored.baseDecimals : stored.quoteDecimals,
        "1"
      ),
    });
    return txResponse("preview-swap", owner, stored, transaction, { assetIn });
  }

  if (path === "/api/v2/fork/tx/preview-borrow-capacity") {
    const collateralAsset = assetFromBody(body.collateralAsset ?? body.asset, "base");
    const debtDecimals = collateralAsset === "base" ? stored.quoteDecimals : stored.baseDecimals;
    const projectedBorrowAmount = body.projectedBorrowAmount == null || body.projectedBorrowAmount === ""
      ? null
      : rawAmount(body, ["projectedBorrowAmount"], debtDecimals, "0");
    const transaction = await buildPreviewBorrowCapacityTx({
      owner,
      market: stored,
      collateralAsset,
      collateralAmount: rawAmount(
        body,
        ["collateralAmount"],
        collateralAsset === "base" ? stored.baseDecimals : stored.quoteDecimals,
        "1"
      ),
      projectedBorrowAmount,
    });
    return txResponse("preview-borrow-capacity", owner, stored, transaction, { collateralAsset });
  }

  if (path === "/api/v2/fork/tx/preview-borrow-position") {
    const positionId = requiredPositionId(body);
    return txResponse(
      "preview-borrow-position",
      owner,
      stored,
      await buildPreviewBorrowPositionTx({ owner, market: stored, positionId }),
      { borrowPositionId: positionId.toBase58() }
    );
  }

  if (path === "/api/v2/fork/tx/add-liquidity") {
    const transaction = (
      await buildAddLiquidityTx({
        owner,
        market: stored,
        baseDepositAmount: rawAmount(body, ["baseDepositAmount", "baseAmount"], stored.baseDecimals, "1"),
        quoteDepositAmount: rawAmount(body, ["quoteDepositAmount", "quoteAmount"], stored.quoteDecimals, "1"),
        minYlpAmount: rawAmount(body, ["minYlpAmount", "minBaseYlpAmount"], stored.baseDecimals, "0"),
      })
    ).serialize({ requireAllSignatures: false, verifySignatures: false }).toString("base64");
    return txResponse("add-liquidity", owner, stored, transaction);
  }

  if (path === "/api/v2/fork/tx/remove-liquidity") {
    const transaction = await buildRemoveLiquidityTx({
      owner,
      market: stored,
      ylpAmount: rawAmount(body, ["ylpAmount", "amount"], stored.baseDecimals, "1"),
      minBaseAmountOut: rawAmount(body, ["minBaseAmountOut"], stored.baseDecimals, "0"),
      minQuoteAmountOut: rawAmount(body, ["minQuoteAmountOut"], stored.quoteDecimals, "0"),
    });
    return txResponse("remove-liquidity", owner, stored, transaction);
  }

  if (path === "/api/v2/fork/tx/swap") {
    const assetIn = assetFromBody(body.assetIn, "base");
    const decimals = assetIn === "base" ? stored.baseDecimals : stored.quoteDecimals;
    const transaction = await buildSwapTx({
      owner,
      market: stored,
      assetIn,
      exactAssetIn: rawAmount(body, ["exactAssetIn", "amountIn", "amount"], decimals, "1"),
      minAssetOut: rawAmount(
        body,
        ["minAssetOut", "minAmountOut"],
        assetIn === "base" ? stored.quoteDecimals : stored.baseDecimals,
        "0"
      ),
    });
    return txResponse("swap", owner, stored, transaction, { assetIn });
  }

  if (path === "/api/v2/fork/tx/deposit-collateral") {
    const marketAsset = assetFromBody(body.marketAsset ?? body.asset, "base");
    const positionId = optionalPublicKey(body.positionId ?? body.borrowPositionId) ?? Keypair.generate().publicKey;
    const borrowPosition = deriveBorrowPosition(new PublicKey(stored.market), positionId);
    const transaction = await buildDepositCollateralTx({
      owner,
      market: stored,
      positionId,
      marketAsset,
      depositAmount: rawAmount(
        body,
        ["depositAmount", "amount"],
        marketAsset === "base" ? stored.baseDecimals : stored.quoteDecimals,
        "1"
      ),
    });
    return txResponse("deposit-collateral", owner, stored, transaction, {
      marketAsset,
      borrowPositionId: positionId.toBase58(),
      borrowPosition: borrowPosition.toBase58(),
    });
  }

  if (path === "/api/v2/fork/tx/fortify-market") {
    const marketAsset = assetFromBody(body.marketAsset ?? body.asset, "quote");
    const transaction = await buildFortifyMarketTx({
      owner,
      market: stored,
      marketAsset,
      amount: rawAmount(
        body,
        ["amount", "donateAmount"],
        marketAsset === "base" ? stored.baseDecimals : stored.quoteDecimals,
        "1"
      ),
    });
    return txResponse("fortify-market", owner, stored, transaction, {
      marketAsset,
      insuranceVault:
        marketAsset === "base" ? stored.baseInsuranceVault : stored.quoteInsuranceVault,
    });
  }

  if (path === "/api/v2/fork/tx/withdraw-collateral") {
    const marketAsset = assetFromBody(body.marketAsset ?? body.asset, "base");
    const positionId = requiredPositionId(body);
    const borrowPosition = deriveBorrowPosition(new PublicKey(stored.market), positionId);
    const transaction = await buildWithdrawCollateralTx({
      owner,
      market: stored,
      positionId,
      marketAsset,
      withdrawAmount: rawAmount(
        body,
        ["withdrawAmount", "amount"],
        marketAsset === "base" ? stored.baseDecimals : stored.quoteDecimals,
        "1"
      ),
      minAssetAmountOut: rawAmount(
        body,
        ["minAssetAmountOut", "minAmountOut"],
        marketAsset === "base" ? stored.baseDecimals : stored.quoteDecimals,
        "0"
      ),
      minLiquidationCfBps: Number(body.minLiquidationCfBps ?? 0),
    });
    return txResponse("withdraw-collateral", owner, stored, transaction, {
      marketAsset,
      borrowPositionId: positionId.toBase58(),
      borrowPosition: borrowPosition.toBase58(),
    });
  }

  if (path === "/api/v2/fork/tx/borrow") {
    const borrowAsset = assetFromBody(body.borrowAsset ?? body.asset, "quote");
    const positionId = requiredPositionId(body);
    const borrowPosition = deriveBorrowPosition(new PublicKey(stored.market), positionId);
    const decimals = borrowAsset === "base" ? stored.baseDecimals : stored.quoteDecimals;
    const amount = rawAmount(body, ["borrowAmount", "amount"], decimals, "1");
    const minDebtAmountOut =
      body.minDebtAmountOut != null && body.minDebtAmountOut !== ""
        ? rawAmount(body, ["minDebtAmountOut"], decimals, "0")
        : amount;
    const transaction = await buildBorrowTx({
      owner,
      market: stored,
      positionId,
      borrowAsset,
      borrowAmount: amount,
      minDebtAmountOut,
      minLiquidationCfBps: Number(body.minLiquidationCfBps ?? 0),
      referrer: optionalPublicKey(body.referrer),
    });
    return txResponse("borrow", owner, stored, transaction, {
      borrowAsset,
      borrowPositionId: positionId.toBase58(),
      borrowPosition: borrowPosition.toBase58(),
      referrer: body.referrer ?? null,
    });
  }

  if (path === "/api/v2/fork/tx/repay") {
    const repayAsset = assetFromBody(body.repayAsset ?? body.asset, "quote");
    const positionId = requiredPositionId(body);
    const borrowPosition = deriveBorrowPosition(new PublicKey(stored.market), positionId);
    const transaction = await buildRepayTx({
      owner,
      market: stored,
      positionId,
      repayAsset,
      repayAmount: rawAmount(
        body,
        ["repayAmount", "amount"],
        repayAsset === "base" ? stored.baseDecimals : stored.quoteDecimals,
        "1"
      ),
    });
    return txResponse("repay", owner, stored, transaction, {
      repayAsset,
      borrowPositionId: positionId.toBase58(),
      borrowPosition: borrowPosition.toBase58(),
    });
  }

  if (path === "/api/v2/fork/tx/open-leverage") {
    const debtAsset = assetFromBody(body.debtAsset ?? body.asset, "quote");
    const positionId = optionalPublicKey(body.positionId) ?? Keypair.generate().publicKey;
    const debtDecimals = debtAsset === "base" ? stored.baseDecimals : stored.quoteDecimals;
    const collateralDecimals = debtAsset === "base" ? stored.quoteDecimals : stored.baseDecimals;
    const built = await buildOpenLeverageTx({
      owner,
      market: stored,
      positionId,
      debtAsset,
      marginAmount: rawAmount(body, ["marginAmount", "amount"], debtDecimals, "1"),
      multiplierBps: BigInt(String(body.multiplierBps ?? 20_000)),
      minCollateralOut: rawAmount(body, ["minCollateralOut", "minAmountOut"], collateralDecimals, "0"),
      referrer: optionalPublicKey(body.referrer),
    });
    return txResponse("open-leverage", owner, stored, built.transaction, {
      debtAsset,
      positionId: positionId.toBase58(),
      leveragePosition: built.leveragePosition.toBase58(),
      leverageCollateralVault: built.leverageCollateralVault.toBase58(),
    });
  }

  if (path === "/api/v2/fork/tx/increase-leverage") {
    const debtAsset = assetFromBody(body.debtAsset ?? body.asset, "quote");
    const positionId = requiredPositionId(body);
    const debtDecimals = debtAsset === "base" ? stored.baseDecimals : stored.quoteDecimals;
    const collateralDecimals = debtAsset === "base" ? stored.quoteDecimals : stored.baseDecimals;
    const transaction = await buildIncreaseLeverageTx({
      owner,
      market: stored,
      positionId,
      debtAsset,
      debtAmount: rawAmount(body, ["debtAmount", "amount"], debtDecimals, "1"),
      minCollateralOut: rawAmount(body, ["minCollateralOut", "minAmountOut"], collateralDecimals, "0"),
    });
    return txResponse("increase-leverage", owner, stored, transaction, {
      debtAsset,
      positionId: positionId.toBase58(),
      leveragePosition: deriveLeveragePosition(new PublicKey(stored.market), positionId).toBase58(),
    });
  }

  if (path === "/api/v2/fork/tx/decrease-leverage") {
    const debtAsset = assetFromBody(body.debtAsset ?? body.asset, "quote");
    const positionId = requiredPositionId(body);
    const debtDecimals = debtAsset === "base" ? stored.baseDecimals : stored.quoteDecimals;
    const collateralDecimals = debtAsset === "base" ? stored.quoteDecimals : stored.baseDecimals;
    const transaction = await buildDecreaseLeverageTx({
      owner,
      market: stored,
      positionId,
      debtAsset,
      collateralAmount: rawAmount(body, ["collateralAmount", "amount"], collateralDecimals, "1"),
      minRepayOut: rawAmount(body, ["minRepayOut", "minAmountOut"], debtDecimals, "0"),
    });
    return txResponse("decrease-leverage", owner, stored, transaction, {
      debtAsset,
      positionId: positionId.toBase58(),
      leveragePosition: deriveLeveragePosition(new PublicKey(stored.market), positionId).toBase58(),
    });
  }

  if (path === "/api/v2/fork/tx/add-leverage-margin") {
    const debtAsset = assetFromBody(body.debtAsset ?? body.asset, "quote");
    const positionId = requiredPositionId(body);
    const debtDecimals = debtAsset === "base" ? stored.baseDecimals : stored.quoteDecimals;
    const transaction = await buildAddLeverageMarginTx({
      owner,
      market: stored,
      positionId,
      debtAsset,
      amount: rawAmount(body, ["amount", "marginAmount"], debtDecimals, "1"),
    });
    return txResponse("add-leverage-margin", owner, stored, transaction, {
      debtAsset,
      positionId: positionId.toBase58(),
    });
  }

  if (path === "/api/v2/fork/tx/remove-leverage-margin") {
    const debtAsset = assetFromBody(body.debtAsset ?? body.asset, "quote");
    const positionId = requiredPositionId(body);
    const debtDecimals = debtAsset === "base" ? stored.baseDecimals : stored.quoteDecimals;
    const amount = rawAmount(body, ["amount", "marginAmount"], debtDecimals, "1");
    const transaction = await buildRemoveLeverageMarginTx({
      owner,
      market: stored,
      positionId,
      debtAsset,
      amount,
      minAmountOut:
        body.minAmountOut == null || body.minAmountOut === ""
          ? amount
          : rawAmount(body, ["minAmountOut"], debtDecimals, "0"),
    });
    return txResponse("remove-leverage-margin", owner, stored, transaction, {
      debtAsset,
      positionId: positionId.toBase58(),
    });
  }

  if (path === "/api/v2/fork/tx/close-leverage") {
    const debtAsset = assetFromBody(body.debtAsset ?? body.asset, "quote");
    const positionId = requiredPositionId(body);
    const debtDecimals = debtAsset === "base" ? stored.baseDecimals : stored.quoteDecimals;
    const transaction = await buildCloseLeverageTx({
      owner,
      market: stored,
      positionId,
      debtAsset,
      minAmountOut: rawAmount(body, ["minAmountOut"], debtDecimals, "0"),
    });
    return txResponse("close-leverage", owner, stored, transaction, {
      debtAsset,
      positionId: positionId.toBase58(),
    });
  }

  if (path === "/api/v2/fork/tx/create-leverage-delegation") {
    const debtAsset = assetFromBody(body.debtAsset ?? body.asset, "quote");
    const positionId = requiredPositionId(body);
    const delegatedProgram = new PublicKey(String(body.delegatedProgram ?? ""));
    const built = await buildCreateLeverageDelegationTx({
      owner,
      market: stored,
      positionId,
      debtAsset,
      delegatedProgram,
      approvedActions: Number(body.approvedActions ?? 0),
    });
    return txResponse("create-leverage-delegation", owner, stored, built.transaction, {
      debtAsset,
      positionId: positionId.toBase58(),
      leverageDelegation: built.leverageDelegation.toBase58(),
    });
  }

  if (path === "/api/v2/fork/tx/update-leverage-delegation") {
    const debtAsset = assetFromBody(body.debtAsset ?? body.asset, "quote");
    const positionId = requiredPositionId(body);
    const delegatedProgram = new PublicKey(String(body.delegatedProgram ?? ""));
    const built = await buildUpdateLeverageDelegationTx({
      owner,
      market: stored,
      positionId,
      debtAsset,
      delegatedProgram,
      approvedActions: Number(body.approvedActions ?? 0),
    });
    return txResponse("update-leverage-delegation", owner, stored, built.transaction, {
      debtAsset,
      positionId: positionId.toBase58(),
      leverageDelegation: built.leverageDelegation.toBase58(),
    });
  }

  if (path === "/api/v2/fork/tx/close-leverage-delegation") {
    const positionId = requiredPositionId(body);
    const built = await buildCloseLeverageDelegationTx({ owner, market: stored, positionId });
    return txResponse("close-leverage-delegation", owner, stored, built.transaction, {
      positionId: positionId.toBase58(),
      leverageDelegation: built.leverageDelegation.toBase58(),
    });
  }

  if (path === "/api/v2/fork/tx/create-leverage-order") {
    const positionId = requiredPositionId(body);
    const orderId = BigInt(String(body.orderId ?? 1));
    const built = await buildCreateLeverageOrderTx({
      owner,
      market: stored,
      positionId,
      orderId,
      kind: Number(body.kind ?? 1),
      triggerCloseoutPriceNad: BigInt(String(body.triggerCloseoutPriceNad ?? 1)),
    });
    return txResponse("create-leverage-order", owner, stored, built.transaction, {
      positionId: positionId.toBase58(),
      orderId: orderId.toString(),
      order: built.order.toBase58(),
    });
  }

  if (path === "/api/v2/fork/tx/update-leverage-order") {
    const positionId = requiredPositionId(body);
    const orderId = BigInt(String(body.orderId ?? 1));
    const built = await buildUpdateLeverageOrderTx({
      owner,
      market: stored,
      positionId,
      orderId,
      kind: Number(body.kind ?? 1),
      triggerCloseoutPriceNad: BigInt(String(body.triggerCloseoutPriceNad ?? 1)),
    });
    return txResponse("update-leverage-order", owner, stored, built.transaction, {
      positionId: positionId.toBase58(),
      orderId: orderId.toString(),
      order: built.order.toBase58(),
    });
  }

  if (path === "/api/v2/fork/tx/delegated-close-leverage") {
    const positionId = requiredPositionId(body);
    const positionOwner = new PublicKey(String(body.positionOwner ?? ""));
    const debtAsset = assetFromBody(body.debtAsset ?? body.asset, "quote");
    const debtDecimals = debtAsset === "base" ? stored.baseDecimals : stored.quoteDecimals;
    const orderId = BigInt(String(body.orderId ?? 1));
    const built = await buildDelegatedCloseLeverageTx({
      executor: owner,
      positionOwner,
      market: stored,
      positionId,
      debtAsset,
      orderId,
      minAmountOut: rawAmount(body, ["minAmountOut"], debtDecimals, "0"),
    });
    return txResponse("delegated-close-leverage", owner, stored, built.transaction, {
      positionOwner: positionOwner.toBase58(),
      positionId: positionId.toBase58(),
      debtAsset,
      orderId: orderId.toString(),
      leveragePosition: built.leveragePosition.toBase58(),
      leverageDelegation: built.leverageDelegation.toBase58(),
      order: built.order.toBase58(),
      custodyTokenAccount: built.custodyTokenAccount.toBase58(),
      executorTokenAccount: built.executorTokenAccount.toBase58(),
      ownerTokenAccount: built.ownerTokenAccount.toBase58(),
    });
  }

  if (path === "/api/v2/fork/tx/liquidate-leverage") {
    const positionId = requiredPositionId(body);
    const positionOwner = new PublicKey(String(body.positionOwner ?? ""));
    const debtAsset = assetFromBody(body.debtAsset ?? body.asset, "quote");
    const built = await buildLiquidateLeveragePositionTx({
      liquidator: owner,
      positionOwner,
      market: stored,
      positionId,
      debtAsset,
    });
    return txResponse("liquidate-leverage", owner, stored, built.transaction, {
      positionOwner: positionOwner.toBase58(),
      positionId: positionId.toBase58(),
      debtAsset,
      leveragePosition: built.leveragePosition.toBase58(),
      liquidatorDebtAccount: built.liquidatorDebtAccount.toBase58(),
      ownerDebtAccount: built.ownerDebtAccount.toBase58(),
    });
  }

  if (path === "/api/v2/fork/tx/trigger-liquidation-auction") {
    const positionId = requiredPositionId(body);
    const debtAsset = assetFromBody(body.debtAsset ?? body.asset, "quote");
    const transaction = await buildStartLiquidationAuctionTx({
      payer: owner,
      market: stored,
      positionId,
      debtAsset,
    });
    return txResponse("trigger-liquidation-auction", owner, stored, transaction, {
      positionId: positionId.toBase58(),
      debtAsset,
      borrowPosition: deriveBorrowPosition(new PublicKey(stored.market), positionId).toBase58(),
    });
  }

  if (path === "/api/v2/fork/tx/bid-liquidation-auction") {
    const positionId = requiredPositionId(body);
    const debtAsset = assetFromBody(body.debtAsset ?? body.asset, "quote");
    const debtDecimals = debtAsset === "base" ? stored.baseDecimals : stored.quoteDecimals;
    const collateralDecimals = debtAsset === "base" ? stored.quoteDecimals : stored.baseDecimals;
    const transaction = await buildFillLiquidationAuctionTx({
      liquidator: owner,
      market: stored,
      positionId,
      debtAsset,
      repayAmount: rawAmount(body, ["repayAmount", "amount"], debtDecimals, "1"),
      minCollateralOut: rawAmount(body, ["minCollateralOut", "minAmountOut"], collateralDecimals, "0"),
    });
    return txResponse("bid-liquidation-auction", owner, stored, transaction, {
      positionId: positionId.toBase58(),
      debtAsset,
    });
  }

  if (path === "/api/v2/fork/tx/settle-liquidation-auction-floor") {
    const positionId = requiredPositionId(body);
    const debtAsset = assetFromBody(body.debtAsset ?? body.asset, "quote");
    const collateralDecimals = debtAsset === "base" ? stored.quoteDecimals : stored.baseDecimals;
    const transaction = await buildBackstopLiquidationAuctionTx({
      liquidator: owner,
      market: stored,
      positionId,
      debtAsset,
      minCallerBountyOut: rawAmount(
        body,
        ["minCallerBountyOut", "minCollateralOut", "minAmountOut"],
        collateralDecimals,
        "0"
      ),
    });
    return txResponse("settle-liquidation-auction-floor", owner, stored, transaction, {
      positionId: positionId.toBase58(),
      debtAsset,
    });
  }

  if (path === "/api/v2/fork/tx/deposit-single-sided") {
    const targetAsset = assetFromBody(body.targetAsset ?? body.asset, "base");
    const transaction = await buildDepositSingleSidedTx({
      owner,
      market: stored,
      targetAsset,
      depositAmount: rawAmount(
        body,
        ["depositAmount", "amount"],
        targetAsset === "base" ? stored.baseDecimals : stored.quoteDecimals,
        "1"
      ),
      minHlpAmount: rawAmount(
        body,
        ["minHlpAmount"],
        targetAsset === "base" ? stored.baseDecimals : stored.quoteDecimals,
        "0"
      ),
    });
    return txResponse("deposit-single-sided", owner, stored, transaction, { targetAsset });
  }

  if (path === "/api/v2/fork/tx/withdraw-single-sided") {
    const targetAsset = assetFromBody(body.targetAsset ?? body.asset, "base");
    const transaction = await buildWithdrawSingleSidedTx({
      owner,
      market: stored,
      targetAsset,
      hlpAmount: rawAmount(
        body,
        ["hlpAmount", "amount"],
        targetAsset === "base" ? stored.baseDecimals : stored.quoteDecimals,
        "1"
      ),
      minTargetAmountOut: rawAmount(
        body,
        ["minTargetAmountOut", "minAmountOut"],
        targetAsset === "base" ? stored.baseDecimals : stored.quoteDecimals,
        "0"
      ),
    });
    return txResponse("withdraw-single-sided", owner, stored, transaction, { targetAsset });
  }

  throw new Error(`Unsupported route: ${req.method} ${path}`);
}

export async function localE2E() {
  const stored = await bootstrap();
  const { payer, provider } = initializeRuntime();
  await fundWallet({ wallet: payer.publicKey.toBase58(), sol: DEFAULT_SOL_FUNDING }, stored);

  const addLiquidityTx = await buildAddLiquidityTx({
    owner: payer.publicKey,
    market: stored,
    baseDepositAmount: parseUnits("1", stored.baseDecimals),
    quoteDepositAmount: parseUnits("1", stored.quoteDecimals),
    minYlpAmount: 0n,
    payerCanSign: true,
  });
  addLiquidityTx.sign(payer);
  const addLiquiditySig = await provider.connection.sendRawTransaction(addLiquidityTx.serialize());
  await provider.connection.confirmTransaction(addLiquiditySig, "confirmed");

  const swapBase64 = await buildSwapTx({
    owner: payer.publicKey,
    market: stored,
    assetIn: "base",
    exactAssetIn: parseUnits("0.1", stored.baseDecimals),
    minAssetOut: 0n,
  });
  const swapTx = Transaction.from(Buffer.from(swapBase64, "base64"));
  swapTx.sign(payer);
  const swapSig = await provider.connection.sendRawTransaction(swapTx.serialize());
  await provider.connection.confirmTransaction(swapSig, "confirmed");

  return {
    ok: true,
    market: stored.market,
    addLiquiditySig,
    swapSig,
    config: forkConfigPayload(stored),
  };
}
