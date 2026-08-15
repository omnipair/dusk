import { createHash } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import http from "node:http";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { Connection, Keypair, PublicKey } from "@solana/web3.js";
import type { Surfnet } from "@solana/surfpool";
import WebSocket from "ws";
import {
  createTcpProxy,
  startSurfpoolRelayWorkerProcess,
  type SurfpoolRelayWorkerConfig,
  type SurfpoolRelayWorkerHandle,
} from "./surfpool_relay_worker.js";

export { createTcpProxy } from "./surfpool_relay_worker.js";

const DEFAULT_DUSK_PROGRAM_ID = "358bjJKXWxeAXAzteX1xTgyd9JNnjtzW8fnwCS8Da1mv";
const DEFAULT_LEVERAGE_DELEGATE_PROGRAM_ID =
  "EPGF9iFrbGnhWgC3To9rC9vxinEYuDHaz4RXgLPvuRkp";
const BPF_LOADER_UPGRADEABLE_ID = new PublicKey(
  "BPFLoaderUpgradeab1e11111111111111111111111",
);
const PROGRAM_TAG = 2;
const PROGRAM_DATA_TAG = 3;
const PROGRAM_DATA_METADATA_BYTES = 45;
const DEFAULT_PAYER_FUNDING_LAMPORTS = 100_000_000_000;
const DEFAULT_EVENT_DRAIN_MIN_INTERVAL_MS = 100;
const DEFAULT_EVENT_DRAIN_MAX_INTERVAL_MS = 250;
const DEFAULT_EVENT_DRAIN_LOG_INTERVAL_MS = 30_000;
const DEFAULT_HEALTH_REQUEST_TIMEOUT_MS = 5_000;
const DEFAULT_HEALTH_MAX_CONNECTIONS = 32;
const DEFAULT_STARTUP_PROBE_TIMEOUT_MS = 15_000;
const DEFAULT_RELAY_HEARTBEAT_INTERVAL_MS = 1_000;
const DEFAULT_RELAY_HEARTBEAT_STALE_MS = 10_000;
const DEFAULT_RELAY_CONNECT_TIMEOUT_MS = 5_000;
const DEFAULT_RELAY_MAX_CONNECTIONS = 256;
const DEFAULT_RELAY_TARGET_PROBE_INTERVAL_MS = 5_000;
const DEFAULT_RELAY_TARGET_PROBE_TIMEOUT_MS = 2_000;
const DEFAULT_RELAY_TARGET_PROBE_FAILURE_THRESHOLD = 3;
const DEFAULT_STARTUP_SETTLEMENT_TIMEOUT_MS = 30_000;

type ExactDeploySurface = Pick<Surfnet, "deploy">;
type ProgramDataWriteSurface = Pick<Surfnet, "setAccount">;
type SurfnetEventDrainSurface = {
  drainEvents(): unknown[];
};

export type SurfnetEventDrainMetrics = Readonly<{
  drainCalls: number;
  drainedEvents: number;
  nonEmptyDrains: number;
  maxBatchSize: number;
  failedDrains: number;
  currentIntervalMs: number;
  lastTickLagMs: number;
  maxTickLagMs: number;
}>;

export type SurfnetEventDrainHandle = {
  failure: Promise<never>;
  drainNow(): number;
  snapshot(): SurfnetEventDrainMetrics;
  stop(): SurfnetEventDrainMetrics;
};

export type HostedSurfpoolControllerConfig = {
  remoteRpcUrl: string;
  payer: Keypair;
  payerFundingLamports: number;
  listenHost: string;
  rpcPort: number;
  wsPort: number;
  duskProgramId: string;
  leverageDelegateProgramId: string;
  duskSoPath: string;
  duskIdlPath: string;
  leverageDelegateSoPath: string;
  leverageDelegateIdlPath: string;
  blockProductionMode: string;
  startupProbeTimeoutMs?: number;
  startupSettlementTimeoutMs?: number;
  relayHeartbeatIntervalMs?: number;
};

export type StartupPhaseTracker = Readonly<{
  track<T>(phase: PromiseLike<T>): Promise<T>;
  activeCount(): number;
  settle(timeoutMs: number): Promise<boolean>;
}>;

/** Retains every raw startup promise even when its abort/failure race loses. */
export function createStartupPhaseTracker(): StartupPhaseTracker {
  const active = new Set<Promise<unknown>>();
  return {
    track<T>(phase: PromiseLike<T>): Promise<T> {
      const tracked = Promise.resolve(phase);
      active.add(tracked);
      void tracked.then(
        () => active.delete(tracked),
        () => active.delete(tracked),
      );
      return tracked;
    },
    activeCount: () => active.size,
    async settle(timeoutMs: number): Promise<boolean> {
      if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1) {
        throw new Error("Startup phase settlement timeout must be a positive safe integer");
      }
      if (active.size === 0) return true;
      let timer: ReturnType<typeof setTimeout> | undefined;
      try {
        return await Promise.race([
          Promise.allSettled([...active]).then(() => active.size === 0),
          new Promise<false>((resolvePromise) => {
            timer = setTimeout(() => resolvePromise(false), timeoutMs);
          }),
        ]);
      } finally {
        if (timer) clearTimeout(timer);
      }
    },
  };
}

export class StartupPhaseSettlementError extends Error {
  constructor(timeoutMs: number, cause: unknown) {
    super(
      `Surfpool startup phases did not settle within ${timeoutMs}ms; ` +
        "refusing in-process Surfnet teardown and requiring a hard process exit",
      { cause },
    );
    this.name = "StartupPhaseSettlementError";
  }
}

export type ControllerReadinessConfig = {
  host: string;
  port: number;
  requestTimeoutMs: number;
  maxConnections: number;
};

export type ControllerReadinessSnapshot = Readonly<{
  status: "starting" | "ready" | "failed" | "stopping";
  startedAt: string;
  readyAt: string | null;
  programCount: number;
}>;

export type ControllerReadinessHandle = {
  host: string;
  port: number;
  failure: Promise<never>;
  snapshot(): ControllerReadinessSnapshot;
  markReady(programCount: number): void;
  markFailed(): void;
  markStopping(): void;
  close(): Promise<void>;
};

