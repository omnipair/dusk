import { randomBytes } from "node:crypto";
import { spawn, type ChildProcess } from "node:child_process";
import { createWriteStream, mkdirSync, rmSync } from "node:fs";
import { connect } from "node:net";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { Keypair } from "@solana/web3.js";

const ROOT = resolve(import.meta.dirname, "../..");
const SERVICE_LOG_DIR = resolve(".protocol-test-lab/services");
const FORK_STATE_ROOT = resolve(".v2-fork-lab");
const CONTROLLER_STATE_DIR = resolve(FORK_STATE_ROOT, "controller");
const API_STATE_DIR = resolve(FORK_STATE_ROOT, "api");
const serviceChildren: ChildProcess[] = [];
const commandChildren: ChildProcess[] = [];
const PROCESS_STOP_GRACE_MS = 2_000;
let cleanupPromise: Promise<void> | null = null;

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

function delay(ms: number): Promise<void> {
  return new Promise((resolvePromise) => setTimeout(resolvePromise, ms));
}

async function portIsOpen(port: number): Promise<boolean> {
  return new Promise((resolvePromise) => {
    const socket = connect({ host: "127.0.0.1", port });
    socket.once("connect", () => {
      socket.destroy();
      resolvePromise(true);
    });
    socket.once("error", () => resolvePromise(false));
    socket.setTimeout(500, () => {
      socket.destroy();
      resolvePromise(false);
    });
  });
}

async function requireFreePorts(): Promise<void> {
  for (const port of [8080, 8898, 8899, 8900]) {
    if (await portIsOpen(port)) {
      throw new Error(`Port ${port} is already in use. Stop the existing fork stack before a fresh isolated run.`);
    }
  }
}

function startService(name: string, command: string, args: string[], env: Record<string, string>): ChildProcess {
  mkdirSync(SERVICE_LOG_DIR, { recursive: true, mode: 0o700 });
  const log = createWriteStream(resolve(SERVICE_LOG_DIR, `${name}.log`), { flags: "w", mode: 0o600 });
  const child = spawn(command, args, {
    cwd: ROOT,
    env: { ...process.env, ...env },
    detached: true,
    stdio: ["ignore", "pipe", "pipe"],
  });
  child.stdout?.pipe(log);
  child.stderr?.pipe(log);
  serviceChildren.push(child);
  return child;
}

async function waitForOutput(child: ChildProcess, pattern: RegExp, timeoutMs: number): Promise<void> {
  const started = Date.now();
  let output = "";
  await new Promise<void>((resolvePromise, reject) => {
    const onData = (chunk: Buffer) => {
      output += chunk.toString("utf8");
      if (pattern.test(output)) {
        cleanupListeners();
        resolvePromise();
      }
    };
    const onExit = (code: number | null) => {
      cleanupListeners();
      reject(new Error(`Service exited before readiness with code ${code}`));
    };
    const timer = setInterval(() => {
      if (Date.now() - started >= timeoutMs) {
        cleanupListeners();
        reject(new Error(`Timed out waiting for service readiness: ${pattern}`));
      }
    }, 250);
    const cleanupListeners = () => {
      clearInterval(timer);
      child.stdout?.off("data", onData);
      child.stderr?.off("data", onData);
      child.off("exit", onExit);
    };
    child.stdout?.on("data", onData);
    child.stderr?.on("data", onData);
    child.once("exit", onExit);
  });
}

async function waitForHealth(
  url: string,
  timeoutMs: number,
  requestTimeoutMs: number,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  let lastFailure = "no response";
  while (Date.now() < deadline) {
    try {
      const remainingMs = Math.max(1, deadline - Date.now());
      const attemptTimeoutMs = Math.min(requestTimeoutMs, remainingMs);
      const response = await fetch(url, {
        signal: AbortSignal.timeout(attemptTimeoutMs),
      });
      if (response.ok) return;
      lastFailure = `HTTP ${response.status}`;
      await response.body?.cancel().catch(() => undefined);
    } catch (error) {
      // Service startup is still in progress.
      lastFailure = error instanceof Error ? error.message : String(error);
    }
    await delay(250);
  }
  throw new Error(
    `Timed out waiting for ${url} after ${timeoutMs}ms; last failure: ${lastFailure}`,
  );
}

