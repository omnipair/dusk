import http from "node:http";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

const DEFAULT_MAX_BODY_BYTES = 1_048_576;
const DEFAULT_MAX_IN_FLIGHT_REQUESTS = 32;
const DEFAULT_BODY_TIMEOUT_MS = 15_000;
const DEFAULT_ROUTE_TIMEOUT_MS = 120_000;

class RequestBodyTooLargeError extends Error {}
class RequestBodyTimeoutError extends Error {}
class RouteTimeoutError extends Error {}

export interface ForkApiConfig {
  port: number;
  surfpoolRpcUrl: string;
  publicRpcUrl: string;
  corsOrigin: string;
  maxBodyBytes: number;
  maxInFlightRequests: number;
  bodyTimeoutMs: number;
  routeTimeoutMs: number;
}

export interface ForkApiDependencies {
  requireForkAdminAuthorization(req: http.IncomingMessage): void;
  route(
    req: http.IncomingMessage,
    body: Record<string, unknown>,
  ): Promise<unknown>;
}

export interface ForkApiRuntime {
  config: ForkApiConfig;
  server: http.Server;
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

function requireProtocol(
  value: string,
  protocols: readonly string[],
  name: string,
): string {
  const parsed = new URL(value);
  if (!protocols.includes(parsed.protocol)) {
    throw new Error(`${name} must use ${protocols.join(" or ")}`);
  }
  return value;
}

function nonBlank(value: string | undefined): string | undefined {
  const trimmed = value?.trim();
  return trimmed ? trimmed : undefined;
}

function requireHostedAdminToken(env: NodeJS.ProcessEnv): void {
  if (
    env.FORK_REQUIRE_ADMIN_TOKEN === "true" &&
    nonBlank(env.FORK_ADMIN_TOKEN) === undefined
  ) {
    throw new Error(
      "FORK_REQUIRE_ADMIN_TOKEN=true requires a nonblank FORK_ADMIN_TOKEN",
    );
  }
}

function publicRpcUrlFromEnv(
  env: NodeJS.ProcessEnv,
  surfpoolRpcUrl: string,
): string {
  const explicitPublicRpcUrl =
    nonBlank(env.PUBLIC_SURFPOOL_RPC_URL) ??
    nonBlank(env.SURFPOOL_RPC_PROXY_URL);
  if (
    env.DUSK_REQUIRE_PUBLIC_RPC_URL === "true" &&
    explicitPublicRpcUrl === undefined
  ) {
    throw new Error(
      "DUSK_REQUIRE_PUBLIC_RPC_URL=true requires an explicit " +
        "PUBLIC_SURFPOOL_RPC_URL or SURFPOOL_RPC_PROXY_URL",
    );
  }
  return requireProtocol(
    explicitPublicRpcUrl ?? surfpoolRpcUrl,
    ["http:", "https:"],
    "PUBLIC_SURFPOOL_RPC_URL",
  );
}

export function forkApiConfigFromEnv(
  env: NodeJS.ProcessEnv = process.env,
): ForkApiConfig {
  requireHostedAdminToken(env);
  const surfpoolRpcUrl = requireProtocol(
    env.SURFPOOL_RPC_URL ?? "http://127.0.0.1:8899",
    ["http:", "https:"],
    "SURFPOOL_RPC_URL",
  );
  return {
    port: positiveInteger(env.PORT ?? env.FORK_API_PORT, 8080, "PORT", 65_535),
    surfpoolRpcUrl,
    publicRpcUrl: publicRpcUrlFromEnv(env, surfpoolRpcUrl),
    corsOrigin: env.FORK_API_CORS_ORIGIN ?? "*",
    maxBodyBytes: positiveInteger(
      env.FORK_API_MAX_BODY_BYTES,
      DEFAULT_MAX_BODY_BYTES,
      "FORK_API_MAX_BODY_BYTES",
      16_777_216,
    ),
    maxInFlightRequests: positiveInteger(
      env.FORK_API_MAX_IN_FLIGHT_REQUESTS,
      DEFAULT_MAX_IN_FLIGHT_REQUESTS,
      "FORK_API_MAX_IN_FLIGHT_REQUESTS",
      256,
    ),
    bodyTimeoutMs: positiveInteger(
      env.FORK_API_BODY_TIMEOUT_MS,
      DEFAULT_BODY_TIMEOUT_MS,
      "FORK_API_BODY_TIMEOUT_MS",
      120_000,
    ),
    routeTimeoutMs: positiveInteger(
      env.FORK_API_ROUTE_TIMEOUT_MS,
      DEFAULT_ROUTE_TIMEOUT_MS,
      "FORK_API_ROUTE_TIMEOUT_MS",
      600_000,
    ),
  };
}

function corsHeaders(config: ForkApiConfig) {
  return {
    "access-control-allow-origin": config.corsOrigin,
    "access-control-allow-methods": "GET, POST, OPTIONS",
    "access-control-allow-headers":
      "content-type, authorization, solana-client, x-fork-admin-token",
  };
}

function replacer(_key: string, value: unknown): unknown {
  if (typeof value === "bigint") return value.toString();
  if (value && typeof value === "object") {
    const maybeBase58 = (value as { toBase58?: unknown }).toBase58;
    if (typeof maybeBase58 === "function") return maybeBase58.call(value);
    if (value.constructor?.name === "BN") return value.toString();
  }
  return value;
}

function sendJson(
  res: http.ServerResponse,
  config: ForkApiConfig,
  status: number,
  value: unknown,
): void {
  if (res.destroyed || res.writableEnded) return;
  res.writeHead(status, {
    "content-type": "application/json",
    ...corsHeaders(config),
  });
  res.end(JSON.stringify(value, replacer));
}

function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message;
  try {
    return String(error);
  } catch {
    return "Unknown fork API error";
  }
}