export type HostedSurfpoolControllerHandle = {
  payer: string;
  payerFundingLamports: number;
  rpcUrl: string;
  wsUrl: string;
  dynamicRpcUrl: string;
  dynamicWsUrl: string;
  programs: Array<{
    programId: string;
    programDataAddress: string;
    binarySha256: string;
  }>;
  failure: Promise<never>;
  stop(): Promise<void>;
};

function saturatedAdd(value: number, increment: number): number {
  return Math.min(Number.MAX_SAFE_INTEGER, value + increment);
}

function positiveSafeInterval(label: string, value: number): number {
  if (!Number.isSafeInteger(value) || value < 1) {
    throw new Error(`${label} must be a positive safe integer`);
  }
  return value;
}

/**
 * Continuously discards Surfnet's unbounded native simnet-event channel.
 *
 * Hosted controllers do not consume event payloads, so retain only saturated,
 * low-cardinality counters and periodically log them. A short interval while
 * events are flowing bounds memory; an idle backoff avoids a 40Hz NAPI call for
 * an empty channel. Tick-lag metrics make controller event-loop stalls visible.
 */
export function startSurfnetEventDrain(
  surfnet: SurfnetEventDrainSurface,
  options: {
    /** Fixed interval retained for deterministic tests and compatibility. */
    intervalMs?: number;
    minIntervalMs?: number;
    maxIntervalMs?: number;
    logIntervalMs?: number;
    now?: () => number;
    logger?: (entry: Record<string, unknown>) => void;
    setTimer?: typeof setTimeout;
    clearTimer?: typeof clearTimeout;
  } = {},
): SurfnetEventDrainHandle {
  const minIntervalMs = positiveSafeInterval(
    "Surfnet event drain minimum interval",
    options.intervalMs ??
      options.minIntervalMs ??
      DEFAULT_EVENT_DRAIN_MIN_INTERVAL_MS,
  );
  const maxIntervalMs = positiveSafeInterval(
    "Surfnet event drain maximum interval",
    options.intervalMs ??
      options.maxIntervalMs ??
      DEFAULT_EVENT_DRAIN_MAX_INTERVAL_MS,
  );
  if (maxIntervalMs < minIntervalMs) {
    throw new Error(
      "Surfnet event drain maximum interval must be at least its minimum interval",
    );
  }
  const logIntervalMs = positiveSafeInterval(
    "Surfnet event drain log interval",
    options.logIntervalMs ?? DEFAULT_EVENT_DRAIN_LOG_INTERVAL_MS,
  );
  const now = options.now ?? Date.now;
  const logger = options.logger ?? ((entry) => console.log(JSON.stringify(entry)));
  const setTimer = options.setTimer ?? setTimeout;
  const clearTimer = options.clearTimer ?? clearTimeout;
  const startedAtMs = now();
  let lastLogAtMs = startedAtMs;
  let drainCalls = 0;
  let drainedEvents = 0;
  let nonEmptyDrains = 0;
  let maxBatchSize = 0;
  let failedDrains = 0;
  let currentIntervalMs = minIntervalMs;
  let lastTickLagMs = 0;
  let maxTickLagMs = 0;
  let scheduledForMs = 0;
  let stopped = false;
  let failed = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let rejectFailure: (error: Error) => void = () => undefined;
  const failure = new Promise<never>((_resolve, reject) => {
    rejectFailure = reject;
  });
  // The controller races this promise once startup reaches its proxy phase.
  // Mark it handled immediately in case the first drain fails before then.
  void failure.catch(() => undefined);

  const snapshot = (): SurfnetEventDrainMetrics => ({
    drainCalls,
    drainedEvents,
    nonEmptyDrains,
    maxBatchSize,
    failedDrains,
    currentIntervalMs,
    lastTickLagMs,
    maxTickLagMs,
  });
  const log = (reason: "started" | "periodic" | "failed" | "stopped", error?: Error) => {
    const metrics = snapshot();
    try {
      logger({
        event: "dusk_surfpool_event_drain",
        reason,
        minIntervalMs,
        maxIntervalMs,
        logIntervalMs,
        uptimeMs: Math.max(0, now() - startedAtMs),
        ...metrics,
        ...(error ? { error: error.message } : {}),
      });
    } catch {
      // Observability must never interrupt the queue drain.
    }
  };
  const fail = (cause: unknown): Error => {
    const error = cause instanceof Error ? cause : new Error(String(cause));
    const wrapped = new Error(`Surfnet event drain failed: ${error.message}`, {
      cause: error,
    });
    if (!failed) {
      failed = true;
      failedDrains = saturatedAdd(failedDrains, 1);
      if (timer) clearTimer(timer);
      timer = undefined;
      log("failed", wrapped);
      rejectFailure(wrapped);
    }
    return wrapped;
  };
  const drainNow = (): number => {
    if (stopped || failed) return 0;
    let batchSize: number;
    try {
      // Do not assign the returned payload array: only its bounded size is
      // observed, allowing all event objects to be collected immediately.
      batchSize = surfnet.drainEvents().length;
    } catch (error) {
      throw fail(error);
    }
    drainCalls = saturatedAdd(drainCalls, 1);
    drainedEvents = saturatedAdd(drainedEvents, batchSize);
    if (batchSize > 0) {
      nonEmptyDrains = saturatedAdd(nonEmptyDrains, 1);
      maxBatchSize = Math.max(maxBatchSize, batchSize);
    }
    const timestamp = now();
    if (timestamp - lastLogAtMs >= logIntervalMs) {
      lastLogAtMs = timestamp;
      log("periodic");
    }
    return batchSize;
  };
  const schedule = () => {
    if (stopped || failed) return;
    scheduledForMs = now() + currentIntervalMs;
    timer = setTimer(tick, currentIntervalMs);
    timer.unref?.();
  };
  const tick = () => {
    timer = undefined;
    const observedAtMs = now();
    lastTickLagMs = Math.max(0, Math.ceil(observedAtMs - scheduledForMs));
    maxTickLagMs = Math.max(maxTickLagMs, lastTickLagMs);
    try {
      const batchSize = drainNow();
      currentIntervalMs = batchSize > 0
        ? minIntervalMs
        : Math.min(maxIntervalMs, currentIntervalMs + minIntervalMs);
    } catch {
      // fail() already rejected the fatal failure promise and stopped ticks.
    }
    schedule();
  };

  log("started");
  // Fail synchronously before the hosted controller performs any deployment
  // or bootstrap side effect. Later ticks report through failure.
  const initialBatchSize = drainNow();
  currentIntervalMs = initialBatchSize > 0
    ? minIntervalMs
    : Math.min(maxIntervalMs, minIntervalMs * 2);
  schedule();

  return {
    failure,
    drainNow,
    snapshot,
    stop() {
      if (stopped) return snapshot();
      if (timer) clearTimer(timer);
      timer = undefined;
      if (!failed) {
        try {
          drainNow();
        } catch {
          // The failure is already represented by failure and the final log.
        }
      }
      stopped = true;
      log("stopped");
      return snapshot();
    },
  };
}

