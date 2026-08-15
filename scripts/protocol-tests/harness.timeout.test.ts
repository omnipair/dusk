import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import http from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, it } from "mocha";
import { Keypair, PublicKey, SystemProgram, Transaction } from "@solana/web3.js";

import {
  ForkApiResponseError,
  MutationOutcomeUncertainError,
  ProtocolTestHarness,
  type ForkConfig,
  type ScenarioDefinition,
} from "./harness.js";

const SYSTEM_PROGRAM = "11111111111111111111111111111111";
const servers: http.Server[] = [];
const outputDirectories: string[] = [];
const originalEnv = {
  FORK_API_URL: process.env.FORK_API_URL,
  PROTOCOL_TEST_HTTP_TIMEOUT_MS: process.env.PROTOCOL_TEST_HTTP_TIMEOUT_MS,
  PROTOCOL_TEST_OUTPUT_DIR: process.env.PROTOCOL_TEST_OUTPUT_DIR,
  PROTOCOL_TEST_SCENARIO_TIMEOUT_MS: process.env.PROTOCOL_TEST_SCENARIO_TIMEOUT_MS,
};
const originalFetch = globalThis.fetch;

function restoreEnv(): void {
  for (const [name, value] of Object.entries(originalEnv)) {
    if (value === undefined) delete process.env[name];
    else process.env[name] = value;
  }
}

function config(rpcUrl: string): ForkConfig {
  return {
    rpcUrl,
    programId: SYSTEM_PROGRAM,
    payer: SYSTEM_PROGRAM,
    market: SYSTEM_PROGRAM,
    markets: [],
    fixtureMode: "mainnet",
    baseMint: SYSTEM_PROGRAM,
    quoteMint: SYSTEM_PROGRAM,
    baseDecimals: 6,
    quoteDecimals: 6,
    baseTokenProgram: SYSTEM_PROGRAM,
    quoteTokenProgram: SYSTEM_PROGRAM,
    ylpMint: SYSTEM_PROGRAM,
    baseHlpMint: SYSTEM_PROGRAM,
    quoteHlpMint: SYSTEM_PROGRAM,
    parameterTimelockSeconds: 0,
    parameterExecutionWindowSeconds: 0,
    seededLiquidity: true,
  };
}

function listen(server: http.Server): Promise<number> {
  return new Promise((resolvePromise, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      server.off("error", reject);
      const address = server.address();
      if (!address || typeof address === "string") {
        reject(new Error("Expected a TCP listener"));
        return;
      }
      resolvePromise(address.port);
    });
  });
}

async function harnessWithApi(
  postResponse: (res: http.ServerResponse) => void,
  timeoutMs = 1_000,
): Promise<ProtocolTestHarness> {
  let forkConfig: ForkConfig;
  const server = http.createServer((req, res) => {
    if (req.method === "GET" && req.url === "/api/v2/fork/config") {
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify({ success: true, data: forkConfig }));
      return;
    }
    if (req.method === "POST") {
      postResponse(res);
      return;
    }
    res.writeHead(404).end();
  });
  servers.push(server);
  const port = await listen(server);
  const apiUrl = `http://127.0.0.1:${port}`;
  forkConfig = config(apiUrl);
  const outputDirectory = mkdtempSync(join(tmpdir(), "dusk-harness-timeout-"));
  outputDirectories.push(outputDirectory);
  process.env.FORK_API_URL = apiUrl;
  process.env.PROTOCOL_TEST_HTTP_TIMEOUT_MS = String(timeoutMs);
  process.env.PROTOCOL_TEST_SCENARIO_TIMEOUT_MS = "5000";
  process.env.PROTOCOL_TEST_OUTPUT_DIR = outputDirectory;
  const harness = new ProtocolTestHarness();
  await harness.initialize();
  return harness;
}

function postScenario(
  id: string,
  onRun?: () => void,
): ScenarioDefinition {
  return {
    id,
    async run(harness) {
      onRun?.();
      await (harness as unknown as {
        post(path: string, body: Record<string, unknown>): Promise<unknown>;
      }).post("/mutation", {});
    },
  };
}

