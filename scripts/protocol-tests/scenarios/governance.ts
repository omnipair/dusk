import { Keypair, PublicKey } from "@solana/web3.js";

import { decodePreviewBorrowPositionReturnData } from "../../../packages/dusk-sdk/src/preview.js";
import { formatUnits, type ProtocolTestHarness, type ScenarioDefinition } from "../harness.js";
import type { TransactionEvidence } from "../types.js";

const reduceOnlyLoanPositionId = Keypair.generate().publicKey;
const reduceOnlyLeveragePositionId = Keypair.generate().publicKey;
const reduceOnlyBlockedOpenPositionId = Keypair.generate().publicKey;
const reduceOnlyLiquidationPositionId = Keypair.generate().publicKey;

function walletAddress(harness: ProtocolTestHarness, wallet: string): string {
  return harness.wallet(wallet).publicKey.toBase58();
}

function previewData(evidence: TransactionEvidence): [string, BufferEncoding] {
  const data = evidence.simulation.returnData?.data;
  if (!data) throw new Error(`${evidence.label} did not return preview data`);
  return data as [string, BufferEncoding];
}

function stateValue(
  market: Awaited<ReturnType<ProtocolTestHarness["market"]>>,
  key: string
): bigint {
  const value = market.state[key];
  if (value === undefined) throw new Error(`Market state does not expose ${key}`);
  return BigInt(value);
}

function eventAmount(data: Record<string, { toString(): string }>, key: string): bigint {
  const value = data[key];
  if (value === undefined) throw new Error(`Protocol auction event does not expose ${key}`);
  return BigInt(value.toString());
}

async function expectReduceOnlyRejection(
  harness: ProtocolTestHarness,
  options: {
    wallet: string;
    endpoint: string;
    label: string;
    body: Record<string, unknown>;
  }
): Promise<void> {
  const evidence = await harness.execute({ ...options, expected: "failure" });
  harness.assertEqual(`${options.label} is gated by reduce-only`, evidence.errorCode, "ReduceOnlyMode");
}