function requiredPath(label: string, candidates: string[]): string {
  const paths = candidates.map((candidate) => resolve(candidate));
  const selected = paths.find(existsSync);
  if (!selected) throw new Error(`${label} not found; tried ${paths.join(", ")}`);
  return selected;
}

function requiredPort(label: string, raw: string): number {
  const value = Number(raw);
  if (!Number.isSafeInteger(value) || value < 1 || value > 65_535) {
    throw new Error(`${label} must be an integer between 1 and 65535`);
  }
  return value;
}

function boundedPositiveInteger(
  label: string,
  raw: string | undefined,
  fallback: number,
  maximum: number,
): number {
  const value = raw === undefined || raw === "" ? fallback : Number(raw);
  if (!Number.isSafeInteger(value) || value < 1) {
    throw new Error(`${label} must be a positive safe integer`);
  }
  if (value > maximum) {
    throw new Error(`${label} must be no greater than ${maximum}`);
  }
  return value;
}

function requiredRemoteRpcUrl(raw: string | undefined): string {
  if (!raw) {
    throw new Error(
      "Hosted Surfpool requires FORK_SDK_REMOTE_RPC_URL or " +
        "SURFPOOL_DATASOURCE_RPC_URL",
    );
  }
  const url = new URL(raw);
  if (url.protocol !== "http:" && url.protocol !== "https:") {
    throw new Error("Surfpool remote datasource must use http:// or https://");
  }
  return url.toString();
}

export function parsePayerFundingLamports(
  raw = String(DEFAULT_PAYER_FUNDING_LAMPORTS),
): number {
  if (!/^\d+$/.test(raw)) {
    throw new Error("SURFPOOL_PAYER_FUNDING_LAMPORTS must be a decimal integer");
  }
  const lamports = Number(raw);
  if (!Number.isSafeInteger(lamports) || lamports < 1) {
    throw new Error(
      "SURFPOOL_PAYER_FUNDING_LAMPORTS must be a positive safe integer",
    );
  }
  return lamports;
}

export function decodeExplicitPayer(env: NodeJS.ProcessEnv = process.env): Keypair {
  const raw = env.FORK_LAB_PAYER_KEYPAIR_JSON ?? env.FORK_LAB_PAYER_KEYPAIR_BASE64;
  if (!raw) {
    throw new Error(
      "Hosted Surfpool requires the same explicit FORK_LAB_PAYER_KEYPAIR_JSON " +
        "or FORK_LAB_PAYER_KEYPAIR_BASE64 on the RPC and API services",
    );
  }
  const trimmed = raw.trim();
  const json = trimmed.startsWith("[")
    ? trimmed
    : Buffer.from(trimmed, "base64").toString("utf8");
  const parsed = JSON.parse(json) as unknown;
  if (
    !Array.isArray(parsed) ||
    parsed.some(
      (byte) => !Number.isInteger(byte) || byte < 0 || byte > 255,
    )
  ) {
    throw new Error("Surfpool payer must decode to a JSON byte array");
  }
  return Keypair.fromSecretKey(Uint8Array.from(parsed as number[]));
}

export function controllerReadinessConfigFromEnv(
  env: NodeJS.ProcessEnv = process.env,
): ControllerReadinessConfig {
  const port = env.PORT === undefined || env.PORT === ""
    ? 0
    : requiredPort("PORT", env.PORT);
  const rpcPort = requiredPort(
    "SURFPOOL_RPC_PORT",
    env.SURFPOOL_RPC_PORT ?? "8899",
  );
  const wsPort = requiredPort(
    "SURFPOOL_WS_PORT",
    env.SURFPOOL_WS_PORT ?? "8900",
  );
  if (port !== 0 && (port === rpcPort || port === wsPort)) {
    throw new Error("PORT must differ from Surfpool RPC and WebSocket ports");
  }
  const host = env.SURFPOOL_HEALTH_HOST ?? "0.0.0.0";
  if (!host.trim()) throw new Error("SURFPOOL_HEALTH_HOST must not be empty");
  return {
    host,
    port,
    requestTimeoutMs: boundedPositiveInteger(
      "SURFPOOL_HEALTH_REQUEST_TIMEOUT_MS",
      env.SURFPOOL_HEALTH_REQUEST_TIMEOUT_MS,
      DEFAULT_HEALTH_REQUEST_TIMEOUT_MS,
      120_000,
    ),
    maxConnections: boundedPositiveInteger(
      "SURFPOOL_HEALTH_MAX_CONNECTIONS",
      env.SURFPOOL_HEALTH_MAX_CONNECTIONS,
      DEFAULT_HEALTH_MAX_CONNECTIONS,
      256,
    ),
  };
}

