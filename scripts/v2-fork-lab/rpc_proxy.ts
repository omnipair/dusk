import { createHash, timingSafeEqual } from "node:crypto";
import http from "node:http";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import WebSocket, {
  WebSocketServer,
  type RawData,
} from "ws";

const BLOCKED_METHOD_PREFIXES = ["surfnet_"];
const BLOCKED_METHODS = new Set(["requestAirdrop"]);
const DEFAULT_MAX_BATCH_ITEMS = 100;
const DEFAULT_MAX_PAYLOAD_BYTES = 1_048_576;
const DEFAULT_MAX_BUFFERED_BYTES = 2_097_152;
const DEFAULT_MAX_PENDING_MESSAGES = 128;
const DEFAULT_MAX_FRAGMENTS = 128;
const DEFAULT_MAX_BUFFERED_CHUNKS = 256;
const DEFAULT_MAX_CLIENTS = 64;
const DEFAULT_WS_CLIENT_MAX_OPERATIONS_PER_WINDOW = 256;
const DEFAULT_WS_GLOBAL_MAX_OPERATIONS_PER_WINDOW = 4_096;
const DEFAULT_WS_OPERATION_WINDOW_MS = 1_000;
const DEFAULT_HANDSHAKE_TIMEOUT_MS = 10_000;
const DEFAULT_HTTP_MAX_BODY_BYTES = 1_048_576;
const DEFAULT_HTTP_MAX_RESPONSE_BYTES = 4_194_304;
const DEFAULT_HTTP_MAX_IN_FLIGHT_REQUESTS = 32;
const DEFAULT_HTTP_TIMEOUT_MS = 15_000;
const DEFAULT_HEALTH_PROBE_TIMEOUT_MS = 5_000;
const DEFAULT_HEALTH_CACHE_MS = 5_000;
const HEALTH_MAX_RESPONSE_BYTES = 65_536;

class HttpBodyTooLargeError extends Error {}
class HttpResponseTooLargeError extends Error {}

export interface RpcProxyConfig {
  port: number;
  targetRpcUrl: string;
  targetWsUrl: string;
  adminToken: string;
  corsOrigin: string;
  extraBlockedMethods: ReadonlySet<string>;
  maxBatchItems: number;
  wsMaxPayloadBytes: number;
  wsMaxBufferedBytes: number;
  wsMaxPendingMessages: number;
  wsMaxFragments: number;
  wsMaxBufferedChunks: number;
  wsMaxClients: number;
  wsClientMaxOperationsPerWindow: number;
  wsGlobalMaxOperationsPerWindow: number;
  wsOperationWindowMs: number;
  wsHandshakeTimeoutMs: number;
  httpMaxBodyBytes: number;
  httpMaxResponseBytes: number;
  httpMaxInFlightRequests: number;
  httpRequestTimeoutMs: number;
  httpUpstreamTimeoutMs: number;
  healthProbeTimeoutMs: number;
  healthCacheMs: number;
}

export interface RpcProxyRuntime {
  config: RpcProxyConfig;
  server: http.Server;
  webSocketServer: WebSocketServer;
  close(): Promise<void>;
}