function errorCode(error: unknown): string | null {
  if (typeof error !== "object" || error === null) return null;
  const code = (error as { code?: unknown }).code;
  return typeof code === "string" && code ? code : null;
}

function hasUncertainOutcome(error: unknown): boolean {
  return (
    typeof error === "object" &&
    error !== null &&
    (error as { uncertainOutcome?: unknown }).uncertainOutcome === true
  );
}

function errorHttpStatus(error: unknown): number | null {
  if (typeof error !== "object" || error === null) return null;
  const status = (error as { httpStatus?: unknown }).httpStatus;
  return Number.isInteger(status) &&
    (status as number) >= 400 &&
    (status as number) <= 599
    ? (status as number)
    : null;
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

function readBody(
  req: http.IncomingMessage,
  maximumBytes: number,
  timeoutMs: number,
): Promise<Record<string, unknown>> {
  const contentLength = singleHeader(req, "content-length");
  if (!contentLength.valid) {
    return Promise.reject(new Error("Ambiguous content-length header"));
  }
  if (contentLength.value !== undefined) {
    const parsed = Number(contentLength.value);
    if (!Number.isSafeInteger(parsed) || parsed < 0) {
      return Promise.reject(new Error("Invalid content-length header"));
    }
    if (parsed > maximumBytes) {
      return Promise.reject(new RequestBodyTooLargeError());
    }
  }

  return new Promise((resolvePromise, reject) => {
    const chunks: Buffer[] = [];
    let total = 0;
    let settled = false;
    const finish = (
      callback: () => void,
      { drain = false }: { drain?: boolean } = {},
    ) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      if (drain) req.resume();
      callback();
    };
    const timer = setTimeout(() => {
      chunks.length = 0;
      finish(
        () => reject(new RequestBodyTimeoutError()),
        { drain: true },
      );
    }, timeoutMs);

    req.on("data", (chunk: Buffer | string) => {
      if (settled) return;
      const buffer = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
      total += buffer.byteLength;
      if (total > maximumBytes) {
        chunks.length = 0;
        finish(
          () => reject(new RequestBodyTooLargeError()),
          { drain: true },
        );
        return;
      }
      chunks.push(buffer);
    });
    req.once("end", () => {
      finish(() => {
        try {
          const text = Buffer.concat(chunks, total).toString("utf8");
          resolvePromise(text ? JSON.parse(text) : {});
        } catch (error) {
          reject(error);
        }
      });
    });
    req.once("error", (error) => {
      finish(() => reject(error));
    });
    req.once("aborted", () => {
      finish(() => reject(new Error("HTTP request was aborted")));
    });
  });
}

