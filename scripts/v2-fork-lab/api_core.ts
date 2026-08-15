import {
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  renameSync,
  writeFileSync,
} from "node:fs";
import http from "node:http";
import {
  createHash,
  createHmac,
  randomBytes,
  timingSafeEqual,
} from "node:crypto";
import { dirname, resolve } from "node:path";
import anchor from "@coral-xyz/anchor";
import BN from "bn.js";
import {
  ASSOCIATED_TOKEN_PROGRAM_ID,
  ExtensionType,
  NATIVE_MINT,
  TOKEN_2022_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  calculateEpochFee,
  createAssociatedTokenAccountIdempotentInstruction,
  createAssociatedTokenAccountInstruction,
  createInitializeMintInstruction,
  createInitializeTransferFeeConfigInstruction,
  createInitializeTransferHookInstruction,
  createMintToCheckedInstruction,
  createTransferCheckedInstruction,
  createTransferCheckedWithTransferHookInstruction,
  getAccount,
  getAssociatedTokenAddressSync,
  getMint,
  getMintLen,
  getTransferFeeConfig,
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
  type AccountMeta,
} from "@solana/web3.js";
import { SCENARIO_CATALOG } from "../protocol-tests/catalog.js";

const DEFAULT_PROGRAM_ID = "358bjJKXWxeAXAzteX1xTgyd9JNnjtzW8fnwCS8Da1mv";
const LEVERAGE_DELEGATE_PROGRAM_ID = new PublicKey("EPGF9iFrbGnhWgC3To9rC9vxinEYuDHaz4RXgLPvuRkp");
const DEFAULT_META_MINT = "METAwkXcqyXKy1AtsSgJ8JiUHwGCafnZL38n3vYmeta";
const DEFAULT_USDC_MINT = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const BPF_LOADER_UPGRADEABLE_ID = new PublicKey("BPFLoaderUpgradeab1e11111111111111111111111");
const TOKEN_METADATA_PROGRAM_ID = new PublicKey("metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s");
const SYSVAR_CLOCK_PUBKEY = new PublicKey("SysvarC1ock11111111111111111111111111111111");
const NAD = 1_000_000_000n;
const DEPLOYMENT_SCHEMA_VERSION = "dusk-deployment.v1";
const DEPLOYMENT_COMMITMENT = "confirmed" as const;
const API_STARTED_AT = new Date().toISOString();
const UPGRADEABLE_PROGRAM_TAG = 2;
const UPGRADEABLE_PROGRAM_DATA_TAG = 3;
const UPGRADEABLE_PROGRAM_DATA_METADATA_BYTES = 45;
const DEFAULT_PUBLIC_RPC_PROBE_TIMEOUT_MS = 5_000;
const PUBLIC_RPC_PROBE_MAX_RESPONSE_BYTES = 65_536;
const PUBLIC_RPC_FILTER_PROBE_METHOD = "surfnet_duskPublicFilterProbe";

function duskEnv(name: string): string | undefined;
function duskEnv(name: string, fallback: string): string;
function duskEnv(name: string, fallback?: string): string | undefined {
  const suffix = name.replace(/^DUSK_/, "");
  return process.env[`DUSK_${suffix}`] ?? fallback;
}

const SURFPOOL_RPC_URL = process.env.SURFPOOL_RPC_URL ?? "http://127.0.0.1:8899";
const PUBLIC_RPC_URL = resolvePublicRpcUrl(process.env, SURFPOOL_RPC_URL);
const HAS_EXPLICIT_PUBLIC_RPC_URL = Boolean(
  process.env.PUBLIC_SURFPOOL_RPC_URL?.trim() ||
    process.env.SURFPOOL_RPC_PROXY_URL?.trim(),
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
type ForkMarketKind = "cpmm" | "concentrated";

type BootstrapMarketDefinition = {
  label: string;
  kind: ForkMarketKind;
  baseMint: PublicKey;
  quoteMint: PublicKey;
  paramsHash: Buffer;
  config: ReturnType<typeof defaultMarketConfig>;
};

type StoredMarket = {
  label: string;
  marketKind?: ForkMarketKind;
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

type BootstrapTransactionEvidence = {
  label: string;
  signature: string;
  instructions: string[];
};

type FutarchyAuthorityBootstrapMode =
  | "transaction"
  | "surfpool-account-seed"
  | "preexisting";

type BootstrapEvidencePayload = {
  transactions: BootstrapTransactionEvidence[];
  futarchyAuthorityBootstrapMode: FutarchyAuthorityBootstrapMode;
};

type ForkState = {
  markets: Record<string, StoredMarket>;
  bootstrapTransactions: BootstrapTransactionEvidence[];
  bootstrapEvidenceDeploymentFingerprint: string | null;
  futarchyAuthorityBootstrapMode: FutarchyAuthorityBootstrapMode | null;
};

type LoadedIdl = {
  idl: anchor.Idl;
  path: string;
  rawSha256: string;
  canonicalSha256: string;
};

export class DeploymentIdentityChangedError extends Error {
  readonly code = "deployment_identity_changed";
  readonly uncertainOutcome = true;

  constructor(
    message = "Surfpool deployment identity changed during the request",
  ) {
    super(message);
    this.name = "DeploymentIdentityChangedError";
  }
}

export class ForkMutationOutcomeUncertainError extends Error {
  readonly code = "fork_mutation_outcome_uncertain";
  readonly httpStatus = 409;
  readonly uncertainOutcome = true;

  constructor(operation: string, cause: unknown) {
    const detail = cause instanceof Error ? cause.message : String(cause);
    super(
      `${operation} may have been partially applied; reconcile fork state before retrying: ${detail}`,
    );
    this.name = "ForkMutationOutcomeUncertainError";
  }
}

export class ForkAdminAuthConfigurationError extends Error {
  readonly code = "fork_admin_auth_not_configured";
  readonly httpStatus = 503;

  constructor() {
    super(
      "Fork admin API is unavailable because FORK_ADMIN_TOKEN is not configured",
    );
    this.name = "ForkAdminAuthConfigurationError";
  }
}

export class ForkAdminAuthenticationRequiredError extends Error {
  readonly code = "fork_admin_auth_required";
  readonly httpStatus = 401;

  constructor() {
    super("Fork admin API requires the x-fork-admin-token request header");
    this.name = "ForkAdminAuthenticationRequiredError";
  }
}

export class ForkAdminAuthorizationError extends Error {
  readonly code = "fork_admin_auth_forbidden";
  readonly httpStatus = 403;

  constructor() {
    super("Fork admin API token is invalid");
    this.name = "ForkAdminAuthorizationError";
  }
}

export class LeverageOrderNotActionableError extends Error {
  readonly code = "leverage_order_not_actionable";
  readonly httpStatus = 409;

  constructor() {
    super("Leverage order is no longer actionable");
    this.name = "LeverageOrderNotActionableError";
  }
}

type LeverageOrderAccountInfo = {
  owner: PublicKey;
  executable: boolean;
  data: Uint8Array;
};

function requireActionableLeverageOrderAccount(
  account: LeverageOrderAccountInfo | null,
): void {
  if (account === null || account.data.byteLength === 0) {
    throw new LeverageOrderNotActionableError();
  }
  if (
    account.executable ||
    !account.owner.equals(LEVERAGE_DELEGATE_PROGRAM_ID)
  ) {
    throw new Error("Leverage order account is invalid");
  }
}

type HeaderReadableRequest = Pick<http.IncomingMessage, "url"> & {
  headers?: http.IncomingHttpHeaders | { get(name: string): string | null };
};

function requestHeader(
  req: HeaderReadableRequest,
  name: string,
): string | undefined {
  const headers = req.headers;
  if (!headers) return undefined;

  const headerGetter = (headers as { get?: unknown }).get;
  if (typeof headerGetter === "function") {
    return headerGetter.call(headers, name) ?? undefined;
  }

  const normalizedName = name.toLowerCase();
  const record = headers as http.IncomingHttpHeaders;
  const direct = record[normalizedName];
  const value =
    direct ??
    Object.entries(record).find(
      ([key]) => key.toLowerCase() === normalizedName,
    )?.[1];
  if (Array.isArray(value)) return value.length === 1 ? value[0] : "";
  return typeof value === "string" ? value : undefined;
}

function isForkAdminPath(rawUrl: string | undefined): boolean {
  const path =
    new URL(rawUrl ?? "/", "http://localhost").pathname.replace(/\/$/, "") ||
    "/";
  return (
    path === "/api/v2/fork/admin" || path.startsWith("/api/v2/fork/admin/")
  );
}

function isForkServerSignedRequest(
  req: HeaderReadableRequest,
  body: Record<string, unknown>,
): boolean {
  const path =
    new URL(req.url ?? "/", "http://localhost").pathname.replace(/\/$/, "") ||
    "/";
  const bootstrapSigned = body.bootstrapSigned;
  return (
    path === "/api/v2/fork/tx/bootstrap-rejection" ||
    path === "/api/v2/fork/tx/create-market" ||
    (bootstrapSigned !== undefined &&
      bootstrapSigned !== null &&
      bootstrapSigned !== false)
  );
}

function bootstrapSignedFromBody(value: unknown): boolean {
  if (value === undefined || value === null || value === false) return false;
  if (value === true) return true;
  throw new Error("bootstrapSigned must be a boolean");
}

function constantTimeTokenEquals(expected: string, received: string): boolean {
  const expectedDigest = createHash("sha256").update(expected, "utf8").digest();
  const receivedDigest = createHash("sha256").update(received, "utf8").digest();
  return timingSafeEqual(expectedDigest, receivedDigest);
}

function requireForkAdminToken(req: HeaderReadableRequest): void {
  const configuredToken = process.env.FORK_ADMIN_TOKEN;
  if (!configuredToken || configuredToken.trim().length === 0) {
    throw new ForkAdminAuthConfigurationError();
  }

  const receivedToken = requestHeader(req, "x-fork-admin-token");
  if (receivedToken === undefined)
    throw new ForkAdminAuthenticationRequiredError();
  if (!constantTimeTokenEquals(configuredToken, receivedToken)) {
    throw new ForkAdminAuthorizationError();
  }
}

export function requireForkAdminAuthorization(
  req: HeaderReadableRequest,
): void {
  if (isForkAdminPath(req.url)) requireForkAdminToken(req);
}

export function requireForkServerSigningAuthorization(
  req: HeaderReadableRequest,
  body: Record<string, unknown>,
): void {
  if (isForkServerSignedRequest(req, body)) {
    requireForkAdminToken(req);
    bootstrapSignedFromBody(body.bootstrapSigned);
  }
}

let runtime:
  | {
      payer: Keypair;
      connection: Connection;
      provider: anchor.AnchorProvider;
      program: any;
      idl: anchor.Idl;
      accountCoder: anchor.BorshAccountsCoder;
      idlPath: string;
      idlRawSha256: string;
      idlCanonicalSha256: string;
    }
  | undefined;
let runtimeError: string | null = null;
const bootstrapPromises = new Map<string, Promise<StoredMarket[]>>();
const bootstrapEvidencePromises = new Map<
  string,
  Promise<BootstrapEvidencePayload>
>();
let bootstrapQueue: Promise<void> = Promise.resolve();
let leverageDelegateProgram: any;
let leverageDelegateIdl: LoadedIdl | undefined;
let forkGenerationPromise: Promise<string> | undefined;
const programBinaryHashPromises = new Map<string, Promise<string>>();
let observedRuntimeForkId: string | undefined;

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

function emptyForkState(): ForkState {
  return {
    markets: {},
    bootstrapTransactions: [],
    bootstrapEvidenceDeploymentFingerprint: null,
    futarchyAuthorityBootstrapMode: null,
  };
}

function normalizeBootstrapTransactions(
  value: unknown,
): BootstrapTransactionEvidence[] {
  if (!Array.isArray(value)) return [];
  return mergeBootstrapTransactions(
    value.flatMap((entry) => {
      if (!entry || typeof entry !== "object") return [];
      const record = entry as Record<string, unknown>;
      if (
        typeof record.label !== "string" ||
        typeof record.signature !== "string" ||
        !Array.isArray(record.instructions) ||
        record.instructions.some((instruction) => typeof instruction !== "string")
      ) {
        return [];
      }
      return [{
        label: record.label,
        signature: record.signature,
        instructions: record.instructions as string[],
      }];
    }),
  );
}

function normalizeForkState(value: unknown): ForkState {
  if (!value || typeof value !== "object") return emptyForkState();
  const record = value as Record<string, unknown>;
  const mode = record.futarchyAuthorityBootstrapMode;
  return {
    markets:
      record.markets && typeof record.markets === "object" && !Array.isArray(record.markets)
        ? record.markets as Record<string, StoredMarket>
        : {},
    bootstrapTransactions: normalizeBootstrapTransactions(record.bootstrapTransactions),
    bootstrapEvidenceDeploymentFingerprint:
      typeof record.bootstrapEvidenceDeploymentFingerprint === "string"
        ? record.bootstrapEvidenceDeploymentFingerprint
        : null,
    futarchyAuthorityBootstrapMode:
      mode === "transaction" ||
      mode === "surfpool-account-seed" ||
      mode === "preexisting"
        ? mode
        : null,
  };
}

function mergeBootstrapTransactions(
  ...collections: BootstrapTransactionEvidence[][]
): BootstrapTransactionEvidence[] {
  const bySignature = new Map<string, BootstrapTransactionEvidence>();
  for (const collection of collections) {
    for (const transaction of collection) {
      const previous = bySignature.get(transaction.signature);
      bySignature.set(transaction.signature, previous
        ? {
            label: previous.label,
            signature: previous.signature,
            instructions: Array.from(new Set([
              ...previous.instructions,
              ...transaction.instructions,
            ])),
          }
        : {
            label: transaction.label,
            signature: transaction.signature,
            instructions: Array.from(new Set(transaction.instructions)),
          });
    }
  }
  return Array.from(bySignature.values());
}

function readState(): ForkState {
  ensureStateDir();
  if (!existsSync(statePath())) return emptyForkState();
  return normalizeForkState(JSON.parse(readFileSync(statePath(), "utf8")));
}

function writeState(
  state: ForkState,
  options: { replaceBootstrapEvidence?: boolean } = {},
) {
  ensureStateDir();
  let next = normalizeForkState(state);
  if (!options.replaceBootstrapEvidence && existsSync(statePath())) {
    const persisted = readState();
    const sameDeployment =
      !persisted.bootstrapEvidenceDeploymentFingerprint ||
      !next.bootstrapEvidenceDeploymentFingerprint ||
      persisted.bootstrapEvidenceDeploymentFingerprint ===
        next.bootstrapEvidenceDeploymentFingerprint;
    if (sameDeployment) {
      next = {
        ...next,
        bootstrapTransactions: mergeBootstrapTransactions(
          persisted.bootstrapTransactions,
          next.bootstrapTransactions,
        ),
        bootstrapEvidenceDeploymentFingerprint:
          persisted.bootstrapEvidenceDeploymentFingerprint ??
          next.bootstrapEvidenceDeploymentFingerprint,
        futarchyAuthorityBootstrapMode:
          persisted.futarchyAuthorityBootstrapMode ??
          next.futarchyAuthorityBootstrapMode,
      };
    }
  }
  const path = statePath();
  const temporaryPath = `${path}.${process.pid}.tmp`;
  writeFileSync(temporaryPath, `${JSON.stringify(next, null, 2)}\n`, {
    mode: 0o600,
  });
  renameSync(temporaryPath, path);
}

function beginBootstrapEvidence(deploymentFingerprint: string): void {
  const state = readState();
  if (state.bootstrapEvidenceDeploymentFingerprint === deploymentFingerprint) {
    return;
  }
  state.bootstrapTransactions = [];
  state.bootstrapEvidenceDeploymentFingerprint = deploymentFingerprint;
  state.futarchyAuthorityBootstrapMode = null;
  writeState(state, { replaceBootstrapEvidence: true });
}

function recordBootstrapTransaction(
  label: string,
  signature: string,
  instructions: string[],
): void {
  const state = readState();
  state.bootstrapTransactions = mergeBootstrapTransactions(
    state.bootstrapTransactions,
    [{ label, signature, instructions }],
  );
  writeState(state, { replaceBootstrapEvidence: true });
}

function recordFutarchyAuthorityBootstrapMode(
  mode: FutarchyAuthorityBootstrapMode,
  options: { onlyIfMissing?: boolean } = {},
): void {
  const state = readState();
  if (options.onlyIfMissing && state.futarchyAuthorityBootstrapMode) return;
  state.futarchyAuthorityBootstrapMode = mode;
  writeState(state, { replaceBootstrapEvidence: true });
}

function normalizePublicUrl(value: string): string {
  if (/^https?:\/\//i.test(value)) return value.replace(/\/$/, "");
  if (value.includes("localhost") || value.includes("127.0.0.1")) return `http://${value}`;
  return `https://${value}`;
}

function nonBlankEnvironmentValue(value: string | undefined): string | undefined {
  const trimmed = value?.trim();
  return trimmed ? trimmed : undefined;
}

export function resolvePublicRpcUrl(
  env: NodeJS.ProcessEnv = process.env,
  surfpoolRpcUrl = env.SURFPOOL_RPC_URL ?? "http://127.0.0.1:8899",
): string {
  const explicitPublicRpcUrl =
    nonBlankEnvironmentValue(env.PUBLIC_SURFPOOL_RPC_URL) ??
    nonBlankEnvironmentValue(env.SURFPOOL_RPC_PROXY_URL);
  if (env.DUSK_REQUIRE_PUBLIC_RPC_URL === "true") {
    if (explicitPublicRpcUrl === undefined) {
      throw new Error(
        "DUSK_REQUIRE_PUBLIC_RPC_URL=true requires an explicit " +
          "PUBLIC_SURFPOOL_RPC_URL or SURFPOOL_RPC_PROXY_URL",
      );
    }
    const parsed = new URL(explicitPublicRpcUrl);
    if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
      throw new Error(
        "PUBLIC_SURFPOOL_RPC_URL must use http: or https: when " +
          "DUSK_REQUIRE_PUBLIC_RPC_URL=true",
      );
    }
  }
  return normalizePublicUrl(explicitPublicRpcUrl ?? surfpoolRpcUrl);
}

function forkHealthPayload(params: {
  publicRpcUrl: string;
  publicRpcVerified: boolean;
  publicRpcFilterVerified: boolean;
  runtimeInitialized: boolean;
  runtimeError: string | null;
  prebootstrappedMarketCount: number;
}): Record<string, unknown> {
  return {
    ok: true,
    publicRpcUrl: params.publicRpcUrl,
    publicRpcVerified: params.publicRpcVerified,
    publicRpcFilterVerified: params.publicRpcFilterVerified,
    runtimeInitialized: params.runtimeInitialized,
    runtimeError: params.runtimeError,
    prebootstrappedMarketCount: params.prebootstrappedMarketCount,
  };
}

function publicForkRpcPayload(publicRpcUrl: string): { rpcUrl: string } {
  return { rpcUrl: publicRpcUrl };
}

type PublicRpcIdentity = {
  genesisHash: string;
  forkId: string;
};

type PublicRpcVerification = PublicRpcIdentity & {
  filterVerified: true;
};

function createGenerationPinnedGenesisHashReader(
  load: () => Promise<string>,
): {
  read(
    generation: string,
    confirmGeneration: () => Promise<string>,
  ): Promise<string>;
  reset(): void;
} {
  let pinned:
    | { generation: string; promise: Promise<string> }
    | undefined;
  return {
    read(generation, confirmGeneration) {
      if (pinned?.generation === generation) return pinned.promise;
      const entry = {
        generation,
        promise: Promise.resolve().then(async () => {
          const genesisHash = await load();
          const confirmedGeneration = await confirmGeneration();
          if (confirmedGeneration !== generation) {
            throw new DeploymentIdentityChangedError(
              "Surfpool fork generation changed while pinning its genesis hash",
            );
          }
          return genesisHash;
        }),
      };
      pinned = entry;
      const pending = entry.promise;
      void pending.catch(() => {
        // A transient startup failure must be recoverable without restarting
        // the process. A successful value remains pinned only to the exact
        // 32-byte fork generation that was re-observed after the remote read.
        if (pinned === entry) pinned = undefined;
      });
      return pending;
    },
    reset() {
      pinned = undefined;
    },
  };
}

const lifecycleGenesisHash = createGenerationPinnedGenesisHashReader(
  () => initializeRuntime().connection.getGenesisHash(),
);

function publicRpcProbeTimeoutMs(
  raw = process.env.FORK_API_PUBLIC_RPC_PROBE_TIMEOUT_MS,
): number {
  if (raw === undefined || raw === "") return DEFAULT_PUBLIC_RPC_PROBE_TIMEOUT_MS;
  const parsed = Number(raw);
  if (!Number.isSafeInteger(parsed) || parsed < 1 || parsed > 60_000) {
    throw new Error(
      "FORK_API_PUBLIC_RPC_PROBE_TIMEOUT_MS must be an integer between 1 and 60000",
    );
  }
  return parsed;
}

async function readBoundedPublicRpcProbeResponse(
  response: Response,
): Promise<Buffer> {
  const contentLength = response.headers.get("content-length");
  if (contentLength !== null) {
    const parsed = Number(contentLength);
    if (!Number.isSafeInteger(parsed) || parsed < 0) {
      await response.body?.cancel().catch(() => undefined);
      throw new Error("Public Surfpool RPC returned an invalid content length");
    }
    if (parsed > PUBLIC_RPC_PROBE_MAX_RESPONSE_BYTES) {
      await response.body?.cancel().catch(() => undefined);
      throw new Error("Public Surfpool RPC probe response is too large");
    }
  }
  if (!response.body) return Buffer.alloc(0);
  const reader = response.body.getReader();
  const chunks: Buffer[] = [];
  let total = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      if (total + value.byteLength > PUBLIC_RPC_PROBE_MAX_RESPONSE_BYTES) {
        await reader.cancel().catch(() => undefined);
        throw new Error("Public Surfpool RPC probe response is too large");
      }
      chunks.push(Buffer.from(value));
      total += value.byteLength;
    }
  } finally {
    reader.releaseLock();
  }
  return Buffer.concat(chunks, total);
}