function positiveInteger(
  value: string | undefined,
  fallback: number,
  name: string,
  maximum = Number.MAX_SAFE_INTEGER,
): number {
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

function requireProtocol(value: string, protocols: readonly string[], name: string): string {
  const parsed = new URL(value);
  if (!protocols.includes(parsed.protocol)) {
    throw new Error(`${name} must use ${protocols.join(" or ")}`);
  }
  return value;
}

export function rpcProxyConfigFromEnv(
  env: NodeJS.ProcessEnv = process.env,
): RpcProxyConfig {
  const adminToken = env.FORK_ADMIN_TOKEN ?? "";
  if (
    env.FORK_REQUIRE_ADMIN_TOKEN === "true" &&
    adminToken.trim().length === 0
  ) {
    throw new Error(
      "FORK_REQUIRE_ADMIN_TOKEN=true requires a nonblank FORK_ADMIN_TOKEN",
    );
  }
  return {
    port: positiveInteger(env.PORT ?? env.FORK_RPC_PROXY_PORT, 8898, "PORT", 65_535),
    targetRpcUrl: requireProtocol(
      env.SURFPOOL_RPC_URL ?? "http://127.0.0.1:8899",
      ["http:", "https:"],
      "SURFPOOL_RPC_URL",
    ),
    targetWsUrl: requireProtocol(
      env.SURFPOOL_WS_URL ?? "ws://127.0.0.1:8900",
      ["ws:", "wss:"],
      "SURFPOOL_WS_URL",
    ),
    adminToken,
    corsOrigin: env.FORK_RPC_PROXY_CORS_ORIGIN ?? "*",
    extraBlockedMethods: new Set(
      (env.FORK_RPC_PROXY_BLOCKED_METHODS ?? "")
        .split(",")
        .map((method) => method.trim())
        .filter(Boolean),
    ),
    maxBatchItems: positiveInteger(
      env.FORK_RPC_PROXY_MAX_BATCH_ITEMS,
      DEFAULT_MAX_BATCH_ITEMS,
      "FORK_RPC_PROXY_MAX_BATCH_ITEMS",
      1_000,
    ),
    wsMaxPayloadBytes: positiveInteger(
      env.FORK_RPC_PROXY_WS_MAX_PAYLOAD_BYTES,
      DEFAULT_MAX_PAYLOAD_BYTES,
      "FORK_RPC_PROXY_WS_MAX_PAYLOAD_BYTES",
      16_777_216,
    ),
    wsMaxBufferedBytes: positiveInteger(
      env.FORK_RPC_PROXY_WS_MAX_BUFFERED_BYTES,
      DEFAULT_MAX_BUFFERED_BYTES,
      "FORK_RPC_PROXY_WS_MAX_BUFFERED_BYTES",
      67_108_864,
    ),
    wsMaxPendingMessages: positiveInteger(
      env.FORK_RPC_PROXY_WS_MAX_PENDING_MESSAGES,
      DEFAULT_MAX_PENDING_MESSAGES,
      "FORK_RPC_PROXY_WS_MAX_PENDING_MESSAGES",
      4_096,
    ),
    wsMaxFragments: positiveInteger(
      env.FORK_RPC_PROXY_WS_MAX_FRAGMENTS,
      DEFAULT_MAX_FRAGMENTS,
      "FORK_RPC_PROXY_WS_MAX_FRAGMENTS",
      1_024,
    ),
    wsMaxBufferedChunks: positiveInteger(
      env.FORK_RPC_PROXY_WS_MAX_BUFFERED_CHUNKS,
      DEFAULT_MAX_BUFFERED_CHUNKS,
      "FORK_RPC_PROXY_WS_MAX_BUFFERED_CHUNKS",
      4_096,
    ),
    wsMaxClients: positiveInteger(
      env.FORK_RPC_PROXY_WS_MAX_CLIENTS,
      DEFAULT_MAX_CLIENTS,
      "FORK_RPC_PROXY_WS_MAX_CLIENTS",
      512,
    ),
    wsClientMaxOperationsPerWindow: positiveInteger(
      env.FORK_RPC_PROXY_WS_CLIENT_MAX_OPERATIONS_PER_WINDOW,
      DEFAULT_WS_CLIENT_MAX_OPERATIONS_PER_WINDOW,
      "FORK_RPC_PROXY_WS_CLIENT_MAX_OPERATIONS_PER_WINDOW",
      1_000_000,
    ),
    wsGlobalMaxOperationsPerWindow: positiveInteger(
      env.FORK_RPC_PROXY_WS_GLOBAL_MAX_OPERATIONS_PER_WINDOW,
      DEFAULT_WS_GLOBAL_MAX_OPERATIONS_PER_WINDOW,
      "FORK_RPC_PROXY_WS_GLOBAL_MAX_OPERATIONS_PER_WINDOW",
      10_000_000,
    ),
    wsOperationWindowMs: positiveInteger(
      env.FORK_RPC_PROXY_WS_OPERATION_WINDOW_MS,
      DEFAULT_WS_OPERATION_WINDOW_MS,
      "FORK_RPC_PROXY_WS_OPERATION_WINDOW_MS",
      60_000,
    ),
    wsHandshakeTimeoutMs: positiveInteger(
      env.FORK_RPC_PROXY_WS_HANDSHAKE_TIMEOUT_MS,
      DEFAULT_HANDSHAKE_TIMEOUT_MS,
      "FORK_RPC_PROXY_WS_HANDSHAKE_TIMEOUT_MS",
      120_000,
    ),
    httpMaxBodyBytes: positiveInteger(
      env.FORK_RPC_PROXY_HTTP_MAX_BODY_BYTES,
      DEFAULT_HTTP_MAX_BODY_BYTES,
      "FORK_RPC_PROXY_HTTP_MAX_BODY_BYTES",
      16_777_216,
    ),
    httpMaxResponseBytes: positiveInteger(
      env.FORK_RPC_PROXY_HTTP_MAX_RESPONSE_BYTES,
      DEFAULT_HTTP_MAX_RESPONSE_BYTES,
      "FORK_RPC_PROXY_HTTP_MAX_RESPONSE_BYTES",
      67_108_864,
    ),
    httpMaxInFlightRequests: positiveInteger(
      env.FORK_RPC_PROXY_HTTP_MAX_IN_FLIGHT_REQUESTS,
      DEFAULT_HTTP_MAX_IN_FLIGHT_REQUESTS,
      "FORK_RPC_PROXY_HTTP_MAX_IN_FLIGHT_REQUESTS",
      256,
    ),
    httpRequestTimeoutMs: positiveInteger(
      env.FORK_RPC_PROXY_HTTP_REQUEST_TIMEOUT_MS,
      DEFAULT_HTTP_TIMEOUT_MS,
      "FORK_RPC_PROXY_HTTP_REQUEST_TIMEOUT_MS",
      120_000,
    ),
    httpUpstreamTimeoutMs: positiveInteger(
      env.FORK_RPC_PROXY_HTTP_UPSTREAM_TIMEOUT_MS,
      DEFAULT_HTTP_TIMEOUT_MS,
      "FORK_RPC_PROXY_HTTP_UPSTREAM_TIMEOUT_MS",
      120_000,
    ),
    healthProbeTimeoutMs: positiveInteger(
      env.FORK_RPC_PROXY_HEALTH_PROBE_TIMEOUT_MS,
      DEFAULT_HEALTH_PROBE_TIMEOUT_MS,
      "FORK_RPC_PROXY_HEALTH_PROBE_TIMEOUT_MS",
      60_000,
    ),
    healthCacheMs: positiveInteger(
      env.FORK_RPC_PROXY_HEALTH_CACHE_MS,
      DEFAULT_HEALTH_CACHE_MS,
      "FORK_RPC_PROXY_HEALTH_CACHE_MS",
      60_000,
    ),
  };
}

function corsHeaders(config: RpcProxyConfig) {
  return {
    "access-control-allow-origin": config.corsOrigin,
    "access-control-allow-methods": "POST, OPTIONS, GET",
    "access-control-allow-headers":
      "content-type, authorization, solana-client, x-fork-admin-token",
  };
}

function singleHeader(
  req: http.IncomingMessage,
  name: string,
): { valid: boolean; value?: string } {
  const distinct = req.headersDistinct?.[name];
  if (distinct !== undefined) {
    return distinct.length === 1
      ? { valid: true, value: distinct[0] }
      : { valid: false };
  }
  const value = req.headers[name];
  if (Array.isArray(value)) return { valid: false };
  return { valid: true, value };
}

function constantTimeTokenEquals(expected: string, received: string): boolean {
  const expectedDigest = createHash("sha256").update(expected, "utf8").digest();
  const receivedDigest = createHash("sha256").update(received, "utf8").digest();
  return timingSafeEqual(expectedDigest, receivedDigest);
}

function isAdmin(req: http.IncomingMessage, config: RpcProxyConfig): boolean {
  if (!config.adminToken) return false;
  const auth = singleHeader(req, "authorization");
  const admin = singleHeader(req, "x-fork-admin-token");
  if (!auth.valid || !admin.valid) return false;
  if (auth.value !== undefined && admin.value !== undefined) return false;

  let received: string | undefined;
  if (auth.value !== undefined) {
    if (!auth.value.startsWith("Bearer ")) return false;
    received = auth.value.slice("Bearer ".length);
  } else {
    received = admin.value;
  }
  return received !== undefined &&
    constantTimeTokenEquals(config.adminToken, received);
}

export function methodIsBlocked(
  method: string,
  extraBlockedMethods: ReadonlySet<string>,
): boolean {
  return (
    BLOCKED_METHODS.has(method) ||
    extraBlockedMethods.has(method) ||
    BLOCKED_METHOD_PREFIXES.some((prefix) => method.startsWith(prefix))
  );
}

function operationCount(payload: unknown): number {
  return Array.isArray(payload) ? Math.max(1, payload.length) : 1;
}

function batchExceedsLimit(payload: unknown, maximumItems: number): boolean {
  return Array.isArray(payload) && payload.length > maximumItems;
}

function extractMethods(payload: unknown): string[] {
  const items = Array.isArray(payload) ? payload : [payload];
  return items.flatMap((item) => {
    if (!item || typeof item !== "object" || !("method" in item)) return [];
    const method = (item as { method: unknown }).method;
    return typeof method === "string" ? [method] : [];
  });
}

function blockedMethods(payload: unknown, config: RpcProxyConfig): string[] {
  return [...new Set(
    extractMethods(payload).filter((method) =>
      methodIsBlocked(method, config.extraBlockedMethods),
    ),
  )];
}

function payloadId(payload: unknown): unknown {
  if (Array.isArray(payload)) return null;
  if (payload && typeof payload === "object" && "id" in payload) {
    return (payload as { id: unknown }).id;
  }
  return null;
}

function blockedResponse(payload: unknown, blocked: readonly string[]) {
  return {
    jsonrpc: "2.0",
    error: {
      code: -32099,
      message: `Privileged RPC methods are blocked by the public proxy: ${blocked.join(", ")}`,
    },
    id: payloadId(payload),
  };
}

function batchLimitResponse(payload: unknown, maximumItems: number) {
  return {
    jsonrpc: "2.0",
    error: {
      code: -32094,
      message: `JSON-RPC batch exceeds ${maximumItems} items`,
    },
    id: payloadId(payload),
  };
}

function readBody(
  req: http.IncomingMessage,
  maximumBytes: number,
): Promise<string> {
  const contentLength = singleHeader(req, "content-length");
  if (!contentLength.valid) {
    return Promise.reject(new Error("Ambiguous content-length header"));
  }
  if (contentLength.value !== undefined) {
    const parsed = Number(contentLength.value);
    if (Number.isFinite(parsed) && parsed > maximumBytes) {
      return Promise.reject(new HttpBodyTooLargeError());
    }
  }

  return new Promise((resolvePromise, reject) => {
    const chunks: Buffer[] = [];
    let total = 0;
    let settled = false;
    req.on("data", (chunk: Buffer | string) => {
      if (settled) return;
      const buffer = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
      total += buffer.byteLength;
      if (total > maximumBytes) {
        settled = true;
        chunks.length = 0;
        reject(new HttpBodyTooLargeError());
        return;
      }
      chunks.push(buffer);
    });
    req.once("end", () => {
      if (settled) return;
      settled = true;
      resolvePromise(Buffer.concat(chunks, total).toString("utf8"));
    });
    req.once("error", (error) => {
      if (settled) return;
      settled = true;
      reject(error);
    });
    req.once("aborted", () => {
      if (settled) return;
      settled = true;
      reject(new Error("HTTP request was aborted"));
    });
  });
}

function sendJson(
  res: http.ServerResponse,
  config: RpcProxyConfig,
  status: number,
  value: unknown,
) {
  res.writeHead(status, {
    "content-type": "application/json",
    ...corsHeaders(config),
  });
  res.end(JSON.stringify(value));
}

async function readResponseBody(
  response: Response,
  maximumBytes: number,
): Promise<Buffer> {
  const contentLength = response.headers.get("content-length");
  if (contentLength !== null) {
    const parsed = Number(contentLength);
    if (Number.isFinite(parsed) && parsed > maximumBytes) {
      await response.body?.cancel().catch(() => undefined);
      throw new HttpResponseTooLargeError();
    }
  }
  if (response.body === null) return Buffer.alloc(0);

  const reader = response.body.getReader();
  const chunks: Buffer[] = [];
  let total = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      if (total + value.byteLength > maximumBytes) {
        await reader.cancel().catch(() => undefined);
        throw new HttpResponseTooLargeError();
      }
      chunks.push(Buffer.from(value));
      total += value.byteLength;
    }
  } finally {
    reader.releaseLock();
  }
  return Buffer.concat(chunks, total);
}

