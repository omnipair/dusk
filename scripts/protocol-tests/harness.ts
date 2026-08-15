import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { BorshCoder, EventParser, utils, type Idl } from "@coral-xyz/anchor";
import { getAccount, getAssociatedTokenAddressSync, TOKEN_2022_PROGRAM_ID } from "@solana/spl-token";
import {
  Connection,
  Keypair,
  PublicKey,
  SYSVAR_CLOCK_PUBKEY,
  Transaction,
  type FetchFn,
} from "@solana/web3.js";

import { SCENARIO_CATALOG } from "./catalog.js";
import type {
  AssertionEvidence,
  InstructionCoverage,
  ProtocolTestRun,
  RunSummary,
  ScenarioCatalogEntry,
  ScenarioEvidence,
  ScenarioResult,
  SimulationEvidence,
  TransactionEvidence,
} from "./types.js";

const DEFAULT_API_URL = "http://127.0.0.1:8080";
const DEFAULT_OUTPUT_DIR = ".protocol-test-lab/runs";
const DEFAULT_HTTP_TIMEOUT_MS = 150_000;
const DEFAULT_SCENARIO_TIMEOUT_MS = 900_000;
const DEVELOPMENT_EMERGENCY_AUTHORITY_DOMAIN =
  "omnipair-dusk-development-reduce-only-emergency-authority-v1";

export class ScenarioTimeoutError extends Error {}
export class MutationOutcomeUncertainError extends Error {}

export class ForkApiResponseError extends Error {
  readonly status: number;
  readonly code: string | null;
  readonly payload: unknown;

  constructor(
    method: string,
    path: string,
    status: number,
    payload: unknown,
  ) {
    const code = typeof payload === "object" && payload !== null &&
        typeof (payload as { code?: unknown }).code === "string" &&
        (payload as { code: string }).code
      ? (payload as { code: string }).code
      : null;
    super(
      `${method} ${path} failed with HTTP ${status}${code ? ` (${code})` : ""}: ${stableJson(payload)}`,
    );
    this.name = "ForkApiResponseError";
    this.status = status;
    this.code = code;
    this.payload = payload;
  }
}

function positiveIntegerEnv(
  name: string,
  fallback: number,
  maximum: number,
): number {
  const value = process.env[name];
  if (value === undefined || value === "") return fallback;
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed <= 0) {
    throw new Error(`${name} must be a positive safe integer`);
  }
  if (parsed > maximum) {
    throw new Error(`${name} must be no greater than ${maximum}`);
  }
  return parsed;
}

async function boundedFetch(
  input: Parameters<typeof globalThis.fetch>[0],
  init: RequestInit | undefined,
  timeoutMs: number,
  timeoutMessage: string,
  externalSignal?: AbortSignal,
): Promise<Response> {
  const controller = new AbortController();
  const signals = [init?.signal, externalSignal]
    .filter((signal): signal is AbortSignal => signal !== undefined && signal !== null);
  const abortFrom = (signal: AbortSignal) => {
    controller.abort(signal.reason ?? new Error("Request was aborted"));
  };
  const listeners = signals.map((signal) => {
    const listener = () => abortFrom(signal);
    if (signal.aborted) abortFrom(signal);
    else signal.addEventListener("abort", listener, { once: true });
    return { signal, listener };
  });
  const timeoutError = new Error(timeoutMessage);
  const timer = setTimeout(() => controller.abort(timeoutError), timeoutMs);
  try {
    const response = await globalThis.fetch(input, {
      ...init,
      signal: controller.signal,
    });
    const body = await response.arrayBuffer();
    const responseHasNoBody =
      response.status === 101 ||
      response.status === 204 ||
      response.status === 205 ||
      response.status === 304;
    return new Response(responseHasNoBody ? null : body, {
      status: response.status,
      statusText: response.statusText,
      headers: response.headers,
    });
  } catch (error) {
    if (controller.signal.aborted && controller.signal.reason instanceof Error) {
      throw controller.signal.reason;
    }
    throw error;
  } finally {
    clearTimeout(timer);
    for (const { signal, listener } of listeners) {
      signal.removeEventListener("abort", listener);
    }
  }
}

export interface ForkConfig {
  rpcUrl: string;
  programId: string;
  payer: string;
  market: string;
  markets: Array<{
    label: string;
    market: string;
    marketKind: "cpmm" | "concentrated";
    baseMint: string;
    quoteMint: string;
    baseDecimals: number;
    quoteDecimals: number;
    paramsHash: string;
    seededLiquidity: boolean;
  }>;
  fixtureMode: "mainnet" | "token2022-fees" | "mixed-decimals";
  baseMint: string;
  quoteMint: string;
  baseDecimals: number;
  quoteDecimals: number;
  baseTokenProgram: string;
  quoteTokenProgram: string;
  ylpMint: string;
  baseHlpMint: string;
  quoteHlpMint: string;
  parameterTimelockSeconds: number;
  parameterExecutionWindowSeconds: number;
  seededLiquidity: boolean;
}

export interface MarketPayload {
  marketAddress: string;
  baseMint: string;
  quoteMint: string;
  baseDecimals: number;
  quoteDecimals: number;
  ylpMint: string;
  baseHlpMint: string;
  quoteHlpMint: string;
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
  config: Record<string, unknown>;
  governanceLockedYlp: string;
  parameterRevisions: string[];
  reduceOnly: boolean;
  state: Record<string, string>;
}

export interface ProbeResult {
  succeeds: boolean;
  errorCode: string | null;
  unitsConsumed: number | null;
  logs: string[];
}

interface ForkTransactionResponse {
  action: string;
  owner: string;
  market: string;
  rpcUrl: string;
  transaction: string;
  [key: string]: unknown;
}

export interface BootstrapTransactionEvidence {
  label: string;
  signature: string;
  instructions: string[];
}

export interface BootstrapEvidence {
  transactions: BootstrapTransactionEvidence[];
  futarchyAuthorityBootstrapMode:
    | "transaction"
    | "surfpool-account-seed"
    | "preexisting";
}

interface IdlInstruction {
  name: string;
  discriminator: number[];
}

interface DuskIdl {
  instructions: IdlInstruction[];
}

export interface ScenarioDefinition {
  id: string;
  fatal?: boolean;
  fixtureModes?: ForkConfig["fixtureMode"][];
  run(harness: ProtocolTestHarness): Promise<void>;
}