export function hostedControllerConfigFromEnv(
  env: NodeJS.ProcessEnv = process.env,
): HostedSurfpoolControllerConfig {
  const rpcPort = requiredPort("SURFPOOL_RPC_PORT", env.SURFPOOL_RPC_PORT ?? "8899");
  const wsPort = requiredPort("SURFPOOL_WS_PORT", env.SURFPOOL_WS_PORT ?? "8900");
  if (rpcPort === wsPort) throw new Error("Surfpool RPC and WebSocket ports must differ");

  return {
    remoteRpcUrl: requiredRemoteRpcUrl(
      env.FORK_SDK_REMOTE_RPC_URL ?? env.SURFPOOL_DATASOURCE_RPC_URL,
    ),
    payer: decodeExplicitPayer(env),
    payerFundingLamports: parsePayerFundingLamports(
      env.SURFPOOL_PAYER_FUNDING_LAMPORTS,
    ),
    listenHost: env.SURFPOOL_HOST ?? "0.0.0.0",
    rpcPort,
    wsPort,
    duskProgramId: env.DUSK_PROGRAM_ID ?? DEFAULT_DUSK_PROGRAM_ID,
    leverageDelegateProgramId:
      env.DUSK_LEVERAGE_DELEGATE_PROGRAM_ID ??
      DEFAULT_LEVERAGE_DELEGATE_PROGRAM_ID,
    duskSoPath: requiredPath("Dusk program binary", [
      env.DUSK_PROGRAM_SO_PATH ?? "target/deploy/dusk.so",
    ]),
    duskIdlPath: requiredPath("Dusk IDL", [
      env.DUSK_IDL_PATH ?? "target/idl/dusk.json",
      "packages/dusk-sdk/src/idl_v2.json",
    ]),
    leverageDelegateSoPath: requiredPath("leverage_delegate program binary", [
      env.DUSK_LEVERAGE_DELEGATE_SO_PATH ??
        "target/deploy/leverage_delegate.so",
    ]),
    leverageDelegateIdlPath: requiredPath("leverage_delegate IDL", [
      env.DUSK_LEVERAGE_DELEGATE_IDL_PATH ??
        "target/idl/leverage_delegate.json",
      "scripts/v2-fork-lab/idl/leverage_delegate.json",
    ]),
    blockProductionMode: env.SURFPOOL_BLOCK_PRODUCTION_MODE ?? "transaction",
    startupProbeTimeoutMs: boundedPositiveInteger(
      "SURFPOOL_STARTUP_PROBE_TIMEOUT_MS",
      env.SURFPOOL_STARTUP_PROBE_TIMEOUT_MS,
      DEFAULT_STARTUP_PROBE_TIMEOUT_MS,
      120_000,
    ),
    startupSettlementTimeoutMs: boundedPositiveInteger(
      "SURFPOOL_STARTUP_SETTLEMENT_TIMEOUT_MS",
      env.SURFPOOL_STARTUP_SETTLEMENT_TIMEOUT_MS,
      DEFAULT_STARTUP_SETTLEMENT_TIMEOUT_MS,
      300_000,
    ),
    relayHeartbeatIntervalMs: boundedPositiveInteger(
      "SURFPOOL_RELAY_HEARTBEAT_INTERVAL_MS",
      env.SURFPOOL_RELAY_HEARTBEAT_INTERVAL_MS,
      DEFAULT_RELAY_HEARTBEAT_INTERVAL_MS,
      60_000,
    ),
  };
}

export function relayWorkerConfigFromEnv(
  controller: Pick<
    HostedSurfpoolControllerConfig,
    "listenHost" | "rpcPort" | "wsPort"
  >,
  readiness = controllerReadinessConfigFromEnv(),
  env: NodeJS.ProcessEnv = process.env,
): SurfpoolRelayWorkerConfig {
  const heartbeatIntervalMs = boundedPositiveInteger(
    "SURFPOOL_RELAY_HEARTBEAT_INTERVAL_MS",
    env.SURFPOOL_RELAY_HEARTBEAT_INTERVAL_MS,
    DEFAULT_RELAY_HEARTBEAT_INTERVAL_MS,
    60_000,
  );
  const heartbeatStaleMs = boundedPositiveInteger(
    "SURFPOOL_RELAY_HEARTBEAT_STALE_MS",
    env.SURFPOOL_RELAY_HEARTBEAT_STALE_MS,
    DEFAULT_RELAY_HEARTBEAT_STALE_MS,
    300_000,
  );
  if (heartbeatStaleMs < heartbeatIntervalMs * 2) {
    throw new Error(
      "SURFPOOL_RELAY_HEARTBEAT_STALE_MS must be at least twice " +
        "SURFPOOL_RELAY_HEARTBEAT_INTERVAL_MS",
    );
  }
  return {
    listenHost: controller.listenHost,
    rpcPort: controller.rpcPort,
    wsPort: controller.wsPort,
    healthHost: readiness.host,
    healthPort: readiness.port,
    healthRequestTimeoutMs: readiness.requestTimeoutMs,
    healthMaxConnections: readiness.maxConnections,
    heartbeatStaleMs,
    upstreamConnectTimeoutMs: boundedPositiveInteger(
      "SURFPOOL_RELAY_CONNECT_TIMEOUT_MS",
      env.SURFPOOL_RELAY_CONNECT_TIMEOUT_MS,
      DEFAULT_RELAY_CONNECT_TIMEOUT_MS,
      120_000,
    ),
    maxConnections: boundedPositiveInteger(
      "SURFPOOL_RELAY_MAX_CONNECTIONS",
      env.SURFPOOL_RELAY_MAX_CONNECTIONS,
      DEFAULT_RELAY_MAX_CONNECTIONS,
      4_096,
    ),
    targetProbeIntervalMs: boundedPositiveInteger(
      "SURFPOOL_RELAY_TARGET_PROBE_INTERVAL_MS",
      env.SURFPOOL_RELAY_TARGET_PROBE_INTERVAL_MS,
      DEFAULT_RELAY_TARGET_PROBE_INTERVAL_MS,
      300_000,
    ),
    targetProbeTimeoutMs: boundedPositiveInteger(
      "SURFPOOL_RELAY_TARGET_PROBE_TIMEOUT_MS",
      env.SURFPOOL_RELAY_TARGET_PROBE_TIMEOUT_MS,
      DEFAULT_RELAY_TARGET_PROBE_TIMEOUT_MS,
      120_000,
    ),
    targetProbeFailureThreshold: boundedPositiveInteger(
      "SURFPOOL_RELAY_TARGET_PROBE_FAILURE_THRESHOLD",
      env.SURFPOOL_RELAY_TARGET_PROBE_FAILURE_THRESHOLD,
      DEFAULT_RELAY_TARGET_PROBE_FAILURE_THRESHOLD,
      20,
    ),
  };
}

function sendReadinessJson(
  response: http.ServerResponse,
  statusCode: number,
  payload: Record<string, unknown>,
  headOnly: boolean,
): void {
  const body = Buffer.from(JSON.stringify(payload));
  response.writeHead(statusCode, {
    "cache-control": "no-store",
    connection: "close",
    "content-length": String(body.byteLength),
    "content-type": "application/json; charset=utf-8",
    "x-content-type-options": "nosniff",
  });
  response.end(headOnly ? undefined : body);
}

/**
 * Starts the small HTTP listener Railway probes while Surfpool is booting.
 * It contains no deployment secrets and cannot report ready until the caller
 * explicitly marks the fully probed controller handle ready.
 */