type PrivateTargetHealth = {
  ok: boolean;
  rpc: boolean;
  websocket: boolean;
  checkedAt: string;
};

async function probePrivateRpc(config: RpcProxyConfig): Promise<void> {
  const response = await fetch(config.targetRpcUrl, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      jsonrpc: "2.0",
      id: "dusk-rpc-proxy-health",
      // Surfpool forwards getGenesisHash to its remote datasource in online
      // fork mode. Use its local-only transport health method so polling this
      // endpoint cannot amplify into mainnet RPC traffic.
      method: "getHealth",
      params: [],
    }),
    signal: AbortSignal.timeout(config.healthProbeTimeoutMs),
  });
  const body = await readResponseBody(response, HEALTH_MAX_RESPONSE_BYTES);
  if (!response.ok) {
    throw new Error(`Private Surfpool RPC health probe returned HTTP ${response.status}`);
  }
  let payload: unknown;
  try {
    payload = JSON.parse(body.toString("utf8"));
  } catch {
    throw new Error("Private Surfpool RPC health probe returned invalid JSON");
  }
  const result = payload && typeof payload === "object"
    ? (payload as { result?: unknown; error?: unknown })
    : null;
  if (
    !result ||
    result.error !== undefined ||
    result.result !== "ok"
  ) {
    throw new Error("Private Surfpool RPC health probe returned an unhealthy result");
  }
}

