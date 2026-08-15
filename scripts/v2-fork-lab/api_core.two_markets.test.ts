import { expect } from "chai";
import { afterEach, beforeEach, describe, it } from "mocha";
import {
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { Keypair, PublicKey, SystemProgram } from "@solana/web3.js";
import {
  ForkAdminAuthConfigurationError,
  ForkAdminAuthenticationRequiredError,
  ForkAdminAuthorizationError,
  ForkMutationOutcomeUncertainError,
  LeverageOrderNotActionableError,
  forkMarketPureHelpers,
  requestLamportAirdrop,
  requireForkAdminAuthorization,
  requireForkServerSigningAuthorization,
  resolvePublicRpcUrl,
  route,
  verifyPublicRpcEndpoint,
} from "./api_core.js";

const META_MINT = new PublicKey("METAwkXcqyXKy1AtsSgJ8JiUHwGCafnZL38n3vYmeta");
const USDC_MINT = new PublicKey("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
const ENV_KEYS = [
  "FORK_BOOTSTRAP_MARKETS",
  "DUSK_MARKET_LABEL",
  "DUSK_FORK_PARAMS_HASH",
  "DUSK_MARKET_PARAMS_HASH",
  "DUSK_FORK_PARAMS_HASH_CPMM",
  "DUSK_FORK_PARAMS_HASH_CONCENTRATED",
  "DUSK_AMM_PEAK_DEPTH_NAD",
  "DUSK_AMM_FADE_SCALE_NAD",
] as const;

describe("v2 fork two-market bootstrap helpers", () => {
  const savedEnvironment = new Map<string, string | undefined>();

  beforeEach(() => {
    for (const key of ENV_KEYS) {
      savedEnvironment.set(key, process.env[key]);
      delete process.env[key];
    }
  });

  afterEach(() => {
    for (const key of ENV_KEYS) {
      const value = savedEnvironment.get(key);
      if (value === undefined) delete process.env[key];
      else process.env[key] = value;
    }
    savedEnvironment.clear();
  });

  it("fails closed on a missing or invalid hosted public RPC URL", () => {
    expect(
      resolvePublicRpcUrl(
        { SURFPOOL_RPC_URL: "http://127.0.0.1:8899" },
        "http://127.0.0.1:8899",
      ),
    ).to.equal("http://127.0.0.1:8899");
    expect(() =>
      resolvePublicRpcUrl({
        DUSK_REQUIRE_PUBLIC_RPC_URL: "true",
        SURFPOOL_RPC_URL: "http://v2-surfpool-rpc.railway.internal:8899",
      }),
    ).to.throw(
      "requires an explicit PUBLIC_SURFPOOL_RPC_URL or SURFPOOL_RPC_PROXY_URL",
    );
    expect(() =>
      resolvePublicRpcUrl({
        DUSK_REQUIRE_PUBLIC_RPC_URL: "true",
        PUBLIC_SURFPOOL_RPC_URL: "ws://fork-rpc.example.test",
      }),
    ).to.throw("must use http: or https:");
    expect(
      resolvePublicRpcUrl({
        DUSK_REQUIRE_PUBLIC_RPC_URL: "true",
        SURFPOOL_RPC_PROXY_URL: "https://fork-rpc.example.test/",
      }),
    ).to.equal("https://fork-rpc.example.test");
  });

  it("defaults mainnet to both curves and synthetic fixtures to CPMM", () => {
    expect(forkMarketPureHelpers.configuredMarketKinds("mainnet")).to.deep.equal([
      "cpmm",
      "concentrated",
    ]);
    expect(
      forkMarketPureHelpers.configuredMarketKinds("token2022-fees")
    ).to.deep.equal(["cpmm"]);
    expect(
      forkMarketPureHelpers.configuredMarketKinds("mixed-decimals")
    ).to.deep.equal(["cpmm"]);
  });

  it("builds distinct, correctly classified META/USDC markets", () => {
    const definitions = forkMarketPureHelpers.bootstrapMarketDefinitions(
      "mainnet",
      META_MINT,
      USDC_MINT
    );

    expect(definitions.map(({ label, kind }) => ({ label, kind }))).to.deep.equal([
      { label: "meta-usdc-cpmm", kind: "cpmm" },
      { label: "meta-usdc-concentrated", kind: "concentrated" },
    ]);
    expect(definitions[0].baseMint.equals(META_MINT)).to.equal(true);
    expect(definitions[0].quoteMint.equals(USDC_MINT)).to.equal(true);
    expect(definitions[0].paramsHash.equals(definitions[1].paramsHash)).to.equal(false);
    expect(definitions.map(({ config }) => forkMarketPureHelpers.marketKindFromConfig(config)))
      .to.deep.equal(["cpmm", "concentrated"]);
  });

  it("builds the canonical writable hLP settlement prefix only while active", () => {
    const addresses = {
      ylpMint: new PublicKey(Buffer.alloc(32, 1)),
      baseHlpYlpVault: new PublicKey(Buffer.alloc(32, 2)),
      quoteHlpYlpVault: new PublicKey(Buffer.alloc(32, 3)),
      baseInterestVault: new PublicKey(Buffer.alloc(32, 4)),
      quoteInterestVault: new PublicKey(Buffer.alloc(32, 5)),
    };
    const inactiveMarket = {
      baseHlpVault: { hlpSupply: "0", residualExposure: "0" },
      quoteHlpVault: { hlpSupply: "0", residualExposure: "0" },
    };
    expect(
      forkMarketPureHelpers.hlpSwapRemainingAccountPrefix(
        addresses,
        inactiveMarket,
      ),
    ).to.deep.equal([]);

    const prefix = forkMarketPureHelpers.hlpSwapRemainingAccountPrefix(
      addresses,
      {
        ...inactiveMarket,
        quoteHlpVault: { hlpSupply: "0", residualExposure: "-1" },
      },
    );
    expect(prefix.map(({ pubkey }) => pubkey.toBase58())).to.deep.equal([
      addresses.ylpMint,
      addresses.baseHlpYlpVault,
      addresses.quoteHlpYlpVault,
      addresses.baseInterestVault,
      addresses.quoteInterestVault,
    ].map((pubkey) => pubkey.toBase58()));
    expect(prefix.every(({ isWritable, isSigner }) => isWritable && !isSigner))
      .to.equal(true);
  });

  it("classifies closed leverage orders without hiding invalid live accounts", () => {
    for (const account of [
      null,
      {
        owner: new PublicKey("EPGF9iFrbGnhWgC3To9rC9vxinEYuDHaz4RXgLPvuRkp"),
        executable: false,
        data: Buffer.alloc(0),
      },
    ]) {
      expect(() =>
        forkMarketPureHelpers.requireActionableLeverageOrderAccount(account)
      ).to.throw(LeverageOrderNotActionableError)
        .with.property("code", "leverage_order_not_actionable");
    }

    for (const account of [
      {
        owner: SystemProgram.programId,
        executable: false,
        data: Buffer.from([1]),
      },
      {
        owner: new PublicKey("EPGF9iFrbGnhWgC3To9rC9vxinEYuDHaz4RXgLPvuRkp"),
        executable: true,
        data: Buffer.from([1]),
      },
    ]) {
      expect(() =>
        forkMarketPureHelpers.requireActionableLeverageOrderAccount(account)
      ).to.throw("Leverage order account is invalid")
        .and.not.to.be.instanceOf(LeverageOrderNotActionableError);
    }

    expect(() =>
      forkMarketPureHelpers.requireActionableLeverageOrderAccount({
        owner: new PublicKey("EPGF9iFrbGnhWgC3To9rC9vxinEYuDHaz4RXgLPvuRkp"),
        executable: false,
        data: Buffer.from([1]),
      })
    ).not.to.throw();
  });

  it("funds only plain on-curve wallets and preserves lamports for sol zero", () => {
    const wallet = Keypair.generate().publicKey;
    const plainSystemAccount = {
      owner: SystemProgram.programId,
      executable: false,
      data: Buffer.alloc(0),
    };
    expect(() =>
      forkMarketPureHelpers.requireFundableForkWallet(wallet, null)
    ).not.to.throw();
    expect(() =>
      forkMarketPureHelpers.requireFundableForkWallet(
        wallet,
        plainSystemAccount,
      )
    ).not.to.throw();
    expect(forkMarketPureHelpers.shouldMutateWalletLamports(0)).to.equal(false);
    expect(forkMarketPureHelpers.shouldMutateWalletLamports(1)).to.equal(true);
    expect(forkMarketPureHelpers.lamportsForSolFunding(0.000000001)).to.equal(1);
    expect(() =>
      forkMarketPureHelpers.lamportsForSolFunding(0.0000000001)
    ).to.throw("safe-integer lamport amount");
    expect(
      forkMarketPureHelpers.monotonicForkTokenFundingAmount(10n, 10n),
    ).to.equal(10n);
    expect(
      forkMarketPureHelpers.monotonicForkTokenFundingAmount(10n, 11n),
    ).to.equal(11n);
    expect(
      forkMarketPureHelpers.monotonicForkTokenFundingAmount(10n, 0n),
    ).to.equal(10n);
    expect(
      forkMarketPureHelpers.additiveForkTokenTopUpAmount(10n, 11n),
    ).to.equal(1n);
    expect(
      forkMarketPureHelpers.additiveForkTokenTopUpAmount(10n, 10n),
    ).to.equal(0n);
    expect(
      forkMarketPureHelpers.additiveForkTokenTopUpAmount(10n, 0n),
    ).to.equal(0n);
    expect(
      forkMarketPureHelpers.grossTransferAmountForNet(
        1_000n,
        (amount) => (amount + 99n) / 100n,
      ),
    ).to.equal(1_011n);
    const configuredFundingAssets = {
      baseMint: "trusted-base",
      quoteMint: "trusted-quote",
      baseTokenProgram: "trusted-base-program",
      quoteTokenProgram: "trusted-quote-program",
    };
    expect(
      forkMarketPureHelpers.forkFundingAssetPairMatches(
        configuredFundingAssets,
        configuredFundingAssets,
      ),
    ).to.equal(true);
    expect(
      forkMarketPureHelpers.forkFundingAssetPairMatches(
        {
          ...configuredFundingAssets,
          baseMint: "untrusted-hook-mint",
        },
        configuredFundingAssets,
      ),
    ).to.equal(false);
    expect(
      forkMarketPureHelpers.forkFundingAssetPairMatches(
        {
          ...configuredFundingAssets,
          quoteTokenProgram: "untrusted-token-program",
        },
        configuredFundingAssets,
      ),
    ).to.equal(false);

    const [pda] = PublicKey.findProgramAddressSync(
      [Buffer.from("not-a-wallet")],
      META_MINT,
    );
    expect(() =>
      forkMarketPureHelpers.requireFundableForkWallet(pda, null)
    ).to.throw("requires an on-curve wallet address");

    for (const existingAccount of [
      {
        owner: new PublicKey("BPFLoaderUpgradeab1e11111111111111111111111"),
        executable: true,
        data: Buffer.from("program"),
      },
      {
        owner: new PublicKey("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"),
        executable: false,
        data: Buffer.from("token-account"),
      },
      {
        owner: SystemProgram.programId,
        executable: false,
        data: Buffer.from("nonce-or-other-system-state"),
      },
    ]) {
      expect(() =>
        forkMarketPureHelpers.requireFundableForkWallet(
          wallet,
          existingAccount,
        )
      ).to.throw("requires an absent or plain SystemProgram-owned wallet account");
    }
  });

  it("marks a failed airdrop confirmation uncertain without a fallback write", async () => {
    const wallet = Keypair.generate().publicKey;
    let requested = 0;
    let confirmed = 0;
    try {
      await requestLamportAirdrop(
        {
          async requestAirdrop(pubkey: PublicKey, lamports: number) {
            expect(pubkey.equals(wallet)).to.equal(true);
            expect(lamports).to.equal(1);
            requested += 1;
            return "airdrop-signature";
          },
          async confirmTransaction() {
            confirmed += 1;
            throw new Error("confirmation unavailable");
          },
        } as any,
        wallet,
        1,
      );
      expect.fail("expected the ambiguous airdrop to be rejected");
    } catch (error) {
      expect(error).to.be.instanceOf(ForkMutationOutcomeUncertainError);
      expect((error as ForkMutationOutcomeUncertainError).uncertainOutcome)
        .to.equal(true);
    }
    expect(requested).to.equal(1);
    expect(confirmed).to.equal(1);
  });

  it("does not report a confirmed failed airdrop as successful funding", async () => {
    const wallet = Keypair.generate().publicKey;
    try {
      await requestLamportAirdrop(
        {
          async requestAirdrop() {
            return "failed-airdrop-signature";
          },
          async confirmTransaction() {
            return {
              context: { slot: 1 },
              value: { err: { InstructionError: [0, "Custom"] } },
            };
          },
        } as any,
        wallet,
        1,
      );
      expect.fail("expected the failed airdrop status to be rejected");
    } catch (error) {
      expect(error).to.be.instanceOf(ForkMutationOutcomeUncertainError);
      expect((error as Error).message).to.include("failed-airdrop-signature");
    }
  });

  it("keeps private Surfpool coordinates out of public discovery payloads", () => {
    const health = forkMarketPureHelpers.forkHealthPayload({
      publicRpcUrl: "https://rpc.example.test",
      publicRpcVerified: true,
      publicRpcFilterVerified: true,
      runtimeInitialized: true,
      runtimeError: null,
      prebootstrappedMarketCount: 2,
    });
    expect(health).to.deep.equal({
      ok: true,
      publicRpcUrl: "https://rpc.example.test",
      publicRpcVerified: true,
      publicRpcFilterVerified: true,
      runtimeInitialized: true,
      runtimeError: null,
      prebootstrappedMarketCount: 2,
    });
    expect(health).not.to.have.property("rpcUrl");

    const configRpc = forkMarketPureHelpers.publicForkRpcPayload(
      "https://rpc.example.test",
    );
    expect(configRpc).to.deep.equal({ rpcUrl: "https://rpc.example.test" });
    expect(configRpc).not.to.have.property("privateRpcUrl");
  });

  it("verifies that the public RPC is filtered and points to the same fork", async () => {
    const programId = Keypair.generate().publicKey;
    const markerData = Buffer.alloc(32, 7);
    const genesisHash = "same-private-genesis";
    const namespace = "public-probe-test";
    const forkId = forkMarketPureHelpers.deriveForkGenerationId(
      namespace,
      genesisHash,
      programId.toBase58(),
      markerData,
    );
    const methods: string[] = [];
    const fetchImpl = async (_input: any, init?: RequestInit) => {
      const request = JSON.parse(String(init?.body)) as { method: string };
      methods.push(request.method);
      if (request.method === "surfnet_duskPublicFilterProbe") {
        return new Response(JSON.stringify({
          jsonrpc: "2.0",
          id: "dusk-public-rpc-readiness",
          error: { code: -32099, message: "blocked" },
        }), { status: 403 });
      }
      if (request.method === "getHealth") {
        return Response.json({
          jsonrpc: "2.0",
          id: "dusk-public-rpc-readiness",
          result: "ok",
        });
      }
      return Response.json({
        jsonrpc: "2.0",
        id: "dusk-public-rpc-readiness",
        result: {
          context: { slot: 1 },
          value: {
            owner: programId.toBase58(),
            data: [markerData.toString("base64"), "base64"],
          },
        },
      });
    };
    expect(
      await verifyPublicRpcEndpoint(
        { genesisHash, forkId },
        {
          publicRpcUrl: "https://rpc.example.test",
          fetchImpl: fetchImpl as typeof fetch,
          timeoutMs: 1_000,
          namespace,
          programId,
        },
      ),
    ).to.deep.equal({ genesisHash, forkId, filterVerified: true });
    expect(methods).to.deep.equal([
      "surfnet_duskPublicFilterProbe",
      "getHealth",
      "getAccountInfo",
    ]);

    const unfilteredFetch = (async () => Response.json({
      jsonrpc: "2.0",
      id: "dusk-public-rpc-readiness",
      error: { code: -32601, message: "method not found" },
    })) as typeof fetch;
    try {
      await verifyPublicRpcEndpoint(
        { genesisHash, forkId },
        {
          publicRpcUrl: "https://raw-surfpool.example.test",
          fetchImpl: unfilteredFetch,
          timeoutMs: 1_000,
          namespace,
          programId,
        },
      );
      expect.fail("expected an unfiltered public RPC to be rejected");
    } catch (error) {
      expect((error as Error).message).to.include("not the filtered");
    }
  });

  it("pins one successful genesis read per exact fork generation", async () => {
    let reads = 0;
    let confirmations = 0;
    const reader = forkMarketPureHelpers.createGenerationPinnedGenesisHashReader(
      async () => {
        reads += 1;
        return "pinned-genesis";
      },
    );
    const confirmA = async () => {
      confirmations += 1;
      return "generation-a";
    };

    expect(await Promise.all([
      reader.read("generation-a", confirmA),
      reader.read("generation-a", confirmA),
      reader.read("generation-a", confirmA),
    ]))
      .to.deep.equal([
        "pinned-genesis",
        "pinned-genesis",
        "pinned-genesis",
      ]);
    expect(await reader.read("generation-a", confirmA)).to.equal("pinned-genesis");
    expect(reads).to.equal(1);
    expect(confirmations).to.equal(1);

    expect(
      await reader.read("generation-b", async () => "generation-b"),
    ).to.equal("pinned-genesis");
    expect(reads).to.equal(2);

    reader.reset();
    expect(
      await reader.read("generation-b", async () => "generation-b"),
    ).to.equal("pinned-genesis");
    expect(reads).to.equal(3);
  });

  it("retries a failed genesis read before pinning the first success", async () => {
    let reads = 0;
    const reader = forkMarketPureHelpers.createGenerationPinnedGenesisHashReader(
      async () => {
        reads += 1;
        if (reads === 1) throw new Error("temporary upstream failure");
        return "recovered-genesis";
      },
    );

    try {
      await reader.read("generation-a", async () => "generation-a");
      expect.fail("expected the initial genesis read to fail");
    } catch (error) {
      expect((error as Error).message).to.equal("temporary upstream failure");
    }
    expect(
      await reader.read("generation-a", async () => "generation-a"),
    ).to.equal("recovered-genesis");
    expect(
      await reader.read("generation-a", async () => "generation-a"),
    ).to.equal("recovered-genesis");
    expect(reads).to.equal(2);
  });

  it("rejects and retries when the fork generation changes during a genesis read", async () => {
    let reads = 0;
    const reader = forkMarketPureHelpers.createGenerationPinnedGenesisHashReader(
      async () => {
        reads += 1;
        return `genesis-${reads}`;
      },
    );

    try {
      await reader.read("generation-a", async () => "generation-b");
      expect.fail("expected a generation race to be rejected");
    } catch (error) {
      expect((error as Error).message).to.include(
        "fork generation changed while pinning its genesis hash",
      );
    }
    expect(
      await reader.read("generation-b", async () => "generation-b"),
    ).to.equal("genesis-2");
    expect(reads).to.equal(2);
  });

  it("uses distinct kind-specific parameter-hash overrides", () => {
    process.env.DUSK_FORK_PARAMS_HASH_CPMM = "11".repeat(32);
    process.env.DUSK_FORK_PARAMS_HASH_CONCENTRATED = "22".repeat(32);

    const definitions = forkMarketPureHelpers.bootstrapMarketDefinitions(
      "mainnet",
      META_MINT,
      USDC_MINT
    );
    expect(definitions.map(({ paramsHash }) => paramsHash.toString("hex"))).to.deep.equal([
      "11".repeat(32),
      "22".repeat(32),
    ]);
  });

  it("rejects a shared parameter hash when both markets are requested", () => {
    process.env.DUSK_FORK_PARAMS_HASH = "33".repeat(32);
    expect(() =>
      forkMarketPureHelpers.bootstrapMarketDefinitions("mainnet", META_MINT, USDC_MINT)
    ).to.throw("cannot identify two markets");
  });

  it("accepts a shared parameter hash for an explicit single market", () => {
    process.env.FORK_BOOTSTRAP_MARKETS = "concentrated";
    process.env.DUSK_FORK_PARAMS_HASH = "44".repeat(32);
    const definitions = forkMarketPureHelpers.bootstrapMarketDefinitions(
      "mainnet",
      META_MINT,
      USDC_MINT
    );
    expect(definitions).to.have.length(1);
    expect(definitions[0].kind).to.equal("concentrated");
    expect(definitions[0].paramsHash.toString("hex")).to.equal("44".repeat(32));
  });

  it("rejects invalid market selection and duplicate hash overrides", () => {
    process.env.FORK_BOOTSTRAP_MARKETS = "invalid";
    expect(() => forkMarketPureHelpers.configuredMarketKinds("mainnet")).to.throw(
      "expected cpmm, concentrated, or both"
    );

    process.env.FORK_BOOTSTRAP_MARKETS = "both";
    process.env.DUSK_FORK_PARAMS_HASH_CPMM = "55".repeat(32);
    process.env.DUSK_FORK_PARAMS_HASH_CONCENTRATED = "55".repeat(32);
    expect(() =>
      forkMarketPureHelpers.bootstrapMarketDefinitions("mainnet", META_MINT, USDC_MINT)
    ).to.throw("distinct params hashes");
  });

  it("classifies raw and API-shaped AMM configs consistently", () => {
    expect(
      forkMarketPureHelpers.marketKindFromConfig({
        amm: { peakDepthNad: "200000000000", fadeScaleNad: "100000000" },
      })
    ).to.equal("concentrated");
    expect(
      forkMarketPureHelpers.marketKindFromConfig({
        amm: { peak_depth_nad: "0", fade_scale_nad: "0" },
      })
    ).to.equal("cpmm");
  });

  it("publishes the reviewed canonical IDL digest independent of formatting", () => {
    const idl = JSON.parse(
      readFileSync(resolve("packages/dusk-sdk/src/idl_v2.json"), "utf8"),
    );
    const canonical = forkMarketPureHelpers.canonicalJson(idl);
    expect(forkMarketPureHelpers.sha256(canonical)).to.equal(
      "fc4ece4350fd9cdb3564cc4a157c8f0f7eafccfe7fe1cb46b43e34e8ad13eed6",
    );
    expect(
      forkMarketPureHelpers.sha256(
        forkMarketPureHelpers.canonicalJson({
          z: [3, 2, 1],
          a: { y: true, x: null },
        }),
      ),
    ).to.equal(
      forkMarketPureHelpers.sha256(
        forkMarketPureHelpers.canonicalJson({
          a: { x: null, y: true },
          z: [3, 2, 1],
        }),
      ),
    );
  });

  it("vendors the exact reviewed leverage-delegate IDL for the API image", () => {
    const vendored = readFileSync(
      resolve("scripts/v2-fork-lab/idl/leverage_delegate.json"),
      "utf8",
    );
    expect(forkMarketPureHelpers.sha256(vendored)).to.equal(
      "948b9475071daa318cbc9f0e3cc2f8d150191a4ec3dc54e63a661ea489cc5f4a",
    );
    expect(
      forkMarketPureHelpers.sha256(
        forkMarketPureHelpers.canonicalJson(JSON.parse(vendored)),
      ),
    ).to.equal(
      "b2051072480d8da1912c3e4a818f9ca105a8013e33dc5bc2912ab19ceeee5ba1",
    );
  });

  it("changes fork identity when the on-chain generation marker changes", () => {
    const first = forkMarketPureHelpers.deriveForkGenerationId(
      "dusk-surfpool",
      "genesis",
      "program",
      Buffer.alloc(32, 1),
    );
    const repeated = forkMarketPureHelpers.deriveForkGenerationId(
      "dusk-surfpool",
      "genesis",
      "program",
      Buffer.alloc(32, 1),
    );
    const reset = forkMarketPureHelpers.deriveForkGenerationId(
      "dusk-surfpool",
      "genesis",
      "program",
      Buffer.alloc(32, 2),
    );
    expect(first).to.equal(repeated);
    expect(first).to.match(/^surfpool-[0-9a-f]{64}$/);
    expect(reset).not.to.equal(first);
  });

  it("treats slot progress as stable but rejects immutable deployment drift", () => {
    const deployment = {
      schemaVersion: "dusk-deployment.v1",
      network: "surfpool",
      forkSourceNetwork: "mainnet",
      genesisHash: "genesis",
      forkId: "fork-a",
      programId: "dusk",
      programDataAddress: "dusk-data",
      programDataSlot: "100",
      programUpgradeAuthority: "dusk-authority",
      leverageDelegateProgramId: "delegate",
      leverageDelegateProgramDataAddress: "delegate-data",
      leverageDelegateProgramDataSlot: "101",
      leverageDelegateUpgradeAuthority: "delegate-authority",
      idlSha256: "canonical",
      idlRawSha256: "raw",
      leverageDelegateIdlSha256: "delegate-canonical",
      leverageDelegateIdlRawSha256: "delegate-raw",
      commitment: "confirmed" as const,
      sourceSlot: 10,
      observedAt: "2026-08-12T00:00:00.000Z",
      apiStartedAt: "2026-08-12T00:00:00.000Z",
      buildRevision: "revision",
      programBinarySha256: "program-binary",
      leverageDelegateBinarySha256: "delegate-binary",
    };
    const fingerprint =
      forkMarketPureHelpers.deploymentIdentityFingerprint(deployment);
    expect(
      forkMarketPureHelpers.deploymentIdentityFingerprint({
        ...deployment,
        sourceSlot: 11,
        observedAt: "2026-08-12T00:00:01.000Z",
      }),
    ).to.equal(fingerprint);
    expect(
      forkMarketPureHelpers.deploymentIdentityFingerprint({
        ...deployment,
        forkId: "fork-b",
      }),
    ).not.to.equal(fingerprint);
    expect(
      forkMarketPureHelpers.deploymentIdentityFingerprint({
        ...deployment,
        programDataSlot: "102",
      }),
    ).not.to.equal(fingerprint);
    expect(
      forkMarketPureHelpers.deploymentIdentityFingerprint({
        ...deployment,
        leverageDelegateUpgradeAuthority: "rotated-authority",
      }),
    ).not.to.equal(fingerprint);
  });

  it("parses immutable and upgradeable ProgramData loader headers", () => {
    const authority = new PublicKey("11111111111111111111111111111111");
    const upgradeable = Buffer.alloc(45);
    upgradeable.writeUInt32LE(3, 0);
    upgradeable.writeBigUInt64LE(123n, 4);
    upgradeable[12] = 1;
    authority.toBuffer().copy(upgradeable, 13);
    expect(
      forkMarketPureHelpers.parseUpgradeableProgramDataHeader(upgradeable),
    ).to.deep.equal({
      programDataSlot: "123",
      upgradeAuthority: authority.toBase58(),
    });

    const immutable = Buffer.alloc(45);
    immutable.writeUInt32LE(3, 0);
    immutable.writeBigUInt64LE(456n, 4);
    expect(
      forkMarketPureHelpers.parseUpgradeableProgramDataHeader(immutable),
    ).to.deep.equal({ programDataSlot: "456", upgradeAuthority: null });

    expect(() =>
      forkMarketPureHelpers.parseUpgradeableProgramDataHeader(Buffer.alloc(12)),
    ).to.throw("Malformed upgradeable ProgramData header");
    const invalidOption = Buffer.from(immutable);
    invalidOption[12] = 2;
    expect(() =>
      forkMarketPureHelpers.parseUpgradeableProgramDataHeader(invalidOption),
    ).to.throw("Malformed upgradeable ProgramData authority option");
  });

  it("carries the newest account-context slot into the response envelope", () => {
    expect(
      forkMarketPureHelpers.maximumResponseSourceSlot({
        data: {
          markets: [
            { state: { sourceSlot: 41, healthSourceSlot: 44 } },
            { state: { sourceSlot: 43 } },
          ],
          ignored: { sourceSlot: null },
        },
      }),
    ).to.equal(44);
    expect(
      forkMarketPureHelpers.maximumResponseSourceSlot({ sourceSlot: -1 }),
    ).to.equal(0);
    expect(
      forkMarketPureHelpers.maximumResponseSourceSlot({
        data: { run: { sourceSlot: Number.MAX_SAFE_INTEGER } },
      }),
    ).to.equal(0);
  });

  it("deduplicates durable bootstrap evidence by genuine transaction signature", () => {
    expect(
      forkMarketPureHelpers.mergeBootstrapTransactions(
        [{
          label: "initialize market meta-usdc-cpmm",
          signature: "signature-a",
          instructions: ["initialize_market"],
        }],
        [
          {
            label: "stale duplicate label",
            signature: "signature-a",
            instructions: ["initialize_market", "initialize_market"],
          },
          {
            label: "initialize market meta-usdc-concentrated",
            signature: "signature-b",
            instructions: ["initialize_market"],
          },
        ],
      ),
    ).to.deep.equal([
      {
        label: "initialize market meta-usdc-cpmm",
        signature: "signature-a",
        instructions: ["initialize_market"],
      },
      {
        label: "initialize market meta-usdc-concentrated",
        signature: "signature-b",
        instructions: ["initialize_market"],
      },
    ]);
  });
});

describe("v2 fork admin authorization", () => {
  let savedAdminToken: string | undefined;

  beforeEach(() => {
    savedAdminToken = process.env.FORK_ADMIN_TOKEN;
    delete process.env.FORK_ADMIN_TOKEN;
  });

  afterEach(() => {
    if (savedAdminToken === undefined) delete process.env.FORK_ADMIN_TOKEN;
    else process.env.FORK_ADMIN_TOKEN = savedAdminToken;
  });

  it("does not require an admin token for non-admin routes", () => {
    expect(() =>
      requireForkAdminAuthorization({ url: "/api/v2/fork/config" }),
    ).not.to.throw();
  });

  it("requires admin auth for every server-signed transaction request", () => {
    expect(() =>
      requireForkServerSigningAuthorization(
        { url: "/api/v2/fork/tx/update-futarchy-authority" },
        { bootstrapSigned: true },
      ),
    ).to.throw(ForkAdminAuthConfigurationError);
    expect(() =>
      requireForkServerSigningAuthorization(
        { url: "/api/v2/fork/tx/bootstrap-rejection" },
        { kind: "futarchy-duplicate" },
      ),
    ).to.throw(ForkAdminAuthConfigurationError);
    expect(() =>
      requireForkServerSigningAuthorization(
        { url: "/api/v2/fork/tx/create-market" },
        {},
      ),
    ).to.throw(ForkAdminAuthConfigurationError);
    expect(() =>
      requireForkServerSigningAuthorization(
        { url: "/api/v2/fork/tx/update-futarchy-authority" },
        { bootstrapSigned: false },
      ),
    ).not.to.throw();
    expect(() =>
      requireForkServerSigningAuthorization(
        { url: "/api/v2/fork/tx/update-futarchy-authority" },
        { bootstrapSigned: "true" },
      ),
    ).to.throw(ForkAdminAuthConfigurationError);
  });

  it("accepts only literal booleans for bootstrapSigned", () => {
    expect(forkMarketPureHelpers.bootstrapSignedFromBody(undefined)).to.equal(
      false,
    );
    expect(forkMarketPureHelpers.bootstrapSignedFromBody(false)).to.equal(
      false,
    );
    expect(forkMarketPureHelpers.bootstrapSignedFromBody(true)).to.equal(true);
    expect(() =>
      forkMarketPureHelpers.bootstrapSignedFromBody("true"),
    ).to.throw("bootstrapSigned must be a boolean");
    expect(() => forkMarketPureHelpers.bootstrapSignedFromBody(1)).to.throw(
      "bootstrapSigned must be a boolean",
    );
  });

  it("accepts the admin token for server-signed transaction requests", () => {
    process.env.FORK_ADMIN_TOKEN = "expected-secret";
    expect(() =>
      requireForkServerSigningAuthorization(
        {
          url: "/api/v2/fork/tx/create-parameter-proposal",
          headers: { "x-fork-admin-token": "expected-secret" },
        },
        { bootstrapSigned: true },
      ),
    ).not.to.throw();
  });

  it("fails closed with a typed 503 when hosted admin auth is not configured", () => {
    let caught: unknown;
    try {
      requireForkAdminAuthorization({ url: "/api/v2/fork/admin/time-travel" });
    } catch (error) {
      caught = error;
    }
    expect(caught).to.be.instanceOf(ForkAdminAuthConfigurationError);
    expect(caught).to.include({
      code: "fork_admin_auth_not_configured",
      httpStatus: 503,
    });
  });

  it("rejects a direct admin route call before it tries to observe Surfpool", async () => {
    let caught: unknown;
    try {
      await route(
        { method: "POST", url: "/api/v2/fork/admin/time-travel" } as any,
        { seconds: 30 },
      );
    } catch (error) {
      caught = error;
    }
    expect(caught).to.be.instanceOf(ForkAdminAuthConfigurationError);
  });

  it("rejects a direct server-signed route before it observes Surfpool", async () => {
    let caught: unknown;
    try {
      await route(
        {
          method: "POST",
          url: "/api/v2/fork/tx/update-futarchy-authority",
        } as any,
        { bootstrapSigned: true, newAuthority: META_MINT.toBase58() },
      );
    } catch (error) {
      caught = error;
    }
    expect(caught).to.be.instanceOf(ForkAdminAuthConfigurationError);
  });

  it("rejects create-market before it observes Surfpool or prepares LP mints", async () => {
    let caught: unknown;
    try {
      await route(
        {
          method: "POST",
          url: "/api/v2/fork/tx/create-market",
        } as any,
        {},
      );
    } catch (error) {
      caught = error;
    }
    expect(caught).to.be.instanceOf(ForkAdminAuthConfigurationError);
  });

  it("distinguishes a missing token (401) from an invalid token (403)", () => {
    process.env.FORK_ADMIN_TOKEN = "expected-secret";

    expect(() =>
      requireForkAdminAuthorization({
        url: "/api/v2/fork/admin/time-travel",
        headers: {},
      }),
    )
      .to.throw(ForkAdminAuthenticationRequiredError)
      .with.property("httpStatus", 401);

    expect(() =>
      requireForkAdminAuthorization({
        url: "/api/v2/fork/admin/time-travel",
        headers: { "x-fork-admin-token": "wrong-secret" },
      }),
    )
      .to.throw(ForkAdminAuthorizationError)
      .with.property("httpStatus", 403);
  });

  it("accepts the exact token through Node-style and get()-style header helpers", () => {
    process.env.FORK_ADMIN_TOKEN = "expected-secret";
    expect(() =>
      requireForkAdminAuthorization({
        url: "/api/v2/fork/admin/time-travel?seconds=30",
        headers: { "X-Fork-Admin-Token": "expected-secret" } as any,
      }),
    ).not.to.throw();
    expect(() =>
      requireForkAdminAuthorization({
        url: "/api/v2/fork/admin/another-operation",
        headers: {
          get: (name: string) =>
            name.toLowerCase() === "x-fork-admin-token"
              ? "expected-secret"
              : null,
        },
      }),
    ).not.to.throw();
  });

  it("uses fixed-length digest comparison and rejects duplicated header values", () => {
    process.env.FORK_ADMIN_TOKEN = "expected-secret";
    expect(
      forkMarketPureHelpers.constantTimeTokenEquals("same", "same"),
    ).to.equal(true);
    expect(
      forkMarketPureHelpers.constantTimeTokenEquals("short", "much-longer"),
    ).to.equal(false);
    expect(() =>
      requireForkAdminAuthorization({
        url: "/api/v2/fork/admin/time-travel",
        headers: {
          "x-fork-admin-token": ["expected-secret", "expected-secret"],
        },
      }),
    ).to.throw(ForkAdminAuthorizationError);
  });
});

describe("v2 fork deterministic controller keypairs", () => {
  const environmentKeys = [
    "FORK_LAB_PAYER_KEYPAIR_JSON",
    "FORK_LAB_PAYER_KEYPAIR_BASE64",
    "FORK_LAB_PAYER_KEYPAIR",
    "ANCHOR_WALLET",
    "FORK_LAB_STATE_DIR",
  ] as const;
  const savedEnvironment = new Map<string, string | undefined>();
  const temporaryDirectories: string[] = [];

  beforeEach(() => {
    for (const key of environmentKeys) {
      savedEnvironment.set(key, process.env[key]);
      delete process.env[key];
    }
  });

  afterEach(() => {
    for (const directory of temporaryDirectories.splice(0)) {
      rmSync(directory, { recursive: true, force: true });
    }
    for (const key of environmentKeys) {
      const value = savedEnvironment.get(key);
      if (value === undefined) delete process.env[key];
      else process.env[key] = value;
    }
    savedEnvironment.clear();
  });

  function temporaryStateDirectory(): string {
    const directory = mkdtempSync(join(tmpdir(), "dusk-fork-keypair-test-"));
    temporaryDirectories.push(directory);
    return directory;
  }

  it("derives replica-stable, domain-separated LP keys from the shared signer", () => {
    const controller = Keypair.fromSeed(Uint8Array.from({ length: 32 }, (_, i) => i + 1));
    process.env.FORK_LAB_PAYER_KEYPAIR_JSON = JSON.stringify(
      Array.from(controller.secretKey),
    );

    process.env.FORK_LAB_STATE_DIR = temporaryStateDirectory();
    const first = forkMarketPureHelpers.loadOrCreateKeypair("mint-market-ylp");
    process.env.FORK_LAB_STATE_DIR = temporaryStateDirectory();
    const second = forkMarketPureHelpers.loadOrCreateKeypair("mint-market-ylp");
    const other = forkMarketPureHelpers.loadOrCreateKeypair("mint-market-base-hlp");

    expect(first.keypair.publicKey.toBase58()).to.equal(
      second.keypair.publicKey.toBase58(),
    );
    expect(first.keypair.publicKey.toBase58()).to.equal(
      forkMarketPureHelpers
        .deriveForkKeypair(controller, "mint-market-ylp")
        .publicKey.toBase58(),
    );
    expect(other.keypair.publicKey.toBase58()).not.to.equal(
      second.keypair.publicKey.toBase58(),
    );
  });

  it("rejects a persisted hosted key that disagrees with derivation", () => {
    const controller = Keypair.fromSeed(new Uint8Array(32).fill(7));
    process.env.FORK_LAB_PAYER_KEYPAIR_JSON = JSON.stringify(
      Array.from(controller.secretKey),
    );
    process.env.FORK_LAB_STATE_DIR = temporaryStateDirectory();
    const persisted = forkMarketPureHelpers.loadOrCreateKeypair("mint-market-ylp");
    writeFileSync(
      persisted.path,
      JSON.stringify(Array.from(Keypair.generate().secretKey)),
    );

    expect(() =>
      forkMarketPureHelpers.loadOrCreateKeypair("mint-market-ylp"),
    ).to.throw("does not match deterministic controller derivation");
  });

  it("keeps the persisted random local fallback when no signer is configured", () => {
    process.env.FORK_LAB_STATE_DIR = temporaryStateDirectory();
    const first = forkMarketPureHelpers.loadOrCreateKeypair("mint-local");
    const repeated = forkMarketPureHelpers.loadOrCreateKeypair("mint-local");
    expect(first.created).to.equal(true);
    expect(repeated.created).to.equal(false);
    expect(repeated.keypair.publicKey.toBase58()).to.equal(
      first.keypair.publicKey.toBase58(),
    );
  });

  it("rejects malformed market config values before preparation can mutate", () => {
    expect(() =>
      forkMarketPureHelpers.marketConfigFromBody({
        swapFeeBps: "not-an-integer",
      }),
    ).to.throw("config.swapFeeBps must be an integer");
  });
});