async function publicRpcProbeRequest(
  publicRpcUrl: string,
  method: string,
  params: unknown[],
  fetchImpl: typeof fetch,
  timeoutMs: number,
): Promise<{ status: number; payload: Record<string, unknown> }> {
  const response = await fetchImpl(publicRpcUrl, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      jsonrpc: "2.0",
      id: "dusk-public-rpc-readiness",
      method,
      params,
    }),
    signal: AbortSignal.timeout(timeoutMs),
  });
  const body = await readBoundedPublicRpcProbeResponse(response);
  let payload: unknown;
  try {
    payload = JSON.parse(body.toString("utf8"));
  } catch {
    throw new Error("Public Surfpool RPC probe returned invalid JSON");
  }
  if (!payload || typeof payload !== "object" || Array.isArray(payload)) {
    throw new Error("Public Surfpool RPC probe returned an invalid payload");
  }
  return { status: response.status, payload: payload as Record<string, unknown> };
}

export async function verifyPublicRpcEndpoint(
  expected: PublicRpcIdentity,
  options: {
    publicRpcUrl?: string;
    fetchImpl?: typeof fetch;
    timeoutMs?: number;
    namespace?: string;
    programId?: PublicKey;
  } = {},
): Promise<PublicRpcVerification> {
  const publicRpcUrl = options.publicRpcUrl ?? PUBLIC_RPC_URL;
  const fetchImpl = options.fetchImpl ?? fetch;
  const timeoutMs = options.timeoutMs ?? publicRpcProbeTimeoutMs();
  const programId = options.programId ?? PROGRAM_ID;
  const namespace = options.namespace ?? process.env.DUSK_FORK_NAMESPACE ?? "dusk-surfpool";

  // Probe a deliberately nonexistent Surfnet method. The filtered public
  // proxy rejects every surfnet_* method before forwarding it, while a raw or
  // misconfigured Surfnet endpoint returns JSON-RPC method-not-found. This
  // proves the browser URL does not expose the cheatcode surface without
  // risking a mutation during readiness.
  const filter = await publicRpcProbeRequest(
    publicRpcUrl,
    PUBLIC_RPC_FILTER_PROBE_METHOD,
    [],
    fetchImpl,
    timeoutMs,
  );
  const filterError = filter.payload.error;
  if (
    filter.status !== 403 ||
    !filterError ||
    typeof filterError !== "object" ||
    (filterError as { code?: unknown }).code !== -32099
  ) {
    throw new Error(
      "PUBLIC_SURFPOOL_RPC_URL is not the filtered Dusk Surfpool proxy",
    );
  }

  // Surfpool forwards getGenesisHash to its configured remote datasource in
  // online fork mode. A readiness poll must not turn into an external RPC
  // dependency or amplify upstream traffic. getHealth is local-only; the
  // random fork-generation marker below provides the exact fork identity.
  const health = await publicRpcProbeRequest(
    publicRpcUrl,
    "getHealth",
    [],
    fetchImpl,
    timeoutMs,
  );
  if (
    health.status < 200 ||
    health.status >= 300 ||
    health.payload.error !== undefined ||
    health.payload.result !== "ok"
  ) {
    throw new Error("Public Surfpool RPC health probe failed");
  }

  const markerAddress = PublicKey.findProgramAddressSync(
    [Buffer.from("surfpool_fork_generation_v1")],
    programId,
  )[0];
  const marker = await publicRpcProbeRequest(
    publicRpcUrl,
    "getAccountInfo",
    [
      markerAddress.toBase58(),
      { commitment: DEPLOYMENT_COMMITMENT, encoding: "base64" },
    ],
    fetchImpl,
    timeoutMs,
  );
  const markerResult = marker.payload.result;
  const markerValue =
    markerResult && typeof markerResult === "object"
      ? (markerResult as { value?: unknown }).value
      : null;
  if (
    marker.status < 200 ||
    marker.status >= 300 ||
    marker.payload.error !== undefined ||
    !markerValue ||
    typeof markerValue !== "object"
  ) {
    throw new Error("Public Surfpool RPC fork marker is missing");
  }
  const owner = (markerValue as { owner?: unknown }).owner;
  const encodedData = (markerValue as { data?: unknown }).data;
  if (
    owner !== programId.toBase58() ||
    !Array.isArray(encodedData) ||
    encodedData.length !== 2 ||
    typeof encodedData[0] !== "string" ||
    encodedData[1] !== "base64"
  ) {
    throw new Error("Public Surfpool RPC fork marker is malformed");
  }
  const markerData = Buffer.from(encodedData[0], "base64");
  if (markerData.length !== 32) {
    throw new Error("Public Surfpool RPC fork marker has invalid data");
  }
  const forkId = deriveForkGenerationId(
    namespace,
    expected.genesisHash,
    programId.toBase58(),
    markerData,
  );
  if (forkId !== expected.forkId) {
    throw new Error("Public Surfpool RPC fork identity does not match the private fork");
  }
  return {
    genesisHash: expected.genesisHash,
    forkId,
    filterVerified: true,
  };
}

function canonicalJson(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  if (value !== null && typeof value === "object") {
    const record = value as Record<string, unknown>;
    return `{${Object.keys(record)
      .sort()
      .map((key) => `${JSON.stringify(key)}:${canonicalJson(record[key])}`)
      .join(",")}}`;
  }
  const serialized = JSON.stringify(value);
  if (serialized === undefined)
    throw new Error("Cannot canonicalize an undefined JSON value");
  return serialized;
}

function sha256(value: string | Buffer): string {
  return createHash("sha256").update(value).digest("hex");
}

function loadJsonIdl(path: string, expectedProgramId: PublicKey): LoadedIdl {
  const raw = readFileSync(path, "utf8");
  const parsed = JSON.parse(raw) as anchor.Idl;
  const address = String((parsed as { address?: unknown }).address ?? "");
  if (address !== expectedProgramId.toBase58()) {
    throw new Error(
      `IDL ${path} declares ${address || "no program address"}; expected ${expectedProgramId.toBase58()}`,
    );
  }
  return {
    idl: parsed,
    path,
    rawSha256: sha256(raw),
    canonicalSha256: sha256(canonicalJson(parsed)),
  };
}

function loadIdl(): LoadedIdl {
  const candidates = [
    duskEnv("IDL_PATH"),
    "target/idl/dusk.json",
    "packages/dusk-sdk/src/idl_v2.json",
  ].filter(Boolean) as string[];

  for (const candidate of candidates) {
    const path = resolve(candidate);
    if (existsSync(path)) {
      return loadJsonIdl(path, PROGRAM_ID);
    }
  }

  throw new Error(
    `Dusk IDL not found. Tried ${candidates.map((path) => resolve(path)).join(", ")}`
  );
}

function loadLeverageDelegateIdl(): LoadedIdl {
  if (leverageDelegateIdl) return leverageDelegateIdl;
  const candidates = [
    duskEnv("LEVERAGE_DELEGATE_IDL_PATH"),
    "target/idl/leverage_delegate.json",
    "scripts/v2-fork-lab/idl/leverage_delegate.json",
  ].filter(Boolean) as string[];
  for (const candidate of candidates) {
    const path = resolve(candidate);
    if (existsSync(path)) {
      leverageDelegateIdl = loadJsonIdl(path, LEVERAGE_DELEGATE_PROGRAM_ID);
      return leverageDelegateIdl;
    }
  }
  throw new Error(
    `Leverage delegate IDL not found. Tried ${candidates.map((path) => resolve(path)).join(", ")}`,
  );
}