function probePrivateWebSocket(config: RpcProxyConfig): Promise<void> {
  return new Promise((resolvePromise, reject) => {
    let settled = false;
    let socket: WebSocket | undefined;
    const finish = (error?: Error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      if (error) {
        if (socket) terminateIfPresent(socket);
        reject(error);
        return;
      }
      if (!socket) {
        reject(new Error("Private Surfpool WebSocket was not created"));
        return;
      }
      // Readiness needs only a successful upgrade. Terminate immediately so a
      // broken peer cannot retain one health socket per cache interval by
      // ignoring the WebSocket close handshake.
      terminateIfPresent(socket);
      resolvePromise();
    };
    const timer = setTimeout(() => {
      finish(new Error("Private Surfpool WebSocket health probe timed out"));
    }, config.healthProbeTimeoutMs);

    try {
      const healthProbeOptions = {
        followRedirects: false,
        handshakeTimeout: config.healthProbeTimeoutMs,
        maxBufferedChunks: config.wsMaxBufferedChunks,
        maxFragments: config.wsMaxFragments,
        maxPayload: config.wsMaxPayloadBytes,
        perMessageDeflate: false,
      };
      socket = new WebSocket(config.targetWsUrl, healthProbeOptions);
    } catch (error) {
      clearTimeout(timer);
      reject(error);
      return;
    }
    const activeSocket = socket;
    activeSocket.once("open", () => finish());
    activeSocket.once("error", (error) => finish(error));
    activeSocket.once("close", () => {
      finish(new Error("Private Surfpool WebSocket closed before readiness"));
    });
    activeSocket.once("unexpected-response", (_request, response) => {
      response.resume();
      finish(new Error("Private Surfpool WebSocket rejected the health probe"));
    });
  });
}