afterEach(async () => {
  globalThis.fetch = originalFetch;
  restoreEnv();
  while (servers.length > 0) {
    const server = servers.pop();
    if (!server) continue;
    server.closeAllConnections();
    if (server.listening) {
      await new Promise<void>((resolvePromise) => server.close(() => resolvePromise()));
    }
  }
  while (outputDirectories.length > 0) {
    const directory = outputDirectories.pop();
    if (directory) rmSync(directory, { recursive: true, force: true });
  }
});

function noSocketHarness(): ProtocolTestHarness {
  const harness = new ProtocolTestHarness();
  harness.currentScenario = { evidence: [] } as unknown as typeof harness.currentScenario;
  (harness as unknown as { persist(): void }).persist = () => {};
  return harness;
}

describe("protocol harness deterministic API rejections", () => {
  it("retains an exact non-2xx status, code, and payload without a socket", async () => {
    const payload = {
      success: false,
      code: "leverage_order_not_actionable",
      error: "Leverage order is no longer actionable",
    };
    globalThis.fetch = async () => Response.json(payload, { status: 409 });
    const harness = noSocketHarness();

    const rejection = await harness.assertApiRejection({
      wallet: "bidder",
      endpoint: "/api/v2/fork/tx/delegated-close-leverage",
      body: { orderId: "1" },
      label: "reject stale delegated close",
      expectedStatus: 409,
      expectedCode: "leverage_order_not_actionable",
    });

    assert.equal(rejection instanceof ForkApiResponseError, true);
    assert.equal(rejection.status, 409);
    assert.equal(rejection.code, "leverage_order_not_actionable");
    assert.deepEqual(rejection.payload, payload);
  });

  it("does not accept uncertainty or infrastructure responses as product rejections", async () => {
    const harness = noSocketHarness();
    const options = {
      wallet: "bidder",
      endpoint: "/api/v2/fork/tx/delegated-close-leverage",
      body: { orderId: "1" },
      label: "reject stale delegated close",
      expectedStatus: 409,
      expectedCode: "leverage_order_not_actionable",
    };

    globalThis.fetch = async () => Response.json({
      success: false,
      uncertainOutcome: true,
      code: "fork_api_route_timeout",
    }, { status: 504 });
    await assert.rejects(
      harness.assertApiRejection(options),
      MutationOutcomeUncertainError,
    );

    globalThis.fetch = async () => Response.json({
      success: false,
      code: "fork_api_unavailable",
    }, { status: 503 });
    await assert.rejects(
      harness.assertApiRejection(options),
      /infrastructure HTTP 503/,
    );
  });
});