export interface ExecuteOptions {
  wallet: string;
  endpoint: string;
  body: Record<string, unknown>;
  label: string;
  expected?: "success" | "failure";
  submit?: boolean;
  apiSigned?: boolean;
}

export interface AssertApiRejectionOptions {
  wallet: string;
  endpoint: string;
  body: Record<string, unknown>;
  label: string;
  expectedStatus: number;
  expectedCode: string;
}

function jsonReplacer(_key: string, value: unknown): unknown {
  return typeof value === "bigint" ? value.toString() : value;
}

function stableJson(value: unknown): string {
  return JSON.stringify(value, jsonReplacer);
}

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function isTransientForkError(error: unknown): boolean {
  if (error instanceof MutationOutcomeUncertainError) return false;
  return /BlockhashNotFound|blockhash not found|Internal error|Failed to fetch accounts from remote|error sending request for url/i.test(
    errorText(error)
  );
}

function parseErrorCode(logs: string[], fallback: unknown): string | null {
  for (const line of logs) {
    const anchorCode = line.match(/Error Code: ([A-Za-z0-9_]+)/)?.[1];
    if (anchorCode) return anchorCode;
    const customCode = line.match(/custom program error: (0x[0-9a-f]+)/i)?.[1];
    if (customCode) return customCode;
  }
  const fallbackText = errorText(fallback);
  return (
    fallbackText.match(/custom program error: (0x[0-9a-f]+)/i)?.[1] ?? null
  );
}

function jsonRpcMethod(init: unknown): string | null {
  if (!init || typeof init !== "object") return null;
  const body = (init as { body?: unknown }).body;
  if (typeof body !== "string") return null;
  try {
    const payload = JSON.parse(body) as { method?: unknown };
    return typeof payload.method === "string" ? payload.method : null;
  } catch {
    return null;
  }
}

function runId(revision: string): string {
  const timestamp = new Date().toISOString().replace(/[:.]/g, "-");
  return `${timestamp}-${revision.slice(0, 8)}`;
}

function emptySummary(): RunSummary {
  return {
    total: 0,
    passed: 0,
    failed: 0,
    skipped: 0,
    notRun: 0,
    transactionsSubmitted: 0,
    expectedFailuresVerified: 0,
    assertions: 0,
    assertionsPassed: 0,
  };
}

function emptyCoverage(idlInstructions: string[]): InstructionCoverage {
  return {
    total: idlInstructions.length,
    catalogTargeted: [],
    catalogMissing: [...idlInstructions],
    executed: [],
    executionMissing: [...idlInstructions],
  };
}

export class ProtocolTestHarness {
  readonly apiUrl: string;
  readonly outputRoot: string;
  readonly idl: DuskIdl;
  readonly idlInstructions: string[];
  readonly wallets: Record<string, Keypair>;
  readonly gitRevision: string;
  readonly id: string;
  readonly runDir: string;
  readonly reportPath: string;
  readonly latestPath: string;
  readonly markdownPath: string;
  readonly issuesPath: string;
  readonly instructionDiscriminators: Map<string, string>;
  readonly httpTimeoutMs: number;
  readonly scenarioTimeoutMs: number;

  config!: ForkConfig;
  connection!: Connection;
  report!: ProtocolTestRun;
  currentScenario: ScenarioResult | null = null;
  private bootstrapEvidencePromise: Promise<BootstrapEvidence> | null = null;
  private currentScenarioAbortController: AbortController | null = null;

  constructor() {
    this.apiUrl = (process.env.FORK_API_URL ?? process.env.V2_FORK_API_URL ?? DEFAULT_API_URL).replace(/\/$/, "");
    this.outputRoot = resolve(process.env.PROTOCOL_TEST_OUTPUT_DIR ?? DEFAULT_OUTPUT_DIR);
    this.httpTimeoutMs = positiveIntegerEnv(
      "PROTOCOL_TEST_HTTP_TIMEOUT_MS",
      DEFAULT_HTTP_TIMEOUT_MS,
      300_000,
    );
    this.scenarioTimeoutMs = positiveIntegerEnv(
      "PROTOCOL_TEST_SCENARIO_TIMEOUT_MS",
      DEFAULT_SCENARIO_TIMEOUT_MS,
      3_600_000,
    );
    this.idl = JSON.parse(readFileSync(resolve("target/idl/dusk.json"), "utf8")) as DuskIdl;
    this.idlInstructions = this.idl.instructions.map((instruction) => instruction.name).sort();
    this.gitRevision = execFileSync("git", ["rev-parse", "HEAD"], { encoding: "utf8" }).trim();
    this.id = runId(this.gitRevision);
    this.runDir = resolve(this.outputRoot, this.id);
    this.reportPath = resolve(this.runDir, "report.json");
    this.latestPath = resolve(this.outputRoot, "latest.json");
    this.markdownPath = resolve(this.runDir, "report.md");
    this.issuesPath = resolve(this.runDir, "issues.md");
    this.wallets = {
      alice: Keypair.generate(),
      bob: Keypair.generate(),
      trader: Keypair.generate(),
      referrer: Keypair.generate(),
      liquidator: Keypair.generate(),
      bidder: Keypair.generate(),
      // Use a domain-derived development key whose address is verified absent
      // from the mainnet fork. The former common [42; 32] seed maps to a live
      // program-owned mainnet account and must not be overwritten by a faucet.
      emergency: Keypair.fromSeed(
        createHash("sha256")
          .update(DEVELOPMENT_EMERGENCY_AUTHORITY_DOMAIN)
          .digest(),
      ),
    };
    this.instructionDiscriminators = new Map(
      this.idl.instructions.map((instruction) => [
        Buffer.from(instruction.discriminator).toString("hex"),
        instruction.name,
      ])
    );
  }