async function probePrivateTargets(
  config: RpcProxyConfig,
): Promise<PrivateTargetHealth> {
  const [rpc, websocket] = await Promise.allSettled([
    probePrivateRpc(config),
    probePrivateWebSocket(config),
  ]);
  return {
    ok: rpc.status === "fulfilled" && websocket.status === "fulfilled",
    rpc: rpc.status === "fulfilled",
    websocket: websocket.status === "fulfilled",
    checkedAt: new Date().toISOString(),
  };
}

function rawDataLength(data: RawData): number {
  if (Array.isArray(data)) {
    return data.reduce((total, chunk) => total + chunk.byteLength, 0);
  }
  return data.byteLength;
}

function rawDataText(data: RawData): string {
  if (Array.isArray(data)) return Buffer.concat(data).toString("utf8");
  if (data instanceof ArrayBuffer) return Buffer.from(data).toString("utf8");
  return data.toString("utf8");
}

function forwardClose(
  destination: WebSocket,
  code: number,
  reason: Buffer,
): void {
  if (code === 1005 || code === 1006 || code === 1015) {
    terminateIfPresent(destination);
    return;
  }
  closeIfOpen(destination, code, reason.toString());
}

function closeIfOpen(socket: WebSocket, code: number, reason: string): void {
  if (
    socket.readyState === WebSocket.OPEN ||
    socket.readyState === WebSocket.CONNECTING
  ) {
    socket.close(code, reason.slice(0, 123));
  }
}

function terminateIfPresent(socket: WebSocket): void {
  if (socket.readyState !== WebSocket.CLOSED) socket.terminate();
}