export async function startControllerReadinessServer(
  config: ControllerReadinessConfig = controllerReadinessConfigFromEnv(),
): Promise<ControllerReadinessHandle> {
  const startedAt = new Date().toISOString();
  let status: ControllerReadinessSnapshot["status"] = "starting";
  let readyAt: string | null = null;
  let programCount = 0;
  let closing: Promise<void> | undefined;
  let rejectFailure: (error: Error) => void = () => undefined;
  const failure = new Promise<never>((_resolvePromise, reject) => {
    rejectFailure = reject;
  });
  void failure.catch(() => undefined);

  const snapshot = (): ControllerReadinessSnapshot => ({
    status,
    startedAt,
    readyAt,
    programCount,
  });
  const server = http.createServer((request, response) => {
    const headOnly = request.method === "HEAD";
    let pathname: string;
    try {
      pathname = request.url
        ? new URL(request.url, "http://readiness.invalid").pathname
        : "/";
    } catch {
      sendReadinessJson(response, 400, { ok: false, status: "bad_request" }, headOnly);
      return;
    }
    if (pathname !== "/health") {
      sendReadinessJson(response, 404, { ok: false, status: "not_found" }, headOnly);
      return;
    }
    if (request.method !== "GET" && request.method !== "HEAD") {
      response.setHeader("allow", "GET, HEAD");
      sendReadinessJson(
        response,
        405,
        { ok: false, status: "method_not_allowed" },
        headOnly,
      );
      return;
    }
    const current = snapshot();
    sendReadinessJson(
      response,
      current.status === "ready" ? 200 : 503,
      {
        ok: current.status === "ready",
        status: current.status,
        startedAt: current.startedAt,
        readyAt: current.readyAt,
        programCount: current.programCount,
      },
      headOnly,
    );
  });
  server.requestTimeout = config.requestTimeoutMs;
  server.headersTimeout = config.requestTimeoutMs;
  server.keepAliveTimeout = 1_000;
  server.maxRequestsPerSocket = 1;
  server.maxConnections = config.maxConnections;
  server.on("clientError", (_error, socket) => {
    if (socket.writable) {
      socket.end("HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n");
    } else {
      socket.destroy();
    }
  });

  const port = await new Promise<number>((resolvePromise, reject) => {
    const onError = (error: Error) => {
      server.off("listening", onListening);
      reject(error);
    };
    const onListening = () => {
      server.off("error", onError);
      const address = server.address();
      if (!address || typeof address === "string") {
        reject(new Error("Controller readiness server did not expose an IP socket"));
        return;
      }
      resolvePromise(address.port);
    };
    server.once("error", onError);
    server.once("listening", onListening);
    server.listen({ host: config.host, port: config.port });
  });
  server.on("error", (error) => {
    if (closing) return;
    status = "failed";
    rejectFailure(new Error(`Controller readiness server failed: ${error.message}`));
  });

  return {
    host: config.host,
    port,
    failure,
    snapshot,
    markReady(count: number) {
      if (!Number.isSafeInteger(count) || count < 1) {
        throw new Error("Readiness program count must be a positive safe integer");
      }
      if (status !== "starting") {
        throw new Error(`Controller cannot become ready from ${status}`);
      }
      programCount = count;
      readyAt = new Date().toISOString();
      status = "ready";
    },
    markFailed() {
      if (status !== "stopping") status = "failed";
    },
    markStopping() {
      if (status !== "failed") status = "stopping";
    },
    close() {
      if (closing) return closing;
      closing = new Promise<void>((resolvePromise) => {
        if (!server.listening) {
          resolvePromise();
          return;
        }
        server.close(() => resolvePromise());
        server.closeAllConnections();
      });
      return closing;
    },
  };
}

export function deployExactProgram(
  surfnet: ExactDeploySurface,
  params: { label: string; programId: string; soPath: string; idlPath: string },
): string {
  const deployedProgramId = surfnet.deploy({
    programId: params.programId,
    soPath: params.soPath,
    idlPath: params.idlPath,
  });
  if (deployedProgramId !== params.programId) {
    throw new Error(
      `${params.label} deployed at ${deployedProgramId}; expected ${params.programId}`,
    );
  }
  return deployedProgramId;
}

export function rewriteUpgradeableProgramDataAuthority(
  data: Buffer,
  authority: PublicKey,
): Buffer {
  if (
    data.length <= PROGRAM_DATA_METADATA_BYTES ||
    data.readUInt32LE(0) !== PROGRAM_DATA_TAG
  ) {
    throw new Error("ProgramData is missing or malformed");
  }
  const authorityOption = data[12];
  if (authorityOption !== 0 && authorityOption !== 1) {
    throw new Error("ProgramData has a malformed upgrade authority option");
  }
  const rewritten = Buffer.from(data);
  rewritten[12] = 1;
  authority.toBuffer().copy(rewritten, 13);
  return rewritten;
}