  async initialize(): Promise<void> {
    this.config = await this.get<ForkConfig>("/api/v2/fork/config");
    const rpcFetch = (async (input: unknown, init?: unknown) => {
      try {
        return await boundedFetch(
          input as Parameters<typeof globalThis.fetch>[0],
          init as RequestInit | undefined,
          this.httpTimeoutMs,
          `Solana RPC request timed out after ${this.httpTimeoutMs}ms`,
          this.currentScenarioAbortController?.signal,
        );
      } catch (error) {
        if (jsonRpcMethod(init) === "sendTransaction") {
          throw new MutationOutcomeUncertainError(
            `Solana sendTransaction outcome is uncertain: ${errorText(error)}`,
          );
        }
        throw error;
      }
    }) as FetchFn;
    this.connection = new Connection(this.config.rpcUrl, {
      commitment: "confirmed",
      fetch: rpcFetch,
    });
    // web3.js caches the blockhash used by the legacy Transaction simulation
    // overload for 30 seconds. Surfnet's transaction-mode bank can invalidate
    // that blockhash after a single submitted transaction, causing later
    // rejection probes to report BlockhashNotFound instead of the protocol
    // error under test. Force every legacy simulation to observe a fresh bank.
    (this.connection as unknown as { _disableBlockhashCaching: boolean })
      ._disableBlockhashCaching = true;
    const scenarios = SCENARIO_CATALOG.map((entry) => this.emptyScenarioResult(entry));
    this.report = {
      schemaVersion: 1,
      runId: this.id,
      status: "running",
      startedAt: new Date().toISOString(),
      finishedAt: null,
      durationMs: null,
      gitRevision: this.gitRevision,
      rpcUrl: this.config.rpcUrl,
      apiUrl: this.apiUrl,
      programId: this.config.programId,
      market: this.config.market,
      wallets: Object.fromEntries(
        Object.entries(this.wallets).map(([name, wallet]) => [name, wallet.publicKey.toBase58()])
      ),
      idlInstructions: this.idlInstructions,
      coverage: emptyCoverage(this.idlInstructions),
      summary: emptySummary(),
      scenarios,
    };
    mkdirSync(this.runDir, { recursive: true, mode: 0o700 });
    this.persist();
  }

  async runScenarios(definitions: ScenarioDefinition[]): Promise<ProtocolTestRun> {
    const implementedIds = new Set(definitions.map((definition) => definition.id));
    let fatalRunError: ScenarioTimeoutError | MutationOutcomeUncertainError | null = null;
    for (const definition of definitions) {
      const result = this.report.scenarios.find((scenario) => scenario.id === definition.id);
      if (!result) throw new Error(`Scenario ${definition.id} is not present in the catalog`);
      if (definition.fixtureModes && !definition.fixtureModes.includes(this.config.fixtureMode)) {
        result.status = "skipped";
        result.startedAt = new Date().toISOString();
        result.finishedAt = result.startedAt;
        result.durationMs = 0;
        result.error = `Requires fixture mode: ${definition.fixtureModes.join(", ")}`;
        console.log(`\n[SKIP] ${result.id}: requires ${definition.fixtureModes.join(", ")}`);
        this.persist();
        continue;
      }
      this.currentScenario = result;
      result.startedAt = new Date().toISOString();
      const started = Date.now();
      console.log(`\n[RUN ] ${result.id}: ${result.title}`);
      const abortController = new AbortController();
      this.currentScenarioAbortController = abortController;
      const timeoutError = new ScenarioTimeoutError(
        `Scenario ${result.id} timed out after ${this.scenarioTimeoutMs}ms; aborting the protocol run to prevent late state mutation`,
      );
      let timeout: NodeJS.Timeout | undefined;
      try {
        await Promise.race([
          definition.run(this),
          new Promise<never>((_resolvePromise, reject) => {
            timeout = setTimeout(() => {
              abortController.abort(timeoutError);
              reject(timeoutError);
            }, this.scenarioTimeoutMs);
          }),
        ]);
        result.status = "passed";
        console.log(`[PASS] ${result.id}`);
      } catch (error) {
        result.status = "failed";
        result.error = errorText(error);
        console.error(`[FAIL] ${result.id}: ${result.error}`);
        if (
          error instanceof ScenarioTimeoutError ||
          error instanceof MutationOutcomeUncertainError
        ) {
          fatalRunError = error;
        }
      } finally {
        if (timeout !== undefined) clearTimeout(timeout);
        this.currentScenarioAbortController = null;
        result.finishedAt = new Date().toISOString();
        result.durationMs = Date.now() - started;
        this.currentScenario = null;
        this.persist();
      }
      if (fatalRunError) break;
      if (definition.fatal && result.status === "failed") {
        console.error(`[STOP] ${result.id} is a required clean-state precondition`);
        break;
      }
    }

    for (const result of this.report.scenarios) {
      if (!implementedIds.has(result.id)) result.status = "not-run";
    }
    this.finalizeReport();
    if (fatalRunError) throw fatalRunError;
    return this.report;
  }