function forwardMessage(
  destination: WebSocket,
  data: RawData,
  isBinary: boolean,
  maxBufferedBytes: number,
): boolean {
  if (destination.readyState !== WebSocket.OPEN) return false;
  if (destination.bufferedAmount + rawDataLength(data) > maxBufferedBytes) {
    closeIfOpen(destination, 1013, "WebSocket proxy backpressure limit exceeded");
    return false;
  }
  destination.send(data, { binary: isBinary }, (error) => {
    if (error) closeIfOpen(destination, 1011, "WebSocket proxy send failed");
  });
  return true;
}

interface OperationBudget {
  windowStartedAtMs: number;
  used: number;
}

function refreshOperationBudget(
  budget: OperationBudget,
  windowMs: number,
  nowMs: number,
): void {
  if (nowMs - budget.windowStartedAtMs < windowMs) return;
  budget.windowStartedAtMs = nowMs;
  budget.used = 0;
}

function consumeOperationBudgets(
  clientBudget: OperationBudget,
  globalBudget: OperationBudget,
  operations: number,
  config: RpcProxyConfig,
): boolean {
  const nowMs = Date.now();
  refreshOperationBudget(clientBudget, config.wsOperationWindowMs, nowMs);
  refreshOperationBudget(globalBudget, config.wsOperationWindowMs, nowMs);
  if (
    operations > config.wsClientMaxOperationsPerWindow - clientBudget.used ||
    operations > config.wsGlobalMaxOperationsPerWindow - globalBudget.used
  ) {
    return false;
  }
  clientBudget.used += operations;
  globalBudget.used += operations;
  return true;
}

function attachWebSocketRelay(
  client: WebSocket,
  request: http.IncomingMessage,
  config: RpcProxyConfig,
  globalOperationBudget: OperationBudget,
): void {
  const upstreamOptions = {
    followRedirects: false,
    handshakeTimeout: config.wsHandshakeTimeoutMs,
    maxBufferedChunks: config.wsMaxBufferedChunks,
    maxFragments: config.wsMaxFragments,
    maxPayload: config.wsMaxPayloadBytes,
    perMessageDeflate: false,
  };
  const upstream = new WebSocket(config.targetWsUrl, upstreamOptions);
  const pending: Array<{ data: RawData; isBinary: boolean }> = [];
  let pendingBytes = 0;
  const clientOperationBudget: OperationBudget = {
    windowStartedAtMs: Date.now(),
    used: 0,
  };

  const closePair = (code: number, reason: string) => {
    closeIfOpen(client, code, reason);
    closeIfOpen(upstream, code, reason);
  };

  client.on("message", (data, isBinary) => {
    if (isBinary) {
      closePair(1003, "Solana JSON-RPC messages must be text");
      return;
    }

    let payload: unknown;
    try {
      payload = JSON.parse(rawDataText(data));
    } catch {
      if (!consumeOperationBudgets(
        clientOperationBudget,
        globalOperationBudget,
        1,
        config,
      )) {
        closePair(1008, "WebSocket operation rate limit exceeded");
        return;
      }
      forwardMessage(
        client,
        Buffer.from(JSON.stringify({
          jsonrpc: "2.0",
          error: { code: -32700, message: "Parse error" },
          id: null,
        })),
        false,
        config.wsMaxBufferedBytes,
      );
      return;
    }

    if (!consumeOperationBudgets(
      clientOperationBudget,
      globalOperationBudget,
      operationCount(payload),
      config,
    )) {
      closePair(1008, "WebSocket operation rate limit exceeded");
      return;
    }

    if (batchExceedsLimit(payload, config.maxBatchItems)) {
      forwardMessage(
        client,
        Buffer.from(JSON.stringify(batchLimitResponse(
          payload,
          config.maxBatchItems,
        ))),
        false,
        config.wsMaxBufferedBytes,
      );
      return;
    }

    const blocked = blockedMethods(payload, config);
    // WebSocket traffic is always public. Do not create an admin WebSocket
    // capability: browser credentials cannot be attached safely and a leaked
    // upgrade header would otherwise expose the entire Surfnet cheatcode API.
    if (blocked.length > 0) {
      forwardMessage(
        client,
        Buffer.from(JSON.stringify(blockedResponse(payload, blocked))),
        false,
        config.wsMaxBufferedBytes,
      );
      return;
    }

    if (upstream.readyState === WebSocket.OPEN) {
      if (!forwardMessage(
        upstream,
        data,
        false,
        config.wsMaxBufferedBytes,
      )) {
        closePair(1013, "WebSocket proxy backpressure limit exceeded");
      }
      return;
    }

    if (upstream.readyState !== WebSocket.CONNECTING) return;
    const dataBytes = rawDataLength(data);
    if (
      pending.length >= config.wsMaxPendingMessages ||
      pendingBytes + dataBytes > config.wsMaxBufferedBytes
    ) {
      closePair(1013, "WebSocket upstream connection queue is full");
      return;
    }
    pending.push({ data, isBinary: false });
    pendingBytes += dataBytes;
  });

  upstream.once("open", () => {
    for (const message of pending) {
      if (!forwardMessage(
        upstream,
        message.data,
        message.isBinary,
        config.wsMaxBufferedBytes,
      )) {
        closePair(1013, "WebSocket proxy backpressure limit exceeded");
        break;
      }
    }
    pending.length = 0;
    pendingBytes = 0;
  });

  upstream.on("message", (data, isBinary) => {
    if (!forwardMessage(
      client,
      data,
      isBinary,
      config.wsMaxBufferedBytes,
    )) {
      closePair(1013, "WebSocket proxy backpressure limit exceeded");
    }
  });

  client.once("close", (code, reason) => {
    forwardClose(upstream, code, reason);
  });
  upstream.once("close", (code, reason) => {
    forwardClose(client, code, reason);
  });
  client.once("error", () => terminateIfPresent(upstream));
  upstream.once("error", () => {
    closeIfOpen(client, 1011, "Private Surfpool WebSocket is unavailable");
  });
  upstream.once("unexpected-response", (_req, response) => {
    response.resume();
    closeIfOpen(client, 1011, "Private Surfpool WebSocket rejected the proxy");
  });
}