export async function alignExactUpgradeableProgramAuthority(
  surfnet: ProgramDataWriteSurface,
  params: {
    rpcUrl: string;
    label: string;
    programId: string;
    soPath: string;
    authority: PublicKey;
  },
): Promise<{
  programDataAddress: string;
  authority: string;
  changed: boolean;
}> {
  const connection = new Connection(params.rpcUrl, "confirmed");
  const programId = new PublicKey(params.programId);
  const program = await connection.getAccountInfo(programId, "confirmed");
  if (!program?.executable || !program.owner.equals(BPF_LOADER_UPGRADEABLE_ID)) {
    throw new Error(`${params.label} is missing at exact address ${params.programId}`);
  }
  if (program.data.length < 36 || program.data.readUInt32LE(0) !== PROGRAM_TAG) {
    throw new Error(`${params.label} has malformed upgradeable Program state`);
  }
  const programDataAddress = new PublicKey(program.data.subarray(4, 36));
  const [expectedProgramDataAddress] = PublicKey.findProgramAddressSync(
    [programId.toBuffer()],
    BPF_LOADER_UPGRADEABLE_ID,
  );
  if (!programDataAddress.equals(expectedProgramDataAddress)) {
    throw new Error(`${params.label} points to unexpected ProgramData`);
  }
  const before = await connection.getAccountInfo(programDataAddress, "confirmed");
  if (
    !before ||
    before.executable ||
    !before.owner.equals(BPF_LOADER_UPGRADEABLE_ID) ||
    before.data.length <= PROGRAM_DATA_METADATA_BYTES ||
    before.data.readUInt32LE(0) !== PROGRAM_DATA_TAG
  ) {
    throw new Error(`${params.label} ProgramData is missing or malformed`);
  }
  const expectedBinary = readFileSync(params.soPath);
  if (!before.data.subarray(PROGRAM_DATA_METADATA_BYTES).equals(expectedBinary)) {
    throw new Error(
      `${params.label} ProgramData bytes do not match ${params.soPath}`,
    );
  }

  const rewritten = rewriteUpgradeableProgramDataAuthority(
    before.data,
    params.authority,
  );
  const alreadyAligned = before.data.equals(rewritten);
  if (!alreadyAligned) {
    surfnet.setAccount(
      programDataAddress.toBase58(),
      before.lamports,
      rewritten,
      BPF_LOADER_UPGRADEABLE_ID.toBase58(),
    );
  }

  const after = await connection.getAccountInfo(programDataAddress, "confirmed");
  if (
    !after ||
    after.executable ||
    !after.owner.equals(BPF_LOADER_UPGRADEABLE_ID) ||
    after.lamports !== before.lamports ||
    !after.data.equals(rewritten)
  ) {
    throw new Error(
      `${params.label} ProgramData changed outside its upgrade-authority header`,
    );
  }
  if (!after.data.subarray(PROGRAM_DATA_METADATA_BYTES).equals(expectedBinary)) {
    throw new Error(
      `${params.label} ProgramData binary changed while aligning upgrade authority`,
    );
  }
  return {
    programDataAddress: programDataAddress.toBase58(),
    authority: params.authority.toBase58(),
    changed: !alreadyAligned,
  };
}

export async function probeExactUpgradeableProgram(params: {
  rpcUrl: string;
  label: string;
  programId: string;
  soPath: string;
  expectedUpgradeAuthority?: string;
}): Promise<{
  programId: string;
  programDataAddress: string;
  binarySha256: string;
}> {
  const connection = new Connection(params.rpcUrl, "confirmed");
  const programId = new PublicKey(params.programId);
  const program = await connection.getAccountInfo(programId, "confirmed");
  if (!program?.executable || !program.owner.equals(BPF_LOADER_UPGRADEABLE_ID)) {
    throw new Error(`${params.label} is missing at exact address ${params.programId}`);
  }
  if (program.data.length < 36 || program.data.readUInt32LE(0) !== PROGRAM_TAG) {
    throw new Error(`${params.label} has malformed upgradeable Program state`);
  }
  const programDataAddress = new PublicKey(program.data.subarray(4, 36));
  const [expectedProgramDataAddress] = PublicKey.findProgramAddressSync(
    [programId.toBuffer()],
    BPF_LOADER_UPGRADEABLE_ID,
  );
  if (!programDataAddress.equals(expectedProgramDataAddress)) {
    throw new Error(`${params.label} points to unexpected ProgramData`);
  }
  const programData = await connection.getAccountInfo(
    programDataAddress,
    "confirmed",
  );
  if (
    !programData ||
    !programData.owner.equals(BPF_LOADER_UPGRADEABLE_ID) ||
    programData.data.length <= PROGRAM_DATA_METADATA_BYTES ||
    programData.data.readUInt32LE(0) !== PROGRAM_DATA_TAG
  ) {
    throw new Error(`${params.label} ProgramData is missing or malformed`);
  }
  const deployedBinary = programData.data.subarray(PROGRAM_DATA_METADATA_BYTES);
  const expectedBinary = readFileSync(params.soPath);
  if (!deployedBinary.equals(expectedBinary)) {
    throw new Error(
      `${params.label} ProgramData bytes do not match ${params.soPath}`,
    );
  }
  const authorityOption = programData.data[12];
  if (authorityOption !== 0 && authorityOption !== 1) {
    throw new Error(`${params.label} ProgramData has malformed upgrade authority`);
  }
  const upgradeAuthority = authorityOption === 1
    ? new PublicKey(programData.data.subarray(13, 45)).toBase58()
    : null;
  if (
    params.expectedUpgradeAuthority !== undefined &&
    upgradeAuthority !== params.expectedUpgradeAuthority
  ) {
    throw new Error(
      `${params.label} upgrade authority is ${upgradeAuthority ?? "immutable"}; ` +
        `expected ${params.expectedUpgradeAuthority}`,
    );
  }
  return {
    programId: params.programId,
    programDataAddress: programDataAddress.toBase58(),
    binarySha256: createHash("sha256").update(deployedBinary).digest("hex"),
  };
}

export async function probeStableWebSocket(
  wsUrl: string,
  timeoutMs = DEFAULT_STARTUP_PROBE_TIMEOUT_MS,
): Promise<void> {
  const boundedTimeoutMs = boundedPositiveInteger(
    "Surfpool stable WebSocket probe timeout",
    String(timeoutMs),
    DEFAULT_STARTUP_PROBE_TIMEOUT_MS,
    120_000,
  );
  await new Promise<void>((resolvePromise, reject) => {
    const socket = new WebSocket(wsUrl, {
      followRedirects: false,
      handshakeTimeout: boundedTimeoutMs,
      perMessageDeflate: false,
    });
    let settled = false;
    const finish = (error?: Error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      socket.removeAllListeners();
      // Terminating a still-CONNECTING socket aborts the handshake and emits
      // "error" on a later tick; keep a swallow listener attached or that
      // late emit becomes an uncaught exception that kills the controller.
      socket.on("error", () => undefined);
      if (socket.readyState !== WebSocket.CLOSED) socket.terminate();
      if (error) reject(error);
      else resolvePromise();
    };
    const timer = setTimeout(() => {
      finish(new Error(
        `Surfpool stable WebSocket probe timed out after ${boundedTimeoutMs}ms`,
      ));
    }, boundedTimeoutMs);
    socket.once("open", () => finish());
    socket.once("error", (error) => finish(new Error(
      `Surfpool stable WebSocket probe failed: ${error.message}`,
    )));
    socket.once("unexpected-response", (_request, response) => {
      response.resume();
      finish(new Error(
        `Surfpool stable WebSocket probe received HTTP ${response.statusCode}`,
      ));
    });
  });
}