describe("protocol harness mutation uncertainty", function () {
  this.timeout(10_000);

  it("stops the run on an explicit uncertainOutcome response", async () => {
    const harness = await harnessWithApi((res) => {
      res.writeHead(504, { "content-type": "application/json" });
      res.end(JSON.stringify({
        success: false,
        uncertainOutcome: true,
        code: "fork_api_route_timeout",
      }));
    });
    let laterScenarioRan = false;
    await assert.rejects(
      harness.runScenarios([
        postScenario("system.bootstrap-clean"),
        postScenario("system.real-wallet-funding", () => {
          laterScenarioRan = true;
        }),
      ]),
      MutationOutcomeUncertainError,
    );
    assert.equal(laterScenarioRan, false);
    assert.equal(harness.report.status, "failed");
    assert.equal(
      harness.report.scenarios.find((scenario) => scenario.id === "system.real-wallet-funding")?.status,
      "not-run",
    );
  });

  it("stops the run when a POST response transport times out", async () => {
    const harness = await harnessWithApi(() => {
      // Deliberately leave the response open past the client deadline.
    }, 30);
    await assert.rejects(
      harness.runScenarios([postScenario("system.bootstrap-clean")]),
      (error: unknown) =>
        error instanceof MutationOutcomeUncertainError &&
        /timed out after 30ms/.test(error.message),
    );
    assert.equal(harness.report.status, "failed");
  });

  it("rebuilds an owner transaction after a pre-submission BlockhashNotFound", async () => {
    const harness = new ProtocolTestHarness();
    harness.config = {
      ...config("http://127.0.0.1:1"),
      programId: Keypair.generate().publicKey.toBase58(),
    };
    const scenario = { evidence: [] as unknown[] };
    harness.currentScenario = scenario as typeof harness.currentScenario;
    let buildCount = 0;
    const builtBlockhashes: string[] = [];
    (harness as unknown as { persist(): void }).persist = () => {};
    (harness as unknown as {
      post(path: string, body: Record<string, unknown>): Promise<Record<string, unknown>>;
    }).post = async (_path, body) => {
      buildCount += 1;
      const owner = new PublicKey(String(body.owner));
      const transaction = new Transaction().add(
        SystemProgram.transfer({ fromPubkey: owner, toPubkey: owner, lamports: 0 }),
      );
      transaction.feePayer = owner;
      transaction.recentBlockhash = Keypair.generate().publicKey.toBase58();
      return {
        action: "test",
        owner: owner.toBase58(),
        market: SYSTEM_PROGRAM,
        rpcUrl: "http://127.0.0.1:1",
        transaction: transaction.serialize({
          requireAllSignatures: false,
          verifySignatures: false,
        }).toString("base64"),
      };
    };
    let simulationCount = 0;
    harness.connection = {
      async simulateTransaction(transaction: Transaction) {
        simulationCount += 1;
        builtBlockhashes.push(String(transaction.recentBlockhash));
        return {
          value: {
            err: simulationCount === 1 ? "BlockhashNotFound" : null,
            logs: [],
            unitsConsumed: 1,
            returnData: null,
          },
        };
      },
    } as unknown as typeof harness.connection;

    const evidence = await harness.execute({
      wallet: "alice",
      endpoint: "/api/v2/fork/tx/swap",
      label: "retry expired blockhash",
      body: { assetIn: "base", exactAssetIn: "1", minAssetOut: "0" },
      submit: false,
    });

    assert.equal(evidence.status, "passed");
    assert.equal(buildCount, 2);
    assert.equal(simulationCount, 2);
    assert.notEqual(builtBlockhashes[0], builtBlockhashes[1]);
    assert.deepEqual(scenario.evidence.at(-1), evidence);
    assert.equal(
      scenario.evidence.some((entry) =>
        typeof entry === "object" &&
        entry !== null &&
        (entry as { label?: unknown }).label === "retry expired blockhash transient fork retries"
      ),
      true,
    );
  });

  it("rebuilds a probe after transient API and blockhash failures", async () => {
    const harness = new ProtocolTestHarness();
    harness.config = {
      ...config("http://127.0.0.1:1"),
      programId: Keypair.generate().publicKey.toBase58(),
    };
    let buildCount = 0;
    (harness as unknown as {
      post(path: string, body: Record<string, unknown>): Promise<Record<string, unknown>>;
    }).post = async (_path, body) => {
      buildCount += 1;
      if (buildCount === 1) {
        throw new Error("POST /api/v2/fork/tx/borrow failed: failed to get genesis hash: Internal error");
      }
      const owner = new PublicKey(String(body.owner));
      const transaction = new Transaction().add(
        SystemProgram.transfer({ fromPubkey: owner, toPubkey: owner, lamports: 0 }),
      );
      transaction.feePayer = owner;
      transaction.recentBlockhash = Keypair.generate().publicKey.toBase58();
      return {
        action: "test",
        owner: owner.toBase58(),
        market: SYSTEM_PROGRAM,
        rpcUrl: "http://127.0.0.1:1",
        transaction: transaction.serialize({
          requireAllSignatures: false,
          verifySignatures: false,
        }).toString("base64"),
      };
    };
    const simulatedBlockhashes: string[] = [];
    harness.connection = {
      async simulateTransaction(transaction: Transaction) {
        simulatedBlockhashes.push(String(transaction.recentBlockhash));
        return simulatedBlockhashes.length === 1
          ? { value: { err: "BlockhashNotFound", logs: [], unitsConsumed: 0 } }
          : {
              value: {
                err: { InstructionError: [0, { Custom: 6000 }] },
                logs: ["Program log: AnchorError occurred. Error Code: ExpectedProbeFailure."],
                unitsConsumed: 42,
              },
            };
      },
    } as unknown as typeof harness.connection;

    const result = await harness.probe("alice", "/api/v2/fork/tx/borrow", {
      positionId: Keypair.generate().publicKey.toBase58(),
    });

    assert.equal(buildCount, 3);
    assert.equal(simulatedBlockhashes.length, 2);
    assert.notEqual(simulatedBlockhashes[0], simulatedBlockhashes[1]);
    assert.equal(result.succeeds, false);
    assert.equal(result.errorCode, "ExpectedProbeFailure");
    assert.equal(result.unitsConsumed, 42);
  });
});
