import { createHash, randomBytes } from "node:crypto";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { Surfnet } from "@solana/surfpool";
import { PublicKey } from "@solana/web3.js";

import {
  createRpcProxyServer,
  rpcProxyConfigFromEnv,
  type RpcProxyRuntime,
} from "./rpc_proxy.js";
import {
  alignExactUpgradeableProgramAuthority,
  startSurfnetEventDrain,
} from "./surfpool_controller.js";

const DUSK_PROGRAM_ID = "358bjJKXWxeAXAzteX1xTgyd9JNnjtzW8fnwCS8Da1mv";
const LEVERAGE_DELEGATE_PROGRAM_ID = "EPGF9iFrbGnhWgC3To9rC9vxinEYuDHaz4RXgLPvuRkp";
const TOKEN_METADATA_PROGRAM_ID = "metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s";

function requiredPath(label: string, candidates: string[]): string {
  const resolvedCandidates = candidates.map((candidate) => resolve(candidate));
  const selected = resolvedCandidates.find(existsSync);
  if (!selected) {
    throw new Error(`${label} not found; tried ${resolvedCandidates.join(", ")}`);
  }
  return selected;
}

function deployExactProgram(params: {
  surfnet: Surfnet;
  label: string;
  programId: string;
  soPath: string;
  idlPath: string;
}) {
  const deployedProgramId = params.surfnet.deploy({
    programId: params.programId,
    soPath: params.soPath,
    idlPath: params.idlPath,
  });
  if (deployedProgramId !== params.programId) {
    throw new Error(
      `${params.label} deployed at ${deployedProgramId}, expected ${params.programId}`
    );
  }
}

function fileSha256(path: string): string {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

async function surfpoolRpc(rpcUrl: string, method: string, params: unknown[]) {
  const response = await fetch(rpcUrl, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }),
  });
  const payload = (await response.json()) as {
    result?: unknown;
    error?: unknown;
  };
  if (payload.error)
    throw new Error(`${method} failed: ${JSON.stringify(payload.error)}`);
  return payload.result;
}