export async function startHostedSurfpoolController(
  config = hostedControllerConfigFromEnv(),
  options: {
    signal?: AbortSignal;
    relayWorker?: SurfpoolRelayWorkerHandle;
  } = {},
): Promise<HostedSurfpoolControllerHandle> {
  const startupPhases = createStartupPhaseTracker();
  let rejectAbort: (error: Error) => void = () => undefined;
  const abortFailure = new Promise<never>((_resolvePromise, reject) => {
    rejectAbort = reject;
  });
  void abortFailure.catch(() => undefined);
  const onAbort = () => rejectAbort(new Error("Surfpool controller startup was aborted"));
  options.signal?.addEventListener("abort", onAbort, { once: true });
  const removeAbortListener = () => {
    options.signal?.removeEventListener("abort", onAbort);
  };
  const requireStartupActive = () => {
    if (options.signal?.aborted) {
      throw new Error("Surfpool controller startup was aborted");
    }
  };
  if (options.signal?.aborted) {
    removeAbortListener();
    throw new Error("Surfpool controller startup was aborted");
  }
  let surfpoolModule: typeof import("@solana/surfpool");
  try {
    surfpoolModule = await Promise.race([
      startupPhases.track(import("@solana/surfpool")),
      abortFailure,
    ]);
  } catch (error) {
    const settled = await startupPhases.settle(
      config.startupSettlementTimeoutMs ?? DEFAULT_STARTUP_SETTLEMENT_TIMEOUT_MS,
    );
    removeAbortListener();
    if (!settled) {
      throw new StartupPhaseSettlementError(
        config.startupSettlementTimeoutMs ?? DEFAULT_STARTUP_SETTLEMENT_TIMEOUT_MS,
        error,
      );
    }
    throw error;
  }
  const { Surfnet } = surfpoolModule;
  requireStartupActive();
  const surfnet = Surfnet.startWithConfig({
    offline: false,
    remoteRpcUrl: config.remoteRpcUrl,
    blockProductionMode: config.blockProductionMode,
    payerSecretKey: Array.from(config.payer.secretKey),
  });
  let eventDrain: SurfnetEventDrainHandle | undefined;
  let shutdownForkRuntime: (() => void) | undefined;
  let relayWorker = options.relayWorker;
  let relayHeartbeatTimer: ReturnType<typeof setInterval> | undefined;
  let stopping: Promise<void> | undefined;

  const stop = () => {
    if (stopping) return stopping;
    stopping = (async () => {
      if (relayHeartbeatTimer) clearInterval(relayHeartbeatTimer);
      relayHeartbeatTimer = undefined;
      relayWorker?.markStopping();
      await relayWorker?.close();
      try {
        shutdownForkRuntime?.();
      } finally {
        removeAbortListener();
        eventDrain?.stop();
        surfnet.stop();
      }
    })();
    return stopping;
  };

  try {
    const activeEventDrain = startSurfnetEventDrain(surfnet);
    eventDrain = activeEventDrain;
    if (!relayWorker) {
      relayWorker = await startSurfpoolRelayWorkerProcess(
        relayWorkerConfigFromEnv(
          config,
          {
            host: "127.0.0.1",
            port: 0,
            requestTimeoutMs: DEFAULT_HEALTH_REQUEST_TIMEOUT_MS,
            maxConnections: DEFAULT_HEALTH_MAX_CONNECTIONS,
          },
        ),
      );
    }
    const activeRelayWorker = relayWorker;
    const publishRelayHeartbeat = () => {
      const metrics = activeEventDrain.snapshot();
      activeRelayWorker.heartbeat({
        atMs: Date.now(),
        drainCalls: metrics.drainCalls,
        drainedEvents: metrics.drainedEvents,
        failedDrains: metrics.failedDrains,
        currentIntervalMs: metrics.currentIntervalMs,
        lastTickLagMs: metrics.lastTickLagMs,
        maxTickLagMs: metrics.maxTickLagMs,
      });
    };
    publishRelayHeartbeat();
    relayHeartbeatTimer = setInterval(
      publishRelayHeartbeat,
      config.relayHeartbeatIntervalMs ?? DEFAULT_RELAY_HEARTBEAT_INTERVAL_MS,
    );
    relayHeartbeatTimer.unref();
    const controllerFailure = Promise.race([
      activeRelayWorker.failure,
      activeEventDrain.failure,
    ]);
    const whileControllerHealthy = <T>(
      startOperation: () => PromiseLike<T>,
    ): Promise<T> => {
      requireStartupActive();
      const tracked = startupPhases.track(startOperation());
      return Promise.race([tracked, controllerFailure, abortFailure]);
    };

    if (surfnet.payer !== config.payer.publicKey.toBase58()) {
      throw new Error("Surfnet did not start with the configured shared payer");
    }
    requireStartupActive();
    surfnet.fundSol(surfnet.payer, config.payerFundingLamports);
    requireStartupActive();
    activeEventDrain.drainNow();
    deployExactProgram(surfnet, {
      label: "dusk",
      programId: config.duskProgramId,
      soPath: config.duskSoPath,
      idlPath: config.duskIdlPath,
    });
    requireStartupActive();
    activeEventDrain.drainNow();
    deployExactProgram(surfnet, {
      label: "leverage_delegate",
      programId: config.leverageDelegateProgramId,
      soPath: config.leverageDelegateSoPath,
      idlPath: config.leverageDelegateIdlPath,
    });
    requireStartupActive();
    activeEventDrain.drainNow();
    await whileControllerHealthy(
      () => alignExactUpgradeableProgramAuthority(surfnet, {
        rpcUrl: surfnet.rpcUrl,
        label: "dusk",
        programId: config.duskProgramId,
        soPath: config.duskSoPath,
        authority: config.payer.publicKey,
      }),
    );
    await whileControllerHealthy(
      () => alignExactUpgradeableProgramAuthority(surfnet, {
        rpcUrl: surfnet.rpcUrl,
        label: "leverage_delegate",
        programId: config.leverageDelegateProgramId,
        soPath: config.leverageDelegateSoPath,
        authority: config.payer.publicKey,
      }),
    );

    process.env.SURFPOOL_RPC_URL = surfnet.rpcUrl;
    process.env.PUBLIC_SURFPOOL_RPC_URL = surfnet.rpcUrl;
    process.env.FORK_LAB_PAYER_KEYPAIR_JSON = JSON.stringify(
      Array.from(config.payer.secretKey),
    );
    process.env.DUSK_REQUIRE_EXPLICIT_FORK_SIGNER = "true";
    process.env.DUSK_REQUIRE_EXTERNAL_FORK_MARKER = "true";
    delete process.env.DUSK_REQUIRE_PREBOOTSTRAPPED_MARKETS;

    const { seedForkGeneration } = (await whileControllerHealthy(
      () => import("./seed_fork_generation.mjs"),
    )) as { seedForkGeneration(): Promise<string> };
    await whileControllerHealthy(() => seedForkGeneration());
    const forkApi = await whileControllerHealthy(() => import("./api_core.js"));
    shutdownForkRuntime = forkApi.shutdownForkRuntime;
    const markets = await whileControllerHealthy(() => forkApi.bootstrapForkMarkets());
    if (markets.length === 0) throw new Error("Surfpool controller bootstrapped no markets");
    shutdownForkRuntime();
    activeEventDrain.drainNow();

    const stable = await whileControllerHealthy(() => activeRelayWorker.activate({
        rpcUrl: surfnet.rpcUrl,
        wsUrl: surfnet.wsUrl,
    }));
    const stableRpcUrl = stable.rpcUrl;
    const stableWsUrl = stable.wsUrl;
    requireStartupActive();
    const duskProgramProbe = startupPhases.track(
      probeExactUpgradeableProgram({
        rpcUrl: stableRpcUrl,
        label: "dusk",
        programId: config.duskProgramId,
        soPath: config.duskSoPath,
        expectedUpgradeAuthority: config.payer.publicKey.toBase58(),
      }),
    );
    const leverageDelegateProgramProbe = startupPhases.track(
      probeExactUpgradeableProgram({
        rpcUrl: stableRpcUrl,
        label: "leverage_delegate",
        programId: config.leverageDelegateProgramId,
        soPath: config.leverageDelegateSoPath,
        expectedUpgradeAuthority: config.payer.publicKey.toBase58(),
      }),
    );
    const programs = await Promise.race([
      Promise.all([duskProgramProbe, leverageDelegateProgramProbe]),
      controllerFailure,
      abortFailure,
    ]);
    await whileControllerHealthy(() => probeStableWebSocket(
        stableWsUrl,
        config.startupProbeTimeoutMs ?? DEFAULT_STARTUP_PROBE_TIMEOUT_MS,
      ));
    activeRelayWorker.markReady(programs.length);
    publishRelayHeartbeat();
    removeAbortListener();
    return {
      payer: surfnet.payer,
      payerFundingLamports: config.payerFundingLamports,
      rpcUrl: stableRpcUrl,
      wsUrl: stableWsUrl,
      dynamicRpcUrl: surfnet.rpcUrl,
      dynamicWsUrl: surfnet.wsUrl,
      programs,
      failure: controllerFailure,
      stop,
    };
  } catch (error) {
    relayWorker?.markFailed();
    const settlementTimeoutMs =
      config.startupSettlementTimeoutMs ?? DEFAULT_STARTUP_SETTLEMENT_TIMEOUT_MS;
    const settled = await startupPhases.settle(settlementTimeoutMs);
    if (!settled) {
      if (relayHeartbeatTimer) clearInterval(relayHeartbeatTimer);
      relayHeartbeatTimer = undefined;
      await relayWorker?.close();
      removeAbortListener();
      throw new StartupPhaseSettlementError(settlementTimeoutMs, error);
    }
    await stop();
    throw error;
  }
}