  async execute(options: ExecuteOptions): Promise<TransactionEvidence> {
    const scenario = this.requireScenario();
    const signer = this.wallet(options.wallet);
    const expected = options.expected ?? "success";
    const started = Date.now();
    let simulation: SimulationEvidence = { err: null, unitsConsumed: null, logs: [], returnData: null };
    let signature: string | null = null;
    let slot: number | null = null;
    let instructionNames: string[] = [];
    let caught: unknown = null;
    let transientRetries = 0;
    let submissionAttempted = false;
    let submissionConfirmed = false;

    // Each attempt rebuilds the transaction through the API. This matters for
    // Surfnet transaction-mode banks, where an otherwise healthy simulation can
    // race past the short-lived blockhash embedded in the previous response.
    for (let attempt = 1; attempt <= 3; attempt += 1) {
      simulation = { err: null, unitsConsumed: null, logs: [], returnData: null };
      signature = null;
      slot = null;
      instructionNames = [];
      caught = null;
      submissionAttempted = false;
      submissionConfirmed = false;
      try {
        const response = await this.post<ForkTransactionResponse>(options.endpoint, {
          owner: signer.publicKey.toBase58(),
          ...options.body,
        });
        const transaction = Transaction.from(Buffer.from(response.transaction, "base64"));
        instructionNames = this.decodeDuskInstructions(transaction);
        if (!options.apiSigned) transaction.sign(signer);
        const simulated = await (this.connection as any).simulateTransaction(transaction);
        simulation = {
          err: simulated.value.err ?? null,
          unitsConsumed: simulated.value.unitsConsumed ?? null,
          logs: simulated.value.logs ?? [],
          returnData: simulated.value.returnData
            ? {
                programId: simulated.value.returnData.programId,
                data: simulated.value.returnData.data as [string, string],
              }
            : null,
        };

        // Infrastructure errors are not evidence of an expected protocol
        // rejection. Rebuild before evaluating either success or failure.
        if (simulation.err) {
          const simulationFailure = new Error(
            `Transaction simulation failed: ${stableJson(simulation.err)}`,
          );
          if (isTransientForkError(simulationFailure)) throw simulationFailure;
        }

        if (expected === "failure") {
          if (!simulation.err) {
            throw new Error("Transaction simulation succeeded but failure was expected");
          }
        } else {
          if (simulation.err) {
            throw new Error(`Transaction simulation failed: ${stableJson(simulation.err)}`);
          }
          if (options.submit !== false) {
            submissionAttempted = true;
            signature = await this.connection.sendRawTransaction(transaction.serialize(), {
              skipPreflight: false,
              maxRetries: 3,
            });
            slot = await this.waitForConfirmation(signature);
            submissionConfirmed = true;
            simulation.cpiEventData = await this.confirmedCpiEventData(signature);
          }
        }
        break;
      } catch (error) {
        caught = submissionAttempted && !submissionConfirmed &&
            !(error instanceof MutationOutcomeUncertainError)
          ? new MutationOutcomeUncertainError(
              `Transaction submission outcome is uncertain${signature ? ` for ${signature}` : ""}: ${errorText(error)}`,
            )
          : error;
        if (signature === null && attempt < 3 && isTransientForkError(caught)) {
          transientRetries += 1;
          await new Promise((resolvePromise) => setTimeout(resolvePromise, 500 * attempt));
          continue;
        }
        break;
      }
    }

    if (transientRetries > 0) {
      scenario.evidence.push({
        type: "observation",
        label: `${options.label} transient fork retries`,
        value: transientRetries,
      });
    }

    const passed = expected === "failure"
      ? Boolean(simulation.err) && caught === null
      : caught === null && (options.submit === false || signature !== null);
    const evidence: TransactionEvidence = {
      type: "transaction",
      label: options.label,
      wallet: signer.publicKey.toBase58(),
      endpoint: options.endpoint,
      expected,
      status: passed ? "passed" : "failed",
      submitted: signature !== null,
      durationMs: Date.now() - started,
      signature,
      slot,
      instructions: instructionNames,
      simulation,
      errorCode: parseErrorCode(simulation.logs, caught ?? simulation.err),
      error: caught === null ? null : errorText(caught),
    };
    scenario.evidence.push(evidence);
    this.persist();

    if (caught instanceof MutationOutcomeUncertainError) throw caught;
    if (!passed) throw new Error(`${options.label}: ${evidence.error ?? "unexpected transaction result"}`);
    return evidence;
  }

  async probe(
    walletName: string,
    endpoint: string,
    body: Record<string, unknown>
  ): Promise<ProbeResult> {
    const signer = this.wallet(walletName);
    for (let attempt = 1; attempt <= 3; attempt += 1) {
      try {
        // Rebuild on every retry so both state-derived accounts and the
        // short-lived Surfnet blockhash come from the current bank.
        const response = await this.post<ForkTransactionResponse>(endpoint, {
          owner: signer.publicKey.toBase58(),
          ...body,
        });
        const transaction = Transaction.from(Buffer.from(response.transaction, "base64"));
        transaction.sign(signer);
        const simulated = await (this.connection as any).simulateTransaction(transaction);
        const logs = simulated.value.logs ?? [];
        if (simulated.value.err != null) {
          const simulationFailure = new Error(
            `Transaction simulation failed: ${stableJson(simulated.value.err)}`,
          );
          if (isTransientForkError(simulationFailure)) throw simulationFailure;
        }
        return {
          succeeds: simulated.value.err == null,
          errorCode: parseErrorCode(logs, simulated.value.err),
          unitsConsumed: simulated.value.unitsConsumed ?? null,
          logs,
        };
      } catch (error) {
        if (attempt === 3 || !isTransientForkError(error)) throw error;
        await new Promise((resolvePromise) => setTimeout(resolvePromise, 500 * attempt));
      }
    }
    throw new Error("Transaction probe retries exhausted");
  }

  async assertApiRejection(
    options: AssertApiRejectionOptions,
  ): Promise<ForkApiResponseError> {
    if (
      !Number.isInteger(options.expectedStatus) ||
      options.expectedStatus < 400 ||
      options.expectedStatus > 499
    ) {
      throw new Error("Expected API rejection status must be a 4xx integer");
    }

    const signer = this.wallet(options.wallet);
    let rejection: unknown;
    try {
      await this.post(options.endpoint, {
        owner: signer.publicKey.toBase58(),
        ...options.body,
      });
    } catch (error) {
      rejection = error;
    }

    if (rejection instanceof MutationOutcomeUncertainError) throw rejection;
    if (!(rejection instanceof ForkApiResponseError)) {
      const detail = rejection === undefined
        ? "the API request succeeded"
        : `a generic infrastructure failure occurred: ${errorText(rejection)}`;
      throw new Error(
        `${options.label}: expected a deterministic API rejection, but ${detail}`,
      );
    }
    if (rejection.status >= 500) {
      throw new Error(
        `${options.label}: expected a deterministic API rejection, received infrastructure HTTP ${rejection.status}`,
      );
    }

    this.assertEqual(
      `${options.label} HTTP status`,
      rejection.status,
      options.expectedStatus,
    );
    this.assertEqual(
      `${options.label} API code`,
      rejection.code,
      options.expectedCode,
    );
    return rejection;
  }

  async buildSignedTransaction(
    walletName: string,
    endpoint: string,
    body: Record<string, unknown>
  ): Promise<Transaction> {
    const signer = this.wallet(walletName);
    const response = await this.post<ForkTransactionResponse>(endpoint, {
      owner: signer.publicKey.toBase58(),
      ...body,
    });
    const transaction = Transaction.from(Buffer.from(response.transaction, "base64"));
    transaction.sign(signer);
    return transaction;
  }

  async simulateBuiltTransaction(transaction: Transaction): Promise<ProbeResult> {
    const simulated = await this.simulateWithTransientRetry(transaction);
    const logs = simulated.value.logs ?? [];
    return {
      succeeds: simulated.value.err == null,
      errorCode: parseErrorCode(logs, simulated.value.err),
      unitsConsumed: simulated.value.unitsConsumed ?? null,
      logs,
    };
  }