function withRouteDeadline<T>(operation: Promise<T>, timeoutMs: number): Promise<T> {
  return new Promise<T>((resolvePromise, reject) => {
    const timer = setTimeout(() => reject(new RouteTimeoutError()), timeoutMs);
    operation.then(
      (value) => {
        clearTimeout(timer);
        resolvePromise(value);
      },
      (error) => {
        clearTimeout(timer);
        reject(error);
      },
    );
  });
}

async function defaultDependencies(): Promise<ForkApiDependencies> {
  return import("./api_core.js");
}

export function createForkApiServer(
  config: ForkApiConfig = forkApiConfigFromEnv(),
  dependencies?: ForkApiDependencies,
): ForkApiRuntime {
  let activeRequests = 0;

  const server = http.createServer(async (req, res) => {
    if (req.method === "OPTIONS") {
      res.writeHead(204, corsHeaders(config));
      res.end();
      return;
    }

    if (activeRequests >= config.maxInFlightRequests) {
      res.shouldKeepAlive = false;
      sendJson(res, config, 503, {
        success: false,
        code: "fork_api_request_limit",
        error: "Dusk fork API is at its request limit",
      });
      return;
    }
    activeRequests += 1;

    let operationOwnsSlot = false;
    let released = false;
    const release = () => {
      if (released) return;
      released = true;
      activeRequests -= 1;
    };

    try {
      const operation = (async () => {
        const api = dependencies ?? await defaultDependencies();
        api.requireForkAdminAuthorization(req);
        const body = req.method === "POST"
          ? await readBody(req, config.maxBodyBytes, config.bodyTimeoutMs)
          : {};
        return api.route(req, body);
      })();
      operationOwnsSlot = true;
      void operation.then(release, release);

      const value = await withRouteDeadline(operation, config.routeTimeoutMs);
      sendJson(res, config, 200, value);
    } catch (error) {
      if (error instanceof RequestBodyTooLargeError) {
        res.shouldKeepAlive = false;
        sendJson(res, config, 413, {
          success: false,
          code: "fork_api_body_too_large",
          error: `Request body exceeds ${config.maxBodyBytes} bytes`,
        });
        return;
      }
      if (error instanceof RequestBodyTimeoutError) {
        res.shouldKeepAlive = false;
        sendJson(res, config, 408, {
          success: false,
          code: "fork_api_body_timeout",
          error: `Request body was not received within ${config.bodyTimeoutMs}ms`,
        });
        return;
      }
      if (error instanceof RouteTimeoutError) {
        res.shouldKeepAlive = false;
        sendJson(res, config, 504, {
          success: false,
          code: "fork_api_route_timeout",
          error: `Dusk fork API route exceeded ${config.routeTimeoutMs}ms`,
          ...(req.method === "POST" ? { uncertainOutcome: true } : {}),
        });
        return;
      }

      const code = errorCode(error);
      const status =
        errorHttpStatus(error) ??
        (req.url?.startsWith("/health")
          ? 503
          : code === "deployment_identity_changed"
            ? 409
            : 400);
      sendJson(res, config, status, {
        success: false,
        error: errorMessage(error),
        ...(code ? { code } : {}),
        ...(hasUncertainOutcome(error) ? { uncertainOutcome: true } : {}),
      });
    } finally {
      if (!operationOwnsSlot) release();
    }
  });
  server.requestTimeout = config.bodyTimeoutMs;

  return {
    config,
    server,
    async close() {
      if (!server.listening) return;
      await new Promise<void>((resolvePromise, reject) => {
        server.close((error) => error ? reject(error) : resolvePromise());
        server.closeAllConnections();
      });
    },
  };
}

function isDirectExecution(): boolean {
  return process.argv[1] !== undefined &&
    fileURLToPath(import.meta.url) === resolve(process.argv[1]);
}

if (isDirectExecution()) {
  const runtime = createForkApiServer();
  runtime.server.listen(runtime.config.port, "0.0.0.0", () => {
    console.log(`Dusk fork API listening on :${runtime.config.port}`);
    console.log(`Surfpool RPC: ${runtime.config.surfpoolRpcUrl}`);
    console.log(`Public RPC: ${runtime.config.publicRpcUrl}`);
    console.log(
      "Dusk fork runtime and deployment identity are verified by /health",
    );
  });

  const shutdown = () => {
    void runtime.close().finally(() => process.exit(0));
  };
  process.once("SIGINT", shutdown);
  process.once("SIGTERM", shutdown);
}