export async function runSurfpoolSdkE2E() {
  const remoteRpcUrl =
    process.env.FORK_SDK_REMOTE_RPC_URL ?? process.env.SURFPOOL_DATASOURCE_RPC_URL;
  const fixture = process.env.FORK_MARKET_FIXTURE ?? "mainnet";
  if (fixture === "mainnet" && !remoteRpcUrl) {
    throw new Error(
      "The META/USDC mainnet fixture requires FORK_SDK_REMOTE_RPC_URL or " +
        "SURFPOOL_DATASOURCE_RPC_URL"
    );
  }

  const duskSoPath = requiredPath("Dusk program binary", [
    process.env.DUSK_PROGRAM_SO_PATH ?? "target/deploy/dusk.so",
  ]);
  const duskIdlPath = requiredPath("Dusk IDL", [
    process.env.DUSK_IDL_PATH ?? "target/idl/dusk.json",
    "packages/dusk-sdk/src/idl_v2.json",
  ]);
  const leverageDelegateSoPath = requiredPath("leverage_delegate program binary", [
    process.env.DUSK_LEVERAGE_DELEGATE_SO_PATH ?? "target/deploy/leverage_delegate.so",
  ]);
  const leverageDelegateIdlPath = requiredPath("leverage_delegate IDL", [
    process.env.DUSK_LEVERAGE_DELEGATE_IDL_PATH ?? "target/idl/leverage_delegate.json",
  ]);
  const tokenMetadataFixtureSoPath = !remoteRpcUrl
    ? requiredPath("Token Metadata fixture binary", [
        process.env.TOKEN_METADATA_FIXTURE_SO_PATH ??
          "target/deploy/token_metadata_fixture.so",
      ])
    : undefined;

  const stateDirectory = mkdtempSync(join(tmpdir(), "dusk-surfpool-sdk-e2e-"));
  const surfnet = Surfnet.startWithConfig({
    offline: !remoteRpcUrl,
    remoteRpcUrl,
    blockProductionMode: "transaction",
  });
  let shutdownForkRuntime: (() => void) | undefined;
  let publicRpcProxy: RpcProxyRuntime | undefined;
  // Surfnet stalls its RPC surface when its event channel fills; drain it
  // continuously exactly like the hosted controller does.
  const eventDrain = startSurfnetEventDrain(surfnet);
  eventDrain.failure.catch(() => undefined);

  try {
    surfnet.fundSol(
      surfnet.payer,
      Number(process.env.FORK_E2E_SOL ?? "100") * 1_000_000_000
    );
    deployExactProgram({
      surfnet,
      label: "dusk",
      programId: DUSK_PROGRAM_ID,
      soPath: duskSoPath,
      idlPath: duskIdlPath,
    });
    deployExactProgram({
      surfnet,
      label: "leverage_delegate",
      programId: LEVERAGE_DELEGATE_PROGRAM_ID,
      soPath: leverageDelegateSoPath,
      idlPath: leverageDelegateIdlPath,
    });
    if (tokenMetadataFixtureSoPath) {
      const deployedProgramId = surfnet.deploy({
        programId: TOKEN_METADATA_PROGRAM_ID,
        soPath: tokenMetadataFixtureSoPath,
      });
      if (deployedProgramId !== TOKEN_METADATA_PROGRAM_ID) {
        throw new Error(
          `Token Metadata fixture deployed at ${deployedProgramId}, expected ${TOKEN_METADATA_PROGRAM_ID}`,
        );
      }
    }

    // Align both ProgramData upgrade authorities to the payer exactly like the
    // hosted controller does, so futarchy initialization succeeds as a real
    // transaction instead of relying on the env-gated account-seed fallback.
    await alignExactUpgradeableProgramAuthority(surfnet, {
      rpcUrl: surfnet.rpcUrl,
      label: "dusk",
      programId: DUSK_PROGRAM_ID,
      soPath: duskSoPath,
      authority: new PublicKey(surfnet.payer),
    });
    await alignExactUpgradeableProgramAuthority(surfnet, {
      rpcUrl: surfnet.rpcUrl,
      label: "leverage_delegate",
      programId: LEVERAGE_DELEGATE_PROGRAM_ID,
      soPath: leverageDelegateSoPath,
      authority: new PublicKey(surfnet.payer),
    });

    // The public identity check requires the filtered proxy, not raw Surfnet;
    // run the real proxy in-process exactly like the hosted topology.
    publicRpcProxy = createRpcProxyServer(rpcProxyConfigFromEnv({
      FORK_ADMIN_TOKEN: randomBytes(32).toString("hex"),
      SURFPOOL_RPC_URL: surfnet.rpcUrl,
      SURFPOOL_WS_URL: surfnet.wsUrl,
    }));
    const publicRpcPort = await new Promise<number>((resolvePromise, reject) => {
      const server = publicRpcProxy!.server;
      server.once("error", reject);
      server.listen(0, "127.0.0.1", () => {
        server.off("error", reject);
        const address = server.address();
        if (!address || typeof address === "string") {
          reject(new Error("Public RPC proxy did not expose a TCP listener"));
          return;
        }
        resolvePromise(address.port);
      });
    });

    process.env.SURFPOOL_RPC_URL = surfnet.rpcUrl;
    process.env.PUBLIC_SURFPOOL_RPC_URL = `http://127.0.0.1:${publicRpcPort}`;
    process.env.FORK_LAB_STATE_DIR = stateDirectory;
    process.env.FORK_LAB_PAYER_KEYPAIR_JSON = JSON.stringify(
      Array.from(surfnet.payerSecretKey)
    );
    process.env.FORK_MARKET_FIXTURE = fixture;
    const { seedForkGeneration } = await import("./seed_fork_generation.mjs");
    await seedForkGeneration();
    process.env.DUSK_REQUIRE_EXTERNAL_FORK_MARKER = "true";
    process.env.DUSK_REQUIRE_EXPLICIT_FORK_SIGNER = "true";

    const forkApi = await import("./api_core.js");
    shutdownForkRuntime = forkApi.shutdownForkRuntime;
    const { bootstrapForkMarkets, localE2E, route } = forkApi;
    delete process.env.DUSK_REQUIRE_PREBOOTSTRAPPED_MARKETS;
    await bootstrapForkMarkets();
    shutdownForkRuntime();
    process.env.DUSK_REQUIRE_PREBOOTSTRAPPED_MARKETS = "true";
    const coldHealthResponse = (await route(
      { method: "GET", url: "/health" } as any,
      {},
    )) as any;
    if (
      coldHealthResponse.ok !== true ||
      coldHealthResponse.deployment?.schemaVersion !== "dusk-deployment.v1"
    ) {
      throw new Error(
        "Cold fork API health did not verify the full deployment identity",
      );
    }
    const result = await localE2E();
    const configResponse = (await route(
      { method: "GET", url: "/api/v2/fork/config" } as any,
      {},
    )) as any;
    const marketsResponse = (await route(
      { method: "GET", url: "/api/v2/markets?limit=100&offset=0" } as any,
      {},
    )) as any;
    const primaryMarket = result.markets[0]?.market;
    if (!primaryMarket)
      throw new Error("Surfpool E2E produced no primary market");
    const detailResponse = (await route(
      { method: "GET", url: `/api/v2/markets/${primaryMarket}` } as any,
      {},
    )) as any;
    const swapBuildResponse = (await route(
      { method: "POST", url: "/api/v2/fork/tx/swap" } as any,
      {
        owner: surfnet.payer,
        market: primaryMarket,
        assetIn: "base",
        exactAssetIn: "1",
        minAssetOut: "0",
      },
    )) as any;
    if (
      detailResponse.data?.marketAddress !== primaryMarket ||
      typeof swapBuildResponse.data?.transaction !== "string" ||
      Buffer.from(swapBuildResponse.data.transaction, "base64").length === 0
    ) {
      throw new Error(
        "Fork API detail or swap transaction-build route failed E2E validation",
      );
    }
    if (configResponse.deployment?.schemaVersion !== "dusk-deployment.v1") {
      throw new Error(
        "Fork API did not return the versioned deployment envelope",
      );
    }
    if (
      configResponse.deployment.forkId !== marketsResponse.deployment?.forkId ||
      configResponse.deployment.idlSha256 !==
        marketsResponse.deployment?.idlSha256
    ) {
      throw new Error(
        "Fork API deployment identity changed between config and market reads",
      );
    }
    const marketObservations = marketsResponse.data?.markets ?? [];
    if (
      marketObservations.length !== result.markets.length ||
      marketObservations.some(
        (market: any) =>
          !Number.isSafeInteger(market.state?.sourceSlot) ||
          !Number.isSafeInteger(market.state?.healthSourceSlot) ||
          (market.state?.observedAt !== null &&
            !Number.isFinite(Date.parse(market.state.observedAt))) ||
          (market.state?.healthObservedAt !== null &&
            !Number.isFinite(Date.parse(market.state.healthObservedAt))),
      )
    ) {
      throw new Error(
        "Fork API market reads are not bound to authoritative RPC observations",
      );
    }
    const newestMarketSourceSlot = Math.max(
      ...marketObservations.flatMap((market: any) => [
        market.state.sourceSlot,
        market.state.healthSourceSlot,
      ]),
    );
    if (marketsResponse.deployment.sourceSlot < newestMarketSourceSlot) {
      throw new Error(
        "Fork API envelope slot predates its market account observations",
      );
    }
    if (
      configResponse.deployment.idlSha256 !==
        "fc4ece4350fd9cdb3564cc4a157c8f0f7eafccfe7fe1cb46b43e34e8ad13eed6" ||
      configResponse.deployment.leverageDelegateIdlSha256 !==
        "b2051072480d8da1912c3e4a818f9ca105a8013e33dc5bc2912ab19ceeee5ba1" ||
      configResponse.deployment.programBinarySha256 !==
        fileSha256(duskSoPath) ||
      configResponse.deployment.leverageDelegateBinarySha256 !==
        fileSha256(leverageDelegateSoPath) ||
      typeof configResponse.deployment.programDataAddress !== "string" ||
      typeof configResponse.deployment.leverageDelegateProgramDataAddress !==
        "string" ||
      !/^\d+$/.test(configResponse.deployment.programDataSlot) ||
      !/^\d+$/.test(
        configResponse.deployment.leverageDelegateProgramDataSlot,
      ) ||
      !/^[0-9a-f]{64}$/.test(configResponse.deployment.deploymentIdentitySha256)
    ) {
      throw new Error(
        "Fork API deployment envelope does not attest the exact deployed artifacts",
      );
    }

    const reservesBeforeReplicaRestart = new Map(
      marketObservations.map((market: any) => [
        market.marketAddress,
        `${market.state.baseLiveReserve}:${market.state.quoteLiveReserve}`,
      ]),
    );
    const payerBalanceBeforeReplicaRestart = (await surfpoolRpc(
      surfnet.rpcUrl,
      "getBalance",
      [surfnet.payer, { commitment: "confirmed" }],
    )) as { value: number };
    if (fixture === "mainnet") {
      rmSync(join(stateDirectory, "state.json"), { force: true });
    }
    shutdownForkRuntime?.();
    const replicaRestartMarkets = (await route(
      { method: "GET", url: "/api/v2/markets?limit=100&offset=0" } as any,
      {},
    )) as any;
    const payerBalanceAfterReplicaRestart = (await surfpoolRpc(
      surfnet.rpcUrl,
      "getBalance",
      [surfnet.payer, { commitment: "confirmed" }],
    )) as { value: number };
    if (
      payerBalanceAfterReplicaRestart.value !==
      payerBalanceBeforeReplicaRestart.value
    ) {
      throw new Error("Cold API replica mutated the RPC controller payer");
    }
    for (const market of replicaRestartMarkets.data?.markets ?? []) {
      const beforeReserves = reservesBeforeReplicaRestart.get(
        market.marketAddress,
      );
      const afterReserves = `${market.state.baseLiveReserve}:${market.state.quoteLiveReserve}`;
      if (beforeReserves !== afterReserves) {
        throw new Error(
          `Cold API replica changed seeded liquidity for ${market.marketAddress}`,
        );
      }
    }

    const preResetForkId = configResponse.deployment.forkId;
    await surfpoolRpc(surfnet.rpcUrl, "surfnet_resetNetwork", []);
    surfnet.fundSol(
      surfnet.payer,
      Number(process.env.FORK_E2E_SOL ?? "100") * 1_000_000_000,
    );
    deployExactProgram({
      surfnet,
      label: "dusk",
      programId: DUSK_PROGRAM_ID,
      soPath: duskSoPath,
      idlPath: duskIdlPath,
    });
    deployExactProgram({
      surfnet,
      label: "leverage_delegate",
      programId: LEVERAGE_DELEGATE_PROGRAM_ID,
      soPath: leverageDelegateSoPath,
      idlPath: leverageDelegateIdlPath,
    });
    if (tokenMetadataFixtureSoPath) {
      const resetMetadataProgramId = surfnet.deploy({
        programId: TOKEN_METADATA_PROGRAM_ID,
        soPath: tokenMetadataFixtureSoPath,
      });
      if (resetMetadataProgramId !== TOKEN_METADATA_PROGRAM_ID) {
        throw new Error(
          "Token Metadata fixture address changed after Surfpool reset",
        );
      }
    }
    // The reset produced fresh ProgramData; re-align both upgrade authorities
    // so the post-reset futarchy initialization is a real transaction too.
    await alignExactUpgradeableProgramAuthority(surfnet, {
      rpcUrl: surfnet.rpcUrl,
      label: "dusk",
      programId: DUSK_PROGRAM_ID,
      soPath: duskSoPath,
      authority: new PublicKey(surfnet.payer),
    });
    await alignExactUpgradeableProgramAuthority(surfnet, {
      rpcUrl: surfnet.rpcUrl,
      label: "leverage_delegate",
      programId: LEVERAGE_DELEGATE_PROGRAM_ID,
      soPath: leverageDelegateSoPath,
      authority: new PublicKey(surfnet.payer),
    });
    await seedForkGeneration();
    delete process.env.DUSK_REQUIRE_PREBOOTSTRAPPED_MARKETS;
    shutdownForkRuntime?.();
    await bootstrapForkMarkets();
    shutdownForkRuntime?.();
    process.env.DUSK_REQUIRE_PREBOOTSTRAPPED_MARKETS = "true";
    const resetHealthResponse = (await route(
      { method: "GET", url: "/health" } as any,
      {},
    )) as any;
    const resetConfigResponse = (await route(
      { method: "GET", url: "/api/v2/fork/config" } as any,
      {},
    )) as any;
    const resetMarketsResponse = (await route(
      { method: "GET", url: "/api/v2/markets?limit=100&offset=0" } as any,
      {},
    )) as any;
    if (
      resetHealthResponse.deployment?.forkId === preResetForkId ||
      resetConfigResponse.deployment?.forkId === preResetForkId ||
      resetConfigResponse.deployment?.forkId !==
        resetMarketsResponse.deployment?.forkId ||
      resetConfigResponse.deployment?.deploymentIdentitySha256 ===
        configResponse.deployment.deploymentIdentitySha256 ||
      resetConfigResponse.deployment.programBinarySha256 !==
        fileSha256(duskSoPath) ||
      resetConfigResponse.deployment.leverageDelegateBinarySha256 !==
        fileSha256(leverageDelegateSoPath)
    ) {
      throw new Error(
        "Fork API did not re-key deployment identity after Surfpool reset/redeploy",
      );
    }
    return {
      ...result,
      mode: "surfpool-sdk",
      rpcUrl: surfnet.rpcUrl,
      deployedPrograms: [
        DUSK_PROGRAM_ID,
        LEVERAGE_DELEGATE_PROGRAM_ID,
        ...(!remoteRpcUrl ? [TOKEN_METADATA_PROGRAM_ID] : []),
      ],
      apiEvidence: {
        deployment: configResponse.deployment,
        responseSlots: {
          coldHealth: coldHealthResponse.deployment.sourceSlot,
          config: configResponse.deployment.sourceSlot,
          markets: marketsResponse.deployment.sourceSlot,
          detail: detailResponse.deployment.sourceSlot,
          transactionBuild: swapBuildResponse.deployment.sourceSlot,
        },
        reset: {
          previousForkId: preResetForkId,
          nextForkId: resetConfigResponse.deployment.forkId,
          healthSlot: resetHealthResponse.deployment.sourceSlot,
          marketCount: resetMarketsResponse.data?.markets?.length ?? 0,
        },
        marketSourceSlots: marketObservations.map((market: any) => ({
          market: market.marketAddress,
          sourceSlot: market.state.sourceSlot,
          healthSourceSlot: market.state.healthSourceSlot,
          observedAt: market.state.observedAt,
          healthObservedAt: market.state.healthObservedAt,
        })),
      },
    };
  } finally {
    shutdownForkRuntime?.();
    await publicRpcProxy?.close();
    eventDrain.stop();
    surfnet.stop();
    rmSync(stateDirectory, { recursive: true, force: true });
  }
}

runSurfpoolSdkE2E()
  .then((result) => {
    console.log(JSON.stringify(result, null, 2));
  })
  .catch((error) => {
    console.error(error);
    process.exitCode = 1;
  });