function getLeverageDelegateProgram() {
  if (leverageDelegateProgram) return leverageDelegateProgram;
  const { provider } = initializeRuntime();
  const { idl } = loadLeverageDelegateIdl();
  leverageDelegateProgram = new anchor.Program(idl, provider);
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

const FORK_DERIVED_KEYPAIR_DOMAIN = "dusk-fork-derived-keypair-v1";

function configuredForkControllerKeypair(): Keypair | null {
  const inline =
    process.env.FORK_LAB_PAYER_KEYPAIR_JSON ??
    process.env.FORK_LAB_PAYER_KEYPAIR_BASE64;
  if (inline) return parseKeypairSecret(inline);

  const configuredPath =
    process.env.FORK_LAB_PAYER_KEYPAIR ?? process.env.ANCHOR_WALLET;
  if (!configuredPath) return null;
  const path = resolve(configuredPath);
  if (!existsSync(path)) {
    throw new Error(`Configured Surfpool controller signer is missing: ${path}`);
  }
  return readKeypairFile(path);
}

function deriveForkKeypair(controller: Keypair, label: string): Keypair {
  if (!label) throw new Error("Derived fork keypair label is required");
  const seed = createHmac(
    "sha256",
    Buffer.from(controller.secretKey.subarray(0, 32)),
  )
    .update(FORK_DERIVED_KEYPAIR_DOMAIN, "utf8")
    .update("\0", "utf8")
    .update(label, "utf8")
    .digest();
  return Keypair.fromSeed(seed);
}

function loadOrCreateKeypair(label: string): {
  keypair: Keypair;
  path: string;
  created: boolean;
} {
  ensureStateDir();
  const safeLabel = label.replace(/[^a-zA-Z0-9_-]/g, "-");
  const path = resolve(stateDir(), `${safeLabel}.json`);
  const controller = configuredForkControllerKeypair();
  const deterministic = controller
    ? deriveForkKeypair(controller, label)
    : null;
  if (existsSync(path)) {
    const persisted = readKeypairFile(path);
    if (
      deterministic &&
      !Buffer.from(persisted.secretKey).equals(
        Buffer.from(deterministic.secretKey),
      )
    ) {
      throw new Error(
        `Persisted hosted fork keypair ${path} does not match deterministic controller derivation`,
      );
    }
    return {
      keypair: deterministic ?? persisted,
      path,
      created: false,
    };
  }
  const keypair = deterministic ?? Keypair.generate();
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

  const configuredKeypairPath =
    process.env.FORK_LAB_PAYER_KEYPAIR ?? process.env.ANCHOR_WALLET;
  const keypairPath = configuredKeypairPath ?? "deployer-keypair.json";
  const resolved = resolve(keypairPath);
  if (existsSync(resolved)) return readKeypairFile(resolved);

  if (process.env.DUSK_REQUIRE_EXPLICIT_FORK_SIGNER === "true") {
    throw new Error(
      "Hosted Surfpool requires FORK_LAB_PAYER_KEYPAIR_JSON, " +
        "FORK_LAB_PAYER_KEYPAIR_BASE64, or an existing explicit signer path",
    );
  }

  return loadOrCreateKeypair("payer").keypair;
}

function initializeRuntime() {
  if (runtime) return runtime;

  try {
    const payer = loadPayer();
    const connection = new Connection(SURFPOOL_RPC_URL, "confirmed");
    // web3.js caches a legacy transaction blockhash for 30 seconds. Surfnet's
    // transaction-mode bank can advance beyond that cached hash much sooner,
    // especially between the two market bootstraps. Force each legacy
    // send/simulation to poll a new blockhash instead of reusing the cache.
    (connection as unknown as { _disableBlockhashCaching: boolean })
      ._disableBlockhashCaching = true;
    const provider = new anchor.AnchorProvider(connection, new anchor.Wallet(payer), {
      commitment: "confirmed",
      preflightCommitment: "confirmed",
      skipPreflight: false,
    });
    const loadedIdl = loadIdl();
    const idl = loadedIdl.idl;
    const program = new anchor.Program(idl as any, provider);
    const accountCoder = new anchor.BorshAccountsCoder(idl);
    anchor.setProvider(provider);
    runtime = {
      payer,
      connection,
      provider,
      program,
      idl,
      accountCoder,
      idlPath: loadedIdl.path,
      idlRawSha256: loadedIdl.rawSha256,
      idlCanonicalSha256: loadedIdl.canonicalSha256,
    };
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

function deriveLeverageCustodyAuthority(order: PublicKey): PublicKey {
  return PublicKey.findProgramAddressSync(
    [seed("leverage_delegate_authority"), order.toBuffer()],
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

function paramsHashForMarket(
  label: string,
  baseMint: PublicKey,
  quoteMint: PublicKey,
  kind: ForkMarketKind = "cpmm",
  allowGenericOverride = true,
): Buffer {
  const kindOverride = duskEnv(`FORK_PARAMS_HASH_${kind.toUpperCase()}`);
  const override =
    kindOverride ??
    (allowGenericOverride
      ? (duskEnv("FORK_PARAMS_HASH") ?? duskEnv("MARKET_PARAMS_HASH"))
      : undefined);
  if (override) {
    const bytes = Buffer.from(override.replace(/^0x/, ""), "hex");
    if (bytes.length !== 32) {
      throw new Error(`DUSK_FORK_PARAMS_HASH_${kind.toUpperCase()} must be 32 bytes`);
    }
    return bytes;
  }
  return createHash("sha256")
    .update(`dusk-mainnet-fork:v2:${kind}:${label}:${baseMint.toBase58()}:${quoteMint.toBase58()}`)
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

function parseUnits(value: string | number | bigint | undefined,
  decimals: number,
): bigint {
  if (typeof value === "bigint") return value;
  const raw = String(value ?? "0").trim();
  if (!/^\d+(\.\d+)?$/.test(raw)) throw new Error(`Invalid decimal amount: ${raw}`);
  const [whole, fraction = ""] = raw.split(".");
  const normalizedFraction = fraction.padEnd(decimals, "0").slice(0, decimals);
  return (
    BigInt(whole) * 10n ** BigInt(decimals) + BigInt(normalizedFraction || "0")
  );
}

async function rpcRequest(method: string, params: unknown[]) {
  const response = await fetch(SURFPOOL_RPC_URL, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }),
  });
  const payload = (await response.json()) as {
    result?: unknown;
    error?: unknown;
  };
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
  let mutationAttempted = false;
  try {
    let absoluteTimestamp: number | null = null;
    let timestampResult: unknown = null;
    if (seconds > 0) {
      const currentSlot = await connection.getSlot("confirmed");
      const [blockTime, clockAccount] = await Promise.all([
        connection.getBlockTime(currentSlot),
        connection.getAccountInfo(SYSVAR_CLOCK_PUBKEY, "confirmed"),
      ]);
      const clockUnixTimestamp = clockAccount && clockAccount.data.length >= 40
        ? Number(clockAccount.data.readBigInt64LE(32))
        : null;
      if (clockUnixTimestamp !== null && !Number.isSafeInteger(clockUnixTimestamp)) {
        throw new Error("Fork Clock unix timestamp is outside the safe integer range");
      }
      // Surfpool time travel mutates the Clock sysvar independently from the
      // remote-backed getBlockTime surface. Always advance from the newest
      // observed time so a later scenario can never request travel backwards.
      const currentTimestamp = Math.max(
        blockTime ?? Number.MIN_SAFE_INTEGER,
        clockUnixTimestamp ?? Number.MIN_SAFE_INTEGER,
        Math.floor(Date.now() / 1_000),
      );
      absoluteTimestamp =
        currentTimestamp * 1_000 + seconds * 1_000;
      mutationAttempted = true;
      timestampResult = await rpcRequest("surfnet_timeTravel", [
        { absoluteTimestamp },
      ]);
    }
    // Surfpool writes an epoch-relative Clock.slot during timestamp travel. Apply
    // absolute-slot travel last so programs observe the same slot returned by RPC.
    const slot = await connection.getSlot("confirmed");
    const absoluteSlot = slot + slots;
    let slotResult: unknown = null;
    if (slots > 0) {
      mutationAttempted = true;
      slotResult = await rpcRequest("surfnet_timeTravel", [{ absoluteSlot }]);
    }
    const clockBeforeNormalization = await connection.getAccountInfo(
      SYSVAR_CLOCK_PUBKEY,
      "confirmed",
    );
    const clockSlotBeforeNormalization = clockBeforeNormalization
      ? clockBeforeNormalization.data.readBigUInt64LE(0).toString()
      : null;
    let normalizationSignature: string | null = null;
    if (seconds === 0 && slots > 0) {
      const { provider, payer } = initializeRuntime();
      mutationAttempted = true;
      normalizationSignature = await provider.sendAndConfirm(
        new Transaction().add(
          ComputeBudgetProgram.setComputeUnitLimit({ units: 200_000 }),
        ),
        [payer],
      );
    }
    const clockAfterNormalization = await connection.getAccountInfo(
      SYSVAR_CLOCK_PUBKEY,
      "confirmed",
    );
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
  } catch (error) {
    if (mutationAttempted) {
      throw new ForkMutationOutcomeUncertainError("Fork time travel", error);
    }
    throw error;
  }
}

type LamportAirdropSurface = Pick<
  Connection,
  "requestAirdrop" | "confirmTransaction"
>;

export async function requestLamportAirdrop(
  connection: LamportAirdropSurface,
  pubkey: PublicKey,
  lamports: number,
): Promise<void> {
  try {
    const signature = await connection.requestAirdrop(pubkey, lamports);
    const confirmation = await connection.confirmTransaction(
      signature,
      "confirmed",
    );
    if (confirmation.value.err) {
      throw new Error(
        `Airdrop transaction ${signature} failed: ${JSON.stringify(confirmation.value.err)}`,
      );
    }
  } catch (error) {
    // Once requestAirdrop has been attempted, a transport or confirmation
    // failure cannot prove that the fork rejected the mutation. Never follow
    // an ambiguous airdrop with an exact surfnet_setAccount write: the airdrop
    // may already have landed and the fallback could lower an existing wallet.
    throw new ForkMutationOutcomeUncertainError("Fork SOL funding", error);
  }
}

async function setLamports(pubkey: PublicKey, sol: number) {
  const lamports = lamportsForSolFunding(sol);
  if (!shouldMutateWalletLamports(sol)) return;
  const { connection } = initializeRuntime();
  await requestLamportAirdrop(connection, pubkey, lamports);
}

function lamportsForSolFunding(sol: number): number {
  const lamports = sol * LAMPORTS_PER_SOL;
  if (!Number.isSafeInteger(lamports) || lamports < 0) {
    throw new Error(
      "Fork SOL funding must resolve to a nonnegative safe-integer lamport amount",
    );
  }
  return lamports;
}

function shouldMutateWalletLamports(sol: number): boolean {
  return sol > 0;
}

type ExistingWalletAccount = {
  owner: PublicKey;
  executable: boolean;
  data: { length: number };
};

function requireFundableForkWallet(
  wallet: PublicKey,
  existingAccount: ExistingWalletAccount | null,
): void {
  if (!PublicKey.isOnCurve(wallet.toBytes())) {
    throw new Error("Fork wallet funding requires an on-curve wallet address");
  }
  if (!existingAccount) return;
  if (
    !existingAccount.owner.equals(SystemProgram.programId) ||
    existingAccount.executable ||
    existingAccount.data.length !== 0
  ) {
    throw new Error(
      "Fork wallet funding requires an absent or plain SystemProgram-owned wallet account",
    );
  }
}

function monotonicForkTokenFundingAmount(
  current: bigint,
  requested: bigint,
): bigint {
  return requested > current ? requested : current;
}

function additiveForkTokenTopUpAmount(
  current: bigint,
  requestedMinimum: bigint,
): bigint {
  return requestedMinimum > current ? requestedMinimum - current : 0n;
}

function grossTransferAmountForNet(
  netAmount: bigint,
  feeForAmount: (amount: bigint) => bigint,
): bigint {
  if (netAmount <= 0n) return 0n;
  let low = netAmount;
  let high = BigInt(Number.MAX_SAFE_INTEGER);
  if (high - feeForAmount(high) < netAmount) {
    throw new Error(
      "Fork token top-up cannot satisfy the requested net amount within the JSON safe-integer range",
    );
  }
  while (low < high) {
    const middle = low + (high - low) / 2n;
    if (middle - feeForAmount(middle) >= netAmount) high = middle;
    else low = middle + 1n;
  }
  return low;
}

type ForkFundingAssetPair = Pick<
  StoredMarket,
  "baseMint" | "quoteMint" | "baseTokenProgram" | "quoteTokenProgram"
>;

function forkFundingAssetPairMatches(
  selected: ForkFundingAssetPair,
  configured: ForkFundingAssetPair,
): boolean {
  return (
    selected.baseMint === configured.baseMint &&
    selected.quoteMint === configured.quoteMint &&
    selected.baseTokenProgram === configured.baseTokenProgram &&
    selected.quoteTokenProgram === configured.quoteTokenProgram
  );
}

type ForkTokenFundingPlan = {
  owner: PublicKey;
  mint: PublicKey;
  requestedMinimum: bigint;
  tokenAccount: PublicKey;
  tokenProgram: PublicKey;
};

async function prepareTokenFunding(
  owner: PublicKey,
  mint: PublicKey,
  amount: bigint,
  tokenProgram: PublicKey,
): Promise<ForkTokenFundingPlan> {
  if (amount > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw new Error(
      `surfnet_setTokenAccount amount is above JSON safe integer range: ${amount}`,
    );
  }
  const { connection } = initializeRuntime();
  const tokenAccount = getAssociatedTokenAddressSync(
    mint,
    owner,
    false,
    tokenProgram,
  );
  const existing = await connection.getAccountInfo(
    tokenAccount,
    DEPLOYMENT_COMMITMENT,
  );
  // Parse an existing ATA before any mutation so a malformed/wrong-program
  // account fails closed. Its amount is deliberately not carried into the
  // execution plan: the public wallet may spend or receive tokens between
  // preparation and the additive top-up.
  if (existing) {
    const decoded = await getAccount(
      connection,
      tokenAccount,
      DEPLOYMENT_COMMITMENT,
      tokenProgram,
    );
    if (
      !decoded.isInitialized ||
      decoded.isFrozen ||
      !decoded.mint.equals(mint) ||
      !decoded.owner.equals(owner)
    ) {
      throw new Error(
        "Fork token funding requires an initialized, unfrozen associated token account owned by the requested wallet",
      );
    }
  }
  return {
    owner,
    mint,
    requestedMinimum: amount,
    tokenAccount,
    tokenProgram,
  };
}

async function setTokenBalance(plan: ForkTokenFundingPlan) {
  const {
    owner,
    mint,
    requestedMinimum,
    tokenAccount,
    tokenProgram,
  } = plan;
  const { connection, payer, provider } = initializeRuntime();
  const latestTarget = await connection.getAccountInfo(
    tokenAccount,
    DEPLOYMENT_COMMITMENT,
  );
  const latestAmount = latestTarget
    ? (await getAccount(
        connection,
        tokenAccount,
        DEPLOYMENT_COMMITMENT,
        tokenProgram,
      )).amount
    : 0n;
  const netTopUp = additiveForkTokenTopUpAmount(
    latestAmount,
    requestedMinimum,
  );
  if (netTopUp === 0n) return;

  if (forkMarketFixture() !== "mainnet") {
    if (!latestTarget) {
      await createAtaIfMissing({
        payer,
        owner,
        mint,
        tokenProgram,
      });
    }
    const decimals = (
      await getMint(connection, mint, DEPLOYMENT_COMMITMENT, tokenProgram)
    ).decimals;
    await provider.sendAndConfirm(
      new Transaction().add(
        createMintToCheckedInstruction(
          mint,
          tokenAccount,
          payer.publicKey,
          netTopUp,
          decimals,
          [],
          tokenProgram,
        ),
      ),
      [payer],
    );
  } else {
    const mintAccount = await getMint(
      connection,
      mint,
      DEPLOYMENT_COMMITMENT,
      tokenProgram,
    );
    let grossTopUp = netTopUp;
    if (tokenProgram.equals(TOKEN_2022_PROGRAM_ID)) {
      const transferFeeConfig = getTransferFeeConfig(mintAccount);
      if (transferFeeConfig) {
        const epoch = BigInt(
          (await connection.getEpochInfo(DEPLOYMENT_COMMITMENT)).epoch,
        );
        grossTopUp = grossTransferAmountForNet(
          netTopUp,
          (candidate) => calculateEpochFee(transferFeeConfig, epoch, candidate),
        );
      }
    }
    if (grossTopUp > BigInt(Number.MAX_SAFE_INTEGER)) {
      throw new Error("Fork token top-up exceeds the JSON safe-integer range");
    }

    // Never exact-write a public user's ATA. A concurrent public transaction
    // or another API replica can credit that ATA after our read; an absolute
    // surfnet_setTokenAccount would then erase the newer balance. Cheatcode
    // only a deterministic controller-owned reservoir and transfer additively
    // through the real token program. Races can overfund or fail, but cannot
    // lower the user's balance.
    const faucetAuthority = deriveForkKeypair(
      payer,
      `public-token-faucet:${mint.toBase58()}:${tokenProgram.toBase58()}`,
    );
    const faucetTokenAccount = getAssociatedTokenAddressSync(
      mint,
      faucetAuthority.publicKey,
      false,
      tokenProgram,
    );
    await rpcRequest("surfnet_setTokenAccount", [
      faucetAuthority.publicKey.toBase58(),
      mint.toBase58(),
      {
        amount: Number.MAX_SAFE_INTEGER,
        state: "initialized",
      },
      tokenProgram.toBase58(),
    ]);

    const instructions: TransactionInstruction[] = [];
    if (!latestTarget) {
      instructions.push(
        createAssociatedTokenAccountIdempotentInstruction(
          payer.publicKey,
          tokenAccount,
          owner,
          mint,
          tokenProgram,
          ASSOCIATED_TOKEN_PROGRAM_ID,
        ),
      );
    }
    instructions.push(
      tokenProgram.equals(TOKEN_2022_PROGRAM_ID)
        ? await createTransferCheckedWithTransferHookInstruction(
            connection,
            faucetTokenAccount,
            mint,
            tokenAccount,
            faucetAuthority.publicKey,
            grossTopUp,
            mintAccount.decimals,
            [],
            DEPLOYMENT_COMMITMENT,
            tokenProgram,
          )
        : createTransferCheckedInstruction(
            faucetTokenAccount,
            mint,
            tokenAccount,
            faucetAuthority.publicKey,
            grossTopUp,
            mintAccount.decimals,
            [],
            tokenProgram,
          ),
    );
    await provider.sendAndConfirm(new Transaction().add(...instructions), [
      payer,
      faucetAuthority,
    ]);
  }

  const finalAmount = (
    await getAccount(
      connection,
      tokenAccount,
      DEPLOYMENT_COMMITMENT,
      tokenProgram,
    )
  ).amount;
  if (finalAmount < requestedMinimum) {
    throw new ForkMutationOutcomeUncertainError(
      "Fork token top-up",
      new Error(
        `wallet balance ${finalAmount} remains below requested minimum ${requestedMinimum}`,
      ),
    );
  }
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

function forkGenerationMarkerAddress(): PublicKey {
  return pda(seed("surfpool_fork_generation_v1"));
}

function controllerSignerMarkerAddress(): PublicKey {
  return pda(seed("surfpool_controller_signer_v1"));
}

function seededLiquidityMarkerAddress(market: PublicKey): PublicKey {
  return pda(seed("surfpool_seeded_liquidity_v1"), market.toBuffer());
}

async function hasSeededLiquidityMarker(market: PublicKey): Promise<boolean> {
  const { connection } = initializeRuntime();
  const marker = seededLiquidityMarkerAddress(market);
  const account = await connection.getAccountInfo(
    marker,
    DEPLOYMENT_COMMITMENT,
  );
  if (!account) return false;
  if (!account.owner.equals(PROGRAM_ID)) {
    throw new Error(
      `Seeded-liquidity marker ${marker.toBase58()} has wrong owner ${account.owner.toBase58()}`,
    );
  }
  if (!account.data.equals(market.toBuffer())) {
    throw new Error(
      `Seeded-liquidity marker ${marker.toBase58()} has invalid market data`,
    );
  }
  return true;
}

async function recordSeededLiquidity(market: PublicKey): Promise<void> {
  await setRawAccount({
    pubkey: seededLiquidityMarkerAddress(market),
    owner: PROGRAM_ID,
    lamports: 1_000_000,
    data: market.toBuffer(),
  });
  if (!(await hasSeededLiquidityMarker(market))) {
    throw new Error(
      `Seeded-liquidity marker was not persisted for ${market.toBase58()}`,
    );
  }
}

async function verifyControllerSignerMarker(): Promise<void> {
  const { connection, payer } = initializeRuntime();
  const marker = controllerSignerMarkerAddress();
  const account = await connection.getAccountInfo(
    marker,
    DEPLOYMENT_COMMITMENT,
  );
  if (!account) {
    throw new Error(
      `Surfpool controller signer marker ${marker.toBase58()} is missing`,
    );
  }
  if (!account.owner.equals(PROGRAM_ID)) {
    throw new Error(
      `Surfpool controller signer marker has wrong owner ${account.owner.toBase58()}`,
    );
  }
  if (!account.data.equals(payer.publicKey.toBuffer())) {
    throw new Error(
      `API signer ${payer.publicKey.toBase58()} does not match the RPC bootstrap controller`,
    );
  }
}

async function recordControllerSignerMarker(): Promise<void> {
  const { connection, payer } = initializeRuntime();
  const marker = controllerSignerMarkerAddress();
  const existing = await connection.getAccountInfo(
    marker,
    DEPLOYMENT_COMMITMENT,
  );
  if (!existing) {
    await setRawAccount({
      pubkey: marker,
      owner: PROGRAM_ID,
      lamports: 1_000_000,
      data: payer.publicKey.toBuffer(),
    });
  }
  await verifyControllerSignerMarker();
}

function deriveForkGenerationId(
  namespace: string,
  genesisHash: string,
  programId: string,
  markerData: Buffer,
): string {
  return `surfpool-${sha256(
    `${namespace}:${genesisHash}:${programId}:${markerData.toString("hex")}`,
  )}`;
}

async function createForkGenerationMarker(): Promise<void> {
  const marker = forkGenerationMarkerAddress();
  await setRawAccount({
    pubkey: marker,
    owner: PROGRAM_ID,
    lamports: 1_000_000,
    data: randomBytes(32),
  });
}

type ForkGenerationObservation = {
  markerData: Buffer;
  markerHex: string;
  sourceSlot: number;
};

async function observeForkGeneration(
  minimumContextSlot: number,
): Promise<ForkGenerationObservation> {
  const { connection } = initializeRuntime();
  const marker = forkGenerationMarkerAddress();
  let observation = await connection.getAccountInfoAndContext(marker, {
    commitment: DEPLOYMENT_COMMITMENT,
    minContextSlot: minimumContextSlot,
  });
  if (!observation.value) {
    if (process.env.DUSK_REQUIRE_EXTERNAL_FORK_MARKER === "true") {
      throw new Error(
        `Surfpool fork generation marker ${marker.toBase58()} is missing; the RPC service must initialize it`,
      );
    }
    if (!forkGenerationPromise) {
      forkGenerationPromise = createForkGenerationMarker()
        .then(() => "created")
        .finally(() => {
          forkGenerationPromise = undefined;
        });
    }
    await forkGenerationPromise;
    observation = await connection.getAccountInfoAndContext(marker, {
      commitment: DEPLOYMENT_COMMITMENT,
      minContextSlot: minimumContextSlot,
    });
  }
  const account = observation.value;
  if (!account)
    throw new Error("Surfpool fork generation marker was not persisted");
  if (!account.owner.equals(PROGRAM_ID)) {
    throw new Error(
      `Surfpool fork generation marker has wrong owner ${account.owner.toBase58()}`,
    );
  }
  if (account.data.length !== 32) {
    throw new Error(
      `Surfpool fork generation marker has invalid length ${account.data.length}; expected 32`,
    );
  }
  return {
    markerData: Buffer.from(account.data),
    markerHex: account.data.toString("hex"),
    sourceSlot: observation.context.slot,
  };
}

function deploymentBuildRevision(): string {
  return (
    process.env.DUSK_BUILD_REVISION ??
    process.env.RAILWAY_GIT_COMMIT_SHA ??
    process.env.VERCEL_GIT_COMMIT_SHA ??
    "snapshot0-local-unversioned"
  );
}

function resetWeb3TransportCachesAfterForkChange(connection: Connection): void {
  const mutable = connection as unknown as {
    _pollingBlockhash: boolean;
    _blockhashInfo: {
      latestBlockhash: null;
      lastFetch: number;
      simulatedSignatures: string[];
      transactionSignatures: string[];
    };
  };
  mutable._pollingBlockhash = false;
  mutable._blockhashInfo = {
    latestBlockhash: null,
    lastFetch: 0,
    simulatedSignatures: [],
    transactionSignatures: [],
  };
}

function parseUpgradeableProgramDataHeader(data: Buffer): {
  programDataSlot: string;
  upgradeAuthority: string | null;
} {
  if (
    data.length < 13 ||
    data.readUInt32LE(0) !== UPGRADEABLE_PROGRAM_DATA_TAG
  ) {
    throw new Error("Malformed upgradeable ProgramData header");
  }
  const programDataSlot = data.readBigUInt64LE(4).toString();
  const authorityOption = data[12];
  if (authorityOption === 0) return { programDataSlot, upgradeAuthority: null };
  if (
    authorityOption !== 1 ||
    data.length < UPGRADEABLE_PROGRAM_DATA_METADATA_BYTES
  ) {
    throw new Error("Malformed upgradeable ProgramData authority option");
  }
  return {
    programDataSlot,
    upgradeAuthority: new PublicKey(data.subarray(13, 45)).toBase58(),
  };
}

function cacheProgramBinaryHash(
  cacheKey: string,
  load: () => Promise<string>,
): Promise<string> {
  const existing = programBinaryHashPromises.get(cacheKey);
  if (existing) return existing;
  const pending = load().catch((error) => {
    if (programBinaryHashPromises.get(cacheKey) === pending) {
      programBinaryHashPromises.delete(cacheKey);
    }
    throw error;
  });
  programBinaryHashPromises.set(cacheKey, pending);
  while (programBinaryHashPromises.size > 32) {
    const oldest = programBinaryHashPromises.keys().next().value as
      | string
      | undefined;
    if (!oldest || oldest === cacheKey) break;
    programBinaryHashPromises.delete(oldest);
  }
  return pending;
}

async function observeUpgradeableProgram(
  programId: PublicKey,
  minimumContextSlot: number,
  forkId: string,
) {
  const { connection } = initializeRuntime();
  const programObservation = await connection.getAccountInfoAndContext(
    programId,
    {
      commitment: DEPLOYMENT_COMMITMENT,
      minContextSlot: minimumContextSlot,
      dataSlice: { offset: 0, length: 36 },
    },
  );
  const programAccount = programObservation.value;
  if (!programAccount?.executable) {
    throw new Error(
      `Program ${programId.toBase58()} is missing or not executable`,
    );
  }
  if (!programAccount.owner.equals(BPF_LOADER_UPGRADEABLE_ID)) {
    throw new Error(
      `Program ${programId.toBase58()} has unsupported loader ${programAccount.owner.toBase58()}`,
    );
  }
  if (
    programAccount.data.length < 36 ||
    programAccount.data.readUInt32LE(0) !== UPGRADEABLE_PROGRAM_TAG
  ) {
    throw new Error(
      `Program ${programId.toBase58()} has malformed upgradeable-loader state`,
    );
  }
  const programDataAddress = new PublicKey(programAccount.data.subarray(4, 36));
  const programDataObservation = await connection.getAccountInfoAndContext(
    programDataAddress,
    {
      commitment: DEPLOYMENT_COMMITMENT,
      minContextSlot: Math.max(
        minimumContextSlot,
        programObservation.context.slot,
      ),
      dataSlice: { offset: 0, length: UPGRADEABLE_PROGRAM_DATA_METADATA_BYTES },
    },
  );
  const programDataAccount = programDataObservation.value;
  if (
    !programDataAccount ||
    !programDataAccount.owner.equals(BPF_LOADER_UPGRADEABLE_ID)
  ) {
    throw new Error(
      `Program data ${programDataAddress.toBase58()} for ${programId.toBase58()} is missing or has the wrong loader`,
    );
  }
  const header = parseUpgradeableProgramDataHeader(programDataAccount.data);
  const cacheKey = canonicalJson({
    forkId,
    programId: programId.toBase58(),
    programDataAddress: programDataAddress.toBase58(),
    programDataSlot: header.programDataSlot,
    upgradeAuthority: header.upgradeAuthority,
  });
  const binarySha256 = await cacheProgramBinaryHash(cacheKey, async () => {
    const fullObservation = await connection.getAccountInfoAndContext(
      programDataAddress,
      {
        commitment: DEPLOYMENT_COMMITMENT,
        minContextSlot: programDataObservation.context.slot,
      },
    );
    const fullAccount = fullObservation.value;
    if (!fullAccount || !fullAccount.owner.equals(BPF_LOADER_UPGRADEABLE_ID)) {
      throw new Error(
        `Program data ${programDataAddress.toBase58()} changed while hashing ${programId.toBase58()}`,
      );
    }
    if (fullAccount.data.length <= UPGRADEABLE_PROGRAM_DATA_METADATA_BYTES) {
      throw new Error(
        `Program data ${programDataAddress.toBase58()} for ${programId.toBase58()} has no binary payload`,
      );
    }
    const fullHeader = parseUpgradeableProgramDataHeader(fullAccount.data);
    if (
      fullHeader.programDataSlot !== header.programDataSlot ||
      fullHeader.upgradeAuthority !== header.upgradeAuthority
    ) {
      throw new DeploymentIdentityChangedError(
        `Program data ${programDataAddress.toBase58()} changed while its binary was being hashed`,
      );
    }
    return sha256(
      fullAccount.data.subarray(UPGRADEABLE_PROGRAM_DATA_METADATA_BYTES),
    );
  });
  return {
    programDataAddress: programDataAddress.toBase58(),
    programDataSlot: header.programDataSlot,
    upgradeAuthority: header.upgradeAuthority,
    binarySha256,
    sourceSlot: Math.min(
      programObservation.context.slot,
      programDataObservation.context.slot,
    ),
  };
}

export async function deploymentEnvelope(minimumSourceSlot = 0) {
  if (!Number.isSafeInteger(minimumSourceSlot) || minimumSourceSlot < 0) {
    throw new Error(
      "Deployment envelope minimum source slot must be a nonnegative safe integer",
    );
  }
  const current = initializeRuntime();
  const delegateIdl = loadLeverageDelegateIdl();
  const forkGeneration = await observeForkGeneration(minimumSourceSlot);
  const genesisHash = await lifecycleGenesisHash.read(
    forkGeneration.markerHex,
    async () => (
      await observeForkGeneration(forkGeneration.sourceSlot)
    ).markerHex,
  );
  const namespace = process.env.DUSK_FORK_NAMESPACE ?? "dusk-surfpool";
  const forkId = deriveForkGenerationId(
    namespace,
    genesisHash,
    PROGRAM_ID.toBase58(),
    forkGeneration.markerData,
  );
  if (
    observedRuntimeForkId &&
    observedRuntimeForkId !== forkId
  ) {
    resetWeb3TransportCachesAfterForkChange(current.connection);
  }
  observedRuntimeForkId = forkId;
  const [duskProgram, delegateProgram] = await Promise.all([
    observeUpgradeableProgram(
      PROGRAM_ID,
      minimumSourceSlot,
      forkId,
    ),
    observeUpgradeableProgram(
      LEVERAGE_DELEGATE_PROGRAM_ID,
      minimumSourceSlot,
      forkId,
    ),
  ]);
  const sourceSlot = Math.min(
    duskProgram.sourceSlot,
    delegateProgram.sourceSlot,
    forkGeneration.sourceSlot,
  );
  const envelope = {
    schemaVersion: DEPLOYMENT_SCHEMA_VERSION,
    network: "surfpool",
    forkSourceNetwork: process.env.SURFPOOL_NETWORK ?? "mainnet",
    genesisHash,
    forkId,
    programId: PROGRAM_ID.toBase58(),
    programDataAddress: duskProgram.programDataAddress,
    programDataSlot: duskProgram.programDataSlot,
    programUpgradeAuthority: duskProgram.upgradeAuthority,
    leverageDelegateProgramId: LEVERAGE_DELEGATE_PROGRAM_ID.toBase58(),
    leverageDelegateProgramDataAddress: delegateProgram.programDataAddress,
    leverageDelegateProgramDataSlot: delegateProgram.programDataSlot,
    leverageDelegateUpgradeAuthority: delegateProgram.upgradeAuthority,
    idlSha256: current.idlCanonicalSha256,
    idlRawSha256: current.idlRawSha256,
    leverageDelegateIdlSha256: delegateIdl.canonicalSha256,
    leverageDelegateIdlRawSha256: delegateIdl.rawSha256,
    commitment: DEPLOYMENT_COMMITMENT,
    sourceSlot,
    observedAt: new Date().toISOString(),
    apiStartedAt: API_STARTED_AT,
    buildRevision: deploymentBuildRevision(),
    programBinarySha256: duskProgram.binarySha256,
    leverageDelegateBinarySha256: delegateProgram.binarySha256,
  };
  return {
    ...envelope,
    deploymentIdentitySha256: deploymentIdentityFingerprint(envelope),
  };
}

function maximumResponseSourceSlot(value: unknown): number {
  if (value === null || typeof value !== "object" || Array.isArray(value))
    return 0;
  const data = (value as Record<string, unknown>).data;
  if (data === null || typeof data !== "object" || Array.isArray(data))
    return 0;
  const dataRecord = data as Record<string, unknown>;
  const markets = Array.isArray(dataRecord.markets)
    ? dataRecord.markets
    : typeof dataRecord.marketAddress === "string"
      ? [dataRecord]
      : [];
  return markets.reduce<number>((maximum, market) => {
    if (market === null || typeof market !== "object" || Array.isArray(market))
      return maximum;
    const state = (market as Record<string, unknown>).state;
    if (state === null || typeof state !== "object" || Array.isArray(state))
      return maximum;
    const stateRecord = state as Record<string, unknown>;
    return [
      stateRecord.sourceSlot,
      stateRecord.healthSourceSlot,
    ].reduce<number>(
      (marketMaximum, sourceSlot) =>
        Number.isSafeInteger(sourceSlot) && (sourceSlot as number) >= 0
          ? Math.max(marketMaximum, sourceSlot as number)
          : marketMaximum,
      maximum,
    );
  }, 0);
}

function deploymentIdentityFingerprint(
  deployment: Record<string, any>,
): string {
  return sha256(
    canonicalJson({
      schemaVersion: deployment.schemaVersion,
      network: deployment.network,
      forkSourceNetwork: deployment.forkSourceNetwork,
      genesisHash: deployment.genesisHash,
      forkId: deployment.forkId,
      programId: deployment.programId,
      programDataAddress: deployment.programDataAddress,
      programDataSlot: deployment.programDataSlot,
      programUpgradeAuthority: deployment.programUpgradeAuthority,
      leverageDelegateProgramId: deployment.leverageDelegateProgramId,
      leverageDelegateProgramDataAddress:
        deployment.leverageDelegateProgramDataAddress,
      leverageDelegateProgramDataSlot:
        deployment.leverageDelegateProgramDataSlot,
      leverageDelegateUpgradeAuthority:
        deployment.leverageDelegateUpgradeAuthority,
      idlSha256: deployment.idlSha256,
      idlRawSha256: deployment.idlRawSha256,
      leverageDelegateIdlSha256: deployment.leverageDelegateIdlSha256,
      leverageDelegateIdlRawSha256: deployment.leverageDelegateIdlRawSha256,
      commitment: deployment.commitment,
      buildRevision: deployment.buildRevision,
      programBinarySha256: deployment.programBinarySha256,
      leverageDelegateBinarySha256: deployment.leverageDelegateBinarySha256,
    }),
  );
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
  onMutationAttempted?: () => void;
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
    params.onMutationAttempted?.();
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
    qEmaHalfLifeMs: toBN(
      duskEnv("Q_EMA_HALF_LIFE_MS") ?? duskEnv("K_EMA_HALF_LIFE_MS") ?? "60000"
    ),
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
    rangeWidthNad: toBN(duskEnv("AMM_RANGE_WIDTH_NAD") ?? "0"),
    concentratedLiquidityShareNad: toBN(
      duskEnv("AMM_CONCENTRATED_LIQUIDITY_SHARE_NAD") ?? "0"
    ),
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
    reserved: Array(33).fill(0),
  };
}

function configuredMarketKinds(fixture: ForkMarketFixture): ForkMarketKind[] {
  const configured = process.env.FORK_BOOTSTRAP_MARKETS ?? (fixture === "mainnet" ? "both" : "cpmm");
  if (configured === "both") return ["cpmm", "concentrated"];
  if (configured === "cpmm" || configured === "concentrated") return [configured];
  throw new Error(
    `Unsupported FORK_BOOTSTRAP_MARKETS: ${configured}; expected cpmm, concentrated, or both`
  );
}

function marketConfigForKind(kind: ForkMarketKind): ReturnType<typeof defaultMarketConfig> {
  const config = defaultMarketConfig();
  if (kind === "cpmm") {
    return {
      ...config,
      amm: {
        ...config.amm,
        peakDepthNad: toBN(0),
        fadeScaleNad: toBN(0),
      },
    };
  }
  return {
    ...config,
    amm: {
      ...config.amm,
      peakDepthNad: toBN(duskEnv("AMM_PEAK_DEPTH_NAD") ?? "200000000000"),
      fadeScaleNad: toBN(duskEnv("AMM_FADE_SCALE_NAD") ?? "100000000"),
    },
  };
}

function marketKindFromConfig(config: any): ForkMarketKind {
  const amm = field<any>(config, "amm") ?? config?.amm ?? {};
  return toBigInt(field(amm, "peakDepthNad", "peak_depth_nad")) > 0n &&
    toBigInt(field(amm, "fadeScaleNad", "fade_scale_nad")) > 0n
    ? "concentrated"
    : "cpmm";
}

function bootstrapMarketDefinitions(
  fixture: ForkMarketFixture,
  baseMint: PublicKey,
  quoteMint: PublicKey
): BootstrapMarketDefinition[] {
  if (baseMint.equals(quoteMint)) throw new Error("Dusk fork base and quote mints must differ");
  const kinds = configuredMarketKinds(fixture);
  if (
    kinds.length > 1 &&
    (duskEnv("FORK_PARAMS_HASH") || duskEnv("MARKET_PARAMS_HASH"))
  ) {
    throw new Error(
      "A shared DUSK_FORK_PARAMS_HASH cannot identify two markets; use " +
        "DUSK_FORK_PARAMS_HASH_CPMM and DUSK_FORK_PARAMS_HASH_CONCENTRATED"
    );
  }
  const configuredLabel = duskEnv("MARKET_LABEL");
  const labelPrefix = configuredLabel ??
    (fixture === "mainnet" ? "meta-usdc" : `dusk-${fixture}-fixture`);
  const definitions = kinds.map((kind) => {
    const label = kinds.length === 1 ? labelPrefix : `${labelPrefix}-${kind}`;
    return {
      label,
      kind,
      baseMint,
      quoteMint,
      paramsHash: paramsHashForMarket(label, baseMint, quoteMint, kind, kinds.length === 1),
      config: marketConfigForKind(kind),
    };
  });
  if (new Set(definitions.map((definition) => definition.paramsHash.toString("hex"))).size !== definitions.length) {
    throw new Error("Configured Dusk markets must use distinct params hashes");
  }
  return definitions;
}

/** Pure bootstrap helpers exported for deterministic tests that do not start Surfpool. */
export const forkMarketPureHelpers = {
  canonicalJson,
  bootstrapSignedFromBody,
  constantTimeTokenEquals,
  createGenerationPinnedGenesisHashReader,
  deploymentIdentityFingerprint,
  deriveForkKeypair,
  deriveForkGenerationId,
  forkHealthPayload,
  isForkAdminPath,
  isForkServerSignedRequest,
  maximumResponseSourceSlot,
  marketConfigFromBody,
  parseUpgradeableProgramDataHeader,
  publicForkRpcPayload,
  requestHeader,
  requireForkAdminAuthorization,
  sha256,
  loadOrCreateKeypair,
  configuredMarketKinds,
  marketConfigForKind,
  marketKindFromConfig,
  bootstrapMarketDefinitions,
  hlpSwapRemainingAccountPrefix,
  lamportsForSolFunding,
  additiveForkTokenTopUpAmount,
  forkFundingAssetPairMatches,
  grossTransferAmountForNet,
  monotonicForkTokenFundingAmount,
  requireFundableForkWallet,
  requireActionableLeverageOrderAccount,
  shouldMutateWalletLamports,
  mergeBootstrapTransactions,
};

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

async function ensureFutarchyAuthority(
  futarchyAuthority: PublicKey,
  onMutationAttempted?: () => void,
) {
  const { program, payer, accountCoder, connection } = initializeRuntime();
  const existing = await program.account.futarchyAuthority.fetchNullable(futarchyAuthority);
  if (existing) {
    recordFutarchyAuthorityBootstrapMode("preexisting", {
      onlyIfMissing: true,
    });
    return existing;
  }

  onMutationAttempted?.();

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
    recordFutarchyAuthorityBootstrapMode("transaction");
    return await program.account.futarchyAuthority.fetch(futarchyAuthority);
  } catch (error) {
    // A transport error can arrive after Surfnet landed the transaction. Re-read
    // the PDA before deciding the initialization failed; fork-history discovery
    // will still recover and verify the genuine signature for API replicas.
    const landed = await program.account.futarchyAuthority.fetchNullable(
      futarchyAuthority,
    );
    if (landed) return landed;
    if (process.env.DUSK_ALLOW_SURFPOOL_AUTHORITY_ACCOUNT_SEED !== "true") {
      throw error;
    }
    console.warn(
      `initFutarchyAuthority failed; explicit Surfpool account-seed fallback is enabled: ${
        error instanceof Error ? error.message : String(error)
      }`
    );
  }

  const [, bump] = PublicKey.findProgramAddressSync([seed("futarchy_authority")], PROGRAM_ID);
  const defaultAuction = {
    accepted_mint: NATIVE_MINT,
    recipients: {
      treasury: payer.publicKey,
      staking_vault: payer.publicKey,
      treasury_bps: 10_000,
      staking_vault_bps: 0,
    },
    params: {
      start_multiplier_bps: 12_000,
      floor_multiplier_bps: 8_000,
      duration_slots: toBN(216_000),
      max_reference_age_slots: toBN(21_600),
    },
  };
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
    protocol_auction_split: {
      fee_auction_bps: 10_000,
      buyback_auction_bps: 0,
    },
    fee_auction: defaultAuction,
    buyback_auction: defaultAuction,
    global_reduce_only: false,
    bump,
  });
  await setRawAccount({
    pubkey: futarchyAuthority,
    owner: PROGRAM_ID,
    lamports: await connection.getMinimumBalanceForRentExemption(data.length),
    data,
  });
  recordFutarchyAuthorityBootstrapMode("surfpool-account-seed");
  return await program.account.futarchyAuthority.fetch(futarchyAuthority);
}

