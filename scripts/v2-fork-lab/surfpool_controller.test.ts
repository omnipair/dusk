import { createHash } from "node:crypto";
import { spawn, type ChildProcess } from "node:child_process";
import { once } from "node:events";
import { existsSync, readFileSync } from "node:fs";
import { createServer as createHttpServer } from "node:http";
import { createServer as createNetServer, connect } from "node:net";
import type { Duplex } from "node:stream";
import { fileURLToPath } from "node:url";
import { expect } from "chai";
import { describe, it } from "mocha";
import { Surfnet } from "@solana/surfpool";
import { Keypair, PublicKey } from "@solana/web3.js";
import { WebSocketServer } from "ws";
import {
  alignExactUpgradeableProgramAuthority,
  controllerReadinessConfigFromEnv,
  createStartupPhaseTracker,
  createTcpProxy,
  decodeExplicitPayer,
  deployExactProgram,
  hostedControllerConfigFromEnv,
  parsePayerFundingLamports,
  probeExactUpgradeableProgram,
  probeStableWebSocket,
  relayWorkerConfigFromEnv,
  rewriteUpgradeableProgramDataAuthority,
  startControllerReadinessServer,
  startHostedSurfpoolController,
  startSurfnetEventDrain,
} from "./surfpool_controller.js";
import {
  probeRawSurfpoolTargets,
  startSurfpoolRelayWorkerProcess,
} from "./surfpool_relay_worker.js";

const UPGRADEABLE_LOADER = new PublicKey(
  "BPFLoaderUpgradeab1e11111111111111111111111",
);

async function withTimeout<T>(
  operation: Promise<T>,
  label: string,
  milliseconds = 5_000,
): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      operation,
      new Promise<never>((_resolve, reject) => {
        timer = setTimeout(
          () => reject(new Error(`${label} timed out after ${milliseconds}ms`)),
          milliseconds,
        );
      }),
    ]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

async function startExternalEchoServer(): Promise<{
  child: ChildProcess;
  port: number;
}> {
  const source = [
    'const http = require("node:http");',
    'const { WebSocketServer } = require("ws");',
    'const server = http.createServer((request, response) => {',
    '  const chunks = [];',
    '  request.on("data", (chunk) => chunks.push(chunk));',
    '  request.on("end", () => {',
    '    let id = null;',
    '    try { id = JSON.parse(Buffer.concat(chunks).toString("utf8")).id; } catch {}',
    '    response.writeHead(200, { "content-type": "application/json", connection: "close" });',
    '    response.end(JSON.stringify({ jsonrpc: "2.0", id, result: "ok" }));',
    '  });',
    '});',
    'const webSocketServer = new WebSocketServer({ server });',
    'server.listen(0, "127.0.0.1", () => console.log(server.address().port));',
    'const stop = () => {',
    '  for (const client of webSocketServer.clients) client.terminate();',
    '  webSocketServer.close(() => server.close(() => process.exit(0)));',
    '};',
    'process.once("SIGTERM", stop);',
    'process.once("SIGINT", stop);',
  ].join("\n");
  const child = spawn(process.execPath, ["-e", source], {
    stdio: ["ignore", "pipe", "inherit"],
  });
  const stdout = child.stdout;
  if (!stdout) throw new Error("External echo server has no stdout pipe");
  const [chunk] = await withTimeout(
    once(stdout, "data") as Promise<[Buffer]>,
    "external echo startup",
  );
  const port = Number(chunk.toString("utf8").trim());
  if (!Number.isSafeInteger(port) || port < 1) {
    child.kill("SIGTERM");
    throw new Error(`External echo server returned invalid port: ${String(port)}`);
  }
  return { child, port };
}

async function stopChild(child: ChildProcess): Promise<void> {
  if (child.exitCode !== null || child.signalCode !== null) return;
  const exited = once(child, "exit").then(() => undefined);
  child.kill("SIGTERM");
  await withTimeout(exited, "child shutdown");
}

async function startSyntheticSurfpoolTargets(): Promise<{
  rpcUrl: string;
  wsUrl: string;
  close(): Promise<void>;
}> {
  const server = createHttpServer((request, response) => {
    const chunks: Buffer[] = [];
    request.on("data", (chunk: Buffer) => chunks.push(chunk));
    request.on("end", () => {
      let id: unknown = null;
      try {
        id = (JSON.parse(Buffer.concat(chunks).toString("utf8")) as { id?: unknown }).id;
      } catch {
        // The probe assertion below rejects malformed replies.
      }
      response.writeHead(200, {
        "content-type": "application/json",
        connection: "close",
      });
      response.end(JSON.stringify({ jsonrpc: "2.0", id, result: "ok" }));
    });
  });
  const webSocketServer = new WebSocketServer({ server });
  server.listen({ host: "127.0.0.1", port: 0 });
  await once(server, "listening");
  const address = server.address();
  if (!address || typeof address === "string") {
    throw new Error("Synthetic Surfpool target did not expose an IP socket");
  }
  return {
    rpcUrl: `http://127.0.0.1:${address.port}`,
    wsUrl: `ws://127.0.0.1:${address.port}`,
    async close() {
      for (const client of webSocketServer.clients) client.terminate();
      await new Promise<void>((resolvePromise) =>
        webSocketServer.close(() => resolvePromise())
      );
      server.closeAllConnections();
      server.close();
      await once(server, "close");
    },
  };
}

async function waitForHealthStatus(
  url: string,
  expectedStatus: number,
  milliseconds = 3_000,
): Promise<Record<string, unknown>> {
  const startedAt = Date.now();
  let lastStatus: number | undefined;
  while (Date.now() - startedAt < milliseconds) {
    try {
      const response = await fetch(url);
      lastStatus = response.status;
      const body = await response.json() as Record<string, unknown>;
      if (response.status === expectedStatus) return body;
    } catch {
      // The worker may be between IPC state transitions; retry within the bound.
    }
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 20));
  }
  throw new Error(
    `Health endpoint did not return ${expectedStatus}; last status ${String(lastStatus)}`,
  );
}