async function runCommand(
  command: string,
  args: string[],
  env: Record<string, string> = {},
  timeout?: { milliseconds: number; label: string },
): Promise<number> {
  const child = spawn(command, args, {
    cwd: ROOT,
    env: { ...process.env, ...env },
    detached: true,
    stdio: "inherit",
  });
  commandChildren.push(child);
  return new Promise((resolvePromise, reject) => {
    let timedOut = false;
    let settled = false;
    const settle = (callback: () => void) => {
      if (settled) return;
      settled = true;
      if (timeoutTimer !== undefined) clearTimeout(timeoutTimer);
      callback();
    };
    const timeoutTimer = timeout === undefined
      ? undefined
      : setTimeout(() => {
          timedOut = true;
          void stopProcessGroup(child).then(
            () => settle(() => reject(new Error(
              `${timeout.label} timed out after ${timeout.milliseconds}ms and its process group was terminated`,
            ))),
            (error) => settle(() => reject(error)),
          );
        }, timeout.milliseconds);
    child.once("error", (error) => {
      if (!timedOut) settle(() => reject(error));
    });
    child.once("exit", (code) => {
      if (!timedOut) settle(() => resolvePromise(code ?? 1));
    });
  });
}

export function processGroupIsAlive(processGroupId: number): boolean {
  try {
    process.kill(-processGroupId, 0);
    return true;
  } catch (error) {
    if (
      error instanceof Error &&
      "code" in error &&
      (error as NodeJS.ErrnoException).code === "ESRCH"
    ) {
      return false;
    }
    if (
      error instanceof Error &&
      "code" in error &&
      (error as NodeJS.ErrnoException).code === "EPERM"
    ) {
      return true;
    }
    throw error;
  }
}

function signalProcessGroupId(
  processGroupId: number,
  signal: NodeJS.Signals,
): void {
  try {
    process.kill(-processGroupId, signal);
  } catch (error) {
    if (
      error instanceof Error &&
      "code" in error &&
      (error as NodeJS.ErrnoException).code === "ESRCH"
    ) {
      return;
    }
    throw error;
  }
}

async function waitForProcessGroupExit(
  processGroupId: number,
  timeoutMs: number,
): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (processGroupIsAlive(processGroupId) && Date.now() < deadline) {
    await delay(25);
  }
  return !processGroupIsAlive(processGroupId);
}

export async function stopProcessGroup(
  child: Pick<ChildProcess, "pid">,
  graceMs = PROCESS_STOP_GRACE_MS,
): Promise<void> {
  const processGroupId = child.pid;
  if (!processGroupId || !processGroupIsAlive(processGroupId)) return;
  signalProcessGroupId(processGroupId, "SIGTERM");
  if (await waitForProcessGroupExit(processGroupId, graceMs)) return;
  signalProcessGroupId(processGroupId, "SIGKILL");
  if (await waitForProcessGroupExit(processGroupId, graceMs)) return;
  throw new Error(
    `Process group ${processGroupId} survived SIGTERM and SIGKILL`,
  );
}

function cleanup(): Promise<void> {
  cleanupPromise ??= (async () => {
    await Promise.all(
      [...commandChildren].reverse().map((child) => stopProcessGroup(child)),
    );
    if (process.env.PROTOCOL_TEST_KEEP_SERVICES === "true") return;
    await Promise.all(
      [...serviceChildren].reverse().map((child) => stopProcessGroup(child)),
    );
    rmSync(FORK_STATE_ROOT, { recursive: true, force: true });
  })();
  return cleanupPromise;
}