  async executeBuiltTransaction(options: {
    wallet: string;
    label: string;
    transaction: Transaction;
    expected?: "success" | "failure";
    submit?: boolean;
  }): Promise<TransactionEvidence> {
    const scenario = this.requireScenario();
    const expected = options.expected ?? "success";
    const started = Date.now();
    const simulated = await this.simulateWithTransientRetry(options.transaction);
    const simulation: SimulationEvidence = {
      err: simulated.value.err ?? null,
      unitsConsumed: simulated.value.unitsConsumed ?? null,
      logs: simulated.value.logs ?? [],
      returnData: simulated.value.returnData
        ? {
            programId: simulated.value.returnData.programId,
            data: simulated.value.returnData.data as [string, string],
          }
        : null,
    };
    let signature: string | null = null;
    let slot: number | null = null;
    let caught: unknown = null;
    let submissionAttempted = false;
    let submissionConfirmed = false;
    try {
      if (expected === "failure") {
        if (!simulation.err) throw new Error("Built transaction succeeded but failure was expected");
      } else {
        if (simulation.err) throw new Error(`Built transaction simulation failed: ${stableJson(simulation.err)}`);
        if (options.submit !== false) {
          submissionAttempted = true;
          signature = await this.connection.sendRawTransaction(options.transaction.serialize(), {
            skipPreflight: false,
            maxRetries: 3,
          });
          slot = await this.waitForConfirmation(signature);
          submissionConfirmed = true;
          simulation.cpiEventData = await this.confirmedCpiEventData(signature);
        }
      }
    } catch (error) {
      caught = submissionAttempted && !submissionConfirmed &&
          !(error instanceof MutationOutcomeUncertainError)
        ? new MutationOutcomeUncertainError(
            `Built transaction submission outcome is uncertain${signature ? ` for ${signature}` : ""}: ${errorText(error)}`,
          )
        : error;
    }
    const passed = expected === "failure"
      ? Boolean(simulation.err) && caught === null
      : caught === null && (options.submit === false || signature !== null);
    const evidence: TransactionEvidence = {
      type: "transaction",
      label: options.label,
      wallet: this.wallet(options.wallet).publicKey.toBase58(),
      endpoint: "built-transaction",
      expected,
      status: passed ? "passed" : "failed",
      submitted: signature !== null,
      durationMs: Date.now() - started,
      signature,
      slot,
      instructions: this.decodeDuskInstructions(options.transaction),
      simulation,
      errorCode: parseErrorCode(simulation.logs, caught ?? simulation.err),
      error: caught === null ? null : errorText(caught),
    };
    scenario.evidence.push(evidence);
    this.persist();
    if (caught instanceof MutationOutcomeUncertainError) throw caught;
    if (!passed) throw new Error(`${options.label}: ${evidence.error ?? "unexpected transaction result"}`);
    return evidence;
  }

  private async simulateWithTransientRetry(transaction: Transaction): Promise<any> {
    for (let attempt = 1; attempt <= 3; attempt += 1) {
      try {
        return await (this.connection as any).simulateTransaction(transaction);
      } catch (error) {
        if (attempt === 3 || !isTransientForkError(error)) throw error;
        await new Promise((resolvePromise) => setTimeout(resolvePromise, 500 * attempt));
      }
    }
    throw new Error("Transaction simulation retries exhausted");
  }

  async fundWallet(name: string, baseAmount = "1000", quoteAmount = "1000", sol = 20): Promise<void> {
    const wallet = this.wallet(name);
    await this.post("/api/v2/fork/fund-wallet", {
      wallet: wallet.publicKey.toBase58(),
      baseAmount,
      quoteAmount,
      sol,
    });
  }

  async timeTravel(seconds: number, slots = 0): Promise<void> {
    const result = await this.post<Record<string, unknown>>("/api/v2/fork/admin/time-travel", {
      seconds,
      slots,
    });
    this.observe(`fork clock advanced ${seconds} seconds and ${slots} slots`, result);
  }

  /// Surfnet's Clock account free-runs with wall time while risk snapshots
  /// only advance when transactions execute, and signature confirmation can
  /// surface before the account store that serves later simulations reflects
  /// the same transaction. Either effect can make a just-refreshed auction
  /// reference simulate as stale. Poll the program-visible precondition
  /// (Clock account slot minus the market risk snapshot slot) until it
  /// objectively holds; never inspect the guarded simulation itself.
  async awaitAuctionReferenceAge(options: {
    label: string;
    minAgeSlots?: bigint;
    maxAgeSlots?: bigint;
    timeoutMs?: number;
  }): Promise<void> {
    const deadline = Date.now() + (options.timeoutMs ?? 20_000);
    const coder = new BorshCoder(this.idl as unknown as Idl);
    const market = new PublicKey(this.config.market);
    let lastSnapshotSlot = 0n;
    let clockSlot = 0n;
    while (Date.now() < deadline) {
      const [marketAccount, clockAccount] = await Promise.all([
        this.connection.getAccountInfo(market, "confirmed"),
        this.connection.getAccountInfo(SYSVAR_CLOCK_PUBKEY, "confirmed"),
      ]);
      if (marketAccount && clockAccount && clockAccount.data.length >= 8) {
        // The raw BorshCoder keeps IDL snake_case field names; only the
        // Program client converts to camelCase.
        const decoded = coder.accounts.decode("Market", marketAccount.data);
        const rawSnapshotSlot = decoded?.risk?.last_snapshot_slot ?? decoded?.risk?.lastSnapshotSlot;
        if (rawSnapshotSlot === undefined) {
          throw new Error(`${options.label}: decoded market account has no risk snapshot slot`);
        }
        lastSnapshotSlot = BigInt(rawSnapshotSlot.toString());
        clockSlot = clockAccount.data.readBigUInt64LE(0);
        const age = clockSlot > lastSnapshotSlot ? clockSlot - lastSnapshotSlot : 0n;
        const minSatisfied = options.minAgeSlots === undefined || age >= options.minAgeSlots;
        const maxSatisfied =
          options.maxAgeSlots === undefined || (lastSnapshotSlot > 0n && age <= options.maxAgeSlots);
        if (minSatisfied && maxSatisfied) {
          this.observe(options.label, {
            lastSnapshotSlot: lastSnapshotSlot.toString(),
            clockSlot: clockSlot.toString(),
            ageSlots: age.toString(),
          });
          return;
        }
      }
      await new Promise((resolvePromise) => setTimeout(resolvePromise, 250));
    }
    throw new Error(
      `${options.label}: auction reference age did not reach the requested bounds before timeout ` +
        `(lastSnapshotSlot=${lastSnapshotSlot}, clockSlot=${clockSlot})`,
    );
  }