describe("Surfpool SDK controller surface", () => {
  it("serves 503 until fully ready, then returns 200 and downgrades on shutdown", async () => {
    const readiness = await startControllerReadinessServer({
      host: "127.0.0.1",
      port: 0,
      requestTimeoutMs: 1_000,
      maxConnections: 4,
    });
    const healthUrl = `http://127.0.0.1:${readiness.port}/health`;
    try {
      const starting = await fetch(healthUrl);
      expect(starting.status).to.equal(503);
      expect(await starting.json()).to.include({ ok: false, status: "starting" });

      readiness.markReady(2);
      const ready = await fetch(healthUrl);
      expect(ready.status).to.equal(200);
      expect(await ready.json()).to.include({
        ok: true,
        status: "ready",
        programCount: 2,
      });

      readiness.markStopping();
      const stopping = await fetch(healthUrl);
      expect(stopping.status).to.equal(503);
      expect(await stopping.json()).to.include({ ok: false, status: "stopping" });
      expect(() => readiness.markReady(2)).to.throw(
        "cannot become ready from stopping",
      );
    } finally {
      await readiness.close();
    }
  });

  it("reports early failure and rejects unsafe Railway health configuration", async () => {
    const readiness = await startControllerReadinessServer({
      host: "127.0.0.1",
      port: 0,
      requestTimeoutMs: 1_000,
      maxConnections: 2,
    });
    try {
      readiness.markFailed();
      const failed = await fetch(`http://127.0.0.1:${readiness.port}/health`);
      expect(failed.status).to.equal(503);
      expect(await failed.json()).to.include({ ok: false, status: "failed" });
    } finally {
      await readiness.close();
    }

    expect(controllerReadinessConfigFromEnv({ PORT: "3210" })).to.deep.equal({
      host: "0.0.0.0",
      port: 3210,
      requestTimeoutMs: 5_000,
      maxConnections: 32,
    });
    expect(() => controllerReadinessConfigFromEnv({ PORT: "8899" })).to.throw(
      "PORT must differ",
    );
    expect(() => controllerReadinessConfigFromEnv({
      SURFPOOL_HEALTH_MAX_CONNECTIONS: "257",
    })).to.throw("no greater than 256");
    expect(() => controllerReadinessConfigFromEnv({
      SURFPOOL_HEALTH_REQUEST_TIMEOUT_MS: "0",
    })).to.throw("positive safe integer");
  });

  it("keeps child-owned relays healthy while the controller event loop is blocked", async function () {
    this.timeout(12_000);
    const echo = await startExternalEchoServer();
    const relay = await startSurfpoolRelayWorkerProcess(
      {
        listenHost: "127.0.0.1",
        rpcPort: 0,
        wsPort: 0,
        healthHost: "127.0.0.1",
        healthPort: 0,
        healthRequestTimeoutMs: 1_000,
        healthMaxConnections: 8,
        heartbeatStaleMs: 1_000,
        upstreamConnectTimeoutMs: 500,
        maxConnections: 32,
        targetProbeIntervalMs: 100,
        targetProbeTimeoutMs: 500,
        targetProbeFailureThreshold: 3,
      },
      { startupTimeoutMs: 5_000, shutdownGraceMs: 1_000 },
    );
    let externalClient: ChildProcess | undefined;
    const healthUrl = `http://127.0.0.1:${relay.healthPort}/health`;
    const heartbeat = () => relay.heartbeat({
      atMs: Date.now(),
      drainCalls: 10,
      drainedEvents: 4,
      failedDrains: 0,
      currentIntervalMs: 100,
      lastTickLagMs: 0,
      maxTickLagMs: 25,
    });
    try {
      const starting = await waitForHealthStatus(healthUrl, 503);
      expect(starting).to.include({
        ok: false,
        status: "starting",
        targetsActive: false,
      });

      const stable = await relay.activate({
        rpcUrl: `http://127.0.0.1:${echo.port}`,
        wsUrl: `ws://127.0.0.1:${echo.port}`,
      });
      heartbeat();
      relay.markReady(2);
      const ready = await waitForHealthStatus(healthUrl, 200);
      expect(ready).to.include({
        ok: true,
        status: "ready",
        programCount: 2,
        targetsActive: true,
        heartbeatFresh: true,
      });

      const rpcPort = Number(new URL(stable.rpcUrl).port);
      const clientSource = [
        'const net = require("node:net");',
        'const startedAt = Date.now();',
        'console.log("armed");',
        'setTimeout(() => {',
        `  const socket = net.connect(${rpcPort}, "127.0.0.1");`,
        '  const timeout = setTimeout(() => { console.error("client timeout"); process.exit(2); }, 2000);',
        '  const chunks = [];',
        '  const body = JSON.stringify({ jsonrpc: "2.0", id: "blocked", method: "getHealth" });',
        '  socket.once("connect", () => socket.write(`POST / HTTP/1.1\\r\\nHost: localhost\\r\\nContent-Type: application/json\\r\\nContent-Length: ${Buffer.byteLength(body)}\\r\\nConnection: close\\r\\n\\r\\n${body}`));',
        '  socket.on("data", (data) => chunks.push(data));',
        '  socket.once("end", () => {',
        '    clearTimeout(timeout);',
        '    console.log(JSON.stringify({ data: Buffer.concat(chunks).toString(), elapsedMs: Date.now() - startedAt, completedAtMs: Date.now() }));',
        '  });',
        '  socket.once("error", (error) => { console.error(error); process.exit(3); });',
        '}, 100);',
      ].join("\n");
      externalClient = spawn(process.execPath, ["-e", clientSource], {
        stdio: ["ignore", "pipe", "inherit"],
      });
      let clientOutput = "";
      let resolveArmed: () => void = () => undefined;
      const armed = new Promise<void>((resolvePromise) => {
        resolveArmed = resolvePromise;
      });
      externalClient.stdout?.on("data", (chunk: Buffer) => {
        clientOutput += chunk.toString("utf8");
        if (clientOutput.includes("armed\n")) resolveArmed();
      });
      await withTimeout(armed, "external relay client arm");

      const blockStartedAtMs = Date.now();
      const blockState = new Int32Array(new SharedArrayBuffer(4));
      Atomics.wait(blockState, 0, 0, 500);
      const blockEndedAtMs = Date.now();
      expect(blockEndedAtMs - blockStartedAtMs).to.be.at.least(450);
      await withTimeout(
        once(externalClient, "exit").then(([code]) => {
          if (code !== 0) throw new Error(`External relay client exited ${String(code)}`);
        }),
        "external relay client",
      );
      const resultLine = clientOutput.trim().split("\n").at(-1);
      const result = JSON.parse(resultLine ?? "null") as {
        data: string;
        elapsedMs: number;
        completedAtMs: number;
      };
      expect(result.data).to.include('"result":"ok"');
      expect(result.elapsedMs).to.be.lessThan(450);
      expect(result.completedAtMs).to.be.lessThan(blockEndedAtMs);

      heartbeat();
      const afterBlock = await waitForHealthStatus(healthUrl, 200);
      expect(afterBlock).to.include({ ok: true, heartbeatFresh: true });
      expect((afterBlock.rpc as { acceptedConnections: number }).acceptedConnections)
        .to.be.at.least(1);

      await new Promise((resolvePromise) => setTimeout(resolvePromise, 1_050));
      const stale = await waitForHealthStatus(healthUrl, 503);
      expect(stale).to.include({
        ok: false,
        status: "ready",
        heartbeatFresh: false,
      });
      heartbeat();
      await waitForHealthStatus(healthUrl, 200);

      relay.markStopping();
      const stopping = await waitForHealthStatus(healthUrl, 503);
      expect(stopping).to.include({ ok: false, status: "stopping" });
    } finally {
      if (
        externalClient &&
        externalClient.exitCode === null &&
        externalClient.signalCode === null
      ) {
        externalClient.kill("SIGTERM");
      }
      const firstClose = relay.close();
      const secondClose = relay.close();
      expect(secondClose).to.equal(firstClose);
      await firstClose;
      await stopChild(echo.child);
    }
  });

  it("treats a stable-listener bind failure as fatal to the relay worker", async function () {
    this.timeout(8_000);
    const occupied = createNetServer();
    occupied.listen({ host: "127.0.0.1", port: 0 });
    await once(occupied, "listening");
    const occupiedAddress = occupied.address();
    if (!occupiedAddress || typeof occupiedAddress === "string") {
      throw new Error("Occupied listener did not expose an IP socket");
    }
    const relay = await startSurfpoolRelayWorkerProcess(
      {
        listenHost: "127.0.0.1",
        rpcPort: occupiedAddress.port,
        wsPort: 0,
        healthHost: "127.0.0.1",
        healthPort: 0,
        healthRequestTimeoutMs: 1_000,
        healthMaxConnections: 4,
        heartbeatStaleMs: 1_000,
        upstreamConnectTimeoutMs: 250,
        maxConnections: 8,
        targetProbeIntervalMs: 100,
        targetProbeTimeoutMs: 50,
        targetProbeFailureThreshold: 2,
      },
      { startupTimeoutMs: 3_000, shutdownGraceMs: 500 },
    );
    try {
      let activationError: Error | undefined;
      try {
        await relay.activate({
          rpcUrl: "http://127.0.0.1:1",
          wsUrl: "ws://127.0.0.1:1",
        });
      } catch (error) {
        activationError = error as Error;
      }
      expect(activationError?.message).to.match(/relay worker failed|EADDRINUSE/);
      const fatal = await withTimeout(
        relay.failure.catch((error: Error) => error),
        "relay fatal failure",
      );
      expect(fatal.message).to.match(/relay worker failed|exited/);
    } finally {
      await relay.close();
      occupied.close();
      await once(occupied, "close");
    }
  });

  it("rejects a dynamic target that loops back to its own stable listener", async function () {
    this.timeout(4_000);
    const stablePort = 31_337;
    const relay = await startSurfpoolRelayWorkerProcess(
      {
        listenHost: "127.0.0.1",
        rpcPort: stablePort,
        wsPort: 0,
        healthHost: "127.0.0.1",
        healthPort: 0,
        healthRequestTimeoutMs: 500,
        healthMaxConnections: 2,
        heartbeatStaleMs: 1_000,
        upstreamConnectTimeoutMs: 100,
        maxConnections: 2,
        targetProbeIntervalMs: 1_000,
        targetProbeTimeoutMs: 100,
        targetProbeFailureThreshold: 2,
      },
      { startupTimeoutMs: 2_000, shutdownGraceMs: 250 },
    );
    try {
      let observed: Error | undefined;
      try {
        await relay.activate({
          rpcUrl: `http://127.0.0.1:${stablePort}`,
          wsUrl: "ws://127.0.0.1:1",
        });
      } catch (error) {
        observed = error as Error;
      }
      expect(observed?.message).to.include(
        "must not point back to its stable relay listener",
      );
    } finally {
      await relay.close();
    }
  });

  it("downgrades health and fails after bounded raw-target probe failures", async function () {
    this.timeout(8_000);
    const targets = await startSyntheticSurfpoolTargets();
    const relay = await startSurfpoolRelayWorkerProcess(
      {
        listenHost: "127.0.0.1",
        rpcPort: 0,
        wsPort: 0,
        healthHost: "127.0.0.1",
        healthPort: 0,
        healthRequestTimeoutMs: 1_000,
        healthMaxConnections: 4,
        heartbeatStaleMs: 2_000,
        upstreamConnectTimeoutMs: 250,
        maxConnections: 8,
        targetProbeIntervalMs: 150,
        targetProbeTimeoutMs: 75,
        targetProbeFailureThreshold: 3,
      },
      { startupTimeoutMs: 3_000, shutdownGraceMs: 500 },
    );
    const healthUrl = `http://127.0.0.1:${relay.healthPort}/health`;
    try {
      await relay.activate(targets);
      relay.heartbeat({
        atMs: Date.now(),
        drainCalls: 1,
        drainedEvents: 0,
        failedDrains: 0,
        currentIntervalMs: 250,
        lastTickLagMs: 0,
        maxTickLagMs: 0,
      });
      relay.markReady(2);
      const healthy = await waitForHealthStatus(healthUrl, 200);
      expect(healthy).to.include({ ok: true, targetProbeHealthy: true });
      expect(healthy.targetProbe).to.deep.include({
        successes: 1,
        consecutiveFailures: 0,
      });

      await targets.close();
      let degraded: Record<string, unknown> | undefined;
      const degradedDeadline = Date.now() + 1_000;
      while (Date.now() < degradedDeadline) {
        const response = await fetch(healthUrl);
        const body = await response.json() as Record<string, unknown>;
        const failures = Number(
          (body.targetProbe as { consecutiveFailures?: unknown } | undefined)
            ?.consecutiveFailures ?? 0,
        );
        if (response.status === 200 && failures >= 1 && failures < 3) {
          degraded = body;
          break;
        }
        await new Promise((resolvePromise) => setTimeout(resolvePromise, 10));
      }
      expect(degraded, "below-threshold target failure remains serving").not.to.equal(
        undefined,
      );
      expect(degraded).to.include({ ok: true, targetProbeHealthy: true });

      const unhealthy = await waitForHealthStatus(healthUrl, 503);
      expect(unhealthy).to.include({ ok: false, targetProbeHealthy: false });
      const serialized = JSON.stringify(unhealthy);
      expect(serialized).not.to.include(targets.rpcUrl);
      expect(serialized).not.to.include(targets.wsUrl);

      const fatal = await withTimeout(
        relay.failure.catch((error: Error) => error),
        "raw target fatal threshold",
      );
      expect(fatal.message).to.include("raw Surfpool target failed 3 consecutive");
    } finally {
      await relay.close();
    }
  });

  it("bounds hung RPC/WS probes and starts both transports in parallel", async function () {
    this.timeout(4_000);
    let rpcObservedAt = 0;
    let wsObservedAt = 0;
    const upgradeSockets = new Set<Duplex>();
    const rpc = createHttpServer((_request, _response) => {
      rpcObservedAt = Date.now();
      // Deliberately never respond; the client timeout owns cleanup.
    });
    const websocket = createHttpServer();
    websocket.on("upgrade", (_request, socket) => {
      wsObservedAt = Date.now();
      upgradeSockets.add(socket);
      socket.once("close", () => upgradeSockets.delete(socket));
      // Deliberately never finish the upgrade.
    });
    rpc.listen({ host: "127.0.0.1", port: 0 });
    websocket.listen({ host: "127.0.0.1", port: 0 });
    await Promise.all([once(rpc, "listening"), once(websocket, "listening")]);
    const rpcAddress = rpc.address();
    const wsAddress = websocket.address();
    if (
      !rpcAddress || typeof rpcAddress === "string" ||
      !wsAddress || typeof wsAddress === "string"
    ) {
      throw new Error("Hung probe fixtures did not expose IP sockets");
    }
    const startedAt = Date.now();
    try {
      let observed: Error | undefined;
      try {
        await probeRawSurfpoolTargets({
          rpcUrl: `http://127.0.0.1:${rpcAddress.port}`,
          wsUrl: `ws://127.0.0.1:${wsAddress.port}`,
        }, 150);
      } catch (error) {
        observed = error as Error;
      }
      const elapsedMs = Date.now() - startedAt;
      expect(observed).to.be.instanceOf(Error);
      expect(rpcObservedAt).to.be.greaterThan(0);
      expect(wsObservedAt).to.be.greaterThan(0);
      expect(Math.abs(rpcObservedAt - wsObservedAt)).to.be.lessThan(75);
      expect(elapsedMs).to.be.lessThan(275);
    } finally {
      for (const socket of upgradeSockets) socket.destroy();
      rpc.closeAllConnections();
      websocket.closeAllConnections();
      rpc.close();
      websocket.close();
      await Promise.all([once(rpc, "close"), once(websocket, "close")]);
    }
  });

  it("retains parallel startup phases after an abort race until both settle", async () => {
    const tracker = createStartupPhaseTracker();
    let resolveFirst: () => void = () => undefined;
    let resolveSecond: () => void = () => undefined;
    const first = tracker.track(new Promise<void>((resolvePromise) => {
      resolveFirst = resolvePromise;
    }));
    const second = tracker.track(new Promise<void>((resolvePromise) => {
      resolveSecond = resolvePromise;
    }));
    await Promise.race([
      Promise.all([first, second]),
      Promise.reject(new Error("startup aborted")),
    ]).catch(() => undefined);
    expect(tracker.activeCount()).to.equal(2);

    let settlementFinished = false;
    const settlement = tracker.settle(500).then((value) => {
      settlementFinished = true;
      return value;
    });
    resolveFirst();
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 10));
    expect(settlementFinished).to.equal(false);
    expect(tracker.activeCount()).to.equal(1);
    resolveSecond();
    expect(await settlement).to.equal(true);
    expect(tracker.activeCount()).to.equal(0);

    const hung = createStartupPhaseTracker();
    hung.track(new Promise<void>(() => undefined));
    expect(await hung.settle(20)).to.equal(false);
  });

  it("awaits graceful exit, SIGTERM, then SIGKILL for an ignoring worker", async function () {
    this.timeout(4_000);
    let child: ChildProcess | undefined;
    let observed: Error | undefined;
    const startedAt = Date.now();
    try {
      await startSurfpoolRelayWorkerProcess(
        {
          listenHost: "127.0.0.1",
          rpcPort: 0,
          wsPort: 0,
          healthHost: "127.0.0.1",
          healthPort: 0,
          healthRequestTimeoutMs: 100,
          healthMaxConnections: 1,
          heartbeatStaleMs: 1_000,
          upstreamConnectTimeoutMs: 100,
          maxConnections: 1,
          targetProbeIntervalMs: 1_000,
          targetProbeTimeoutMs: 100,
          targetProbeFailureThreshold: 1,
        },
        {
          modulePath: fileURLToPath(new URL(
            "./fixtures/relay_worker_ignore_signals.mjs",
            import.meta.url,
          )),
          startupTimeoutMs: 100,
          shutdownGraceMs: 100,
          onChildSpawn: (value) => { child = value; },
        },
      );
    } catch (error) {
      observed = error as Error;
    }
    expect(observed?.message).to.include("startup timed out");
    expect(Date.now() - startedAt).to.be.at.least(250);
    expect(child?.signalCode).to.equal("SIGKILL");
  });

  it("pins direct Railway starts and all nested service config paths", () => {
    const starts = new Map([
      ["railway/v2-surfpool-rpc.json", "node dist/v2-fork-lab/surfpool_controller.js"],
      ["railway/v2-rpc-proxy.json", "node dist/v2-fork-lab/rpc_proxy.js"],
      ["railway/v2-fork-api.json", "node dist/v2-fork-lab/api.js"],
    ]);
    for (const [path, expectedStart] of starts) {
      const config = JSON.parse(readFileSync(path, "utf8")) as {
        deploy: { startCommand: string };
      };
      expect(config.deploy.startCommand, path).to.equal(expectedStart);
    }
    const dockerfile = readFileSync("Dockerfile.v2-fork-api", "utf8");
    for (const gate of [
      "DUSK_REQUIRE_EXPLICIT_FORK_SIGNER=true",
      "DUSK_REQUIRE_EXTERNAL_FORK_MARKER=true",
      "DUSK_REQUIRE_PREBOOTSTRAPPED_MARKETS=true",
      "DUSK_REQUIRE_PUBLIC_RPC_URL=true",
      "FORK_REQUIRE_ADMIN_TOKEN=true",
    ]) {
      expect(dockerfile).to.include(gate);
    }
    for (const path of starts.keys()) {
      expect(existsSync(path), path).to.equal(true);
    }
    const runbook = readFileSync("scripts/v2-fork-lab/README.md", "utf8");
    for (const path of starts.keys()) expect(runbook).to.include(`/${path}`);
  });

  it("bounds relay target-monitor and startup-settlement environment values", () => {
    const payer = Keypair.generate();
    const baseController = {
      listenHost: "127.0.0.1",
      rpcPort: 8899,
      wsPort: 8900,
    };
    const relayConfig = relayWorkerConfigFromEnv(
      baseController,
      { host: "127.0.0.1", port: 0, requestTimeoutMs: 1_000, maxConnections: 4 },
      {},
    );
    expect(relayConfig).to.include({
      targetProbeIntervalMs: 5_000,
      targetProbeTimeoutMs: 2_000,
      targetProbeFailureThreshold: 3,
    });
    expect(() => relayWorkerConfigFromEnv(baseController, undefined, {
      SURFPOOL_RELAY_TARGET_PROBE_FAILURE_THRESHOLD: "21",
    })).to.throw("no greater than 20");
    expect(() => hostedControllerConfigFromEnv({
      FORK_LAB_PAYER_KEYPAIR_JSON: JSON.stringify(Array.from(payer.secretKey)),
      FORK_SDK_REMOTE_RPC_URL: "https://api.mainnet-beta.solana.com",
      SURFPOOL_STARTUP_SETTLEMENT_TIMEOUT_MS: "300001",
    })).to.throw("no greater than 300000");
  });

  it("probes the stable WebSocket path with a bounded handshake", async () => {
    const httpServer = createHttpServer();
    const webSocketServer = new WebSocketServer({ server: httpServer });
    httpServer.listen({ host: "127.0.0.1", port: 0 });
    await once(httpServer, "listening");
    const address = httpServer.address();
    if (!address || typeof address === "string") {
      throw new Error("Synthetic WebSocket server did not expose an IP socket");
    }
    try {
      await probeStableWebSocket(`ws://127.0.0.1:${address.port}`, 1_000);
      let observed: Error | undefined;
      try {
        await probeStableWebSocket("ws://127.0.0.1:1", 25);
      } catch (error) {
        observed = error as Error;
      }
      expect(observed?.message).to.include("Surfpool stable WebSocket probe failed");
    } finally {
      for (const client of webSocketServer.clients) client.terminate();
      await new Promise<void>((resolvePromise) => webSocketServer.close(() => resolvePromise()));
      httpServer.closeAllConnections();
      httpServer.close();
      await once(httpServer, "close");
    }
  });

  it("rejects an HTTP upgrade denial without leaking a late socket error", async () => {
    const httpServer = createHttpServer((_request, response) => {
      response.writeHead(403, { "content-type": "text/plain" });
      response.end("denied");
    });
    httpServer.listen({ host: "127.0.0.1", port: 0 });
    await once(httpServer, "listening");
    const address = httpServer.address();
    if (!address || typeof address === "string") {
      throw new Error("Synthetic HTTP server did not expose an IP socket");
    }
    try {
      let observed: Error | undefined;
      try {
        await probeStableWebSocket(`ws://127.0.0.1:${address.port}`, 1_000);
      } catch (error) {
        observed = error as Error;
      }
      expect(observed?.message).to.include("received HTTP 403");
      // Terminating the still-CONNECTING probe socket emits its abort error on
      // a later tick; give it time to surface so a regression fails here
      // instead of crashing an unrelated test.
      await new Promise((resolvePromise) => setTimeout(resolvePromise, 50));
    } finally {
      httpServer.closeAllConnections();
      httpServer.close();
      await once(httpServer, "close");
    }
  });

  it("continuously drains simnet events using counter-only metrics and stops cleanly", async () => {
    const payloadMarker = "must-not-be-retained-or-logged";
    const batches: unknown[][] = [
      [{ payloadMarker }, { payloadMarker }],
      [],
      [{ payloadMarker }],
    ];
    let calls = 0;
    const logs: Array<Record<string, unknown>> = [];
    const drain = startSurfnetEventDrain(
      {
        drainEvents() {
          calls += 1;
          return batches.shift() ?? [];
        },
      },
      {
        intervalMs: 1_000,
        logIntervalMs: 60_000,
        logger: (entry) => logs.push(entry),
      },
    );

    expect(drain.snapshot()).to.deep.equal({
      drainCalls: 1,
      drainedEvents: 2,
      nonEmptyDrains: 1,
      maxBatchSize: 2,
      failedDrains: 0,
      currentIntervalMs: 1_000,
      lastTickLagMs: 0,
      maxTickLagMs: 0,
    });
    expect(drain.drainNow()).to.equal(0);
    expect(drain.drainNow()).to.equal(1);
    const stopped = drain.stop();
    expect(stopped).to.deep.equal({
      drainCalls: 4,
      drainedEvents: 3,
      nonEmptyDrains: 2,
      maxBatchSize: 2,
      failedDrains: 0,
      currentIntervalMs: 1_000,
      lastTickLagMs: 0,
      maxTickLagMs: 0,
    });
    expect(logs.map((entry) => entry.reason)).to.deep.equal([
      "started",
      "stopped",
    ]);
    expect(JSON.stringify(logs)).not.to.include(payloadMarker);

    const callsAfterStop = calls;
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 10));
    expect(calls).to.equal(callsAfterStop);
    expect(drain.drainNow()).to.equal(0);
  });

  it("backs an idle event drain off to 250ms and records controller tick lag", () => {
    let nowMs = 0;
    let scheduled:
      | { callback: () => void; delayMs: number; handle: ReturnType<typeof setTimeout> }
      | undefined;
    const batches: unknown[][] = [[], [], [{}]];
    const fakeSetTimer = ((callback: () => void, delayMs?: number) => {
      const handle = { unref() {} } as unknown as ReturnType<typeof setTimeout>;
      scheduled = { callback, delayMs: Number(delayMs), handle };
      return handle;
    }) as unknown as typeof setTimeout;
    const fakeClearTimer = ((handle: ReturnType<typeof setTimeout>) => {
      if (scheduled?.handle === handle) scheduled = undefined;
    }) as typeof clearTimeout;
    const drain = startSurfnetEventDrain(
      { drainEvents: () => batches.shift() ?? [] },
      {
        minIntervalMs: 100,
        maxIntervalMs: 250,
        logIntervalMs: 60_000,
        now: () => nowMs,
        logger: () => undefined,
        setTimer: fakeSetTimer,
        clearTimer: fakeClearTimer,
      },
    );

    expect(scheduled?.delayMs).to.equal(200);
    nowMs = 250;
    const idleTick = scheduled?.callback;
    expect(idleTick).to.be.a("function");
    idleTick?.();
    expect(drain.snapshot()).to.include({
      currentIntervalMs: 250,
      lastTickLagMs: 50,
      maxTickLagMs: 50,
    });
    expect(scheduled?.delayMs).to.equal(250);

    nowMs = 500;
    const activeTick = scheduled?.callback;
    expect(activeTick).to.be.a("function");
    activeTick?.();
    expect(drain.snapshot()).to.include({
      currentIntervalMs: 100,
      lastTickLagMs: 0,
      maxTickLagMs: 50,
      drainedEvents: 1,
      nonEmptyDrains: 1,
    });
    expect(scheduled?.delayMs).to.equal(100);
    drain.stop();
    expect(scheduled).to.equal(undefined);
  });

  it("fails synchronously when the initial event drain is unavailable", async () => {
    let calls = 0;
    const logs: Array<Record<string, unknown>> = [];
    expect(() =>
      startSurfnetEventDrain(
        {
          drainEvents() {
            calls += 1;
            throw new Error("native event queue unavailable");
          },
        },
        {
          intervalMs: 1,
          logIntervalMs: 60_000,
          logger: (entry) => logs.push(entry),
        },
      )
    ).to.throw(
      "Surfnet event drain failed: native event queue unavailable",
    );
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 10));
    expect(calls).to.equal(1);
    expect(logs.map((entry) => entry.reason)).to.deep.equal([
      "started",
      "failed",
    ]);
  });

  it("stops Surfnet before startup side effects when the first drain fails", async () => {
    const originalStartWithConfig = Surfnet.startWithConfig;
    let stops = 0;
    let funds = 0;
    let deploys = 0;
    const fakeSurfnet = {
      drainEvents() {
        throw new Error("early native event failure");
      },
      stop() {
        stops += 1;
      },
      fundSol() {
        funds += 1;
      },
      deploy() {
        deploys += 1;
        return "unexpected";
      },
    } as unknown as Surfnet;
    Surfnet.startWithConfig = () => fakeSurfnet;
    const payer = Keypair.generate();
    try {
      let observed: Error | undefined;
      try {
        await startHostedSurfpoolController({
          remoteRpcUrl: "https://api.mainnet-beta.solana.com",
          payer,
          payerFundingLamports: 1,
          listenHost: "127.0.0.1",
          rpcPort: 8899,
          wsPort: 8900,
          duskProgramId: "dusk",
          leverageDelegateProgramId: "leverage",
          duskSoPath: "package.json",
          duskIdlPath: "package.json",
          leverageDelegateSoPath: "package.json",
          leverageDelegateIdlPath: "package.json",
          blockProductionMode: "transaction",
        });
      } catch (error) {
        observed = error as Error;
      }
      expect(observed?.message).to.equal(
        "Surfnet event drain failed: early native event failure",
      );
    } finally {
      Surfnet.startWithConfig = originalStartWithConfig;
    }
    expect(stops).to.equal(1);
    expect(funds).to.equal(0);
    expect(deploys).to.equal(0);
  });

  it("requires one shared payer and one remote fork datasource", () => {
    const payer = Keypair.generate();
    const json = JSON.stringify(Array.from(payer.secretKey));
    expect(
      decodeExplicitPayer({ FORK_LAB_PAYER_KEYPAIR_JSON: json }).publicKey.toBase58(),
    ).to.equal(payer.publicKey.toBase58());
    expect(
      decodeExplicitPayer({
        FORK_LAB_PAYER_KEYPAIR_BASE64: Buffer.from(json).toString("base64"),
      }).publicKey.toBase58(),
    ).to.equal(payer.publicKey.toBase58());
    expect(() => decodeExplicitPayer({})).to.throw(
      "requires the same explicit FORK_LAB_PAYER_KEYPAIR_JSON",
    );
    const invalidBytes = JSON.stringify([...payer.secretKey.slice(0, 63), 256]);
    expect(() =>
      decodeExplicitPayer({ FORK_LAB_PAYER_KEYPAIR_JSON: invalidBytes }),
    ).to.throw("JSON byte array");
    expect(() =>
      hostedControllerConfigFromEnv({ FORK_LAB_PAYER_KEYPAIR_JSON: json }),
    ).to.throw("requires FORK_SDK_REMOTE_RPC_URL");
  });

  it("funds the shared controller payer with an explicit safe lamport amount", () => {
    expect(parsePayerFundingLamports()).to.equal(100_000_000_000);
    expect(parsePayerFundingLamports("250000000000")).to.equal(250_000_000_000);
    expect(() => parsePayerFundingLamports("1e11")).to.throw("decimal integer");
    expect(() => parsePayerFundingLamports("0")).to.throw("positive safe integer");
    expect(() => parsePayerFundingLamports("9007199254740992")).to.throw(
      "positive safe integer",
    );
  });

  it("rejects any SDK deployment that changes a pinned program address", () => {
    const params = {
      label: "dusk",
      programId: "expected",
      soPath: "dusk.so",
      idlPath: "dusk.json",
    };
    expect(
      deployExactProgram({ deploy: () => "expected" }, params),
    ).to.equal("expected");
    expect(() =>
      deployExactProgram({ deploy: () => "different" }, params),
    ).to.throw("deployed at different; expected expected");
  });

  it("rewrites only the ProgramData authority header", () => {
    const authority = Keypair.generate().publicKey;
    const binary = Buffer.from("exact-program-binary");
    const original = Buffer.alloc(45 + binary.length, 0xa5);
    original.writeUInt32LE(3, 0);
    original.writeBigUInt64LE(42n, 4);
    original[12] = 0;
    binary.copy(original, 45);

    const rewritten = rewriteUpgradeableProgramDataAuthority(
      original,
      authority,
    );
    expect(rewritten).not.to.equal(original);
    expect(original[12]).to.equal(0);
    expect(rewritten.subarray(0, 12).equals(original.subarray(0, 12))).to.equal(
      true,
    );
    expect(rewritten[12]).to.equal(1);
    expect(new PublicKey(rewritten.subarray(13, 45)).equals(authority)).to.equal(
      true,
    );
    expect(rewritten.subarray(45).equals(binary)).to.equal(true);

    const malformed = Buffer.from(original);
    malformed[12] = 2;
    expect(() =>
      rewriteUpgradeableProgramDataAuthority(malformed, authority),
    ).to.throw("malformed upgrade authority option");
  });

  it("forwards both stable RPC and WS ports and closes them", async () => {
    const upstream = createNetServer((socket) => socket.pipe(socket));
    upstream.listen({ host: "127.0.0.1", port: 0 });
    await once(upstream, "listening");
    const address = upstream.address();
    if (!address || typeof address === "string") {
      throw new Error("Echo server did not expose an IP socket");
    }
    const [rpcProxy, wsProxy] = await Promise.all([
      createTcpProxy({
        label: "test RPC",
        listenHost: "127.0.0.1",
        listenPort: 0,
        targetUrl: `http://127.0.0.1:${address.port}`,
      }),
      createTcpProxy({
        label: "test WebSocket",
        listenHost: "127.0.0.1",
        listenPort: 0,
        targetUrl: `ws://127.0.0.1:${address.port}`,
      }),
    ]);
    try {
      for (const [proxy, payload] of [
        [rpcProxy, "dusk-rpc-proxy-smoke"],
        [wsProxy, "dusk-ws-proxy-smoke"],
      ] as const) {
        const client = connect({ host: "127.0.0.1", port: proxy.port });
        await once(client, "connect");
        client.write(payload);
        const [data] = (await once(client, "data")) as [Buffer];
        expect(data.toString("utf8")).to.equal(payload);
        client.end();
        await once(client, "close");
      }
    } finally {
      await Promise.all([rpcProxy.close(), wsProxy.close()]);
      upstream.close();
      await once(upstream, "close");
    }
  });

  it("keeps the stable listener alive after one upstream connection refusal", async () => {
    const reservation = createNetServer();
    reservation.listen({ host: "127.0.0.1", port: 0 });
    await once(reservation, "listening");
    const reservedAddress = reservation.address();
    if (!reservedAddress || typeof reservedAddress === "string") {
      throw new Error("Port reservation did not expose an IP socket");
    }
    const upstreamPort = reservedAddress.port;
    reservation.close();
    await once(reservation, "close");

    const proxy = await createTcpProxy({
      label: "nonfatal reset test",
      listenHost: "127.0.0.1",
      listenPort: 0,
      targetUrl: `http://127.0.0.1:${upstreamPort}`,
      connectTimeoutMs: 250,
    });
    const upstream = createNetServer((socket) => socket.pipe(socket));
    try {
      const refusedClient = connect({ host: "127.0.0.1", port: proxy.port });
      refusedClient.on("error", () => undefined);
      await withTimeout(once(refusedClient, "close").then(() => undefined), "refused client close");
      expect(proxy.snapshot()).to.include({
        acceptedConnections: 1,
        activeConnections: 0,
        upstreamErrors: 1,
      });
      const listenerStatus = await Promise.race([
        proxy.failure.then(
          () => "failed",
          () => "failed",
        ),
        new Promise<"active">((resolvePromise) =>
          setTimeout(() => resolvePromise("active"), 25)
        ),
      ]);
      expect(listenerStatus).to.equal("active");

      upstream.listen({ host: "127.0.0.1", port: upstreamPort });
      await once(upstream, "listening");
      const recoveredClient = connect({ host: "127.0.0.1", port: proxy.port });
      await once(recoveredClient, "connect");
      recoveredClient.write("relay-recovered");
      const [data] = (await once(recoveredClient, "data")) as [Buffer];
      expect(data.toString("utf8")).to.equal("relay-recovered");
      recoveredClient.end();
      await once(recoveredClient, "close");
      expect(proxy.snapshot().acceptedConnections).to.equal(2);
    } finally {
      await proxy.close();
      if (upstream.listening) {
        upstream.close();
        await once(upstream, "close");
      }
    }
  });

  it("probes a pinned program and exact binary through the stable RPC port", async () => {
    const programId = new PublicKey(
      "358bjJKXWxeAXAzteX1xTgyd9JNnjtzW8fnwCS8Da1mv",
    );
    const [programDataAddress] = PublicKey.findProgramAddressSync(
      [programId.toBuffer()],
      UPGRADEABLE_LOADER,
    );
    const binaryPath = "package.json";
    const binary = readFileSync(binaryPath);
    const programState = Buffer.alloc(36);
    programState.writeUInt32LE(2, 0);
    programDataAddress.toBuffer().copy(programState, 4);
    let programDataState = Buffer.alloc(45 + binary.length);
    programDataState.writeUInt32LE(3, 0);
    programDataState.writeBigUInt64LE(7n, 4);
    programDataState[12] = 0;
    binary.copy(programDataState, 45);

    const rpc = createHttpServer((request, response) => {
      const chunks: Buffer[] = [];
      request.on("data", (chunk: Buffer) => chunks.push(chunk));
      request.on("end", () => {
        const body = JSON.parse(Buffer.concat(chunks).toString("utf8")) as {
          id: number;
          params: [string];
        };
        const address = body.params[0];
        const data = address === programId.toBase58()
          ? programState
          : address === programDataAddress.toBase58()
            ? programDataState
            : undefined;
        const value = data
          ? {
              data: [data.toString("base64"), "base64"],
              executable: address === programId.toBase58(),
              lamports: 1,
              owner: UPGRADEABLE_LOADER.toBase58(),
              rentEpoch: 0,
              space: data.length,
            }
          : null;
        response.writeHead(200, { "content-type": "application/json" });
        response.end(
          JSON.stringify({
            jsonrpc: "2.0",
            id: body.id,
            result: { context: { slot: 7 }, value },
          }),
        );
      });
    });
    rpc.listen({ host: "127.0.0.1", port: 0 });
    await once(rpc, "listening");
    const rpcAddress = rpc.address();
    if (!rpcAddress || typeof rpcAddress === "string") {
      throw new Error("Synthetic RPC did not expose an IP socket");
    }
    const proxy = await createTcpProxy({
      label: "synthetic exact-program RPC",
      listenHost: "127.0.0.1",
      listenPort: 0,
      targetUrl: `http://127.0.0.1:${rpcAddress.port}`,
    });
    try {
      const authority = Keypair.generate().publicKey;
      let writes = 0;
      const aligned = await alignExactUpgradeableProgramAuthority(
        {
          setAccount(address, lamports, data, owner) {
            expect(address).to.equal(programDataAddress.toBase58());
            expect(lamports).to.equal(1);
            expect(owner).to.equal(UPGRADEABLE_LOADER.toBase58());
            programDataState = Buffer.from(data);
            writes += 1;
          },
        },
        {
          rpcUrl: `http://127.0.0.1:${proxy.port}`,
          label: "dusk",
          programId: programId.toBase58(),
          soPath: binaryPath,
          authority,
        },
      );
      expect(aligned).to.deep.equal({
        programDataAddress: programDataAddress.toBase58(),
        authority: authority.toBase58(),
        changed: true,
      });
      expect(writes).to.equal(1);
      expect(programDataState.subarray(45).equals(binary)).to.equal(true);

      const idempotent = await alignExactUpgradeableProgramAuthority(
        { setAccount: () => { writes += 1; } },
        {
          rpcUrl: `http://127.0.0.1:${proxy.port}`,
          label: "dusk",
          programId: programId.toBase58(),
          soPath: binaryPath,
          authority,
        },
      );
      expect(idempotent.changed).to.equal(false);
      expect(writes).to.equal(1);

      const observed = await probeExactUpgradeableProgram({
        rpcUrl: `http://127.0.0.1:${proxy.port}`,
        label: "dusk",
        programId: programId.toBase58(),
        soPath: binaryPath,
        expectedUpgradeAuthority: authority.toBase58(),
      });
      expect(observed.programId).to.equal(programId.toBase58());
      expect(observed.programDataAddress).to.equal(programDataAddress.toBase58());
      expect(observed.binarySha256).to.equal(
        createHash("sha256").update(binary).digest("hex"),
      );
    } finally {
      await proxy.close();
      rpc.closeAllConnections();
      rpc.close();
      await once(rpc, "close");
    }
  });
});