async function main(): Promise<void> {
  process.once("SIGINT", () => {
    void cleanup().finally(() => process.exit(130));
  });
  process.once("SIGTERM", () => {
    void cleanup().finally(() => process.exit(143));
  });

  const httpTimeoutMs = positiveIntegerEnv(
    "PROTOCOL_TEST_HTTP_TIMEOUT_MS",
    150_000,
    300_000,
  );
  const runTimeoutMs = positiveIntegerEnv(
    "PROTOCOL_TEST_RUN_TIMEOUT_MS",
    10_800_000,
    43_200_000,
  );

  await requireFreePorts();
  if (process.env.PROTOCOL_TEST_SKIP_BUILD !== "true") {
    for (const program of ["dusk", "leverage_delegate"]) {
      const buildCode = await runCommand("anchor", [
        "build",
        "-p",
        program,
        "--",
        "--features",
        "development",
      ]);
      if (buildCode !== 0) {
        throw new Error(`${program} build failed with code ${buildCode}`);
      }
    }
    const controllerBuildCode = await runCommand("npm", [
      "run",
      "build:v2-fork-rpc-controller",
    ]);
    if (controllerBuildCode !== 0) {
      throw new Error(
        `Surfpool SDK controller build failed with code ${controllerBuildCode}`,
      );
    }
  }

  const remoteRpcUrl =
    process.env.FORK_SDK_REMOTE_RPC_URL ??
    process.env.SURFPOOL_DATASOURCE_RPC_URL ??
    "https://api.mainnet-beta.solana.com";
  const payerEnv: Record<string, string> = process.env
    .FORK_LAB_PAYER_KEYPAIR_JSON
    ? {
        FORK_LAB_PAYER_KEYPAIR_JSON:
          process.env.FORK_LAB_PAYER_KEYPAIR_JSON,
      }
    : process.env.FORK_LAB_PAYER_KEYPAIR_BASE64
      ? {
          FORK_LAB_PAYER_KEYPAIR_BASE64:
            process.env.FORK_LAB_PAYER_KEYPAIR_BASE64,
        }
      : {
          FORK_LAB_PAYER_KEYPAIR_JSON: JSON.stringify(
            Array.from(Keypair.generate().secretKey),
          ),
        };
  const adminToken =
    process.env.FORK_ADMIN_TOKEN ?? randomBytes(32).toString("hex");
  const sharedForkEnv = {
    ...payerEnv,
    FORK_ADMIN_TOKEN: adminToken,
    FORK_BOOTSTRAP_MARKETS: "both",
  };

  rmSync(FORK_STATE_ROOT, { recursive: true, force: true });
  const surfpool = startService("surfpool", "npm", ["run", "v2-fork:surfpool"], {
    ...sharedForkEnv,
    FORK_LAB_STATE_DIR: CONTROLLER_STATE_DIR,
    FORK_LAB_STATE_PATH: resolve(CONTROLLER_STATE_DIR, "state.json"),
    FORK_SDK_REMOTE_RPC_URL: remoteRpcUrl,
    SURFPOOL_HOST: "127.0.0.1",
    SURFPOOL_RPC_PORT: "8899",
    SURFPOOL_WS_PORT: "8900",
  });
  await waitForOutput(
    surfpool,
    /"event":"dusk_surfpool_controller_ready"/,
    240_000,
  );

  const proxy = startService("rpc-proxy", "npm", ["run", "v2-fork:rpc-proxy"], {
    FORK_ADMIN_TOKEN: adminToken,
    PORT: "8898",
    SURFPOOL_RPC_URL: "http://127.0.0.1:8899",
    SURFPOOL_WS_URL: "ws://127.0.0.1:8900",
    PUBLIC_SURFPOOL_RPC_URL: "http://127.0.0.1:8898",
    FORK_RPC_PROXY_PORT: "8898",
  });
  await waitForOutput(proxy, /RPC proxy listening on :8898/, 30_000);

  const api = startService("api", "npm", ["run", "v2-fork:api"], {
    ...sharedForkEnv,
    FORK_LAB_STATE_DIR: API_STATE_DIR,
    FORK_LAB_STATE_PATH: resolve(API_STATE_DIR, "state.json"),
    PORT: "8080",
    SURFPOOL_RPC_URL: "http://127.0.0.1:8899",
    SURFPOOL_RPC_PROXY_URL: "http://127.0.0.1:8898",
    PUBLIC_SURFPOOL_RPC_URL: "http://127.0.0.1:8898",
    FORK_API_PORT: "8080",
    DUSK_REQUIRE_EXPLICIT_FORK_SIGNER: "true",
    DUSK_REQUIRE_EXTERNAL_FORK_MARKER: "true",
    DUSK_REQUIRE_PREBOOTSTRAPPED_MARKETS: "true",
  });
  await waitForOutput(api, /fork API listening on :8080/, 30_000);
  await waitForHealth(
    "http://127.0.0.1:8080/health",
    30_000,
    httpTimeoutMs,
  );
  await waitForHealth(
    "http://127.0.0.1:8080/api/v2/fork/test-catalog",
    30_000,
    httpTimeoutMs,
  );
  await waitForHealth(
    "http://127.0.0.1:8080/api/v2/fork/config",
    120_000,
    httpTimeoutMs,
  );

  const testCode = await runCommand(
    "node",
    ["--loader", "ts-node/esm", "scripts/protocol-tests/run.ts"],
    {
      FORK_API_URL: "http://127.0.0.1:8080",
      FORK_ADMIN_TOKEN: adminToken,
      TS_NODE_PROJECT: "scripts/protocol-tests/tsconfig.json",
    },
    {
      milliseconds: runTimeoutMs,
      label: "Protocol client command",
    },
  );
  if (testCode !== 0) process.exitCode = testCode;
}

function isDirectExecution(): boolean {
  return process.argv[1] !== undefined &&
    fileURLToPath(import.meta.url) === resolve(process.argv[1]);
}

if (isDirectExecution()) {
  main()
    .catch((error) => {
      console.error(error);
      process.exitCode = 1;
    })
    .finally(cleanup);
}
