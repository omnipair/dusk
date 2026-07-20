import { spawn, type ChildProcess } from "node:child_process";
import { createWriteStream, mkdirSync, rmSync } from "node:fs";
import { connect } from "node:net";
import { resolve } from "node:path";

const ROOT = resolve(import.meta.dirname, "../..");
const SERVICE_LOG_DIR = resolve(".protocol-test-lab/services");
const FORK_STATE_DIR = resolve(".v2-fork-lab");
const children: ChildProcess[] = [];

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
  children.push(child);
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

async function waitForHealth(url: string, timeoutMs: number): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(url);
      if (response.ok) return;
    } catch {
      // Service startup is still in progress.
    }
    await delay(250);
  }
  throw new Error(`Timed out waiting for ${url}`);
}

async function runCommand(command: string, args: string[], env: Record<string, string> = {}): Promise<number> {
  const child = spawn(command, args, {
    cwd: ROOT,
    env: { ...process.env, ...env },
    stdio: "inherit",
  });
  return new Promise((resolvePromise, reject) => {
    child.once("error", reject);
    child.once("exit", (code) => resolvePromise(code ?? 1));
  });
}

function cleanup(): void {
  if (process.env.PROTOCOL_TEST_KEEP_SERVICES === "true") return;
  for (const child of children.reverse()) {
    if (!child.pid || child.exitCode !== null) continue;
    try {
      process.kill(-child.pid, "SIGTERM");
    } catch {
      // The process group may already have exited.
    }
  }
  rmSync(FORK_STATE_DIR, { recursive: true, force: true });
}

async function main(): Promise<void> {
  process.once("SIGINT", () => {
    cleanup();
    process.exit(130);
  });
  process.once("SIGTERM", () => {
    cleanup();
    process.exit(143);
  });

  await requireFreePorts();
  if (process.env.PROTOCOL_TEST_SKIP_BUILD !== "true") {
    const buildCode = await runCommand("anchor", ["build", "-p", "dusk", "--", "--features", "development"]);
    if (buildCode !== 0) throw new Error(`Dusk build failed with code ${buildCode}`);
  }

  rmSync(FORK_STATE_DIR, { recursive: true, force: true });
  const surfpool = startService("surfpool", "npm", ["run", "v2-fork:surfpool"], {
    FORK_LAB_BUILD: "false",
    FORK_LAB_DEPLOYMENT_TIMEOUT_SECONDS: "240",
  });
  await waitForOutput(surfpool, /Surfpool fork is running local dusk artifact/, 240_000);

  const proxy = startService("rpc-proxy", "npm", ["run", "v2-fork:rpc-proxy"], {
    SURFPOOL_RPC_URL: "http://127.0.0.1:8899",
    PUBLIC_SURFPOOL_RPC_URL: "http://127.0.0.1:8898",
    FORK_RPC_PROXY_PORT: "8898",
  });
  await waitForOutput(proxy, /RPC proxy listening on :8898/, 30_000);

  const api = startService("api", "npm", ["run", "v2-fork:api"], {
    SURFPOOL_RPC_URL: "http://127.0.0.1:8899",
    SURFPOOL_RPC_PROXY_URL: "http://127.0.0.1:8898",
    PUBLIC_SURFPOOL_RPC_URL: "http://127.0.0.1:8898",
    FORK_API_PORT: "8080",
  });
  await waitForOutput(api, /fork API listening on :8080/, 30_000);
  await waitForHealth("http://127.0.0.1:8080/health", 30_000);
  await waitForHealth("http://127.0.0.1:8080/api/v2/fork/test-catalog", 30_000);
  await waitForHealth("http://127.0.0.1:8080/api/v2/fork/config", 120_000);

  const testCode = await runCommand(
    "node",
    ["--loader", "ts-node/esm", "scripts/protocol-tests/run.ts"],
    { TS_NODE_PROJECT: "scripts/protocol-tests/tsconfig.json" }
  );
  if (testCode !== 0) process.exitCode = testCode;
}

main()
  .catch((error) => {
    console.error(error);
    process.exitCode = 1;
  })
  .finally(cleanup);