async function createHookedLpMintIfMissing(params: {
  label: string;
  decimals: number;
  mintAuthority: PublicKey;
  onMutationAttempted?: () => void;
}) {
  const { connection, payer } = initializeRuntime();
  const { keypair, path } = loadOrCreateKeypair(`mint-${params.label}`);
  const existing = await connection.getAccountInfo(keypair.publicKey, "confirmed");
  if (!existing) {
    params.onMutationAttempted?.();
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
    const baseMint = await createFixtureAssetMintIfMissing({
      label: `${fixture}-base`,
      decimals: 6,
      tokenProgram: TOKEN_2022_PROGRAM_ID,
      transferFeeBps: 100,
    });
    const quoteMint = await createFixtureAssetMintIfMissing({
      label: `${fixture}-quote`,
      decimals: 6,
      tokenProgram: TOKEN_2022_PROGRAM_ID,
      transferFeeBps: 50,
    });
    return [baseMint, quoteMint];
  }
  const baseMint = await createFixtureAssetMintIfMissing({
    label: `${fixture}-zero`,
    decimals: 0,
    tokenProgram: TOKEN_PROGRAM_ID,
  });
  const quoteMint = await createFixtureAssetMintIfMissing({
    label: `${fixture}-nine`,
    decimals: 9,
    tokenProgram: TOKEN_PROGRAM_ID,
  });
  return [baseMint, quoteMint];
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
  marketKind?: ForkMarketKind;
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
    marketKind: params.marketKind,
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
  baseMint: PublicKey;
  quoteMint: PublicKey;
  config: Record<string, unknown>;
}) {
  const { connection, program, payer } = initializeRuntime();
  const marketLabel = params.label.trim();
  if (!marketLabel) throw new Error("Market name is required");
  if (params.baseMint.equals(params.quoteMint)) throw new Error("Choose two different token mints");
  const config = marketConfigFromBody(params.config);
  const marketKind = marketKindFromConfig(config);

  const baseMint = params.baseMint;
  const quoteMint = params.quoteMint;
  const paramsHash = paramsHashForCustomMarket(
    marketLabel,
    baseMint,
    quoteMint,
  );
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
  const mintLabel = `${marketLabel}-${paramsHash.toString("hex").slice(0, 10)}`;
  let mutationAttempted = false;
  const markMutationAttempted = () => {
    mutationAttempted = true;
  };

  try {
    // Keep controller writes sequential. Promise.all can reject before a
    // sibling write starts, returning a certain-looking error while that
    // sibling subsequently mutates the fork.
    const ylp = await createHookedLpMintIfMissing({
      label: `${mintLabel}-ylp`,
      decimals: baseDecimals,
      mintAuthority: addresses.market,
      onMutationAttempted: markMutationAttempted,
    });
    const baseHlp = await createHookedLpMintIfMissing({
      label: `${mintLabel}-base-hlp`,
      decimals: baseDecimals,
      mintAuthority: addresses.market,
      onMutationAttempted: markMutationAttempted,
    });
    const quoteHlp = await createHookedLpMintIfMissing({
      label: `${mintLabel}-quote-hlp`,
      decimals: quoteDecimals,
      mintAuthority: addresses.market,
      onMutationAttempted: markMutationAttempted,
    });

    const futarchy = await ensureFutarchyAuthority(
      addresses.futarchyAuthority,
      markMutationAttempted,
    );
    const teamTreasury =
      field<PublicKey>(
        field(futarchy, "recipients"),
        "teamTreasury",
        "team_treasury",
      ) ?? payer.publicKey;
    const teamTreasuryWsolAccount = await createAtaIfMissing({
      payer,
      owner: teamTreasury,
      mint: NATIVE_MINT,
      tokenProgram: TOKEN_PROGRAM_ID,
      allowOwnerOffCurve: true,
      onMutationAttempted: markMutationAttempted,
    });
    const instruction = await program.methods
      .initializeMarket({
        config,
        paramsHash: Array.from(paramsHash),
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
      label: marketLabel,
      marketKind,
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
  } catch (error) {
    if (
      mutationAttempted &&
      !(error instanceof ForkMutationOutcomeUncertainError)
    ) {
      throw new ForkMutationOutcomeUncertainError(
        "Fork market preparation",
        error,
      );
    }
    throw error;
  }
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
  // Surfnet's transaction-mode bank advances around each submitted write.
  // Serializing controller-owned mint creation prevents sibling transactions
  // from racing on a blockhash that another submission has just advanced.
  const ylp = await createHookedLpMintIfMissing({
    label: `${marketLabel}-ylp`,
    decimals: stored.baseDecimals,
    mintAuthority: addresses.market,
  });
  const baseHlp = await createHookedLpMintIfMissing({
    label: `${marketLabel}-base-hlp`,
    decimals: stored.baseDecimals,
    mintAuthority: addresses.market,
  });
  const quoteHlp = await createHookedLpMintIfMissing({
    label: `${marketLabel}-quote-hlp`,
    decimals: stored.quoteDecimals,
    mintAuthority: addresses.market,
  });
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
  const markets = await bootstrapMarkets();
  const primary = markets[0];
  if (!primary) throw new Error("Dusk fork bootstrap produced no markets");
  return primary;
}

/** Single-controller entrypoint used by the Surfpool RPC service before API replicas start. */
export async function bootstrapForkMarkets(): Promise<StoredMarket[]> {
  await recordControllerSignerMarker();
  return bootstrapMarkets();
}

async function bootstrapMarkets(
  expectedDeploymentFingerprint?: string,
): Promise<StoredMarket[]> {
  initializeRuntime();
  const observed = await deploymentEnvelope();
  const deploymentFingerprint = deploymentIdentityFingerprint(observed);
  if (
    expectedDeploymentFingerprint &&
    expectedDeploymentFingerprint !== deploymentFingerprint
  ) {
    throw new DeploymentIdentityChangedError(
      "Deployment identity changed before market bootstrap",
    );
  }
  const existing = bootstrapPromises.get(deploymentFingerprint);
  if (existing) return existing;
  for (const cachedFingerprint of bootstrapPromises.keys()) {
    if (cachedFingerprint !== deploymentFingerprint) {
      bootstrapPromises.delete(cachedFingerprint);
    }
  }
  const pending = bootstrapQueue.then(async () => {
    const before = await deploymentEnvelope();
    if (deploymentIdentityFingerprint(before) !== deploymentFingerprint) {
      throw new DeploymentIdentityChangedError(
        "Deployment identity changed while bootstrap was waiting to start",
      );
    }
    const markets =
      process.env.DUSK_REQUIRE_PREBOOTSTRAPPED_MARKETS === "true"
        ? await verifyPrebootstrappedMarkets()
        : await bootstrapUncached(deploymentFingerprint);
    const after = await deploymentEnvelope();
    if (deploymentIdentityFingerprint(after) !== deploymentFingerprint) {
      throw new DeploymentIdentityChangedError(
        "Deployment identity changed while bootstrapping markets",
      );
    }
    return markets;
  });
  bootstrapQueue = pending.then(
    () => undefined,
    () => undefined,
  );
  const guarded = pending.catch((error) => {
    if (bootstrapPromises.get(deploymentFingerprint) === guarded) {
      bootstrapPromises.delete(deploymentFingerprint);
    }
    throw error;
  });
  bootstrapPromises.set(deploymentFingerprint, guarded);
  return guarded;
}

async function bootstrapUncached(
  deploymentFingerprint: string,
): Promise<StoredMarket[]> {
  beginBootstrapEvidence(deploymentFingerprint);
  const { payer } = initializeRuntime();
  const fixture = forkMarketFixture();
  await setLamports(payer.publicKey, DEFAULT_SOL_FUNDING);
  const [baseMint, quoteMint] = await fixtureAssetMints(fixture);
  const definitions = bootstrapMarketDefinitions(fixture, baseMint, quoteMint);
  const state = readState();
  const markets: StoredMarket[] = [];
  for (const definition of definitions) {
    markets.push(await bootstrapMarketUncached(definition, state));
  }
  return markets;
}

type ExpectedLpBootstrapTarget = {
  kind: "ylp" | "baseHlp" | "quoteHlp";
  mint: string;
  metadata: string;
  validation: string | undefined;
};

function expectedLpBootstrapTargets(
  market: StoredMarket,
): ExpectedLpBootstrapTarget[] {
  return [
    {
      kind: "ylp",
      mint: market.ylpMint,
      metadata: market.ylpTokenMetadata,
      validation: market.transferHookValidationAccounts.ylp,
    },
    {
      kind: "baseHlp",
      mint: market.baseHlpMint,
      metadata: market.baseHlpTokenMetadata,
      validation: market.transferHookValidationAccounts.baseHlp,
    },
    {
      kind: "quoteHlp",
      mint: market.quoteHlpMint,
      metadata: market.quoteHlpTokenMetadata,
      validation: market.transferHookValidationAccounts.quoteHlp,
    },
  ];
}

function publicKeyAt(accountKeys: PublicKey[], index: number): string | null {
  return accountKeys[index]?.toBase58() ?? null;
}

function matchesBootstrapAccounts(
  accountKeys: PublicKey[],
  expected: Array<string | PublicKey>,
): boolean {
  return expected.every((value, index) =>
    publicKeyAt(accountKeys, index) ===
      (value instanceof PublicKey ? value.toBase58() : value),
  );
}

function expectedBootstrapInstructionLabel(
  instructionName: string,
  accountKeys: PublicKey[],
  markets: StoredMarket[],
  payer: PublicKey,
): string | null {
  if (instructionName === "init_futarchy_authority") {
    return matchesBootstrapAccounts(accountKeys, [
      payer,
      pda(seed("futarchy_authority")),
      deriveProgramDataAddress(),
      SystemProgram.programId,
    ])
      ? "initialize futarchy authority"
      : null;
  }

  if (instructionName === "initialize_market") {
    const market = markets.find((candidate) =>
      matchesBootstrapAccounts(accountKeys, [
        payer,
        candidate.baseMint,
        candidate.quoteMint,
        candidate.ylpMint,
        candidate.baseHlpMint,
        candidate.quoteHlpMint,
        candidate.market,
        pda(seed("futarchy_authority")),
      ]) &&
      publicKeyAt(accountKeys, 18) === SystemProgram.programId.toBase58() &&
      publicKeyAt(accountKeys, 19) === TOKEN_PROGRAM_ID.toBase58() &&
      publicKeyAt(accountKeys, 20) === TOKEN_2022_PROGRAM_ID.toBase58() &&
      publicKeyAt(accountKeys, 21) === candidate.eventAuthority &&
      publicKeyAt(accountKeys, 22) === PROGRAM_ID.toBase58()
    );
    return market ? `initialize market ${market.label}` : null;
  }

  if (instructionName === "initialize_lp_transfer_hook") {
    for (const market of markets) {
      for (const target of expectedLpBootstrapTargets(market)) {
        if (
          target.validation &&
          matchesBootstrapAccounts(accountKeys, [
            payer,
            market.market,
            target.mint,
            target.validation,
            SystemProgram.programId,
          ])
        ) {
          return `initialize ${market.label} ${target.kind} transfer hook`;
        }
      }
    }
    return null;
  }

  if (instructionName === "initialize_lp_metadata") {
    for (const market of markets) {
      for (const target of expectedLpBootstrapTargets(market)) {
        if (matchesBootstrapAccounts(accountKeys, [
          payer,
          market.market,
          target.mint,
          target.metadata,
          SystemProgram.programId,
          SYSVAR_INSTRUCTIONS_PUBKEY,
          TOKEN_2022_PROGRAM_ID,
          TOKEN_METADATA_PROGRAM_ID,
        ])) {
          return `initialize ${market.label} ${target.kind} metadata`;
        }
      }
    }
  }

  return null;
}

function bootstrapInstructionName(data: Buffer): string | null {
  if (data.length < 8) return null;
  const instructions = (initializeRuntime().idl as unknown as {
    instructions?: Array<{ name?: unknown; discriminator?: unknown }>;
  }).instructions ?? [];
  for (const instruction of instructions) {
    if (
      typeof instruction.name === "string" &&
      Array.isArray(instruction.discriminator) &&
      Buffer.from(instruction.discriminator).equals(data.subarray(0, 8))
    ) {
      return instruction.name;
    }
  }
  return null;
}

function transactionAccountKeys(transaction: any): PublicKey[] {
  const message = transaction.transaction.message;
  const staticAccountKeys = message.staticAccountKeys ?? message.accountKeys ?? [];
  const loadedAddresses = transaction.meta?.loadedAddresses;
  return [
    ...staticAccountKeys,
    ...(loadedAddresses?.writable ?? []),
    ...(loadedAddresses?.readonly ?? []),
  ].map((key: PublicKey | { pubkey: PublicKey }) =>
    key instanceof PublicKey ? key : key.pubkey,
  );
}

function compiledInstructionData(instruction: any): Buffer {
  return typeof instruction.data === "string"
    ? Buffer.from(anchor.utils.bytes.bs58.decode(instruction.data))
    : Buffer.from(instruction.data ?? []);
}

async function verifiedBootstrapTransaction(
  signature: string,
  markets: StoredMarket[],
): Promise<{ evidence: BootstrapTransactionEvidence; slot: number } | null> {
  const { connection, payer } = initializeRuntime();
  const transaction = await connection.getTransaction(signature, {
    commitment: "confirmed",
    maxSupportedTransactionVersion: 0,
  });
  if (!transaction || transaction.meta?.err) return null;

  const message: any = transaction.transaction.message;
  const transactionKeys = transactionAccountKeys(transaction);
  const compiledInstructions =
    message.compiledInstructions ?? message.instructions ?? [];
  const matchedInstructions: string[] = [];
  const labels: string[] = [];
  for (const instruction of compiledInstructions) {
    if (
      publicKeyAt(transactionKeys, instruction.programIdIndex) !==
      PROGRAM_ID.toBase58()
    ) {
      continue;
    }
    const instructionName = bootstrapInstructionName(
      compiledInstructionData(instruction),
    );
    if (!instructionName) continue;
    const accountIndexes: number[] =
      instruction.accountKeyIndexes ?? instruction.accounts ?? [];
    const instructionAccountKeys = accountIndexes.map(
      (index) => transactionKeys[index],
    );
    if (instructionAccountKeys.some((key) => !key)) continue;
    const label = expectedBootstrapInstructionLabel(
      instructionName,
      instructionAccountKeys,
      markets,
      payer.publicKey,
    );
    if (!label) continue;
    matchedInstructions.push(instructionName);
    labels.push(label);
  }
  if (matchedInstructions.length === 0) return null;
  return {
    evidence: {
      label: Array.from(new Set(labels)).join("; "),
      signature,
      instructions: Array.from(new Set(matchedInstructions)),
    },
    slot: transaction.slot,
  };
}

type BootstrapDiscoveryTarget = {
  address: PublicKey;
  instruction: string;
  label: string;
};

function bootstrapDiscoveryTargets(
  markets: StoredMarket[],
): BootstrapDiscoveryTarget[] {
  return [
    {
      address: pda(seed("futarchy_authority")),
      instruction: "init_futarchy_authority",
      label: "initialize futarchy authority",
    },
    ...markets.flatMap((market) => [
      {
        address: new PublicKey(market.market),
        instruction: "initialize_market",
        label: `initialize market ${market.label}`,
      },
      ...expectedLpBootstrapTargets(market).map((target) => ({
        address: new PublicKey(target.metadata),
        instruction: "initialize_lp_metadata",
        label: `initialize ${market.label} ${target.kind} metadata`,
      })),
    ]),
  ];
}

function evidenceMatchesTarget(
  evidence: BootstrapTransactionEvidence,
  target: BootstrapDiscoveryTarget,
): boolean {
  return evidence.label === target.label &&
    evidence.instructions.includes(target.instruction);
}

async function bootstrapEvidencePayloadUncached(
  markets: StoredMarket[],
  deploymentFingerprint: string,
): Promise<BootstrapEvidencePayload> {
  const { connection } = initializeRuntime();
  const state = readState();
  const stateMatchesDeployment =
    state.bootstrapEvidenceDeploymentFingerprint === deploymentFingerprint;
  const verifiedBySignature = new Map<
    string,
    { evidence: BootstrapTransactionEvidence; slot: number }
  >();
  const verificationPromises = new Map<
    string,
    Promise<{ evidence: BootstrapTransactionEvidence; slot: number } | null>
  >();
  const maxTransactionFetches = 512;

  const verify = (
    signature: string,
  ): Promise<{ evidence: BootstrapTransactionEvidence; slot: number } | null> => {
    const existing = verificationPromises.get(signature);
    if (existing) return existing;
    if (verificationPromises.size >= maxTransactionFetches) {
      return Promise.reject(new Error(
        `Bootstrap evidence discovery exceeded ${maxTransactionFetches} unique transaction fetches`,
      ));
    }
    const pending = verifiedBootstrapTransaction(signature, markets).then((entry) => {
      if (entry) verifiedBySignature.set(signature, entry);
      return entry;
    });
    verificationPromises.set(signature, pending);
    return pending;
  };

  const persistedTransactions = stateMatchesDeployment
    ? state.bootstrapTransactions
    : [];
  for (let offset = 0; offset < persistedTransactions.length; offset += 8) {
    await Promise.all(
      persistedTransactions.slice(offset, offset + 8).map((transaction) =>
        verify(transaction.signature),
      ),
    );
  }

  const targets = bootstrapDiscoveryTargets(markets);
  const pageSize = 32;
  const maxPagesPerTarget = 8;
  for (const target of targets) {
    if (Array.from(verifiedBySignature.values()).some((entry) =>
      evidenceMatchesTarget(entry.evidence, target)
    )) {
      continue;
    }

    let before: string | undefined;
    let found = false;
    for (let page = 0; page < maxPagesPerTarget && !found; page += 1) {
      let signatures;
      try {
        signatures = await connection.getSignaturesForAddress(
          target.address,
          { limit: pageSize, ...(before ? { before } : {}) },
          "confirmed",
        );
      } catch (error) {
        throw new Error(
          `Unable to discover ${target.label} from ${target.address.toBase58()}: ${
            error instanceof Error ? error.message : String(error)
          }`,
        );
      }
      if (signatures.length === 0) break;
      const successfulSignatures = signatures.filter((signature) => !signature.err);
      for (let offset = 0; offset < successfulSignatures.length; offset += 8) {
        await Promise.all(
          successfulSignatures.slice(offset, offset + 8).map((signature) =>
            verify(signature.signature),
          ),
        );
        found = Array.from(verifiedBySignature.values()).some((entry) =>
          evidenceMatchesTarget(entry.evidence, target)
        );
        if (found) break;
      }
      before = signatures[signatures.length - 1]?.signature;
      if (signatures.length < pageSize) break;
    }
  }

  // Select one genuine transaction per semantic bootstrap target. A fork may
  // expose older history for the same deterministic PDA after a service
  // restart; the latest successful target transaction is authoritative and
  // must not inflate instruction counts.
  const verified = targets.flatMap((target) => {
    const matches = Array.from(verifiedBySignature.values())
      .filter((entry) => evidenceMatchesTarget(entry.evidence, target))
      .sort((left, right) =>
        right.slot - left.slot ||
        right.evidence.signature.localeCompare(left.evidence.signature),
      );
    return matches.slice(0, 1);
  }).sort((left, right) =>
    left.slot - right.slot ||
    left.evidence.signature.localeCompare(right.evidence.signature),
  );
  const transactions = mergeBootstrapTransactions(
    verified.map((entry) => entry.evidence),
  );
  const hasAuthorityTransaction = transactions.some((transaction) =>
    transaction.instructions.includes("init_futarchy_authority"),
  );
  const futarchyAuthorityBootstrapMode = hasAuthorityTransaction
    ? "transaction"
    : stateMatchesDeployment && state.futarchyAuthorityBootstrapMode
      ? state.futarchyAuthorityBootstrapMode
      : "preexisting";
  const missingTargets = targets.filter((target) =>
    !transactions.some((transaction) =>
      evidenceMatchesTarget(transaction, target)
    ),
  ).filter((target) =>
    target.instruction !== "init_futarchy_authority" ||
    futarchyAuthorityBootstrapMode !== "surfpool-account-seed",
  );
  if (missingTargets.length > 0) {
    throw new Error(
      `Incomplete confirmed bootstrap evidence after bounded fork-history scan: ${
        missingTargets.map((target) => target.label).join(", ")
      }`,
    );
  }
  return { transactions, futarchyAuthorityBootstrapMode };
}

async function bootstrapEvidencePayload(
  markets: StoredMarket[],
  deploymentFingerprint: string,
): Promise<BootstrapEvidencePayload> {
  for (const cachedFingerprint of bootstrapEvidencePromises.keys()) {
    if (cachedFingerprint !== deploymentFingerprint) {
      bootstrapEvidencePromises.delete(cachedFingerprint);
    }
  }
  const existing = bootstrapEvidencePromises.get(deploymentFingerprint);
  if (existing) return existing;
  const pending = bootstrapEvidencePayloadUncached(
    markets,
    deploymentFingerprint,
  );
  const guarded = pending.catch((error) => {
    if (bootstrapEvidencePromises.get(deploymentFingerprint) === guarded) {
      bootstrapEvidencePromises.delete(deploymentFingerprint);
    }
    throw error;
  });
  bootstrapEvidencePromises.set(deploymentFingerprint, guarded);
  return guarded;
}

function requireMatchingPublicKey(
  actual: PublicKey | undefined,
  expected: PublicKey,
  label: string,
): void {
  if (!actual?.equals(expected)) {
    throw new Error(
      `${label} mismatch: expected ${expected.toBase58()}, received ${actual?.toBase58() ?? "missing"}`,
    );
  }
}

function prebootstrappedMarketDefinitions(): BootstrapMarketDefinition[] {
  const fixture = forkMarketFixture();
  if (fixture === "mainnet") {
    return bootstrapMarketDefinitions(
      fixture,
      new PublicKey(duskEnv("BASE_MINT") ?? DEFAULT_META_MINT),
      new PublicKey(duskEnv("QUOTE_MINT") ?? DEFAULT_USDC_MINT),
    );
  }

  // Synthetic fixtures generate mint keypairs during controller bootstrap. A
  // read-only verifier can use a shared manifest, but must never recreate them.
  const state = readState();
  const saved = Object.values(state.markets)[0];
  if (!saved) {
    throw new Error(
      `Read-only ${fixture} verification requires the RPC controller state manifest`,
    );
  }
  return bootstrapMarketDefinitions(
    fixture,
    new PublicKey(saved.baseMint),
    new PublicKey(saved.quoteMint),
  );
}

async function requireConfiguredForkFundingAssets(
  selected: ForkFundingAssetPair,
): Promise<void> {
  const definition = prebootstrappedMarketDefinitions()[0];
  if (!definition) {
    throw new Error("Public fork funding requires a configured market fixture");
  }
  const [baseTokenProgram, quoteTokenProgram] = await Promise.all([
    tokenProgramForMint(definition.baseMint),
    tokenProgramForMint(definition.quoteMint),
  ]);
  const configured: ForkFundingAssetPair = {
    baseMint: definition.baseMint.toBase58(),
    quoteMint: definition.quoteMint.toBase58(),
    baseTokenProgram: baseTokenProgram.toBase58(),
    quoteTokenProgram: quoteTokenProgram.toBase58(),
  };
  if (!forkFundingAssetPairMatches(selected, configured)) {
    throw new Error(
      "Public fork funding is restricted to the configured fixture asset pair",
    );
  }
}

async function verifyPrebootstrappedMarket(
  definition: BootstrapMarketDefinition,
): Promise<StoredMarket> {
  const { connection, program } = initializeRuntime();
  const addresses = deriveMarketAddresses(
    definition.baseMint,
    definition.quoteMint,
    definition.paramsHash,
  );
  const account = await program.account.market.fetchNullable(addresses.market);
  if (!account) {
    throw new Error(
      `Market ${addresses.market.toBase58()} is not bootstrapped; the Surfpool RPC controller must initialize it before API replicas start`,
    );
  }

  const baseSide = field<any>(account, "baseSide", "base_side");
  const quoteSide = field<any>(account, "quoteSide", "quote_side");
  const insurance = field<any>(account, "insurance");
  requireMatchingPublicKey(
    field<PublicKey>(baseSide, "assetMint", "asset_mint"),
    definition.baseMint,
    `${definition.label} base mint`,
  );
  requireMatchingPublicKey(
    field<PublicKey>(quoteSide, "assetMint", "asset_mint"),
    definition.quoteMint,
    `${definition.label} quote mint`,
  );
  const onChainParamsHash = Buffer.from(
    field<number[] | Uint8Array>(account, "paramsHash", "params_hash") ?? [],
  );
  if (!onChainParamsHash.equals(definition.paramsHash)) {
    throw new Error(`${definition.label} parameter hash does not match config`);
  }
  if (marketKindFromConfig(field(account, "config")) !== definition.kind) {
    throw new Error(`${definition.label} AMM kind does not match config`);
  }

  const expectedVaults: Array<[PublicKey | undefined, PublicKey, string]> = [
    [
      field<PublicKey>(baseSide, "reserveVault", "reserve_vault"),
      addresses.baseReserveVault,
      "base reserve vault",
    ],
    [
      field<PublicKey>(quoteSide, "reserveVault", "reserve_vault"),
      addresses.quoteReserveVault,
      "quote reserve vault",
    ],
    [
      field<PublicKey>(baseSide, "collateralVault", "collateral_vault"),
      addresses.baseCollateralVault,
      "base collateral vault",
    ],
    [
      field<PublicKey>(quoteSide, "collateralVault", "collateral_vault"),
      addresses.quoteCollateralVault,
      "quote collateral vault",
    ],
    [
      field<PublicKey>(baseSide, "interestVault", "interest_vault"),
      addresses.baseInterestVault,
      "base interest vault",
    ],
    [
      field<PublicKey>(quoteSide, "interestVault", "interest_vault"),
      addresses.quoteInterestVault,
      "quote interest vault",
    ],
    [
      field<PublicKey>(insurance, "baseVault", "base_vault"),
      addresses.baseInsuranceVault,
      "base insurance vault",
    ],
    [
      field<PublicKey>(insurance, "quoteVault", "quote_vault"),
      addresses.quoteInsuranceVault,
      "quote insurance vault",
    ],
  ];
  for (const [actual, expected, label] of expectedVaults) {
    requireMatchingPublicKey(actual, expected, `${definition.label} ${label}`);
  }

  const ylpMint = field<PublicKey>(account, "ylpMint", "ylp_mint");
  const baseHlpMint = field<PublicKey>(baseSide, "hlpMint", "hlp_mint");
  const quoteHlpMint = field<PublicKey>(quoteSide, "hlpMint", "hlp_mint");
  if (!ylpMint || !baseHlpMint || !quoteHlpMint) {
    throw new Error(`${definition.label} is missing one or more LP mints`);
  }

  const requiredAccounts = [
    pda(seed("futarchy_authority")),
    deriveTransferHookValidationAddress(ylpMint),
    deriveTransferHookValidationAddress(baseHlpMint),
    deriveTransferHookValidationAddress(quoteHlpMint),
    tokenMetadataPda(ylpMint),
    tokenMetadataPda(baseHlpMint),
    tokenMetadataPda(quoteHlpMint),
  ];
  const requiredObservations = await connection.getMultipleAccountsInfo(
    requiredAccounts,
    DEPLOYMENT_COMMITMENT,
  );
  for (let index = 0; index < requiredAccounts.length; index += 1) {
    const observed = requiredObservations[index];
    if (!observed) {
      throw new Error(
        `${definition.label} prebootstrap account ${requiredAccounts[index].toBase58()} is missing`,
      );
    }
    const expectedOwner =
      index < 4 ? PROGRAM_ID : TOKEN_METADATA_PROGRAM_ID;
    if (!observed.owner.equals(expectedOwner)) {
      throw new Error(
        `${definition.label} prebootstrap account ${requiredAccounts[index].toBase58()} has wrong owner`,
      );
    }
  }
  if (!(await hasSeededLiquidityMarker(addresses.market))) {
    throw new Error(
      `${definition.label} has no seeded-liquidity marker; the RPC controller must finish bootstrap`,
    );
  }
  const baseLiveReserve = toBigInt(
    field(field(baseSide, "reserves"), "liveReserve", "live_reserve"),
  );
  const quoteLiveReserve = toBigInt(
    field(field(quoteSide, "reserves"), "liveReserve", "live_reserve"),
  );
  if (baseLiveReserve <= 0n || quoteLiveReserve <= 0n) {
    throw new Error(`${definition.label} initial liquidity is incomplete`);
  }

  const [baseTokenProgram, quoteTokenProgram] = await Promise.all([
    tokenProgramForMint(definition.baseMint),
    tokenProgramForMint(definition.quoteMint),
  ]);
  return storedMarketDefinition({
    label: definition.label,
    marketKind: definition.kind,
    addresses,
    paramsHash: definition.paramsHash,
    baseMint: definition.baseMint,
    quoteMint: definition.quoteMint,
    baseDecimals: Number(
      field(baseSide, "assetDecimals", "asset_decimals") ?? 0,
    ),
    quoteDecimals: Number(
      field(quoteSide, "assetDecimals", "asset_decimals") ?? 0,
    ),
    baseTokenProgram,
    quoteTokenProgram,
    ylpMint,
    baseHlpMint,
    quoteHlpMint,
    seededLiquidity: true,
  });
}

async function verifyPrebootstrappedMarkets(): Promise<StoredMarket[]> {
  await verifyControllerSignerMarker();
  const definitions = prebootstrappedMarketDefinitions();
  const markets: StoredMarket[] = [];
  for (const definition of definitions) {
    markets.push(await verifyPrebootstrappedMarket(definition));
  }
  return markets;
}

async function bootstrapMarketUncached(
  definition: BootstrapMarketDefinition,
  state: ForkState,
): Promise<StoredMarket> {
  const { payer, program } = initializeRuntime();
  const {
    label: marketLabel,
    kind: marketKind,
    baseMint,
    quoteMint,
    paramsHash,
    config,
  } = definition;
  const addresses = deriveMarketAddresses(baseMint, quoteMint, paramsHash);

  const existingMarketAccount = await program.account.market.fetchNullable(addresses.market);
  if (
    !existingMarketAccount &&
    process.env.DUSK_REQUIRE_PREBOOTSTRAPPED_MARKETS === "true"
  ) {
    throw new Error(
      `Market ${addresses.market.toBase58()} is not bootstrapped; the Surfpool RPC controller must initialize it before API replicas start`,
    );
  }

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
  let ylpMint = field<PublicKey>(existingMarketAccount, "ylpMint", "ylp_mint");
  const existingBaseSide = field<any>(existingMarketAccount, "baseSide", "base_side");
  const existingQuoteSide = field<any>(existingMarketAccount, "quoteSide", "quote_side");
  let baseHlpMint = field<PublicKey>(existingBaseSide, "hlpMint", "hlp_mint");
  let quoteHlpMint = field<PublicKey>(existingQuoteSide, "hlpMint", "hlp_mint");

  if (!existingMarketAccount) {
    // These are controller-signed writes. Keep them sequential so Surfnet's
    // transaction-mode bank cannot invalidate a sibling's recent blockhash.
    const ylp = await createHookedLpMintIfMissing({
      label: lpLabels.ylp,
      decimals: baseDecimals,
      mintAuthority: addresses.market,
    });
    const baseHlp = await createHookedLpMintIfMissing({
      label: lpLabels.baseHlp,
      decimals: baseDecimals,
      mintAuthority: addresses.market,
    });
    const quoteHlp = await createHookedLpMintIfMissing({
      label: lpLabels.quoteHlp,
      decimals: quoteDecimals,
      mintAuthority: addresses.market,
    });
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
        config,
        paramsHash: Array.from(paramsHash),
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

  // Hook initialization also submits controller-owned transactions and must
  // obey the same one-write-at-a-time blockhash discipline.
  const ylpHookValidation = await ensureLpTransferHook({
    market: addresses.market,
    lpMint: ylpMint,
  });
  const baseHlpHookValidation = await ensureLpTransferHook({
    market: addresses.market,
    lpMint: baseHlpMint,
  });
  const quoteHlpHookValidation = await ensureLpTransferHook({
    market: addresses.market,
    lpMint: quoteHlpMint,
  });
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

  let seededLiquidity = await hasSeededLiquidityMarker(addresses.market);
  if (!seededLiquidity && existingMarketAccount) {
    const baseLiveReserve = toBigInt(
      field(field(existingBaseSide, "reserves"), "liveReserve", "live_reserve"),
    );
    const quoteLiveReserve = toBigInt(
      field(
        field(existingQuoteSide, "reserves"),
        "liveReserve",
        "live_reserve",
      ),
    );
    if (baseLiveReserve > 0n || quoteLiveReserve > 0n) {
      // Adopt forks created before the durable marker was introduced without depositing twice.
      await recordSeededLiquidity(addresses.market);
      seededLiquidity = true;
    } else if (process.env.DUSK_REQUIRE_PREBOOTSTRAPPED_MARKETS === "true") {
      throw new Error(
        `Market ${addresses.market.toBase58()} has no seeded-liquidity marker; the Surfpool RPC controller must finish bootstrap`,
      );
    }
  }
  const stored: StoredMarket = {
    label: marketLabel,
    marketKind,
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
    seededLiquidity,
    transferHookValidationAccounts,
  };

  state.markets[marketLabel] = stored;
  writeState(state);

  if (duskEnv("SEED_LIQUIDITY") !== "0" && !stored.seededLiquidity) {
    await seedInitialLiquidity(stored);
    await recordSeededLiquidity(addresses.market);
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

  const [baseFunding, quoteFunding] = await Promise.all([
    prepareTokenFunding(payer.publicKey, baseMint, baseAmount, baseProgram),
    prepareTokenFunding(payer.publicKey, quoteMint, quoteAmount, quoteProgram),
  ]);
  await setTokenBalance(baseFunding);
  await setTokenBalance(quoteFunding);
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
    qEmaHalfLifeMs: stringValue(field(config, "qEmaHalfLifeMs", "q_ema_half_life_ms")),
    maxDailyBorrowBps: Number(field(config, "maxDailyBorrowBps", "max_daily_borrow_bps") ?? 0),
    globalHealthContributionCapBps: Number(
      field(config, "globalHealthContributionCapBps", "global_health_contribution_cap_bps") ?? 0
    ),
    borrowMarketHealthFloorBps: Number(
      field(config, "borrowMarketHealthFloorBps", "borrow_market_health_floor_bps") ?? 0
    ),
    amm: {
      rangeWidthNad: stringValue(field(amm, "rangeWidthNad", "range_width_nad")),
      concentratedLiquidityShareNad: stringValue(
        field(amm, "concentratedLiquidityShareNad", "concentrated_liquidity_share_nad")
      ),
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
  const transaction = new Transaction().add(
    ComputeBudgetProgram.requestHeapFrame({ bytes: 256 * 1024 }),
    ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
    instruction,
  );
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
    "marketPreview",
    Buffer.from(returnData.data[0], returnData.data[1]),
  );
  return {
    health: field<any>(preview, "health"),
    sourceSlot: simulation.context.slot,
  };
}

async function marketPayload(stored: StoredMarket) {
  const { connection, program } = initializeRuntime();
  const marketAddress = new PublicKey(stored.market);
  const healthObservation = await currentMarketHealth(marketAddress);
  const marketObservation = await program.account.market.fetchAndContext(
    marketAddress,
    DEPLOYMENT_COMMITMENT,
  );
  const marketAccount = marketObservation.data;
  const health = healthObservation.health;
  const sourceSlot = marketObservation.context.slot;
  const sourceBlockTime = await connection.getBlockTime(sourceSlot);
  const healthBlockTime = await connection.getBlockTime(
    healthObservation.sourceSlot,
  );
  const sourceObservedAt =
    sourceBlockTime === null
      ? null
      : new Date(sourceBlockTime * 1_000).toISOString();
  const healthObservedAt =
    healthBlockTime === null
      ? null
      : new Date(healthBlockTime * 1_000).toISOString();
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
  const insurance = field<any>(marketAccount, "insurance");
  const fixedBaseShares = toBigInt(field(debt, "fixedBaseShares", "fixed_base_shares"));
  const fixedQuoteShares = toBigInt(field(debt, "fixedQuoteShares", "fixed_quote_shares"));
  const baseBorrowIndexNad = toBigInt(field(debt, "baseBorrowIndexNad", "base_borrow_index_nad"));
  const quoteBorrowIndexNad = toBigInt(field(debt, "quoteBorrowIndexNad", "quote_borrow_index_nad"));

  return {
    label: stored.label,
    marketKind: stored.marketKind ?? marketKindFromConfig(config),
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
    createdAt: null,
    updatedAt: null,
    observedAt: sourceObservedAt,
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
      healthSourceSlot: healthObservation.sourceSlot,
      healthObservedAt,
      sourceTxSig: null,
      sourceSlot,
      observedAt: sourceObservedAt,
    },
  };
}

function forkConfigPayload(stored: StoredMarket, markets: StoredMarket[]) {
  return {
    ...publicForkRpcPayload(PUBLIC_RPC_URL),
    programId: PROGRAM_ID.toBase58(),
    payer: initializeRuntime().payer.publicKey.toBase58(),
    market: stored.market,
    primaryMarket: stored.market,
    label: stored.label,
    marketKind: stored.marketKind ?? "cpmm",
    markets: markets.map((market) => ({
      label: market.label,
      market: market.market,
      marketKind: market.marketKind ?? "cpmm",
      baseMint: market.baseMint,
      quoteMint: market.quoteMint,
      baseDecimals: market.baseDecimals,
      quoteDecimals: market.quoteDecimals,
      paramsHash: market.paramsHash,
      seededLiquidity: market.seededLiquidity,
    })),
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

type HlpSwapAccountAddresses = Pick<
  ReturnType<typeof marketFromStored>,
  | "ylpMint"
  | "baseHlpYlpVault"
  | "quoteHlpYlpVault"
  | "baseInterestVault"
  | "quoteInterestVault"
>;

function hlpSwapRemainingAccountPrefix(
  addresses: HlpSwapAccountAddresses,
  marketAccount: unknown,
): AccountMeta[] {
  const baseHlpVault = field(marketAccount, "baseHlpVault", "base_hlp_vault");
  const quoteHlpVault = field(marketAccount, "quoteHlpVault", "quote_hlp_vault");
  const active =
    toBigInt(field(baseHlpVault, "hlpSupply", "hlp_supply")) > 0n ||
    toBigInt(field(quoteHlpVault, "hlpSupply", "hlp_supply")) > 0n ||
    toBigInt(field(baseHlpVault, "residualExposure", "residual_exposure")) !== 0n ||
    toBigInt(field(quoteHlpVault, "residualExposure", "residual_exposure")) !== 0n;
  if (!active) return [];
  return [
    addresses.ylpMint,
    addresses.baseHlpYlpVault,
    addresses.quoteHlpYlpVault,
    addresses.baseInterestVault,
    addresses.quoteInterestVault,
  ].map((pubkey) => ({ pubkey, isWritable: true, isSigner: false }));
}

async function hlpSwapRemainingAccounts(
  addresses: HlpSwapAccountAddresses & { market: PublicKey },
): Promise<AccountMeta[]> {
  const { program } = initializeRuntime();
  const marketAccount = await program.account.market.fetch(addresses.market);
  return hlpSwapRemainingAccountPrefix(addresses, marketAccount);
}

async function resolveStoredMarket(
  marketAddress: string,
  fallback: StoredMarket,
): Promise<StoredMarket> {
  if (!marketAddress || marketAddress === fallback.market) return fallback;
  const state = readState();
  const saved = Object.values(state.markets).find((market) => market.market === marketAddress);

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
    label: saved?.label ?? `market-${market.toBase58().slice(0, 8)}`,
    marketKind: marketKindFromConfig(field(account, "config")),
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

  const remainingAccounts = await hlpSwapRemainingAccounts(m);
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

async function buildClaimYieldTx(params: {
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
      .claimYield({ tokenKind: params.tokenKind === "ylp" ? { ylp: {} } : { hlp: {} } })
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
        rangeWidthNad: toBN(String(concentration.rangeWidthNad)),
        concentratedLiquidityShareNad: toBN(
          String(concentration.concentratedLiquidityShareNad)
        ),
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
        qMs: toBN(String(ema.qMs)),
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

function integerText(value: unknown, label: string): string {
  if (BN.isBN(value)) return value.toString(10);
  if (typeof value === "bigint") return value.toString();
  if (typeof value === "number") {
    if (!Number.isSafeInteger(value)) {
      throw new Error(`${label} must be a safe integer or decimal string`);
    }
    return String(value);
  }
  if (typeof value === "string" && /^-?\d+$/.test(value.trim())) {
    return value.trim();
  }
  throw new Error(`${label} must be an integer`);
}

function boundedIntegerNumber(
  value: unknown,
  label: string,
  minimum: number,
  maximum: number,
): number {
  const parsed = Number(integerText(value, label));
  if (!Number.isSafeInteger(parsed) || parsed < minimum || parsed > maximum) {
    throw new Error(`${label} must be between ${minimum} and ${maximum}`);
  }
  return parsed;
}

function boundedIntegerBn(
  value: unknown,
  label: string,
  minimum: bigint,
  maximum: bigint,
): BN {
  const text = integerText(value, label);
  const parsed = BigInt(text);
  if (parsed < minimum || parsed > maximum) {
    throw new Error(
      `${label} must be between ${minimum.toString()} and ${maximum.toString()}`,
    );
  }
  return new BN(text, 10);
}

function marketConfigFromBody(config: Record<string, unknown>) {
  const amm = (config.amm as Record<string, unknown> | undefined) ?? defaultAmmConfig();
  const defaultIrm = defaultMarketConfig().irm;
  const irm = (config.irm as Record<string, unknown> | undefined) ?? defaultIrm;
  const u16 = (value: unknown, label: string) =>
    boundedIntegerNumber(value, label, 0, 65_535);
  const u64 = (value: unknown, label: string) =>
    boundedIntegerBn(value, label, 0n, (1n << 64n) - 1n);
  const i64 = (value: unknown, label: string) =>
    boundedIntegerBn(value, label, -(1n << 63n), (1n << 63n) - 1n);
  const reserved = Array.isArray(amm.reserved)
    ? amm.reserved.map((value, index) =>
        boundedIntegerNumber(value, `config.amm.reserved[${index}]`, 0, 255),
      )
    : Array(33).fill(0);
  if (reserved.length !== 33) {
    throw new Error("config.amm.reserved must contain exactly 33 bytes");
  }
  return {
    swapFeeBps: u16(config.swapFeeBps, "config.swapFeeBps"),
    divergenceFeeShareCapBps: u16(
      config.divergenceFeeShareCapBps ?? 0,
      "config.divergenceFeeShareCapBps",
    ),
    volatilityFeeShareCapBps: u16(
      config.volatilityFeeShareCapBps ?? 0,
      "config.volatilityFeeShareCapBps",
    ),
    targetHlpLeverageBps: u16(
      config.targetHlpLeverageBps,
      "config.targetHlpLeverageBps",
    ),
    settlementDivergenceBps: u16(
      config.settlementDivergenceBps,
      "config.settlementDivergenceBps",
    ),
    emaHalfLifeMs: u64(config.emaHalfLifeMs, "config.emaHalfLifeMs"),
    directionalEmaHalfLifeMs: u64(
      config.directionalEmaHalfLifeMs,
      "config.directionalEmaHalfLifeMs",
    ),
    qEmaHalfLifeMs: u64(config.qEmaHalfLifeMs, "config.qEmaHalfLifeMs"),
    maxDailyBorrowBps: u16(
      config.maxDailyBorrowBps,
      "config.maxDailyBorrowBps",
    ),
    globalHealthContributionCapBps: u16(
      config.globalHealthContributionCapBps,
      "config.globalHealthContributionCapBps",
    ),
    borrowMarketHealthFloorBps: u16(
      config.borrowMarketHealthFloorBps,
      "config.borrowMarketHealthFloorBps",
    ),
    amm: {
      rangeWidthNad: u64(amm.rangeWidthNad, "config.amm.rangeWidthNad"),
      concentratedLiquidityShareNad: u64(
        amm.concentratedLiquidityShareNad,
        "config.amm.concentratedLiquidityShareNad",
      ),
      centerEmaHalfLifeMs: u64(
        amm.centerEmaHalfLifeMs,
        "config.amm.centerEmaHalfLifeMs",
      ),
      volatilityHalfLifeMs: u64(
        amm.volatilityHalfLifeMs,
        "config.amm.volatilityHalfLifeMs",
      ),
      adjustmentThresholdNad: u64(
        amm.adjustmentThresholdNad,
        "config.amm.adjustmentThresholdNad",
      ),
      adjustmentStepNad: u64(
        amm.adjustmentStepNad,
        "config.amm.adjustmentStepNad",
      ),
      minAdjustmentIntervalSlots: u64(
        amm.minAdjustmentIntervalSlots,
        "config.amm.minAdjustmentIntervalSlots",
      ),
      volatilityShockCapNad: u64(
        amm.volatilityShockCapNad,
        "config.amm.volatilityShockCapNad",
      ),
      volatilityCapNad: u64(
        amm.volatilityCapNad,
        "config.amm.volatilityCapNad",
      ),
      divergenceFeeCoefficientNad: u64(
        amm.divergenceFeeCoefficientNad,
        "config.amm.divergenceFeeCoefficientNad",
      ),
      volatilityFeeCoefficientNad: u64(
        amm.volatilityFeeCoefficientNad,
        "config.amm.volatilityFeeCoefficientNad",
      ),
      reserved,
    },
    irm: {
      targetUtilizationBps: u16(
        irm.targetUtilizationBps ?? defaultIrm.targetUtilizationBps,
        "config.irm.targetUtilizationBps",
      ),
      curveSteepnessNad: u64(
        irm.curveSteepnessNad ?? defaultIrm.curveSteepnessNad,
        "config.irm.curveSteepnessNad",
      ),
      adjustmentSpeedPerYear: u64(
        irm.adjustmentSpeedPerYear ?? defaultIrm.adjustmentSpeedPerYear,
        "config.irm.adjustmentSpeedPerYear",
      ),
    },
    startTime: i64(config.startTime, "config.startTime"),
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
  let builder = program.methods
      .openLeverage({
        positionId: params.positionId,
        debtAsset: debtIsBase ? 0 : 1,
        marginAmount: toBN(params.marginAmount),
        multiplierBps: toBN(params.multiplierBps),
        minCollateralOut: toBN(params.minCollateralOut),
        referrer: params.referrer,
      })
      .accounts({
        market: m.market,
        futarchyAuthority: m.futarchyAuthority,
        owner: params.owner,
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
      });
  const hLpAccounts = await hlpSwapRemainingAccounts(m);
  if (hLpAccounts.length > 0) builder = builder.remainingAccounts(hLpAccounts);
  instructions.push(await builder.instruction());
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
  let builder = program.methods
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
      });
  const hLpAccounts = await hlpSwapRemainingAccounts(m);
  if (hLpAccounts.length > 0) builder = builder.remainingAccounts(hLpAccounts);
  return serializeOwnerTransaction(params.owner, [await builder.instruction()]);
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
  let builder = program.methods
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
      });
  const hLpAccounts = await hlpSwapRemainingAccounts(m);
  if (hLpAccounts.length > 0) builder = builder.remainingAccounts(hLpAccounts);
  return serializeOwnerTransaction(params.owner, [await builder.instruction()]);
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
  let builder = program.methods
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
      });
  const hLpAccounts = await hlpSwapRemainingAccounts(m);
  if (hLpAccounts.length > 0) builder = builder.remainingAccounts(hLpAccounts);
  instructions.push(await builder.instruction());
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
  const { program, provider } = initializeRuntime();
  const delegateProgram = getLeverageDelegateProgram();
  const m = marketFromStored(params.market);
  const debtIsBase = params.debtAsset === "base";
  const debtMint = debtIsBase ? m.baseMint : m.quoteMint;
  const collateralMint = debtIsBase ? m.quoteMint : m.baseMint;
  const debtTokenProgram = debtIsBase ? m.baseTokenProgram : m.quoteTokenProgram;
  const leveragePosition = deriveLeveragePosition(m.market, params.positionId);
  const order = deriveLeverageOrder(leveragePosition, params.positionOwner, params.orderId);
  requireActionableLeverageOrderAccount(
    await provider.connection.getAccountInfo(order, DEPLOYMENT_COMMITMENT),
  );
  const referral = await leveragePositionReferralAccounts(
    m.market,
    params.positionId,
    debtMint
  );
  const leverageDelegation = deriveLeverageDelegation(leveragePosition);
  const custodyAuthority = deriveLeverageCustodyAuthority(order);
  const instructions: TransactionInstruction[] = [];

  const custodyAccount = await ataInstructionIfMissing({
    payer: params.executor,
    owner: custodyAuthority,
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
      custodyAuthority,
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
      custodyAuthority,
      custodyTokenAccount: custodyAccount.address,
      executorTokenAccount: executorAccount.address,
      ownerTokenAccount: ownerAccount.address,
      tokenMint: debtMint,
      executor: params.executor,
      tokenProgram: TOKEN_PROGRAM_ID,
      token2022Program: TOKEN_2022_PROGRAM_ID,
    })
    .instruction();
  const hLpAccounts = await hlpSwapRemainingAccounts(m);

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
      // The program strips the hLP settlement prefix before splitting the
      // delegated callback accounts at `beforeAccountsLen`.
      .remainingAccounts([
        ...hLpAccounts,
        ...beforeInstruction.keys,
        ...afterInstruction.keys,
      ])
      .instruction()
  );

  return {
    transaction: await serializeOwnerTransaction(params.executor, instructions),
    leveragePosition,
    leverageDelegation,
    order,
    custodyAuthority,
    custodyTokenAccount: custodyAccount.address,
    executorTokenAccount: executorAccount.address,
    ownerTokenAccount: ownerAccount.address,
  };
}

async function buildLiquidateLeverageTx(params: {
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
  let builder = program.methods
      .liquidateLeverage({ debtAsset: debtIsBase ? 0 : 1 })
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
      });
  const hLpAccounts = await hlpSwapRemainingAccounts(m);
  if (hLpAccounts.length > 0) builder = builder.remainingAccounts(hLpAccounts);
  instructions.push(await builder.instruction());
  return {
    transaction: await serializeOwnerTransaction(params.liquidator, instructions),
    leveragePosition,
    liquidatorDebtAccount,
    ownerDebtAccount: ownerDebtAccountResult.address,
  };
}

async function buildTriggerLiquidationAuctionTx(params: {
  payer: PublicKey;
  market: StoredMarket;
  positionId: PublicKey;
  debtAsset: MarketAsset;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const instruction = await program.methods
    .triggerLiquidationAuction()
    .accounts({
      market: m.market,
      borrowPosition: deriveBorrowPosition(m.market, params.positionId),
      debtAssetMint: params.debtAsset === "base" ? m.baseMint : m.quoteMint,
    })
    .instruction();
  return serializeOwnerTransaction(params.payer, [instruction]);
}

async function buildBidLiquidationAuctionTx(params: {
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
      .bidLiquidationAuction({
        repayAmount: toBN(params.repayAmount),
        minCollateralOut: toBN(params.minCollateralOut),
      })
      .accounts({
        market: m.market,
        futarchyAuthority: m.futarchyAuthority,
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
  return serializeOwnerTransaction(params.liquidator, instructions);
}

async function buildSettleLiquidationAuctionFloorTx(params: {
  liquidator: PublicKey;
  market: StoredMarket;
  positionId: PublicKey;
  debtAsset: MarketAsset;
  repayAmount: bigint;
  minCollateralOut: bigint;
  maxInsuranceDraw: bigint;
  maxSocializedLoss: bigint;
}) {
  const { program } = initializeRuntime();
  const m = marketFromStored(params.market);
  const debtIsBase = params.debtAsset === "base";
  const debtMint = debtIsBase ? m.baseMint : m.quoteMint;
  const collateralMint = debtIsBase ? m.quoteMint : m.baseMint;
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
      .settleLiquidationAuctionFloor({
        repayAmount: toBN(params.repayAmount),
        minCollateralOut: toBN(params.minCollateralOut),
        maxInsuranceDraw: toBN(params.maxInsuranceDraw),
        maxSocializedLoss: toBN(params.maxSocializedLoss),
      })
      .accounts({
        market: m.market,
        futarchyAuthority: m.futarchyAuthority,
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
  const projectionId = (eventType: string, address: PublicKey) =>
    createHash("sha256")
      .update(`${stored.market}:${eventType}:${address.toBase58()}`)
      .digest()
      .readUIntBE(0, 6);

  const borrowPositionAddress = deriveBorrowPosition(market, positionId);
  const borrowPosition = await program.account.borrowPosition.fetchNullable(borrowPositionAddress);
  if (borrowPosition) {
    const auctionDebtAssetCode = Number(field(borrowPosition, "auctionDebtAsset", "auction_debt_asset"));
    positions.push({
      id: projectionId("borrow_position", borrowPositionAddress),
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
      id: projectionId("leverage_position", leveragePositionAddress),
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
        id: projectionId("leverage_delegation", leverageDelegationAddress),
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

let walletFundingQueue: Promise<void> = Promise.resolve();

function serializeWalletFunding<T>(operation: () => Promise<T>): Promise<T> {
  const pending = walletFundingQueue.then(operation);
  walletFundingQueue = pending.then(
    () => undefined,
    () => undefined,
  );
  return pending;
}

async function fundWallet(body: Record<string, unknown>, stored: StoredMarket) {
  if (!ALLOW_PUBLIC_FUNDING) throw new Error("Public fork wallet funding is disabled");
  // This is a signer-integrity boundary, not a market launch gate. Funding an
  // arbitrary Token-2022 mint would let its transfer hook select the shared
  // controller payer as a writable signer in the server-signed top-up. Both
  // configured curves remain supported because they share this trusted asset
  // pair.
  await requireConfiguredForkFundingAssets(stored);
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
  lamportsForSolFunding(sol);
  if (baseAmount > maxBaseAmount || quoteAmount > maxQuoteAmount) {
    throw new Error(`Fork token funding is capped at ${MAX_TOKEN_FUNDING_UI} UI units`);
  }
  return serializeWalletFunding(async () => {
    const { connection } = initializeRuntime();
    const existingAccount = await connection.getAccountInfo(
      owner,
      DEPLOYMENT_COMMITMENT,
    );
    requireFundableForkWallet(owner, existingAccount);

    // Resolve and validate both token-account transitions before the first
    // airdrop or cheatcode. The public faucet is a monotonic top-up: it must
    // never erase a user's existing fork balance.
    const basePlan = await prepareTokenFunding(
      owner,
      new PublicKey(stored.baseMint),
      baseAmount,
      new PublicKey(stored.baseTokenProgram),
    );
    const quotePlan = await prepareTokenFunding(
      owner,
      new PublicKey(stored.quoteMint),
      quoteAmount,
      new PublicKey(stored.quoteTokenProgram),
    );

    try {
      if (shouldMutateWalletLamports(sol)) await setLamports(owner, sol);
      await setTokenBalance(basePlan);
      await setTokenBalance(quotePlan);
    } catch (error) {
      if (error instanceof ForkMutationOutcomeUncertainError) throw error;
      throw new ForkMutationOutcomeUncertainError("Fork wallet funding", error);
    }
    return {
      wallet: owner.toBase58(),
      sol,
      baseAmount: baseAmount.toString(),
      quoteAmount: quoteAmount.toString(),
      baseMint: stored.baseMint,
      quoteMint: stored.quoteMint,
    };
  });
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

async function routeWithoutDeploymentEnvelope(
  req: http.IncomingMessage,
  body: Record<string, unknown>,
  expectedDeployment: Awaited<ReturnType<typeof deploymentEnvelope>>,
) {
  const expectedDeploymentFingerprint =
    deploymentIdentityFingerprint(expectedDeployment);
  const url = new URL(req.url ?? "/", "http://localhost");
  const path = url.pathname.replace(/\/$/, "") || "/";

  if (req.method === "GET" && path === "/health") {
    initializeRuntime();
    const shouldVerifyPublicRpc =
      process.env.DUSK_REQUIRE_PUBLIC_RPC_URL === "true" ||
      HAS_EXPLICIT_PUBLIC_RPC_URL;
    const publicRpcVerification = shouldVerifyPublicRpc
      ? await verifyPublicRpcEndpoint({
          genesisHash: expectedDeployment.genesisHash,
          forkId: expectedDeployment.forkId,
        })
      : null;
    const prebootstrappedMarkets =
      process.env.DUSK_REQUIRE_PREBOOTSTRAPPED_MARKETS === "true"
        ? await bootstrapMarkets(expectedDeploymentFingerprint)
        : [];
    return forkHealthPayload({
      publicRpcUrl: PUBLIC_RPC_URL,
      publicRpcVerified: publicRpcVerification !== null,
      publicRpcFilterVerified:
        publicRpcVerification?.filterVerified === true,
      runtimeInitialized: Boolean(runtime),
      runtimeError,
      prebootstrappedMarketCount: prebootstrappedMarkets.length,
    });
  }

  if (req.method === "GET" && path === "/api/v2/fork/test-catalog") {
    return { success: true, data: { scenarios: SCENARIO_CATALOG } };
  }

  if (req.method === "GET" && path === "/api/v2/fork/test-runs") {
    return { success: true, data: { runs: listProtocolTestRuns() } };
  }

  if (req.method === "GET" && path === "/api/v2/fork/test-runs/latest") {
    const report = readProtocolTestRun(
      resolve(protocolTestRunsDir(), "latest.json"),
    );
    return { success: true, data: { run: report } };
  }

  const protocolTestRunMatch = path.match(
    /^\/api\/v2\/fork\/test-runs\/([a-zA-Z0-9._-]+)$/,
  );
  if (req.method === "GET" && protocolTestRunMatch) {
    return {
      success: true,
      data: {
        run: readProtocolTestRun(protocolTestRunPath(protocolTestRunMatch[1])),
      },
    };
  }

  const bootstrappedMarkets = await bootstrapMarkets(
    expectedDeploymentFingerprint,
  );
  let stored = bootstrappedMarkets[0];
  if (!stored) throw new Error("Dusk fork bootstrap produced no markets");
  const requestedMarket = url.searchParams.get("market");

  if (req.method === "GET" && path === "/api/v2/fork/config") {
    return {
      success: true,
      data: forkConfigPayload(stored, bootstrappedMarkets),
    };
  }

  if (req.method === "GET" && path === "/api/v2/fork/bootstrap-evidence") {
    return {
      success: true,
      data: await bootstrapEvidencePayload(
        bootstrappedMarkets,
        expectedDeploymentFingerprint,
      ),
    };
  }

  if (req.method === "GET" && path === "/api/v2/fork/futarchy") {
    return { success: true, data: await futarchyPayload() };
  }

  if (req.method === "GET" && path === "/api/v2/markets") {
    const requestedLimit = Number(
      url.searchParams.get("limit") ?? bootstrappedMarkets.length,
    );
    const requestedOffset = Number(url.searchParams.get("offset") ?? 0);
    const limit =
      Number.isSafeInteger(requestedLimit) && requestedLimit > 0
        ? Math.min(requestedLimit, 100)
        : bootstrappedMarkets.length;
    const offset =
      Number.isSafeInteger(requestedOffset) && requestedOffset >= 0
        ? requestedOffset
        : 0;
    const page = bootstrappedMarkets.slice(offset, offset + limit);
    return {
      success: true,
      data: {
        markets: await Promise.all(page.map(marketPayload)),
        pagination: { limit, offset, total: bootstrappedMarkets.length },
      },
    };
  }

  const marketDetailMatch = path.match(/^\/api\/v2\/markets\/([^/]+)$/);
  if (req.method === "GET" && marketDetailMatch) {
    const selected = await resolveStoredMarket(marketDetailMatch[1], stored);
    return { success: true, data: await marketPayload(selected) };
  }

  const marketSwapsMatch = path.match(/^\/api\/v2\/markets\/([^/]+)\/swaps$/);
  if (req.method === "GET" && marketSwapsMatch) {
    await resolveStoredMarket(marketSwapsMatch[1], stored);
    return {
      success: true,
      data: { swaps: [], pagination: { limit: 100, offset: 0, total: 0 } },
    };
  }

  const userPositionsMatch = path.match(
    /^\/api\/v2\/users\/([^/]+)\/positions$/,
  );
  if (req.method === "GET" && userPositionsMatch) {
    const wallet = new PublicKey(userPositionsMatch[1]);
    const positionId = optionalPublicKey(url.searchParams.get("positionId"));
    const positionMarkets = requestedMarket
      ? [await resolveStoredMarket(requestedMarket, stored)]
      : bootstrappedMarkets;
    return {
      success: true,
      data: {
        positions: (
          await Promise.all(
            positionMarkets.map((market) =>
              userPositionsPayload(wallet, market, positionId),
            ),
          )
        ).flat(),
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
    if (requestedMarket)
      stored = await resolveStoredMarket(requestedMarket, stored);
    const owner = new PublicKey(String(url.searchParams.get("owner") ?? ""));
    const asset = assetFromBody(url.searchParams.get("asset"), "base");
    const tokenKind = yieldTokenKindFromBody(
      url.searchParams.get("tokenKind"),
      "ylp",
    );
    const market = marketFromStored(stored);
    const lpMint =
      optionalPublicKey(url.searchParams.get("lpMint")) ??
      (tokenKind === "ylp"
        ? market.ylpMint
        : asset === "base"
          ? market.baseHlpMint
          : market.quoteHlpMint);
    return {
      success: true,
      data: {
        yieldAccount: await yieldAccountPayload(
          stored,
          owner,
          lpMint,
          asset,
          tokenKind,
        ),
      },
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
      data: await timeTravel(
        Number(body.seconds ?? 30),
        Number(body.slots ?? 0),
      ),
    };
  }

  const owner = new PublicKey(
    String(body.owner ?? body.wallet ?? body.publicKey ?? ""),
  );

  if (path === "/api/v2/fork/tx/create-market") {
    const config = body.config as Record<string, unknown> | undefined;
    if (!config) throw new Error("config is required");
    if (
      typeof body.baseMint !== "string" ||
      typeof body.quoteMint !== "string"
    ) {
      throw new Error(
        "baseMint and quoteMint are required in canonical market order",
      );
    }
    const prepared = await prepareCreateMarketTx({
      owner,
      label: String(body.label ?? "").trim(),
      baseMint: new PublicKey(body.baseMint),
      quoteMint: new PublicKey(body.quoteMint),
      config,
    });
    return txResponse(
      "create-market",
      owner,
      prepared.stored,
      prepared.transaction,
      {
        label: prepared.stored.label,
        marketKind: prepared.stored.marketKind,
        config: prepared.config,
        baseMint: prepared.stored.baseMint,
        quoteMint: prepared.stored.quoteMint,
        ylpMint: prepared.stored.ylpMint,
        baseHlpMint: prepared.stored.baseHlpMint,
        quoteHlpMint: prepared.stored.quoteHlpMint,
      },
    );
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
    return txResponse(
      "bootstrap-rejection",
      payer.publicKey,
      stored,
      transaction,
      { kind },
    );
  }

  if (path === "/api/v2/fork/tx/set-global-reduce-only") {
    const reduceOnly = Boolean(body.reduceOnly);
    const transaction = await buildSetGlobalReduceOnlyTx({
      authority: owner,
      reduceOnly,
    });
    return txResponse("set-global-reduce-only", owner, stored, transaction, {
      reduceOnly,
    });
  }

  if (path === "/api/v2/fork/tx/set-reduce-only") {
    const reduceOnly = Boolean(body.reduceOnly);
    const transaction = await buildSetMarketReduceOnlyTx({
      authority: owner,
      market: stored,
      reduceOnly,
    });
    return txResponse("set-reduce-only", owner, stored, transaction, {
      reduceOnly,
    });
  }

  if (path === "/api/v2/fork/tx/update-futarchy-authority") {
    const bootstrapSigned = bootstrapSignedFromBody(body.bootstrapSigned);
    const authority = bootstrapSigned
      ? initializeRuntime().payer.publicKey
      : owner;
    const newAuthority = new PublicKey(String(body.newAuthority ?? ""));
    const transaction = await buildUpdateFutarchyAuthorityTx({
      authority,
      newAuthority,
      bootstrapSigned,
    });
    return txResponse(
      "update-futarchy-authority",
      authority,
      stored,
      transaction,
      {
        newAuthority: newAuthority.toBase58(),
        bootstrapSigned,
      },
    );
  }

  if (path === "/api/v2/fork/tx/update-protocol-revenue") {
    const revenueDistribution = body.revenueDistribution as
      | Record<string, unknown>
      | null
      | undefined;
    const protocolAuctionSplit = body.protocolAuctionSplit as
      | Record<string, unknown>
      | null
      | undefined;
    const transaction = await buildUpdateProtocolRevenueTx({
      authority: owner,
      swapBps: body.swapBps == null ? null : Number(body.swapBps),
      interestBps: body.interestBps == null ? null : Number(body.interestBps),
      maxReferralInterestShareBps:
        body.maxReferralInterestShareBps == null
          ? null
          : Number(body.maxReferralInterestShareBps),
      revenueDistribution:
        revenueDistribution == null
          ? null
          : {
              futarchyTreasuryBps: Number(
                revenueDistribution.futarchyTreasuryBps,
              ),
              buybacksVaultBps: Number(revenueDistribution.buybacksVaultBps),
              teamTreasuryBps: Number(revenueDistribution.teamTreasuryBps),
            },
      protocolAuctionSplit:
        protocolAuctionSplit == null
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
    const auctionParamsBody = body.params as
      | Record<string, unknown>
      | null
      | undefined;
    const transaction = await buildUpdateProtocolAuctionConfigTx({
      authority: owner,
      lane,
      acceptedMint: optionalPublicKey(body.acceptedMint),
      auctionParams:
        auctionParamsBody == null
          ? null
          : {
              startMultiplierBps: Number(auctionParamsBody.startMultiplierBps),
              floorMultiplierBps: Number(auctionParamsBody.floorMultiplierBps),
              durationSlots: BigInt(String(auctionParamsBody.durationSlots)),
              maxReferenceAgeSlots: BigInt(
                String(auctionParamsBody.maxReferenceAgeSlots),
              ),
            },
    });
    return txResponse(
      "update-protocol-auction-config",
      owner,
      stored,
      transaction,
      { lane },
    );
  }

  if (path === "/api/v2/fork/tx/update-protocol-auction-recipients") {
    const lane = protocolAuctionLaneFromBody(body.lane, "fee");
    const transaction = await buildUpdateProtocolAuctionRecipientsTx({
      authority: owner,
      lane,
      treasury: optionalPublicKey(body.treasury),
      stakingVault: optionalPublicKey(body.stakingVault),
      treasuryBps: body.treasuryBps == null ? null : Number(body.treasuryBps),
      stakingVaultBps:
        body.stakingVaultBps == null ? null : Number(body.stakingVaultBps),
    });
    return txResponse(
      "update-protocol-auction-recipients",
      owner,
      stored,
      transaction,
      { lane },
    );
  }

  if (path === "/api/v2/fork/tx/update-protocol-auction-route") {
    const lane = protocolAuctionLaneFromBody(body.lane, "fee");
    const soldAsset = assetFromBody(body.soldAsset, "base");
    const transaction = await buildUpdateProtocolAuctionRouteTx({
      authority: owner,
      market: stored,
      lane,
      soldAsset,
      referenceMarket:
        optionalPublicKey(body.referenceMarket) ?? PublicKey.default,
    });
    return txResponse(
      "update-protocol-auction-route",
      owner,
      stored,
      transaction,
      {
        lane,
        soldAsset,
      },
    );
  }

  if (path === "/api/v2/fork/tx/settle-protocol-auction") {
    const lane = protocolAuctionLaneFromBody(body.lane, "fee");
    const source = protocolRevenueSourceFromBody(body.source);
    const soldAsset = assetFromBody(body.soldAsset, "base");
    const authority = await futarchyPayload();
    const auction =
      lane === "fee" ? authority.feeAuction : authority.buybackAuction;
    const acceptedMint = new PublicKey(auction.acceptedMint);
    const { connection } = initializeRuntime();
    const acceptedMintInfo = await connection.getAccountInfo(
      acceptedMint,
      "confirmed",
    );
    if (!acceptedMintInfo)
      throw new Error(
        `Accepted mint ${acceptedMint.toBase58()} does not exist`,
      );
    const acceptedTokenProgram = acceptedMintInfo.owner;
    if (
      !acceptedTokenProgram.equals(TOKEN_PROGRAM_ID) &&
      !acceptedTokenProgram.equals(TOKEN_2022_PROGRAM_ID)
    ) {
      throw new Error(
        `Accepted mint ${acceptedMint.toBase58()} has an unsupported token program`,
      );
    }
    const acceptedMintAccount = await getMint(
      connection,
      acceptedMint,
      "confirmed",
      acceptedTokenProgram,
    );
    const soldDecimals =
      soldAsset === "base" ? stored.baseDecimals : stored.quoteDecimals;
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
      referenceMarket: new PublicKey(
        String(body.referenceMarket ?? stored.market),
      ),
      soldAmount: parseUnits(String(body.soldAmount ?? "0"), soldDecimals),
      maxPaymentAmount: parseUnits(
        String(body.maxPaymentAmount ?? "0"),
        acceptedMintAccount.decimals,
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
    const bootstrapSigned = bootstrapSignedFromBody(body.bootstrapSigned);
    const proposer = bootstrapSigned
      ? initializeRuntime().payer.publicKey
      : owner;
    const nonce = BigInt(String(body.nonce ?? Date.now()));
    const update = body.update as Record<string, unknown> | undefined;
    if (!update) throw new Error("update is required");
    const decimals = await mintDecimals(
      new PublicKey(stored.ylpMint),
      TOKEN_2022_PROGRAM_ID,
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
      },
    );
  }

  if (path === "/api/v2/fork/tx/support-parameter-proposal") {
    const bootstrapSigned = bootstrapSignedFromBody(body.bootstrapSigned);
    const supporter = bootstrapSigned
      ? initializeRuntime().payer.publicKey
      : owner;
    const proposal = new PublicKey(String(body.proposal ?? ""));
    const decimals = await mintDecimals(
      new PublicKey(stored.ylpMint),
      TOKEN_2022_PROGRAM_ID,
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
      },
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
    return txResponse(
      "execute-parameter-proposal",
      owner,
      stored,
      transaction,
      {
        proposal: proposal.toBase58(),
      },
    );
  }

  if (path === "/api/v2/fork/tx/withdraw-parameter-support") {
    const bootstrapSigned = bootstrapSignedFromBody(body.bootstrapSigned);
    const supporter = bootstrapSigned
      ? initializeRuntime().payer.publicKey
      : owner;
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
      },
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
    return txResponse(
      "configure-referral-partner",
      owner,
      stored,
      built.transaction,
      {
        referrer: referrer.toBase58(),
        interestShareBps,
        active,
        referralPartner: built.referralPartner.toBase58(),
      },
    );
  }

  if (path === "/api/v2/fork/tx/set-referral-recipient") {
    const recipient = new PublicKey(String(body.recipient ?? ""));
    const built = await buildSetReferralRecipientTx({
      authority: owner,
      recipient,
    });
    return txResponse(
      "set-referral-recipient",
      owner,
      stored,
      built.transaction,
      {
        recipient: recipient.toBase58(),
        referralPartner: built.referralPartner.toBase58(),
      },
    );
  }

  if (path === "/api/v2/fork/tx/claim-referral-interest") {
    const asset = assetFromBody(body.asset ?? body.claimAsset, "quote");
    const assetMint = new PublicKey(
      asset === "base" ? stored.baseMint : stored.quoteMint,
    );
    const tokenProgram = new PublicKey(
      asset === "base" ? stored.baseTokenProgram : stored.quoteTokenProgram,
    );
    const built = await buildClaimReferralInterestTx({
      authority: owner,
      market: stored,
      assetMint,
      tokenProgram,
    });
    return txResponse(
      "claim-referral-interest",
      owner,
      stored,
      built.transaction,
      {
        asset,
        recipient: built.recipient.toBase58(),
        referralPartner: built.referralPartner.toBase58(),
        referralAccrual: built.referralAccrual.toBase58(),
        recipientTokenAccount: built.recipientTokenAccount.toBase58(),
      },
    );
  }

  if (path === "/api/v2/fork/tx/set-yield-recipient") {
    const asset = assetFromBody(body.asset, "base");
    const tokenKind = yieldTokenKindFromBody(body.tokenKind, "ylp");
    const recipient = new PublicKey(String(body.recipient ?? owner.toBase58()));
    const transaction = await buildSetYieldRecipientTx({
      owner,
      market: stored,
      asset,
      tokenKind,
      recipient,
    });
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
    const transaction = await buildClaimYieldTx({
      owner,
      market: stored,
      asset,
      tokenKind,
      recipient,
    });
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
    const lpMint =
      tokenKind === "ylp"
        ? m.ylpMint
        : asset === "base"
          ? m.baseHlpMint
          : m.quoteHlpMint;
    const mint = await getMint(
      initializeRuntime().connection,
      lpMint,
      "confirmed",
      TOKEN_2022_PROGRAM_ID,
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
    return txResponse(
      "preview-market",
      owner,
      stored,
      await buildPreviewMarketTx(owner, stored),
    );
  }

  if (path === "/api/v2/fork/tx/preview-add-liquidity") {
    const transaction = await buildPreviewAddLiquidityTx({
      owner,
      market: stored,
      baseDepositAmount: rawAmount(
        body,
        ["baseDepositAmount", "baseAmount"],
        stored.baseDecimals,
        "1",
      ),
      quoteDepositAmount: rawAmount(
        body,
        ["quoteDepositAmount", "quoteAmount"],
        stored.quoteDecimals,
        "1",
      ),
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
        "1",
      ),
    });
    return txResponse("preview-swap", owner, stored, transaction, { assetIn });
  }

  if (path === "/api/v2/fork/tx/preview-borrow-capacity") {
    const collateralAsset = assetFromBody(
      body.collateralAsset ?? body.asset,
      "base",
    );
    const debtDecimals =
      collateralAsset === "base" ? stored.quoteDecimals : stored.baseDecimals;
    const projectedBorrowAmount =
      body.projectedBorrowAmount == null || body.projectedBorrowAmount === ""
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
        "1",
      ),
      projectedBorrowAmount,
    });
    return txResponse("preview-borrow-capacity", owner, stored, transaction, {
      collateralAsset,
    });
  }

  if (path === "/api/v2/fork/tx/preview-borrow-position") {
    const positionId = requiredPositionId(body);
    return txResponse(
      "preview-borrow-position",
      owner,
      stored,
      await buildPreviewBorrowPositionTx({ owner, market: stored, positionId }),
      { borrowPositionId: positionId.toBase58() },
    );
  }

  if (path === "/api/v2/fork/tx/add-liquidity") {
    const transaction = (
      await buildAddLiquidityTx({
        owner,
        market: stored,
        baseDepositAmount: rawAmount(
          body,
          ["baseDepositAmount", "baseAmount"],
          stored.baseDecimals,
          "1",
        ),
        quoteDepositAmount: rawAmount(
          body,
          ["quoteDepositAmount", "quoteAmount"],
          stored.quoteDecimals,
          "1",
        ),
        minYlpAmount: rawAmount(
          body,
          ["minYlpAmount", "minBaseYlpAmount"],
          stored.baseDecimals,
          "0",
        ),
      })
    )
      .serialize({ requireAllSignatures: false, verifySignatures: false })
      .toString("base64");
    return txResponse("add-liquidity", owner, stored, transaction);
  }

  if (path === "/api/v2/fork/tx/remove-liquidity") {
    const transaction = await buildRemoveLiquidityTx({
      owner,
      market: stored,
      ylpAmount: rawAmount(
        body,
        ["ylpAmount", "amount"],
        stored.baseDecimals,
        "1",
      ),
      minBaseAmountOut: rawAmount(
        body,
        ["minBaseAmountOut"],
        stored.baseDecimals,
        "0",
      ),
      minQuoteAmountOut: rawAmount(
        body,
        ["minQuoteAmountOut"],
        stored.quoteDecimals,
        "0",
      ),
    });
    return txResponse("remove-liquidity", owner, stored, transaction);
  }

  if (path === "/api/v2/fork/tx/swap") {
    const assetIn = assetFromBody(body.assetIn, "base");
    const decimals =
      assetIn === "base" ? stored.baseDecimals : stored.quoteDecimals;
    const transaction = await buildSwapTx({
      owner,
      market: stored,
      assetIn,
      exactAssetIn: rawAmount(
        body,
        ["exactAssetIn", "amountIn", "amount"],
        decimals,
        "1",
      ),
      minAssetOut: rawAmount(
        body,
        ["minAssetOut", "minAmountOut"],
        assetIn === "base" ? stored.quoteDecimals : stored.baseDecimals,
        "0",
      ),
    });
    return txResponse("swap", owner, stored, transaction, { assetIn });
  }

  if (path === "/api/v2/fork/tx/deposit-collateral") {
    const marketAsset = assetFromBody(body.marketAsset ?? body.asset, "base");
    const positionId =
      optionalPublicKey(body.positionId ?? body.borrowPositionId) ??
      Keypair.generate().publicKey;
    const borrowPosition = deriveBorrowPosition(
      new PublicKey(stored.market),
      positionId,
    );
    const transaction = await buildDepositCollateralTx({
      owner,
      market: stored,
      positionId,
      marketAsset,
      depositAmount: rawAmount(
        body,
        ["depositAmount", "amount"],
        marketAsset === "base" ? stored.baseDecimals : stored.quoteDecimals,
        "1",
      ),
    });
    return txResponse("deposit-collateral", owner, stored, transaction, {
      marketAsset,
      borrowPositionId: positionId.toBase58(),
      borrowPosition: borrowPosition.toBase58(),
    });
  }

  if (path === "/api/v2/fork/tx/withdraw-collateral") {
    const marketAsset = assetFromBody(body.marketAsset ?? body.asset, "base");
    const positionId = requiredPositionId(body);
    const borrowPosition = deriveBorrowPosition(
      new PublicKey(stored.market),
      positionId,
    );
    const transaction = await buildWithdrawCollateralTx({
      owner,
      market: stored,
      positionId,
      marketAsset,
      withdrawAmount: rawAmount(
        body,
        ["withdrawAmount", "amount"],
        marketAsset === "base" ? stored.baseDecimals : stored.quoteDecimals,
        "1",
      ),
      minAssetAmountOut: rawAmount(
        body,
        ["minAssetAmountOut", "minAmountOut"],
        marketAsset === "base" ? stored.baseDecimals : stored.quoteDecimals,
        "0",
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
    const borrowPosition = deriveBorrowPosition(
      new PublicKey(stored.market),
      positionId,
    );
    const decimals =
      borrowAsset === "base" ? stored.baseDecimals : stored.quoteDecimals;
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
    const borrowPosition = deriveBorrowPosition(
      new PublicKey(stored.market),
      positionId,
    );
    const transaction = await buildRepayTx({
      owner,
      market: stored,
      positionId,
      repayAsset,
      repayAmount: rawAmount(
        body,
        ["repayAmount", "amount"],
        repayAsset === "base" ? stored.baseDecimals : stored.quoteDecimals,
        "1",
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
    const positionId =
      optionalPublicKey(body.positionId) ?? Keypair.generate().publicKey;
    const debtDecimals =
      debtAsset === "base" ? stored.baseDecimals : stored.quoteDecimals;
    const collateralDecimals =
      debtAsset === "base" ? stored.quoteDecimals : stored.baseDecimals;
    const built = await buildOpenLeverageTx({
      owner,
      market: stored,
      positionId,
      debtAsset,
      marginAmount: rawAmount(
        body,
        ["marginAmount", "amount"],
        debtDecimals,
        "1",
      ),
      multiplierBps: BigInt(String(body.multiplierBps ?? 20_000)),
      minCollateralOut: rawAmount(
        body,
        ["minCollateralOut", "minAmountOut"],
        collateralDecimals,
        "0",
      ),
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
    const debtDecimals =
      debtAsset === "base" ? stored.baseDecimals : stored.quoteDecimals;
    const collateralDecimals =
      debtAsset === "base" ? stored.quoteDecimals : stored.baseDecimals;
    const transaction = await buildIncreaseLeverageTx({
      owner,
      market: stored,
      positionId,
      debtAsset,
      debtAmount: rawAmount(body, ["debtAmount", "amount"], debtDecimals, "1"),
      minCollateralOut: rawAmount(
        body,
        ["minCollateralOut", "minAmountOut"],
        collateralDecimals,
        "0",
      ),
    });
    return txResponse("increase-leverage", owner, stored, transaction, {
      debtAsset,
      positionId: positionId.toBase58(),
      leveragePosition: deriveLeveragePosition(
        new PublicKey(stored.market),
        positionId,
      ).toBase58(),
    });
  }

  if (path === "/api/v2/fork/tx/decrease-leverage") {
    const debtAsset = assetFromBody(body.debtAsset ?? body.asset, "quote");
    const positionId = requiredPositionId(body);
    const debtDecimals =
      debtAsset === "base" ? stored.baseDecimals : stored.quoteDecimals;
    const collateralDecimals =
      debtAsset === "base" ? stored.quoteDecimals : stored.baseDecimals;
    const transaction = await buildDecreaseLeverageTx({
      owner,
      market: stored,
      positionId,
      debtAsset,
      collateralAmount: rawAmount(
        body,
        ["collateralAmount", "amount"],
        collateralDecimals,
        "1",
      ),
      minRepayOut: rawAmount(
        body,
        ["minRepayOut", "minAmountOut"],
        debtDecimals,
        "0",
      ),
    });
    return txResponse("decrease-leverage", owner, stored, transaction, {
      debtAsset,
      positionId: positionId.toBase58(),
      leveragePosition: deriveLeveragePosition(
        new PublicKey(stored.market),
        positionId,
      ).toBase58(),
    });
  }

  if (path === "/api/v2/fork/tx/add-leverage-margin") {
    const debtAsset = assetFromBody(body.debtAsset ?? body.asset, "quote");
    const positionId = requiredPositionId(body);
    const debtDecimals =
      debtAsset === "base" ? stored.baseDecimals : stored.quoteDecimals;
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
    const debtDecimals =
      debtAsset === "base" ? stored.baseDecimals : stored.quoteDecimals;
    const amount = rawAmount(
      body,
      ["amount", "marginAmount"],
      debtDecimals,
      "1",
    );
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
    const debtDecimals =
      debtAsset === "base" ? stored.baseDecimals : stored.quoteDecimals;
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
    return txResponse(
      "create-leverage-delegation",
      owner,
      stored,
      built.transaction,
      {
        debtAsset,
        positionId: positionId.toBase58(),
        leverageDelegation: built.leverageDelegation.toBase58(),
      },
    );
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
    return txResponse(
      "update-leverage-delegation",
      owner,
      stored,
      built.transaction,
      {
        debtAsset,
        positionId: positionId.toBase58(),
        leverageDelegation: built.leverageDelegation.toBase58(),
      },
    );
  }

  if (path === "/api/v2/fork/tx/close-leverage-delegation") {
    const positionId = requiredPositionId(body);
    const built = await buildCloseLeverageDelegationTx({
      owner,
      market: stored,
      positionId,
    });
    return txResponse(
      "close-leverage-delegation",
      owner,
      stored,
      built.transaction,
      {
        positionId: positionId.toBase58(),
        leverageDelegation: built.leverageDelegation.toBase58(),
      },
    );
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
      triggerCloseoutPriceNad: BigInt(
        String(body.triggerCloseoutPriceNad ?? 1),
      ),
    });
    return txResponse(
      "create-leverage-order",
      owner,
      stored,
      built.transaction,
      {
        positionId: positionId.toBase58(),
        orderId: orderId.toString(),
        order: built.order.toBase58(),
      },
    );
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
      triggerCloseoutPriceNad: BigInt(
        String(body.triggerCloseoutPriceNad ?? 1),
      ),
    });
    return txResponse(
      "update-leverage-order",
      owner,
      stored,
      built.transaction,
      {
        positionId: positionId.toBase58(),
        orderId: orderId.toString(),
        order: built.order.toBase58(),
      },
    );
  }

  if (path === "/api/v2/fork/tx/delegated-close-leverage") {
    const positionId = requiredPositionId(body);
    const positionOwner = new PublicKey(String(body.positionOwner ?? ""));
    const debtAsset = assetFromBody(body.debtAsset ?? body.asset, "quote");
    const debtDecimals =
      debtAsset === "base" ? stored.baseDecimals : stored.quoteDecimals;
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
    return txResponse(
      "delegated-close-leverage",
      owner,
      stored,
      built.transaction,
      {
        positionOwner: positionOwner.toBase58(),
        positionId: positionId.toBase58(),
        debtAsset,
        orderId: orderId.toString(),
        leveragePosition: built.leveragePosition.toBase58(),
        leverageDelegation: built.leverageDelegation.toBase58(),
        order: built.order.toBase58(),
        custodyAuthority: built.custodyAuthority.toBase58(),
        custodyTokenAccount: built.custodyTokenAccount.toBase58(),
        executorTokenAccount: built.executorTokenAccount.toBase58(),
        ownerTokenAccount: built.ownerTokenAccount.toBase58(),
      },
    );
  }

  if (path === "/api/v2/fork/tx/liquidate-leverage") {
    const positionId = requiredPositionId(body);
    const positionOwner = new PublicKey(String(body.positionOwner ?? ""));
    const debtAsset = assetFromBody(body.debtAsset ?? body.asset, "quote");
    const built = await buildLiquidateLeverageTx({
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
    const transaction = await buildTriggerLiquidationAuctionTx({
      payer: owner,
      market: stored,
      positionId,
      debtAsset,
    });
    return txResponse(
      "trigger-liquidation-auction",
      owner,
      stored,
      transaction,
      {
        positionId: positionId.toBase58(),
        debtAsset,
        borrowPosition: deriveBorrowPosition(
          new PublicKey(stored.market),
          positionId,
        ).toBase58(),
      },
    );
  }

  if (path === "/api/v2/fork/tx/bid-liquidation-auction") {
    const positionId = requiredPositionId(body);
    const debtAsset = assetFromBody(body.debtAsset ?? body.asset, "quote");
    const debtDecimals =
      debtAsset === "base" ? stored.baseDecimals : stored.quoteDecimals;
    const collateralDecimals =
      debtAsset === "base" ? stored.quoteDecimals : stored.baseDecimals;
    const transaction = await buildBidLiquidationAuctionTx({
      liquidator: owner,
      market: stored,
      positionId,
      debtAsset,
      repayAmount: rawAmount(
        body,
        ["repayAmount", "amount"],
        debtDecimals,
        "1",
      ),
      minCollateralOut: rawAmount(
        body,
        ["minCollateralOut", "minAmountOut"],
        collateralDecimals,
        "0",
      ),
    });
    return txResponse("bid-liquidation-auction", owner, stored, transaction, {
      positionId: positionId.toBase58(),
      debtAsset,
    });
  }

  if (path === "/api/v2/fork/tx/settle-liquidation-auction-floor") {
    const positionId = requiredPositionId(body);
    const debtAsset = assetFromBody(body.debtAsset ?? body.asset, "quote");
    const debtDecimals =
      debtAsset === "base" ? stored.baseDecimals : stored.quoteDecimals;
    const collateralDecimals =
      debtAsset === "base" ? stored.quoteDecimals : stored.baseDecimals;
    const transaction = await buildSettleLiquidationAuctionFloorTx({
      liquidator: owner,
      market: stored,
      positionId,
      debtAsset,
      repayAmount: rawAmount(
        body,
        ["repayAmount", "amount"],
        debtDecimals,
        "1",
      ),
      minCollateralOut: rawAmount(
        body,
        ["minCollateralOut", "minAmountOut"],
        collateralDecimals,
        "0",
      ),
      maxInsuranceDraw: rawAmount(
        body,
        ["maxInsuranceDraw"],
        debtDecimals,
        "0",
      ),
      maxSocializedLoss: rawAmount(
        body,
        ["maxSocializedLoss"],
        debtDecimals,
        "0",
      ),
    });
    return txResponse(
      "settle-liquidation-auction-floor",
      owner,
      stored,
      transaction,
      {
        positionId: positionId.toBase58(),
        debtAsset,
      },
    );
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
        "1",
      ),
      minHlpAmount: rawAmount(
        body,
        ["minHlpAmount"],
        targetAsset === "base" ? stored.baseDecimals : stored.quoteDecimals,
        "0",
      ),
    });
    return txResponse("deposit-single-sided", owner, stored, transaction, {
      targetAsset,
    });
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
        "1",
      ),
      minTargetAmountOut: rawAmount(
        body,
        ["minTargetAmountOut", "minAmountOut"],
        targetAsset === "base" ? stored.baseDecimals : stored.quoteDecimals,
        "0",
      ),
    });
    return txResponse("withdraw-single-sided", owner, stored, transaction, {
      targetAsset,
    });
  }

  throw new Error(`Unsupported route: ${req.method} ${path}`);
}

/** Every successful read and transaction-build response is reset-race detectable. */
export async function route(
  req: http.IncomingMessage,
  body: Record<string, unknown>,
) {
  requireForkAdminAuthorization(req);
  requireForkServerSigningAuthorization(req, body);
  const before = await deploymentEnvelope();
  const beforeFingerprint = deploymentIdentityFingerprint(before);
  const value = await routeWithoutDeploymentEnvelope(
    req,
    body,
    before,
  );
  let after: Awaited<ReturnType<typeof deploymentEnvelope>>;
  try {
    after = await deploymentEnvelope(maximumResponseSourceSlot(value));
  } catch (error) {
    if (error instanceof DeploymentIdentityChangedError) throw error;
    const detail = error instanceof Error ? error.message : String(error);
    throw new DeploymentIdentityChangedError(
      `Unable to re-observe Surfpool deployment identity after handling the request: ${detail}`,
    );
  }
  if (beforeFingerprint !== deploymentIdentityFingerprint(after)) {
    throw new DeploymentIdentityChangedError(
      "Surfpool deployment identity changed while handling the API request",
    );
  }
  return { ...(value as Record<string, unknown>), deployment: after };
}

export async function localE2E() {
  const markets = await bootstrapMarkets();
  const stored = markets[0];
  if (!stored) throw new Error("Dusk fork bootstrap produced no markets");
  const { payer, provider } = initializeRuntime();
  await fundWallet({ wallet: payer.publicKey.toBase58(), sol: DEFAULT_SOL_FUNDING }, stored);
  const results = [];
  for (const market of markets) {
    const addLiquidityTx = await buildAddLiquidityTx({
      owner: payer.publicKey,
      market,
      baseDepositAmount: parseUnits("1", market.baseDecimals),
      quoteDepositAmount: parseUnits("1", market.quoteDecimals),
      minYlpAmount: 0n,
      payerCanSign: true,
    });
    addLiquidityTx.sign(payer);
    const addLiquiditySig = await provider.connection.sendRawTransaction(addLiquidityTx.serialize());
    await provider.connection.confirmTransaction(addLiquiditySig, "confirmed");

    const swapBase64 = await buildSwapTx({
      owner: payer.publicKey,
      market,
      assetIn: "base",
      exactAssetIn: parseUnits(market.baseDecimals === 0 ? "1" : "0.1", market.baseDecimals),
      minAssetOut: 0n,
    });
    const swapTx = Transaction.from(Buffer.from(swapBase64, "base64"));
    swapTx.sign(payer);
    const swapSig = await provider.connection.sendRawTransaction(swapTx.serialize());
    await provider.connection.confirmTransaction(swapSig, "confirmed");
    results.push({
      market: market.market,
      marketKind: market.marketKind ?? "cpmm",
      addLiquiditySig,
      swapSig,
    });
  }

  return {
    ok: true,
    market: stored.market,
    markets: results,
    addLiquiditySig: results[0]?.addLiquiditySig,
    swapSig: results[0]?.swapSig,
    config: forkConfigPayload(stored, markets),
  };
}

export function shutdownForkRuntime() {
  const connection = runtime?.connection as any;
  if (connection?._rpcWebSocketIdleTimeout) {
    clearTimeout(connection._rpcWebSocketIdleTimeout);
    connection._rpcWebSocketIdleTimeout = null;
  }
  if (connection?._rpcWebSocketHeartbeat) {
    clearInterval(connection._rpcWebSocketHeartbeat);
    connection._rpcWebSocketHeartbeat = null;
  }
  connection && (connection._subscriptionsByHash = {});
  connection && (connection._subscriptionCallbacksByServerSubscriptionId = {});
  const socket = connection?._rpcWebSocket;
  socket?.removeAllListeners?.();
  try {
    socket?.close?.();
  } catch {
    // The RPC socket may already have completed its explicit idle close.
  }
  runtime = undefined;
  leverageDelegateProgram = undefined;
  bootstrapPromises.clear();
  bootstrapEvidencePromises.clear();
  bootstrapQueue = Promise.resolve();
  observedRuntimeForkId = undefined;
  lifecycleGenesisHash.reset();
}