export const GOVERNANCE_SCENARIOS: ScenarioDefinition[] = [
  {
    id: "governance.authority-rotation",
    async run(harness) {
      const alice = walletAddress(harness, "alice");
      const bob = walletAddress(harness, "bob");
      const initial = await harness.futarchy();
      harness.assertTrue("bootstrap authority is distinct from test users", initial.authority !== alice);

      const unauthorizedBeforeHandoff = await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/update-futarchy-authority",
        label: "reject futarchy rotation by non-authority",
        expected: "failure",
        body: { newAuthority: bob },
      });
      harness.assertEqual(
        "non-authority rotation fails at signer binding",
        unauthorizedBeforeHandoff.errorCode,
        "InvalidFutarchyAuthority"
      );

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-futarchy-authority",
        label: "bootstrap authority hands governance to Alice",
        apiSigned: true,
        body: { bootstrapSigned: true, newAuthority: alice },
      });
      harness.assertEqual("Alice receives futarchy authority", (await harness.futarchy()).authority, alice);

      const bootstrapAfterHandoff = await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-futarchy-authority",
        label: "reject former bootstrap authority after handoff",
        apiSigned: true,
        expected: "failure",
        body: { bootstrapSigned: true, newAuthority: bob },
      });
      harness.assertEqual(
        "former bootstrap signer loses governance control",
        bootstrapAfterHandoff.errorCode,
        "InvalidFutarchyAuthority"
      );

      const bobBeforeRotation = await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/update-futarchy-authority",
        label: "reject Bob before Alice rotates authority",
        expected: "failure",
        body: { newAuthority: bob },
      });
      harness.assertEqual(
        "pending recipient has no authority before rotation",
        bobBeforeRotation.errorCode,
        "InvalidFutarchyAuthority"
      );

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-futarchy-authority",
        label: "Alice rotates futarchy authority to Bob",
        body: { newAuthority: bob },
      });
      harness.assertEqual("Bob receives futarchy authority", (await harness.futarchy()).authority, bob);

      const aliceAfterRotation = await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-futarchy-authority",
        label: "reject former Alice authority after rotation",
        expected: "failure",
        body: { newAuthority: alice },
      });
      harness.assertEqual(
        "former authority loses control immediately",
        aliceAfterRotation.errorCode,
        "InvalidFutarchyAuthority"
      );

      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/update-futarchy-authority",
        label: "Bob returns futarchy authority to Alice",
        body: { newAuthority: alice },
      });
      harness.assertEqual("Alice is final governance authority", (await harness.futarchy()).authority, alice);
    },
  },
  {
    id: "governance.revenue-and-recipients",
    async run(harness) {
      const before = await harness.futarchy();
      const alice = walletAddress(harness, "alice");
      harness.assertEqual("revenue scenario starts under Alice authority", before.authority, alice);

      const unauthorizedRevenue = await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/update-protocol-revenue",
        label: "reject protocol revenue update by non-authority",
        expected: "failure",
        body: { swapBps: 1 },
      });
      harness.assertEqual(
        "unauthorized revenue update fails at signer binding",
        unauthorizedRevenue.errorCode,
        "InvalidFutarchyAuthority"
      );

      const invalidSwap = await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-protocol-revenue",
        label: "reject protocol swap share above 100 percent",
        expected: "failure",
        body: { swapBps: 10_001 },
      });
      harness.assertEqual("swap revenue cap is enforced", invalidSwap.errorCode, "InvalidSwapFeeBps");

      const invalidInterest = await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-protocol-revenue",
        label: "reject protocol interest share above 100 percent",
        expected: "failure",
        body: { interestBps: 10_001 },
      });
      harness.assertEqual(
        "interest revenue cap is enforced",
        invalidInterest.errorCode,
        "InvalidInterestFeeBps"
      );

      const invalidReferral = await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-protocol-revenue",
        label: "reject referral interest-share cap above 100 percent",
        expected: "failure",
        body: { maxReferralInterestShareBps: 10_001 },
      });
      harness.assertEqual(
        "referral interest-share cap is enforced",
        invalidReferral.errorCode,
        "InvalidReferralInterestShareBps"
      );

      const invalidDistribution = await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-protocol-revenue",
        label: "reject revenue distribution that does not sum to 100 percent",
        expected: "failure",
        body: {
          revenueDistribution: {
            futarchyTreasuryBps: 2_000,
            buybacksVaultBps: 3_000,
            teamTreasuryBps: 4_999,
          },
        },
      });
      harness.assertEqual(
        "revenue distribution sum is enforced",
        invalidDistribution.errorCode,
        "InvalidDistribution"
      );

      const invalidAuctionSplit = await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-protocol-revenue",
        label: "reject protocol auction split that does not sum to 100 percent",
        expected: "failure",
        body: { protocolAuctionSplit: { feeAuctionBps: 6_000, buybackAuctionBps: 3_999 } },
      });
      harness.assertEqual(
        "protocol auction split sum is enforced",
        invalidAuctionSplit.errorCode,
        "InvalidDistribution"
      );
      harness.assertEqual("rejected revenue updates preserve state", await harness.futarchy(), before);

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-protocol-revenue",
        label: "apply valid protocol revenue boundaries",
        body: {
          swapBps: 10_000,
          interestBps: 2_345,
          maxReferralInterestShareBps: 10_000,
          revenueDistribution: {
            futarchyTreasuryBps: 2_000,
            buybacksVaultBps: 3_000,
            teamTreasuryBps: 5_000,
          },
          protocolAuctionSplit: { feeAuctionBps: 6_000, buybackAuctionBps: 4_000 },
        },
      });
      const updated = await harness.futarchy();
      harness.assertEqual("maximum swap revenue share is stored", updated.revenueShare.swapBps, 10_000);
      harness.assertEqual("interest revenue share is stored", updated.revenueShare.interestBps, 2_345);
      harness.assertEqual(
        "maximum referral interest share is stored",
        updated.maxReferralInterestShareBps,
        10_000
      );
      harness.assertEqual(
        "revenue distribution is stored exactly",
        updated.revenueDistribution,
        { futarchyTreasuryBps: 2_000, buybacksVaultBps: 3_000, teamTreasuryBps: 5_000 }
      );
      harness.assertEqual(
        "auction split is stored exactly",
        updated.protocolAuctionSplit,
        { feeAuctionBps: 6_000, buybackAuctionBps: 4_000 }
      );

      const unauthorizedRecipients = await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/update-revenue-recipients",
        label: "reject revenue recipient update by non-authority",
        expected: "failure",
        body: { teamTreasury: walletAddress(harness, "bob") },
      });
      harness.assertEqual(
        "unauthorized recipient update fails at signer binding",
        unauthorizedRecipients.errorCode,
        "InvalidFutarchyAuthority"
      );

      const replacementRecipients = {
        futarchyTreasury: walletAddress(harness, "bob"),
        buybacksVault: walletAddress(harness, "referrer"),
        teamTreasury: walletAddress(harness, "liquidator"),
      };
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-revenue-recipients",
        label: "update all protocol revenue recipients",
        body: replacementRecipients,
      });
      harness.assertEqual(
        "all protocol revenue recipients update exactly",
        (await harness.futarchy()).recipients,
        replacementRecipients
      );

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-revenue-recipients",
        label: "restore protocol revenue recipients",
        body: before.recipients,
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-protocol-revenue",
        label: "restore protocol revenue configuration",
        body: {
          swapBps: before.revenueShare.swapBps,
          interestBps: before.revenueShare.interestBps,
          maxReferralInterestShareBps: before.maxReferralInterestShareBps,
          revenueDistribution: before.revenueDistribution,
          protocolAuctionSplit: before.protocolAuctionSplit,
        },
      });
      harness.assertEqual("governance revenue state restores exactly", await harness.futarchy(), before);
    },
  },
  {
    id: "governance.market-authorities",
    async run(harness) {
      const initial = await harness.market();
      const alice = walletAddress(harness, "alice");
      const bob = walletAddress(harness, "bob");
      const referrer = walletAddress(harness, "referrer");

      const unauthorized = await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/set-manager",
        label: "reject manager rotation by non-manager",
        expected: "failure",
        body: { newManager: bob },
      });
      harness.assertEqual("manager signer binding is enforced", unauthorized.errorCode, "InvalidMarketManager");

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/set-manager",
        label: "bootstrap manager schedules handoff to Alice",
        apiSigned: true,
        body: { bootstrapSigned: true, newManager: alice },
      });
      let market = await harness.market();
      harness.assertEqual("manager does not change when handoff is scheduled", market.manager, initial.manager);
      harness.assertEqual("manager handoff records pending Alice", market.pendingManager.newAuthority, alice);
      harness.assertEqual("manager handoff is active", market.pendingManager.active, true);

      const earlyHandoff = await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/set-manager",
        label: "reject manager handoff before timelock",
        apiSigned: true,
        expected: "failure",
        body: { bootstrapSigned: true, newManager: alice },
      });
      harness.assertEqual(
        "manager timelock cannot execute early",
        earlyHandoff.errorCode,
        "GovernanceTimelockNotReady"
      );
      await harness.timeTravel(0, harness.config.governanceDelaySlots + 10);
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/set-manager",
        label: "bootstrap manager completes handoff to Alice",
        apiSigned: true,
        body: { bootstrapSigned: true, newManager: alice },
      });
      market = await harness.market();
      harness.assertEqual("Alice becomes market manager after timelock", market.manager, alice);
      harness.assertEqual("applied manager handoff clears pending state", market.pendingManager.active, false);

      const formerManager = await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/set-operator",
        label: "reject former bootstrap manager after handoff",
        apiSigned: true,
        expected: "failure",
        body: { bootstrapSigned: true, newOperator: bob },
      });
      harness.assertEqual("former manager loses role control", formerManager.errorCode, "InvalidMarketManager");

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/set-operator",
        label: "Alice schedules Bob as market operator",
        body: { newOperator: bob },
      });
      market = await harness.market();
      harness.assertEqual("operator remains unchanged during timelock", market.operator, initial.operator);
      harness.assertEqual("Bob is recorded as pending operator", market.pendingOperator.newAuthority, bob);

      const earlyOperator = await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/set-operator",
        label: "reject operator update before timelock",
        expected: "failure",
        body: { newOperator: bob },
      });
      harness.assertEqual(
        "operator timelock cannot execute early",
        earlyOperator.errorCode,
        "GovernanceTimelockNotReady"
      );

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/set-operator",
        label: "overwrite pending operator with referrer",
        body: { newOperator: referrer },
      });
      market = await harness.market();
      harness.assertEqual("latest pending operator replaces prior candidate", market.pendingOperator.newAuthority, referrer);
      await harness.timeTravel(0, harness.config.governanceDelaySlots + 10);
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/set-operator",
        label: "apply referrer as market operator",
        body: { newOperator: referrer },
      });
      harness.assertEqual("referrer becomes operator after timelock", (await harness.market()).operator, referrer);

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/set-manager",
        label: "Alice schedules Bob as market manager",
        body: { newManager: bob },
      });
      await harness.timeTravel(0, harness.config.governanceDelaySlots + 10);
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/set-manager",
        label: "Alice applies Bob as market manager",
        body: { newManager: bob },
      });
      harness.assertEqual("Bob becomes market manager", (await harness.market()).manager, bob);

      const formerAlice = await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/set-operator",
        label: "reject former Alice manager after Bob rotation",
        expected: "failure",
        body: { newOperator: bob },
      });
      harness.assertEqual("former Alice manager loses control", formerAlice.errorCode, "InvalidMarketManager");

      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/set-manager",
        label: "Bob schedules manager return to Alice",
        body: { newManager: alice },
      });
      await harness.timeTravel(0, harness.config.governanceDelaySlots + 10);
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/set-manager",
        label: "Bob returns market manager to Alice",
        body: { newManager: alice },
      });
      harness.assertEqual("Alice is final market manager", (await harness.market()).manager, alice);

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/set-operator",
        label: "schedule restoration of original operator",
        body: { newOperator: initial.operator },
      });
      await harness.timeTravel(0, harness.config.governanceDelaySlots + 10);
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/set-operator",
        label: "restore original market operator",
        body: { newOperator: initial.operator },
      });
      market = await harness.market();
      harness.assertEqual("market operator is restored", market.operator, initial.operator);
      harness.assertEqual("no operator update remains pending", market.pendingOperator.active, false);
      harness.assertEqual("no manager update remains pending", market.pendingManager.active, false);
    },
  },
  {
    id: "governance.market-config",
    async run(harness) {
      const before = await harness.market();
      const original = before.config;

      const unauthorized = await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/update-config",
        label: "reject market config update by non-manager",
        expected: "failure",
        body: { config: { ...original, swapFeeBps: 31 } },
      });
      harness.assertEqual(
        "market config signer binding is enforced",
        unauthorized.errorCode,
        "InvalidMarketConfigAuthority"
      );

      const invalidConfigs: Array<[string, Record<string, string | number>, string]> = [
        ["swap fee above maximum", { ...original, swapFeeBps: 10_001 }, "InvalidSwapFeeBps"],
        ["manager fee above maximum", { ...original, managerFeeBps: 501 }, "InvalidMarketConfig"],
        ["nonzero legacy protocol fee", { ...original, protocolFeeBps: 1 }, "InvalidMarketConfig"],
        ["unsupported hLP leverage", { ...original, targetHlpLeverageBps: 19_999 }, "InvalidMarketConfig"],
        ["settlement divergence above maximum", { ...original, settlementDivergenceBps: 10_001 }, "InvalidMarketConfig"],
        ["symmetric EMA below minimum", { ...original, emaHalfLifeMs: "59999" }, "InvalidMarketConfig"],
        ["directional EMA above maximum", { ...original, directionalEmaHalfLifeMs: "43200001" }, "InvalidMarketConfig"],
        ["zero K EMA half life", { ...original, kEmaHalfLifeMs: "0" }, "InvalidMarketConfig"],
        ["daily borrow limit above maximum", { ...original, maxDailyBorrowBps: 10_001 }, "InvalidMarketConfig"],
        ["global contribution cap below one", { ...original, globalHealthContributionCapBps: 9_999 }, "InvalidMarketConfig"],
        ["borrow health floor below one", { ...original, borrowMarketHealthFloorBps: 9_999 }, "InvalidMarketConfig"],
        [
          "global contribution cap below health floor",
          { ...original, globalHealthContributionCapBps: 11_000, borrowMarketHealthFloorBps: 12_000 },
          "InvalidMarketConfig",
        ],
      ];
      for (const [label, config, errorCode] of invalidConfigs) {
        const rejected = await harness.execute({
          wallet: "alice",
          endpoint: "/api/v2/fork/tx/update-config",
          label: `reject ${label}`,
          expected: "failure",
          body: { config },
        });
        harness.assertEqual(`${label} returns deterministic error`, rejected.errorCode, errorCode);
      }
      harness.assertEqual("invalid configs leave no pending update", (await harness.market()).pendingConfig.active, false);

      const identical = await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-config",
        label: "reject scheduling an identical market config",
        expected: "failure",
        body: { config: original },
      });
      harness.assertEqual("identical config is rejected", identical.errorCode, "InvalidArgument");

      const boundaryConfig = {
        ...original,
        swapFeeBps: 10_000,
        managerFeeBps: 500,
        operatorFeeBps: 500,
        protocolFeeBps: 0,
        targetHlpLeverageBps: 20_000,
        settlementDivergenceBps: 10_000,
        emaHalfLifeMs: "43200000",
        directionalEmaHalfLifeMs: "60000",
        kEmaHalfLifeMs: "43200000",
        maxDailyBorrowBps: 10_000,
        globalHealthContributionCapBps: 15_000,
        borrowMarketHealthFloorBps: 11_000,
      };
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-config",
        label: "schedule valid market config boundary values",
        body: { config: boundaryConfig },
      });
      let market = await harness.market();
      harness.assertEqual("scheduled config does not apply immediately", market.config, original);
      harness.assertEqual("valid config is recorded as pending", market.pendingConfig.active, true);
      harness.assertEqual("pending config stores exact boundary values", market.pendingConfig.config, boundaryConfig);

      const early = await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-config",
        label: "reject config application before timelock",
        expected: "failure",
        body: { config: boundaryConfig },
      });
      harness.assertEqual("config timelock cannot execute early", early.errorCode, "GovernanceTimelockNotReady");
      await harness.timeTravel(0, harness.config.governanceDelaySlots + 10);
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-config",
        label: "apply valid market config boundary values",
        body: { config: boundaryConfig },
      });
      market = await harness.market();
      harness.assertEqual("boundary config applies exactly", market.config, boundaryConfig);
      harness.assertEqual("applied config clears pending state", market.pendingConfig.active, false);

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-config",
        label: "schedule restoration of original market config",
        body: { config: original },
      });
      await harness.timeTravel(0, harness.config.governanceDelaySlots + 10);
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-config",
        label: "restore original market config",
        body: { config: original },
      });
      market = await harness.market();
      harness.assertEqual("market config restores exactly", market.config, original);
      harness.assertEqual("restored config leaves no pending update", market.pendingConfig.active, false);
    },
  },
  {
    id: "governance.reduce-only-matrix",
    async run(harness) {
      await harness.fundWallet("emergency", "0", "0", 20);
      const emergency = walletAddress(harness, "emergency");
      const referrer = walletAddress(harness, "referrer");
      const trader = walletAddress(harness, "trader");
      const revenueBefore = await harness.futarchy();
      harness.assertEqual(
        "development emergency signer is deterministic",
        emergency,
        "2iXtA8oeZqUU5pofxK971TCEvFGfems2AcDRaZHKD2pQ"
      );
      harness.assertEqual("global reduce-only starts disabled", (await harness.futarchy()).globalReduceOnly, false);
      harness.assertEqual("market reduce-only starts disabled", (await harness.market()).reduceOnly, false);

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-protocol-revenue",
        label: "enable referral interest for reduce-only claim coverage",
        body: { interestBps: 10_000, maxReferralInterestShareBps: 5_000 },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/configure-referral",
        label: "list reduce-only referral",
        body: { referrer, interestShareBps: 5_000, active: true },
      });
      await harness.execute({
        wallet: "referrer",
        endpoint: "/api/v2/fork/tx/set-referral-recipient",
        label: "set reduce-only referral recipient",
        body: { recipient: trader },
      });

      const unauthorizedGlobal = await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/set-global-reduce-only",
        label: "reject global reduce-only update by normal governance signer",
        expected: "failure",
        body: { reduceOnly: true },
      });
      harness.assertEqual(
        "global emergency authority is hard-bound",
        unauthorizedGlobal.errorCode,
        "InvalidReduceOnlyAuthority"
      );
      const unauthorizedMarket = await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/set-reduce-only",
        label: "reject market reduce-only update by normal governance signer",
        expected: "failure",
        body: { reduceOnly: true },
      });
      harness.assertEqual(
        "market emergency authority is hard-bound",
        unauthorizedMarket.errorCode,
        "InvalidReduceOnlyAuthority"
      );

      const ylpBefore = await harness.lpBalance("trader", harness.config.ylpMint);
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/add-liquidity",
        label: "prepare removable yLP before emergency mode",
        body: { baseDepositAmount: "5", quoteDepositAmount: "5", minYlpAmount: "0" },
      });
      const ylpMinted = await harness.lpBalance("trader", harness.config.ylpMint) - ylpBefore;
      harness.assertTrue("reduce-only fixture mints yLP", ylpMinted > 0n, ylpMinted);

      const hlpBefore = await harness.lpBalance("trader", harness.config.baseHlpMint);
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/deposit-single-sided",
        label: "prepare removable hLP before emergency mode",
        body: { targetAsset: "base", depositAmount: "2", minHlpAmount: "0" },
      });
      const hlpMinted = await harness.lpBalance("trader", harness.config.baseHlpMint) - hlpBefore;
      harness.assertTrue("reduce-only fixture mints hLP", hlpMinted > 0n, hlpMinted);

      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "accrue claimable LP fees before emergency mode",
        body: { assetIn: "base", exactAssetIn: "10", minAssetOut: "0" },
      });
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/deposit-collateral",
        label: "prepare collateralized loan before emergency mode",
        body: {
          positionId: reduceOnlyLoanPositionId.toBase58(),
          marketAsset: "base",
          depositAmount: "20",
        },
      });
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/borrow",
        label: "prepare referred debt before emergency mode",
        body: {
          positionId: reduceOnlyLoanPositionId.toBase58(),
          borrowAsset: "quote",
          borrowAmount: "1",
          minDebtAmountOut: "1",
          minLiquidationCfBps: 0,
          referrer,
        },
      });
      await harness.timeTravel(0, 2_160_000);
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/open-leverage",
        label: "prepare leverage position before emergency mode",
        body: {
          positionId: reduceOnlyLeveragePositionId.toBase58(),
          debtAsset: "quote",
          marginAmount: "2",
          multiplierBps: 20_000,
          minCollateralOut: "0",
        },
      });

      await harness.execute({
        wallet: "emergency",
        endpoint: "/api/v2/fork/tx/set-reduce-only",
        label: "enable market reduce-only mode",
        body: { reduceOnly: true },
      });
      harness.assertEqual("market reduce-only flag is enabled", (await harness.market()).reduceOnly, true);
      harness.assertEqual("market mode does not alter global flag", (await harness.futarchy()).globalReduceOnly, false);

      await expectReduceOnlyRejection(harness, {
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/add-liquidity",
        label: "add yLP liquidity",
        body: { baseDepositAmount: "0.1", quoteDepositAmount: "0.1", minYlpAmount: "0" },
      });
      await expectReduceOnlyRejection(harness, {
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/deposit-single-sided",
        label: "deposit new hLP liquidity",
        body: { targetAsset: "base", depositAmount: "0.1", minHlpAmount: "0" },
      });
      await expectReduceOnlyRejection(harness, {
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "execute a spot swap",
        body: { assetIn: "base", exactAssetIn: "0.1", minAssetOut: "0" },
      });
      await expectReduceOnlyRejection(harness, {
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/borrow",
        label: "increase loan debt",
        body: {
          positionId: reduceOnlyLoanPositionId.toBase58(),
          borrowAsset: "quote",
          borrowAmount: "0.1",
          minDebtAmountOut: "0",
          minLiquidationCfBps: 0,
        },
      });
      await expectReduceOnlyRejection(harness, {
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/open-leverage",
        label: "open a leverage position",
        body: {
          positionId: reduceOnlyBlockedOpenPositionId.toBase58(),
          debtAsset: "quote",
          marginAmount: "1",
          multiplierBps: 20_000,
          minCollateralOut: "0",
        },
      });
      await expectReduceOnlyRejection(harness, {
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/increase-leverage",
        label: "increase leverage debt",
        body: {
          positionId: reduceOnlyLeveragePositionId.toBase58(),
          debtAsset: "quote",
          debtAmount: "0.1",
          minCollateralOut: "0",
        },
      });
      await expectReduceOnlyRejection(harness, {
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/remove-leverage-margin",
        label: "remove leverage margin",
        body: {
          positionId: reduceOnlyLeveragePositionId.toBase58(),
          debtAsset: "quote",
          amount: "0.1",
          minAmountOut: "0",
        },
      });

      const traderBaseBeforeClaim = await harness.tokenBalance(
        "trader",
        harness.config.baseMint,
        harness.config.baseTokenProgram
      );
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/claim-yield",
        label: "claim yLP yield during market reduce-only",
        body: { asset: "base", tokenKind: "ylp", recipient: trader },
      });
      harness.assertTrue(
        "LP yield claim remains available in reduce-only",
        await harness.tokenBalance("trader", harness.config.baseMint, harness.config.baseTokenProgram) >
          traderBaseBeforeClaim
      );
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/deposit-collateral",
        label: "add collateral during market reduce-only",
        body: {
          positionId: reduceOnlyLoanPositionId.toBase58(),
          marketAsset: "base",
          depositAmount: "1",
        },
      });
      const debtBearingWithdrawal = await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/withdraw-collateral",
        label: "reject collateral withdrawal while emergency-mode debt remains",
        expected: "failure",
        body: {
          positionId: reduceOnlyLoanPositionId.toBase58(),
          marketAsset: "base",
          withdrawAmount: "1",
          minAssetAmountOut: "0",
          minLiquidationCfBps: 0,
        },
      });
      harness.assertEqual(
        "reduce-only collateral exit requires own debt repayment",
        debtBearingWithdrawal.errorCode,
        "ReduceOnlyHasDebt"
      );
      const loanPreviewEvidence = await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/preview-borrow-position",
        label: "preview exact debt for emergency-mode repayment",
        submit: false,
        body: { positionId: reduceOnlyLoanPositionId.toBase58() },
      });
      const loanPreview = decodePreviewBorrowPositionReturnData(previewData(loanPreviewEvidence));
      const exactLoanDebt = BigInt(loanPreview.fixedQuoteDebt.toString());
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/repay",
        label: "fully repay loan during market reduce-only",
        body: {
          positionId: reduceOnlyLoanPositionId.toBase58(),
          repayAsset: "quote",
          repayAmount: formatUnits(exactLoanDebt, harness.config.quoteDecimals),
        },
      });
      const traderQuoteBeforeReferralClaim = await harness.tokenBalance(
        "trader",
        harness.config.quoteMint,
        harness.config.quoteTokenProgram
      );
      await harness.execute({
        wallet: "referrer",
        endpoint: "/api/v2/fork/tx/claim-referral-interest",
        label: "claim referral interest during market reduce-only",
        body: { asset: "quote" },
      });
      harness.assertTrue(
        "referral interest claim remains available in reduce-only",
        await harness.tokenBalance("trader", harness.config.quoteMint, harness.config.quoteTokenProgram) >
          traderQuoteBeforeReferralClaim
      );
      let loanPositions = await harness.positions("bob", reduceOnlyLoanPositionId);
      harness.assertEqual("repay clears emergency-mode loan shares", BigInt(loanPositions[0].payload.fixedQuoteShares), 0n);
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/withdraw-collateral",
        label: "withdraw debt-free collateral during market reduce-only",
        body: {
          positionId: reduceOnlyLoanPositionId.toBase58(),
          marketAsset: "base",
          withdrawAmount: "21",
          minAssetAmountOut: "0",
          minLiquidationCfBps: 0,
        },
      });
      loanPositions = await harness.positions("bob", reduceOnlyLoanPositionId);
      harness.assertEqual("debt-free collateral exits completely", BigInt(loanPositions[0].payload.baseCollateral), 0n);

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/add-leverage-margin",
        label: "add leverage margin during market reduce-only",
        body: { positionId: reduceOnlyLeveragePositionId.toBase58(), debtAsset: "quote", amount: "0.1" },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/decrease-leverage",
        label: "decrease leverage during market reduce-only",
        body: {
          positionId: reduceOnlyLeveragePositionId.toBase58(),
          debtAsset: "quote",
          collateralAmount: "0.1",
          minRepayOut: "0",
        },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/close-leverage",
        label: "close leverage during market reduce-only",
        body: { positionId: reduceOnlyLeveragePositionId.toBase58(), debtAsset: "quote", minAmountOut: "0" },
      });
      harness.assertEqual(
        "leverage position closes during emergency mode",
        (await harness.positions("alice", reduceOnlyLeveragePositionId)).length,
        0
      );

      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/withdraw-single-sided",
        label: "withdraw hLP during market reduce-only",
        body: {
          targetAsset: "base",
          hlpAmount: formatUnits(hlpMinted, harness.config.baseDecimals),
          minTargetAmountOut: "0",
        },
      });
      harness.assertEqual(
        "hLP emergency exit burns prepared shares",
        await harness.lpBalance("trader", harness.config.baseHlpMint),
        hlpBefore
      );
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/remove-liquidity",
        label: "remove yLP liquidity during market reduce-only",
        body: {
          ylpAmount: formatUnits(ylpMinted, harness.config.baseDecimals),
          minBaseAmountOut: "0",
          minQuoteAmountOut: "0",
        },
      });
      harness.assertEqual(
        "yLP emergency exit burns prepared shares",
        await harness.lpBalance("trader", harness.config.ylpMint),
        ylpBefore
      );

      await harness.execute({
        wallet: "emergency",
        endpoint: "/api/v2/fork/tx/set-reduce-only",
        label: "disable market reduce-only mode",
        body: { reduceOnly: false },
      });
      harness.assertEqual("market reduce-only flag clears", (await harness.market()).reduceOnly, false);

      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/open-leverage",
        label: "open leverage fixture for emergency liquidation",
        body: {
          positionId: reduceOnlyLiquidationPositionId.toBase58(),
          debtAsset: "quote",
          marginAmount: "10",
          multiplierBps: 20_000,
          minCollateralOut: "0",
        },
      });
      await harness.fundWallet("trader", "100000", "100000");
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "shock leverage collateral before emergency liquidation",
        body: { assetIn: "base", exactAssetIn: "80000", minAssetOut: "0" },
      });
      await harness.execute({
        wallet: "emergency",
        endpoint: "/api/v2/fork/tx/set-reduce-only",
        label: "re-enable market reduce-only for liquidation",
        body: { reduceOnly: true },
      });
      await harness.execute({
        wallet: "liquidator",
        endpoint: "/api/v2/fork/tx/liquidate-leverage",
        label: "liquidate unhealthy leverage during market reduce-only",
        body: {
          positionOwner: walletAddress(harness, "bob"),
          positionId: reduceOnlyLiquidationPositionId.toBase58(),
          debtAsset: "quote",
        },
      });
      harness.assertEqual(
        "liquidation remains available in reduce-only",
        (await harness.positions("bob", reduceOnlyLiquidationPositionId)).length,
        0
      );
      await harness.execute({
        wallet: "emergency",
        endpoint: "/api/v2/fork/tx/set-reduce-only",
        label: "disable market reduce-only after liquidation",
        body: { reduceOnly: false },
      });
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "restore pool ratio after emergency liquidation",
        body: { assetIn: "quote", exactAssetIn: "44000", minAssetOut: "0" },
      });
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "checkpoint restored spot price after emergency liquidation",
        body: { assetIn: "quote", exactAssetIn: "0.001", minAssetOut: "0" },
      });
      await harness.timeTravel(1, 1_000);
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "persist restored EMA after emergency liquidation",
        body: { assetIn: "quote", exactAssetIn: "0.001", minAssetOut: "0" },
      });

      const globalYlpBefore = await harness.lpBalance("trader", harness.config.ylpMint);
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/add-liquidity",
        label: "prepare yLP exit for global reduce-only",
        body: { baseDepositAmount: "1", quoteDepositAmount: "1", minYlpAmount: "0" },
      });
      const globalYlpMinted = await harness.lpBalance("trader", harness.config.ylpMint) - globalYlpBefore;
      await harness.execute({
        wallet: "emergency",
        endpoint: "/api/v2/fork/tx/set-global-reduce-only",
        label: "enable global reduce-only mode",
        body: { reduceOnly: true },
      });
      harness.assertEqual("global reduce-only flag is enabled", (await harness.futarchy()).globalReduceOnly, true);
      harness.assertEqual("global mode leaves market-local flag clear", (await harness.market()).reduceOnly, false);
      await expectReduceOnlyRejection(harness, {
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "execute a swap under global mode",
        body: { assetIn: "base", exactAssetIn: "0.1", minAssetOut: "0" },
      });
      const unauthorizedClear = await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/set-global-reduce-only",
        label: "reject global emergency-mode clear by governance",
        expected: "failure",
        body: { reduceOnly: false },
      });
      harness.assertEqual(
        "governance cannot clear global emergency mode",
        unauthorizedClear.errorCode,
        "InvalidReduceOnlyAuthority"
      );
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/remove-liquidity",
        label: "remove yLP liquidity during global reduce-only",
        body: {
          ylpAmount: formatUnits(globalYlpMinted, harness.config.baseDecimals),
          minBaseAmountOut: "0",
          minQuoteAmountOut: "0",
        },
      });
      harness.assertEqual(
        "global reduce-only preserves yLP exits",
        await harness.lpBalance("trader", harness.config.ylpMint),
        globalYlpBefore
      );
      await harness.execute({
        wallet: "emergency",
        endpoint: "/api/v2/fork/tx/set-global-reduce-only",
        label: "disable global reduce-only mode",
        body: { reduceOnly: false },
      });
      harness.assertEqual("global reduce-only flag clears", (await harness.futarchy()).globalReduceOnly, false);
      harness.assertEqual("market reduce-only remains clear", (await harness.market()).reduceOnly, false);
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-protocol-revenue",
        label: "restore protocol revenue after reduce-only coverage",
        body: {
          swapBps: revenueBefore.revenueShare.swapBps,
          interestBps: revenueBefore.revenueShare.interestBps,
          maxReferralInterestShareBps: revenueBefore.maxReferralInterestShareBps,
          revenueDistribution: revenueBefore.revenueDistribution,
          protocolAuctionSplit: revenueBefore.protocolAuctionSplit,
        },
      });
    },
  },
  {
    id: "governance.protocol-auction",
    async run(harness) {
      const before = await harness.futarchy();
      const alice = walletAddress(harness, "alice");
      const bob = walletAddress(harness, "bob");
      const referrer = walletAddress(harness, "referrer");
      harness.assertEqual("protocol auction scenario starts under Alice authority", before.authority, alice);

      const unauthorizedConfig = await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/update-protocol-auction-config",
        label: "reject protocol auction config update by non-authority",
        expected: "failure",
        body: { lane: "fee", acceptedMint: harness.config.quoteMint },
      });
      harness.assertEqual(
        "auction config signer binding is enforced",
        unauthorizedConfig.errorCode,
        "InvalidFutarchyAuthority"
      );
      const unauthorizedRecipients = await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/update-protocol-auction-recipients",
        label: "reject protocol auction recipients update by non-authority",
        expected: "failure",
        body: { lane: "fee", treasury: bob },
      });
      harness.assertEqual(
        "auction recipient signer binding is enforced",
        unauthorizedRecipients.errorCode,
        "InvalidFutarchyAuthority"
      );

      const zeroMint = await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-protocol-auction-config",
        label: "reject zero protocol auction accepted mint",
        expected: "failure",
        body: { lane: "fee", acceptedMint: PublicKey.default.toBase58() },
      });
      harness.assertEqual("zero accepted mint is rejected", zeroMint.errorCode, "InvalidMint");

      const invalidAuctionParams: Array<[string, Record<string, number | string>]> = [
        [
          "auction floor above start",
          {
            startMultiplierBps: 10_000,
            floorMultiplierBps: 10_001,
            durationSlots: "10",
            maxReferenceAgeSlots: "10",
          },
        ],
        [
          "zero auction duration",
          {
            startMultiplierBps: 10_000,
            floorMultiplierBps: 8_000,
            durationSlots: "0",
            maxReferenceAgeSlots: "10",
          },
        ],
        [
          "zero reference age",
          {
            startMultiplierBps: 10_000,
            floorMultiplierBps: 8_000,
            durationSlots: "10",
            maxReferenceAgeSlots: "0",
          },
        ],
      ];
      for (const [label, params] of invalidAuctionParams) {
        const rejected = await harness.execute({
          wallet: "alice",
          endpoint: "/api/v2/fork/tx/update-protocol-auction-config",
          label: `reject ${label}`,
          expected: "failure",
          body: { lane: "fee", params },
        });
        harness.assertEqual(`${label} returns deterministic error`, rejected.errorCode, "InvalidAuctionConfig");
      }

      const invalidRecipientSplit = await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-protocol-auction-recipients",
        label: "reject protocol auction recipient split below 100 percent",
        expected: "failure",
        body: { lane: "fee", treasuryBps: 7_000, stakingVaultBps: 2_999 },
      });
      harness.assertEqual(
        "auction recipient sum is enforced",
        invalidRecipientSplit.errorCode,
        "InvalidDistribution"
      );
      const recipientBpsOverflow = await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-protocol-auction-recipients",
        label: "reject protocol auction recipient share above 100 percent",
        expected: "failure",
        body: { lane: "fee", treasuryBps: 10_001 },
      });
      harness.assertEqual(
        "individual auction recipient cap is enforced",
        recipientBpsOverflow.errorCode,
        "InvalidDistribution"
      );
      harness.assertEqual("rejected auction config preserves fee lane", (await harness.futarchy()).feeAuction, before.feeAuction);

      const feeParams = {
        startMultiplierBps: 10_000,
        floorMultiplierBps: 10_000,
        durationSlots: "20",
        maxReferenceAgeSlots: "5",
      };
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-protocol-auction-config",
        label: "configure fee auction against quote mint",
        body: { lane: "fee", acceptedMint: harness.config.quoteMint, params: feeParams },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-protocol-auction-recipients",
        label: "configure fee auction recipient split",
        body: {
          lane: "fee",
          treasury: bob,
          stakingVault: referrer,
          treasuryBps: 7_000,
          stakingVaultBps: 3_000,
        },
      });
      let authority = await harness.futarchy();
      harness.assertEqual("fee auction accepted mint updates", authority.feeAuction.acceptedMint, harness.config.quoteMint);
      harness.assertEqual("fee auction curve updates exactly", authority.feeAuction.params, feeParams);
      harness.assertEqual(
        "fee auction recipients update exactly",
        authority.feeAuction.recipients,
        { treasury: bob, stakingVault: referrer, treasuryBps: 7_000, stakingVaultBps: 3_000 }
      );

      const buybackParams = {
        startMultiplierBps: 12_000,
        floorMultiplierBps: 8_000,
        durationSlots: "100",
        maxReferenceAgeSlots: "100",
      };
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-protocol-auction-config",
        label: "configure buyback auction boundary values",
        body: { lane: "buyback", acceptedMint: harness.config.baseMint, params: buybackParams },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-protocol-auction-recipients",
        label: "configure buyback auction staking-only split",
        body: {
          lane: "buyback",
          treasury: bob,
          stakingVault: referrer,
          treasuryBps: 0,
          stakingVaultBps: 10_000,
        },
      });
      authority = await harness.futarchy();
      harness.assertEqual("buyback auction accepted mint updates", authority.buybackAuction.acceptedMint, harness.config.baseMint);
      harness.assertEqual("buyback auction curve updates exactly", authority.buybackAuction.params, buybackParams);
      harness.assertEqual(
        "buyback staking-only split is valid",
        authority.buybackAuction.recipients,
        { treasury: bob, stakingVault: referrer, treasuryBps: 0, stakingVaultBps: 10_000 }
      );

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-protocol-revenue",
        label: "route all new protocol swap revenue to fee auction",
        body: {
          swapBps: 10_000,
          protocolAuctionSplit: { feeAuctionBps: 10_000, buybackAuctionBps: 0 },
        },
      });
      const marketBeforeAccrual = await harness.market();
      const liabilityBeforeAccrual = stateValue(marketBeforeAccrual, "baseProtocolFeeLiability");
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "accrue base fee-auction liability",
        body: { assetIn: "base", exactAssetIn: "10", minAssetOut: "0" },
      });
      let market = await harness.market();
      let soldAmount = stateValue(market, "baseProtocolFeeLiability");
      harness.assertTrue("swap creates additional fee-auction liability", soldAmount > liabilityBeforeAccrual, {
        before: liabilityBeforeAccrual,
        after: soldAmount,
      });
      harness.assertTrue("fee-auction liability is token-backed", stateValue(market, "baseSwapFeeVaultBalance") >= soldAmount);

      await harness.timeTravel(0, 10);
      const staleReference = await harness.execute({
        wallet: "bidder",
        endpoint: "/api/v2/fork/tx/settle-protocol-auction",
        label: "reject protocol auction against stale market reference",
        expected: "failure",
        body: {
          lane: "fee",
          soldAsset: "base",
          soldAmount: formatUnits(soldAmount, harness.config.baseDecimals),
          maxPaymentAmount: "1000000",
        },
      });
      harness.assertEqual(
        "stale auction reference is rejected",
        staleReference.errorCode,
        "StaleAuctionReference"
      );

      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "refresh auction reference with real swap",
        body: { assetIn: "base", exactAssetIn: "0.001", minAssetOut: "0" },
      });
      market = await harness.market();
      soldAmount = stateValue(market, "baseProtocolFeeLiability");
      const insufficientPayment = await harness.execute({
        wallet: "bidder",
        endpoint: "/api/v2/fork/tx/settle-protocol-auction",
        label: "reject protocol auction above bidder payment limit",
        expected: "failure",
        body: {
          lane: "fee",
          soldAsset: "base",
          soldAmount: formatUnits(soldAmount, harness.config.baseDecimals),
          maxPaymentAmount: "0.000001",
        },
      });
      harness.assertEqual(
        "auction payment slippage guard is enforced",
        insufficientPayment.errorCode,
        "InsufficientAuctionPayment"
      );

      const baseFeeVault = new PublicKey(market.baseFeeVault);
      const feeVaultBefore = await harness.tokenAccountBalance(baseFeeVault, harness.config.baseTokenProgram);
      const trackedFeeVaultBefore = stateValue(market, "baseSwapFeeVaultBalance");
      const bidderBaseBefore = await harness.tokenBalance(
        "bidder",
        harness.config.baseMint,
        harness.config.baseTokenProgram
      );
      const bidderQuoteBefore = await harness.tokenBalance(
        "bidder",
        harness.config.quoteMint,
        harness.config.quoteTokenProgram
      );
      const treasuryQuoteBefore = await harness.tokenBalance(
        "bob",
        harness.config.quoteMint,
        harness.config.quoteTokenProgram
      );
      const stakingQuoteBefore = await harness.tokenBalance(
        "referrer",
        harness.config.quoteMint,
        harness.config.quoteTokenProgram
      );
      const settlement = await harness.execute({
        wallet: "bidder",
        endpoint: "/api/v2/fork/tx/settle-protocol-auction",
        label: "settle full base fee-auction liability",
        body: {
          lane: "fee",
          soldAsset: "base",
          soldAmount: formatUnits(soldAmount, harness.config.baseDecimals),
          maxPaymentAmount: "1000000",
        },
      });
      const settlementEvents = harness.events(settlement, "ProtocolAuctionSettled");
      harness.assertEqual("protocol auction emits one settlement receipt", settlementEvents.length, 1);
      const receipt = settlementEvents[0].data as Record<string, { toString(): string }>;
      const eventSold = eventAmount(receipt, "sold_amount");
      const paymentAmount = eventAmount(receipt, "payment_amount");
      const treasuryAmount = eventAmount(receipt, "treasury_amount");
      const stakingAmount = eventAmount(receipt, "staking_vault_amount");
      harness.assertEqual("auction receipt records exact sold liability", eventSold, soldAmount);
      harness.assertEqual("auction payment split conserves payment", treasuryAmount + stakingAmount, paymentAmount);
      harness.assertEqual("auction receipt clears fee liability", eventAmount(receipt, "remaining_fee_liability"), 0n);

      const marketAfter = await harness.market();
      harness.assertEqual("fee lane liability clears on-chain", stateValue(marketAfter, "baseProtocolFeeLiability"), 0n);
      harness.assertEqual(
        "tracked fee-vault balance debits sold amount",
        trackedFeeVaultBefore - stateValue(marketAfter, "baseSwapFeeVaultBalance"),
        soldAmount
      );
      harness.assertEqual(
        "token fee vault debits sold amount",
        feeVaultBefore - await harness.tokenAccountBalance(baseFeeVault, harness.config.baseTokenProgram),
        soldAmount
      );
      harness.assertEqual(
        "bidder receives exact sold base",
        await harness.tokenBalance("bidder", harness.config.baseMint, harness.config.baseTokenProgram) - bidderBaseBefore,
        soldAmount
      );
      harness.assertEqual(
        "bidder pays exact accepted quote",
        bidderQuoteBefore -
          await harness.tokenBalance("bidder", harness.config.quoteMint, harness.config.quoteTokenProgram),
        paymentAmount
      );
      harness.assertEqual(
        "treasury receives exact auction split",
        await harness.tokenBalance("bob", harness.config.quoteMint, harness.config.quoteTokenProgram) - treasuryQuoteBefore,
        treasuryAmount
      );
      harness.assertEqual(
        "staking vault receives exact auction split",
        await harness.tokenBalance("referrer", harness.config.quoteMint, harness.config.quoteTokenProgram) -
          stakingQuoteBefore,
        stakingAmount
      );

      const replay = await harness.execute({
        wallet: "bidder",
        endpoint: "/api/v2/fork/tx/settle-protocol-auction",
        label: "reject replay after fee liability is exhausted",
        expected: "failure",
        body: {
          lane: "fee",
          soldAsset: "base",
          soldAmount: formatUnits(soldAmount, harness.config.baseDecimals),
          maxPaymentAmount: "1000000",
        },
      });
      harness.assertEqual("settled liability cannot be sold twice", replay.errorCode, "UnbackedFeeLiability");

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-protocol-auction-config",
        label: "restore fee auction config",
        body: {
          lane: "fee",
          acceptedMint: before.feeAuction.acceptedMint,
          params: before.feeAuction.params,
        },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-protocol-auction-recipients",
        label: "restore fee auction recipients",
        body: { lane: "fee", ...before.feeAuction.recipients },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-protocol-auction-config",
        label: "restore buyback auction config",
        body: {
          lane: "buyback",
          acceptedMint: before.buybackAuction.acceptedMint,
          params: before.buybackAuction.params,
        },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-protocol-auction-recipients",
        label: "restore buyback auction recipients",
        body: { lane: "buyback", ...before.buybackAuction.recipients },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-protocol-revenue",
        label: "restore protocol revenue after auction",
        body: {
          swapBps: before.revenueShare.swapBps,
          interestBps: before.revenueShare.interestBps,
          maxReferralInterestShareBps: before.maxReferralInterestShareBps,
          revenueDistribution: before.revenueDistribution,
          protocolAuctionSplit: before.protocolAuctionSplit,
        },
      });
      authority = await harness.futarchy();
      harness.assertEqual("fee auction accepted mint restores", authority.feeAuction.acceptedMint, before.feeAuction.acceptedMint);
      harness.assertEqual("fee auction parameters restore", authority.feeAuction.params, before.feeAuction.params);
      harness.assertEqual("fee auction recipients restore", authority.feeAuction.recipients, before.feeAuction.recipients);
      harness.assertTrue(
        "fee auction settlement slot advances monotonically",
        BigInt(authority.feeAuction.lastSettlementSlot) > BigInt(before.feeAuction.lastSettlementSlot)
      );
      harness.assertEqual("buyback auction config restores", authority.buybackAuction, before.buybackAuction);
      harness.assertEqual("protocol swap share restores", authority.revenueShare, before.revenueShare);
      harness.assertEqual("protocol auction split restores", authority.protocolAuctionSplit, before.protocolAuctionSplit);
    },
  },
];