function publicUrl(value: string): string {
  const parsed = new URL(value);
  parsed.username = "";
  parsed.password = "";
  parsed.search = "";
  parsed.hash = "";
  return parsed.toString();
}

export function createRpcProxyServer(
  config: RpcProxyConfig = rpcProxyConfigFromEnv(),
): RpcProxyRuntime {
  const webSocketServerOptions = {
    noServer: true,
    clientTracking: true,
    maxBufferedChunks: config.wsMaxBufferedChunks,
    maxFragments: config.wsMaxFragments,
    maxPayload: config.wsMaxPayloadBytes,
    perMessageDeflate: false,
  };
  const webSocketServer = new WebSocketServer(webSocketServerOptions);
  const globalOperationBudget: OperationBudget = {
    windowStartedAtMs: Date.now(),
    used: 0,
  };
  let activeHttpRequests = 0;
  let cachedHealth: { value: PrivateTargetHealth; checkedAtMs: number } | null = null;
  let pendingHealth: Promise<PrivateTargetHealth> | null = null;
  const privateTargetHealth = (): Promise<PrivateTargetHealth> => {
    const now = Date.now();
    if (
      cachedHealth &&
      now - cachedHealth.checkedAtMs < config.healthCacheMs
    ) {
      return Promise.resolve(cachedHealth.value);
    }
    if (pendingHealth) return pendingHealth;
    pendingHealth = probePrivateTargets(config).then((value) => {
      cachedHealth = { value, checkedAtMs: Date.now() };
      return value;
    }).finally(() => {
      pendingHealth = null;
    });
    return pendingHealth;
  };

  const server = http.createServer(async (req, res) => {
    if (req.method === "OPTIONS") {
      res.writeHead(204, corsHeaders(config));
      res.end();
      return;
    }

    if (req.method === "GET" && req.url === "/health") {
      const health = await privateTargetHealth();
      sendJson(res, config, health.ok ? 200 : 503, {
        ...health,
      });
      return;
    }

    if (req.method !== "POST") {
      sendJson(res, config, 405, { error: "method_not_allowed" });
      return;
    }

    if (activeHttpRequests >= config.httpMaxInFlightRequests) {
      res.shouldKeepAlive = false;
      sendJson(res, config, 503, {
        jsonrpc: "2.0",
        error: {
          code: -32095,
          message: "Public Surfpool RPC proxy is at its request limit",
        },
        id: null,
      });
      return;
    }
    activeHttpRequests += 1;
    let handlerSettled = false;
    let responseSettled = false;
    let slotReleased = false;
    const releaseHttpSlot = () => {
      if (slotReleased || !handlerSettled || !responseSettled) return;
      slotReleased = true;
      activeHttpRequests -= 1;
    };
    const settleResponse = () => {
      responseSettled = true;
      releaseHttpSlot();
    };
    // A response buffer remains process-owned until Node flushes it to the
    // downstream socket. Keep the aggregate-memory slot through finish/close,
    // not merely through res.end(), and terminate a stalled downstream peer.
    res.once("finish", settleResponse);
    res.once("close", settleResponse);
    res.setTimeout(config.httpRequestTimeoutMs, () => res.destroy());

    try {
      const body = await readBody(req, config.httpMaxBodyBytes);
      const payload = JSON.parse(body);
      if (batchExceedsLimit(payload, config.maxBatchItems)) {
        sendJson(
          res,
          config,
          400,
          batchLimitResponse(payload, config.maxBatchItems),
        );
        return;
      }
      const blocked = blockedMethods(payload, config);

      if (blocked.length > 0 && !isAdmin(req, config)) {
        sendJson(res, config, 403, blockedResponse(payload, blocked));
        return;
      }

      const upstream = await fetch(config.targetRpcUrl, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body,
        signal: AbortSignal.timeout(config.httpUpstreamTimeoutMs),
      });

      const responseBody = await readResponseBody(
        upstream,
        config.httpMaxResponseBytes,
      );
      res.writeHead(upstream.status, {
        "content-type": upstream.headers.get("content-type") ?? "application/json",
        ...corsHeaders(config),
      });
      res.end(responseBody);
    } catch (error) {
      if (error instanceof HttpBodyTooLargeError) {
        res.shouldKeepAlive = false;
        sendJson(res, config, 413, {
          jsonrpc: "2.0",
          error: {
            code: -32098,
            message: `JSON-RPC request body exceeds ${config.httpMaxBodyBytes} bytes`,
          },
          id: null,
        });
        return;
      }
      if (error instanceof HttpResponseTooLargeError) {
        sendJson(res, config, 502, {
          jsonrpc: "2.0",
          error: {
            code: -32096,
            message: `Private Surfpool RPC response exceeds ${config.httpMaxResponseBytes} bytes`,
          },
          id: null,
        });
        return;
      }
      if (error instanceof Error && error.name === "TimeoutError") {
        sendJson(res, config, 504, {
          jsonrpc: "2.0",
          error: {
            code: -32097,
            message: "Private Surfpool RPC timed out",
          },
          id: null,
        });
        return;
      }
      sendJson(res, config, 500, {
        jsonrpc: "2.0",
        error: {
          code: -32603,
          message: error instanceof Error ? error.message : String(error),
        },
        id: null,
      });
    } finally {
      handlerSettled = true;
      if (res.writableFinished || res.destroyed) responseSettled = true;
      releaseHttpSlot();
    }
  });
  server.requestTimeout = config.httpRequestTimeoutMs;

  server.on("upgrade", (request, socket, head) => {
    if (webSocketServer.clients.size >= config.wsMaxClients) {
      socket.end(
        "HTTP/1.1 503 Service Unavailable\r\n" +
        "Connection: close\r\n" +
        "Content-Length: 0\r\n\r\n",
      );
      return;
    }
    try {
      webSocketServer.handleUpgrade(request, socket, head, (client) => {
        webSocketServer.emit("connection", client, request);
      });
    } catch {
      socket.destroy();
    }
  });

  webSocketServer.on("connection", (client, request) => {
    attachWebSocketRelay(client, request, config, globalOperationBudget);
  });

  return {
    config,
    server,
    webSocketServer,
    async close() {
      for (const client of webSocketServer.clients) terminateIfPresent(client);
      await new Promise<void>((resolve) => webSocketServer.close(() => resolve()));
      if (server.listening) {
        await new Promise<void>((resolve, reject) => {
          server.close((error) => error ? reject(error) : resolve());
        });
      }
    },
  };
}

function isDirectExecution(): boolean {
  return process.argv[1] !== undefined &&
    fileURLToPath(import.meta.url) === resolve(process.argv[1]);
}

if (isDirectExecution()) {
  const runtime = createRpcProxyServer();
  runtime.server.listen(runtime.config.port, "0.0.0.0", () => {
    console.log(
      `Dusk fork RPC proxy listening on :${runtime.config.port}; HTTP ${publicUrl(runtime.config.targetRpcUrl)}; WebSocket ${publicUrl(runtime.config.targetWsUrl)}`,
    );
  });

  const shutdown = () => {
    void runtime.close().finally(() => process.exit(0));
  };
  process.once("SIGINT", shutdown);
  process.once("SIGTERM", shutdown);
}