  async market(): Promise<MarketPayload> {
    return this.get<MarketPayload>(`/api/v2/markets/${this.config.market}`);
  }

  async futarchy(): Promise<any> {
    return this.get<any>("/api/v2/fork/futarchy");
  }

  async bootstrapEvidence(): Promise<BootstrapEvidence> {
    this.bootstrapEvidencePromise ??= this.get<BootstrapEvidence>(
      "/api/v2/fork/bootstrap-evidence",
    );
    return this.bootstrapEvidencePromise;
  }

  async recordConfirmedSignature(label: string, signature: string): Promise<TransactionEvidence> {
    const scenario = this.requireScenario();
    let confirmed: Awaited<ReturnType<Connection["getTransaction"]>> = null;
    for (let attempt = 0; attempt < 20 && !confirmed; attempt += 1) {
      confirmed = await this.connection.getTransaction(signature, {
        commitment: "confirmed",
        maxSupportedTransactionVersion: 0,
      });
      if (!confirmed) await new Promise((resolvePromise) => setTimeout(resolvePromise, 100));
    }
    if (!confirmed) throw new Error(`Confirmed bootstrap transaction was not found: ${signature}`);
    const logs = confirmed.meta?.logMessages ?? [];
    const instructions = this.decodeConfirmedDuskInstructions(confirmed.transaction.message);
    const passed = confirmed.meta?.err == null;
    const evidence: TransactionEvidence = {
      type: "transaction",
      label,
      wallet: this.config.payer,
      endpoint: "/api/v2/fork/bootstrap-evidence",
      expected: "success",
      status: passed ? "passed" : "failed",
      submitted: true,
      durationMs: 0,
      signature,
      slot: confirmed.slot,
      instructions,
      simulation: {
        err: confirmed.meta?.err ?? null,
        unitsConsumed: confirmed.meta?.computeUnitsConsumed == null
          ? null
          : Number(confirmed.meta.computeUnitsConsumed),
        logs,
        returnData: null,
      },
      errorCode: parseErrorCode(logs, confirmed.meta?.err),
      error: passed ? null : stableJson(confirmed.meta?.err),
    };
    scenario.evidence.push(evidence);
    this.persist();
    if (!passed) throw new Error(`${label}: confirmed bootstrap transaction failed`);
    return evidence;
  }

  async positions(walletName: string, positionId: PublicKey): Promise<any[]> {
    const wallet = this.wallet(walletName);
    const payload = await this.get<{ positions: any[] }>(
      `/api/v2/users/${wallet.publicKey.toBase58()}/positions?positionId=${positionId.toBase58()}`
    );
    return payload.positions;
  }

  async yieldAccount(
    walletName: string,
    asset: "base" | "quote",
    tokenKind: "ylp" | "hlp"
  ): Promise<any | null> {
    const owner = this.wallet(walletName).publicKey;
    const params = new URLSearchParams({
      owner: owner.toBase58(),
      asset,
      tokenKind,
    });
    const payload = await this.get<{ yieldAccount: any | null }>(
      `/api/v2/fork/yield-account?${params.toString()}`
    );
    return payload.yieldAccount;
  }

  async tokenBalance(walletName: string, mint: string, tokenProgram: string): Promise<bigint> {
    const wallet = this.wallet(walletName);
    const address = getAssociatedTokenAddressSync(
      new PublicKey(mint),
      wallet.publicKey,
      false,
      new PublicKey(tokenProgram)
    );
    const info = await this.connection.getAccountInfo(address, "confirmed");
    if (!info) return 0n;
    return (await getAccount(this.connection, address, "confirmed", new PublicKey(tokenProgram))).amount;
  }

  async lpBalance(walletName: string, mint: string): Promise<bigint> {
    return this.tokenBalance(walletName, mint, TOKEN_2022_PROGRAM_ID.toBase58());
  }

  async tokenAccountBalance(address: PublicKey, tokenProgram: string): Promise<bigint> {
    const info = await this.connection.getAccountInfo(address, "confirmed");
    if (!info) return 0n;
    return (await getAccount(this.connection, address, "confirmed", new PublicKey(tokenProgram))).amount;
  }

  events(evidence: TransactionEvidence, eventName?: string): Array<{ name: string; data: any }> {
    const coder = new BorshCoder(this.idl as unknown as Idl);
    const parser = new EventParser(new PublicKey(this.config.programId), coder);
    const cpiEvents = (evidence.simulation.cpiEventData ?? [])
      .map((data) => coder.events.decode(data))
      .filter((event): event is { name: string; data: any } => event !== null);
    return [...parser.parseLogs(evidence.simulation.logs), ...cpiEvents]
      .filter((event) => eventName === undefined || event.name === eventName);
  }

  private async confirmedCpiEventData(signature: string): Promise<string[]> {
    const eventTag = Buffer.alloc(8);
    eventTag.writeBigUInt64LE(0x1d9acb512ea545e4n);
    for (let attempt = 0; attempt < 10; attempt += 1) {
      const transaction = await this.connection.getTransaction(signature, {
        commitment: "confirmed",
        maxSupportedTransactionVersion: 0,
      });
      if (transaction) {
        const eventData: string[] = [];
        const message = transaction.transaction.message;
        const accountKeys = [
          ...message.staticAccountKeys,
          ...(transaction.meta?.loadedAddresses?.writable ?? []),
          ...(transaction.meta?.loadedAddresses?.readonly ?? []),
        ];
        const duskProgramId = new PublicKey(this.config.programId);
        for (const group of transaction.meta?.innerInstructions ?? []) {
          for (const instruction of group.instructions) {
            const compiled = instruction as {
              data?: string;
              programIdIndex?: number;
            };
            const invokedProgram = compiled.programIdIndex === undefined
              ? undefined
              : accountKeys[compiled.programIdIndex];
            if (!compiled.data || !invokedProgram?.equals(duskProgramId)) continue;
            const data = Buffer.from(utils.bytes.bs58.decode(compiled.data));
            if (data.length > eventTag.length && data.subarray(0, eventTag.length).equals(eventTag)) {
              eventData.push(data.subarray(eventTag.length).toString("base64"));
            }
          }
        }
        return eventData;
      }
      await new Promise((resolvePromise) => setTimeout(resolvePromise, 100));
    }
    return [];
  }