async function main() {
  const controllerConfig = hostedControllerConfigFromEnv();
  const relayWorker = await startSurfpoolRelayWorkerProcess(
    relayWorkerConfigFromEnv(controllerConfig),
  );
  console.log(JSON.stringify({
    event: "dusk_surfpool_controller_health_listening",
    host: relayWorker.healthHost,
    port: relayWorker.healthPort,
    path: "/health",
    owner: "relay-worker",
  }));
  let controller: HostedSurfpoolControllerHandle | undefined;
  let requestedSignal: NodeJS.Signals | undefined;
  const startupAbort = new AbortController();
  let resolveSignal: (signal: NodeJS.Signals) => void = () => undefined;
  const signal = new Promise<NodeJS.Signals>((resolvePromise) => {
    resolveSignal = resolvePromise;
  });
  const requestShutdown = (value: NodeJS.Signals) => {
    if (requestedSignal) return;
    requestedSignal = value;
    relayWorker.markStopping();
    startupAbort.abort(new Error(`Surfpool controller received ${value}`));
    resolveSignal(value);
  };
  const onSigint = () => requestShutdown("SIGINT");
  const onSigterm = () => requestShutdown("SIGTERM");
  process.once("SIGINT", onSigint);
  process.once("SIGTERM", onSigterm);
  void relayWorker.failure.catch((error) => {
    startupAbort.abort(error);
  });
  try {
    controller = await startHostedSurfpoolController(
      controllerConfig,
      { signal: startupAbort.signal, relayWorker },
    );
    if (requestedSignal) return;
    console.log(
      JSON.stringify({
        event: "dusk_surfpool_controller_ready",
        payer: controller.payer,
        payerFundingLamports: controller.payerFundingLamports,
        rpcUrl: controller.rpcUrl,
        wsUrl: controller.wsUrl,
        healthPort: relayWorker.healthPort,
        programs: controller.programs,
      }),
    );
    await Promise.race([signal, controller.failure, relayWorker.failure]);
  } catch (error) {
    if (error instanceof StartupPhaseSettlementError) {
      relayWorker.markFailed();
      console.error(error);
      // An unresolved native startup phase makes in-process Surfnet teardown
      // unsafe. The settlement grace has elapsed, so terminate the container.
      process.exit(1);
    }
    if (requestedSignal) return;
    relayWorker.markFailed();
    throw error;
  } finally {
    process.off("SIGINT", onSigint);
    process.off("SIGTERM", onSigterm);
    if (controller) await controller.stop();
    else await relayWorker.close();
  }
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  main().catch((error) => {
    console.error(error);
    // startHostedSurfpoolController() has already awaited its cleanup path.
    // Exit explicitly so a native Surfnet handle cannot leave a failed hosted
    // startup alive without RPC/WS listeners for Railway to restart.
    process.exit(1);
  });
}