  async solBalance(walletName: string): Promise<number> {
    return this.connection.getBalance(this.wallet(walletName).publicKey, "confirmed");
  }

  wallet(name: string): Keypair {
    const wallet = this.wallets[name];
    if (!wallet) throw new Error(`Unknown test wallet: ${name}`);
    return wallet;
  }

  observe(label: string, value: unknown): void {
    this.requireScenario().evidence.push({ type: "observation", label, value });
    this.persist();
  }

  assertEqual(label: string, actual: unknown, expected: unknown): void {
    const actualJson = stableJson(actual);
    const expectedJson = stableJson(expected);
    const passed = actualJson === expectedJson;
    this.recordAssertion({
      type: "assertion",
      label,
      status: passed ? "passed" : "failed",
      expected,
      actual,
      detail: passed ? null : `Expected ${expectedJson}, received ${actualJson}`,
    });
    if (!passed) throw new Error(`${label}: expected ${expectedJson}, received ${actualJson}`);
  }

  assertTrue(label: string, condition: boolean, actual: unknown = condition): void {
    const evidence: AssertionEvidence = {
      type: "assertion",
      label,
      status: condition ? "passed" : "failed",
      expected: true,
      actual,
      detail: condition ? null : "Condition was false",
    };
    this.recordAssertion(evidence);
    if (!condition) throw new Error(`${label}: condition was false`);
  }

  private recordAssertion(evidence: AssertionEvidence): void {
    this.requireScenario().evidence.push(evidence);
    this.persist();
  }

  private requireScenario(): ScenarioResult {
    if (!this.currentScenario) throw new Error("Evidence was recorded outside a running scenario");
    return this.currentScenario;
  }

  private decodeDuskInstructions(transaction: Transaction): string[] {
    const programId = new PublicKey(this.config.programId);
    return transaction.instructions
      .filter((instruction) => instruction.programId.equals(programId) && instruction.data.length >= 8)
      .map((instruction) => this.instructionDiscriminators.get(instruction.data.subarray(0, 8).toString("hex")))
      .filter((name): name is string => Boolean(name));
  }

  private decodeConfirmedDuskInstructions(message: any): string[] {
    const programId = new PublicKey(this.config.programId);
    const accountKeys: PublicKey[] = (message.staticAccountKeys ?? message.accountKeys ?? [])
      .map((key: PublicKey | { pubkey: PublicKey }) => key instanceof PublicKey ? key : key.pubkey);
    const instructions = message.compiledInstructions ?? message.instructions ?? [];
    return instructions
      .filter((instruction: any) => accountKeys[instruction.programIdIndex]?.equals(programId))
      .map((instruction: any) => {
        const data = typeof instruction.data === "string"
          ? Buffer.from(utils.bytes.bs58.decode(instruction.data))
          : Buffer.from(instruction.data);
        return data.length >= 8
          ? this.instructionDiscriminators.get(data.subarray(0, 8).toString("hex"))
          : undefined;
      })
      .filter((name: string | undefined): name is string => Boolean(name));
  }

  private async waitForConfirmation(signature: string): Promise<number> {
    const deadline = Date.now() + 30_000;
    while (Date.now() < deadline) {
      try {
        const status = (await this.connection.getSignatureStatuses([signature], {
          searchTransactionHistory: true,
        })).value[0];
        if (status?.err) throw new Error(`Transaction ${signature} failed: ${stableJson(status.err)}`);
        if (status?.confirmationStatus === "confirmed" || status?.confirmationStatus === "finalized") {
          return status.slot;
        }
      } catch (error) {
        if (!isTransientForkError(error)) throw error;
      }
      await new Promise((resolvePromise) => setTimeout(resolvePromise, 250));
    }
    throw new Error(`Timed out waiting for transaction confirmation: ${signature}`);
  }

  private async get<T>(path: string): Promise<T> {
    return this.fetchJson<T>(path, { method: "GET" });
  }

  private async post<T = unknown>(path: string, body: Record<string, unknown>): Promise<T> {
    return this.fetchJson<T>(path, { method: "POST", body: JSON.stringify(body) });
  }

  private async fetchJson<T>(path: string, init: RequestInit): Promise<T> {
    const headers: Record<string, string> = {
      "content-type": "application/json",
    };
    let serverSigned =
      path === "/api/v2/fork/tx/bootstrap-rejection" ||
      path === "/api/v2/fork/tx/create-market";
    if (typeof init.body === "string" && init.body) {
      try {
        const bootstrapSigned = JSON.parse(init.body)?.bootstrapSigned;
        serverSigned =
          serverSigned ||
          (bootstrapSigned !== undefined &&
            bootstrapSigned !== null &&
            bootstrapSigned !== false);
      } catch {
        // The API owns malformed-JSON reporting; do not hide it in the harness.
      }
    }
    if (
      (path.startsWith("/api/v2/fork/admin/") || serverSigned) &&
      process.env.FORK_ADMIN_TOKEN
    ) {
      headers["x-fork-admin-token"] = process.env.FORK_ADMIN_TOKEN;
    }
    const method = init.method ?? "GET";
    let response: Response;
    try {
      response = await boundedFetch(
        `${this.apiUrl}${path}`,
        {
          ...init,
          headers,
        },
        this.httpTimeoutMs,
        `${method} ${path} timed out after ${this.httpTimeoutMs}ms`,
        this.currentScenarioAbortController?.signal,
      );
    } catch (error) {
      if (method === "POST") {
        throw new MutationOutcomeUncertainError(
          `${method} ${path} outcome is uncertain: ${errorText(error)}`,
        );
      }
      throw error;
    }
    let payload: any;
    try {
      payload = await response.json();
    } catch (error) {
      if (method === "POST") {
        throw new MutationOutcomeUncertainError(
          `${method} ${path} returned an unreadable response; outcome is uncertain: ${errorText(error)}`,
        );
      }
      throw error;
    }
    if (method === "POST" && payload?.uncertainOutcome === true) {
      throw new MutationOutcomeUncertainError(
        `${method} ${path} outcome is uncertain: ${stableJson(payload)}`,
      );
    }
    if (!response.ok) {
      throw new ForkApiResponseError(method, path, response.status, payload);
    }
    if (payload?.success === false) {
      throw new Error(`${method} ${path} failed: ${stableJson(payload)}`);
    }
    return (payload?.data ?? payload) as T;
  }

  private emptyScenarioResult(entry: ScenarioCatalogEntry): ScenarioResult {
    return {
      ...entry,
      status: "not-run",
      startedAt: null,
      finishedAt: null,
      durationMs: null,
      evidence: [],
      error: null,
    };
  }

  private summarize(): RunSummary {
    const summary = emptySummary();
    summary.total = this.report.scenarios.length;
    for (const scenario of this.report.scenarios) {
      if (scenario.status === "passed") summary.passed += 1;
      if (scenario.status === "failed") summary.failed += 1;
      if (scenario.status === "skipped") summary.skipped += 1;
      if (scenario.status === "not-run") summary.notRun += 1;
      for (const evidence of scenario.evidence) {
        if (evidence.type === "transaction" && evidence.signature) summary.transactionsSubmitted += 1;
        if (evidence.type === "transaction" && evidence.expected === "failure" && evidence.status === "passed") {
          summary.expectedFailuresVerified += 1;
        }
        if (evidence.type === "assertion") {
          summary.assertions += 1;
          if (evidence.status === "passed") summary.assertionsPassed += 1;
        }
      }
    }
    return summary;
  }

  private coverage(): InstructionCoverage {
    const catalogTargeted = [...new Set(SCENARIO_CATALOG.flatMap((scenario) => scenario.instructions))]
      .filter((name) => this.idlInstructions.includes(name))
      .sort();
    const executed = [...new Set(
      this.report.scenarios.flatMap((scenario) =>
        scenario.evidence.flatMap((evidence) => evidence.type === "transaction" ? evidence.instructions : [])
      )
    )].sort();
    return {
      total: this.idlInstructions.length,
      catalogTargeted,
      catalogMissing: this.idlInstructions.filter((name) => !catalogTargeted.includes(name)),
      executed,
      executionMissing: this.idlInstructions.filter((name) => !executed.includes(name)),
    };
  }

  private finalizeReport(): void {
    this.report.finishedAt = new Date().toISOString();
    this.report.durationMs = Date.now() - Date.parse(this.report.startedAt);
    this.report.status = this.report.scenarios.some(
      (scenario) => scenario.status === "failed",
    )
      ? "failed"
      : "passed";
    this.persist();
  }

  private persist(): void {
    this.report.summary = this.summarize();
    this.report.coverage = this.coverage();
    const json = `${JSON.stringify(this.report, jsonReplacer, 2)}\n`;
    mkdirSync(this.runDir, { recursive: true, mode: 0o700 });
    writeFileSync(this.reportPath, json, { mode: 0o600 });
    writeFileSync(this.latestPath, json, { mode: 0o600 });
    writeFileSync(this.markdownPath, this.renderMarkdown(), { mode: 0o600 });
    writeFileSync(this.issuesPath, this.renderIssues(), { mode: 0o600 });
  }

  private renderMarkdown(): string {
    const lines = [
      `# Dusk protocol client run ${this.report.runId}`,
      "",
      `- Status: **${this.report.status}**`,
      `- Revision: \`${this.report.gitRevision}\``,
      `- Market: \`${this.report.market}\``,
      `- Started: ${this.report.startedAt}`,
      `- Finished: ${this.report.finishedAt ?? "running"}`,
      `- Scenarios: ${this.report.summary.passed} passed, ${this.report.summary.failed} failed, ${this.report.summary.notRun} not run`,
      `- Transactions: ${this.report.summary.transactionsSubmitted} confirmed, ${this.report.summary.expectedFailuresVerified} expected failures verified`,
      `- Assertions: ${this.report.summary.assertionsPassed}/${this.report.summary.assertions} passed`,
      `- Executed instructions: ${this.report.coverage.executed.length}/${this.report.coverage.total}`,
      "",
      "## Scenarios",
      "",
      "| Status | Scenario | Feature | Transactions | Assertions |",
      "| --- | --- | --- | ---: | ---: |",
    ];
    for (const scenario of this.report.scenarios) {
      const transactions = scenario.evidence.filter((evidence) => evidence.type === "transaction").length;
      const assertions = scenario.evidence.filter((evidence) => evidence.type === "assertion").length;
      lines.push(`| ${scenario.status} | \`${scenario.id}\` ${scenario.title} | ${scenario.feature} | ${transactions} | ${assertions} |`);
    }
    lines.push("", "## Missing Execution Coverage", "");
    lines.push(this.report.coverage.executionMissing.length
      ? this.report.coverage.executionMissing.map((name) => `- \`${name}\``).join("\n")
      : "All IDL instructions were executed.");
    lines.push("");
    return `${lines.join("\n")}\n`;
  }

  private renderIssues(): string {
    const failed = this.report.scenarios.filter((scenario) => scenario.status === "failed");
    const lines = [`# Issues from ${this.report.runId}`, ""];
    if (!failed.length) {
      lines.push("No scenario failures recorded.", "");
      return `${lines.join("\n")}\n`;
    }
    for (const scenario of failed) {
      lines.push(`## ${scenario.id}: ${scenario.title}`, "", `- Error: ${scenario.error ?? "Unknown"}`);
      for (const evidence of scenario.evidence) {
        if (evidence.type === "transaction" && evidence.status === "failed") {
          lines.push(`- Transaction step: ${evidence.label}`, `- Error code: ${evidence.errorCode ?? "unparsed"}`);
        }
        if (evidence.type === "assertion" && evidence.status === "failed") {
          lines.push(`- Assertion: ${evidence.label}`, `- Detail: ${evidence.detail ?? "mismatch"}`);
        }
      }
      lines.push("");
    }
    return `${lines.join("\n")}\n`;
  }
}

export function formatUnits(raw: bigint, decimals: number): string {
  const divisor = 10n ** BigInt(decimals);
  const whole = raw / divisor;
  const fraction = (raw % divisor).toString().padStart(decimals, "0").replace(/0+$/, "");
  return fraction ? `${whole}.${fraction}` : whole.toString();
}
