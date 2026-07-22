import { PublicKey } from "@solana/web3.js";

import {
  decodePreviewMarketReturnData,
  decodePreviewSwapReturnData,
} from "../../../packages/dusk-sdk/src/preview.js";
import { formatUnits, type ProtocolTestHarness, type ScenarioDefinition } from "../harness.js";
import type { TransactionEvidence } from "../types.js";

type MarketAsset = "base" | "quote";

function raw(uiAmount: number, decimals: number): bigint {
  return BigInt(uiAmount) * 10n ** BigInt(decimals);
}

function stateValue(
  market: Awaited<ReturnType<ProtocolTestHarness["market"]>>,
  key: string
): bigint {
  const value = market.state[key];
  if (value === undefined) throw new Error(`Market state does not expose ${key}`);
  return BigInt(value);
}

function previewData(evidence: TransactionEvidence): [string, BufferEncoding] {
  const data = evidence.simulation.returnData?.data;
  if (!data) throw new Error(`${evidence.label} did not return preview data`);
  return data as [string, BufferEncoding];
}

function integer(value: { toString(): string } | bigint | number): bigint {
  return BigInt(value.toString());
}

function decimalsFor(harness: ProtocolTestHarness, asset: MarketAsset): number {
  return asset === "base" ? harness.config.baseDecimals : harness.config.quoteDecimals;
}

function mintFor(harness: ProtocolTestHarness, asset: MarketAsset): string {
  return asset === "base" ? harness.config.baseMint : harness.config.quoteMint;
}

function tokenProgramFor(harness: ProtocolTestHarness, asset: MarketAsset): string {
  return asset === "base" ? harness.config.baseTokenProgram : harness.config.quoteTokenProgram;
}

async function firstPassingRawSwap(
  harness: ProtocolTestHarness,
  assetIn: MarketAsset
): Promise<bigint> {
  const decimals = decimalsFor(harness, assetIn);
  let low = 0n;
  let high = raw(1, decimals);
  const highProbe = await harness.probe("trader", "/api/v2/fork/tx/preview-swap", {
    assetIn,
    exactAssetIn: formatUnits(high, decimals),
  });
  harness.assertTrue(`${assetIn} one-token swap preview succeeds`, highProbe.succeeds, highProbe.errorCode);

  while (low + 1n < high) {
    const middle = (low + high) / 2n;
    const probe = await harness.probe("trader", "/api/v2/fork/tx/preview-swap", {
      assetIn,
      exactAssetIn: formatUnits(middle, decimals),
    });
    if (probe.succeeds) high = middle;
    else low = middle;
  }
  return high;
}

async function largestPassingHlpDeposit(
  harness: ProtocolTestHarness,
  wallet: string,
  targetAsset: MarketAsset,
  low: bigint,
  high: bigint
): Promise<bigint> {
  const decimals = decimalsFor(harness, targetAsset);
  const lowProbe = await harness.probe(wallet, "/api/v2/fork/tx/deposit-single-sided", {
    targetAsset,
    depositAmount: formatUnits(low, decimals),
    minHlpAmount: "0",
  });
  harness.assertTrue(`${targetAsset} hLP lower search bound succeeds`, lowProbe.succeeds, lowProbe.errorCode);
  const highProbe = await harness.probe(wallet, "/api/v2/fork/tx/deposit-single-sided", {
    targetAsset,
    depositAmount: formatUnits(high, decimals),
    minHlpAmount: "0",
  });
  harness.assertEqual(`${targetAsset} hLP upper bound reaches cash headroom`, highProbe.errorCode, "InsufficientBorrowHeadroom");

  let passing = low;
  let failing = high;
  while (passing + 1n < failing) {
    const middle = (passing + failing) / 2n;
    const probe = await harness.probe(wallet, "/api/v2/fork/tx/deposit-single-sided", {
      targetAsset,
      depositAmount: formatUnits(middle, decimals),
      minHlpAmount: "0",
    });
    if (probe.succeeds) passing = middle;
    else failing = middle;
  }
  return passing;
}

async function previewMarket(harness: ProtocolTestHarness, label: string) {
  const evidence = await harness.execute({
    wallet: "trader",
    endpoint: "/api/v2/fork/tx/preview-market",
    label,
    submit: false,
    body: {},
  });
  return decodePreviewMarketReturnData(previewData(evidence));
}

export const MARKET_BOUNDARY_SCENARIOS: ScenarioDefinition[] = [
  {
    id: "swap.boundary-search",
    async run(harness) {
      for (const assetIn of ["base", "quote"] as const) {
        const decimals = decimalsFor(harness, assetIn);
        const minimum = await firstPassingRawSwap(harness, assetIn);
        harness.observe(`${assetIn} minimum executable swap raw units`, minimum);

        if (minimum > 1n) {
          await harness.execute({
            wallet: "trader",
            endpoint: "/api/v2/fork/tx/preview-swap",
            label: `reject ${assetIn} swap one raw unit below minimum`,
            expected: "failure",
            body: { assetIn, exactAssetIn: formatUnits(minimum - 1n, decimals) },
          });
        }
        await harness.execute({
          wallet: "trader",
          endpoint: "/api/v2/fork/tx/swap",
          label: `execute minimum raw-unit ${assetIn} swap`,
          body: { assetIn, exactAssetIn: formatUnits(minimum, decimals), minAssetOut: "0" },
        });

        const walletBalance = await harness.tokenBalance(
          "trader",
          mintFor(harness, assetIn),
          tokenProgramFor(harness, assetIn)
        );
        await harness.execute({
          wallet: "trader",
          endpoint: "/api/v2/fork/tx/swap",
          label: `simulate ${assetIn} swap at exact funded-wallet boundary`,
          submit: false,
          body: { assetIn, exactAssetIn: formatUnits(walletBalance, decimals), minAssetOut: "0" },
        });
        const overBalance = await harness.execute({
          wallet: "trader",
          endpoint: "/api/v2/fork/tx/swap",
          label: `reject ${assetIn} swap one raw unit beyond wallet balance`,
          expected: "failure",
          body: { assetIn, exactAssetIn: formatUnits(walletBalance + 1n, decimals), minAssetOut: "0" },
        });
        harness.assertTrue(
          `${assetIn} funded-wallet overflow fails in token transfer`,
          overBalance.errorCode !== null,
          overBalance.errorCode
        );
      }
    },
  },
  {
    id: "swap.active-hlp-checkpoints",
    async run(harness) {
      const baseBefore = await harness.lpBalance("trader", harness.config.baseHlpMint);
      const quoteBefore = await harness.lpBalance("trader", harness.config.quoteHlpMint);
      const previewBefore = await previewMarket(harness, "preview market before activating both hLP sides");

      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/deposit-single-sided",
        label: "activate base hLP vault for swap settlement",
        body: { targetAsset: "base", depositAmount: "20", minHlpAmount: "0" },
      });
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/deposit-single-sided",
        label: "activate quote hLP vault for swap settlement",
        body: { targetAsset: "quote", depositAmount: "20", minHlpAmount: "0" },
      });
      const active = await previewMarket(harness, "preview market with both hLP sides active");
      harness.assertTrue("base-denominated hLP funding debt is active", integer(active.base.hlpFundingDebt) > 0n, active.base.hlpFundingDebt);
      harness.assertTrue("quote-denominated hLP funding debt is active", integer(active.quote.hlpFundingDebt) > 0n, active.quote.hlpFundingDebt);

      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "swap base while both hLP vaults settle",
        body: { assetIn: "base", exactAssetIn: "10", minAssetOut: "0" },
      });
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "swap quote while both hLP vaults settle",
        body: { assetIn: "quote", exactAssetIn: "10", minAssetOut: "0" },
      });
      const afterSwaps = await previewMarket(harness, "preview both hLP sides after bidirectional swaps");
      harness.assertTrue("base hLP funding remains accounted after swaps", integer(afterSwaps.base.hlpFundingDebt) > 0n, afterSwaps.base.hlpFundingDebt);
      harness.assertTrue("quote hLP funding remains accounted after swaps", integer(afterSwaps.quote.hlpFundingDebt) > 0n, afterSwaps.quote.hlpFundingDebt);

      const baseMinted = await harness.lpBalance("trader", harness.config.baseHlpMint) - baseBefore;
      const quoteMinted = await harness.lpBalance("trader", harness.config.quoteHlpMint) - quoteBefore;
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/withdraw-single-sided",
        label: "withdraw active base hLP test shares",
        body: { targetAsset: "base", hlpAmount: formatUnits(baseMinted, harness.config.baseDecimals), minTargetAmountOut: "0" },
      });
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/withdraw-single-sided",
        label: "withdraw active quote hLP test shares",
        body: { targetAsset: "quote", hlpAmount: formatUnits(quoteMinted, harness.config.quoteDecimals), minTargetAmountOut: "0" },
      });
      const finalPreview = await previewMarket(harness, "preview market after both hLP exits");
      harness.assertEqual("base hLP funding debt returns to baseline", integer(finalPreview.base.hlpFundingDebt), integer(previewBefore.base.hlpFundingDebt));
      harness.assertEqual("quote hLP funding debt returns to baseline", integer(finalPreview.quote.hlpFundingDebt), integer(previewBefore.quote.hlpFundingDebt));
      harness.assertEqual("base hLP wallet balance returns to baseline", await harness.lpBalance("trader", harness.config.baseHlpMint), baseBefore);
      harness.assertEqual("quote hLP wallet balance returns to baseline", await harness.lpBalance("trader", harness.config.quoteHlpMint), quoteBefore);
    },
  },
  {
    id: "hlp.cash-and-daily-limit-boundaries",
    async run(harness) {
      await harness.fundWallet("bidder", "1000000", "1000000");
      for (const targetAsset of ["base", "quote"] as const) {
        const decimals = decimalsFor(harness, targetAsset);
        const hlpMint = targetAsset === "base" ? harness.config.baseHlpMint : harness.config.quoteHlpMint;
        const debtAsset = targetAsset === "base" ? "quote" : "base";
        const bucketKey = debtAsset === "base" ? "baseDailyBorrowedBucket" : "quoteDailyBorrowedBucket";
        const debtPreviewKey = debtAsset === "base" ? "base" : "quote";
        const before = await harness.market();
        const sharesBefore = await harness.lpBalance("bidder", hlpMint);
        const debtBeforePreview = await previewMarket(harness, `preview before ${targetAsset} hLP cash-boundary search`);
        const maximum = await largestPassingHlpDeposit(
          harness,
          "bidder",
          targetAsset,
          raw(1, decimals),
          raw(900_000, decimals)
        );
        harness.observe(`${targetAsset} maximum hLP deposit at current cash headroom`, {
          raw: maximum,
          ui: formatUnits(maximum, decimals),
        });

        const rejected = await harness.execute({
          wallet: "bidder",
          endpoint: "/api/v2/fork/tx/deposit-single-sided",
          label: `reject ${targetAsset} hLP deposit one raw unit beyond cash headroom`,
          expected: "failure",
          body: { targetAsset, depositAmount: formatUnits(maximum + 1n, decimals), minHlpAmount: "0" },
        });
        harness.assertEqual(`${targetAsset} hLP cash boundary has deterministic error`, rejected.errorCode, "InsufficientBorrowHeadroom");

        await harness.execute({
          wallet: "bidder",
          endpoint: "/api/v2/fork/tx/deposit-single-sided",
          label: `execute maximum ${targetAsset} hLP cash-boundary deposit`,
          body: { targetAsset, depositAmount: formatUnits(maximum, decimals), minHlpAmount: "0" },
        });
        const afterDeposit = await harness.market();
        const afterDepositPreview = await previewMarket(harness, `preview maximum ${targetAsset} hLP deposit`);
        harness.assertEqual(
          `${targetAsset} hLP internal debt does not consume normal daily borrow bucket`,
          stateValue(afterDeposit, bucketKey),
          stateValue(before, bucketKey)
        );
        harness.assertTrue(
          `${targetAsset} hLP deposit creates opposite-side funding debt`,
          integer(afterDepositPreview[debtPreviewKey].hlpFundingDebt) > integer(debtBeforePreview[debtPreviewKey].hlpFundingDebt),
          afterDepositPreview[debtPreviewKey].hlpFundingDebt
        );

        const minted = await harness.lpBalance("bidder", hlpMint) - sharesBefore;
        await harness.execute({
          wallet: "bidder",
          endpoint: "/api/v2/fork/tx/withdraw-single-sided",
          label: `withdraw maximum ${targetAsset} hLP cash-boundary deposit`,
          body: { targetAsset, hlpAmount: formatUnits(minted, decimals), minTargetAmountOut: "0" },
        });
        const afterExit = await harness.market();
        const afterExitPreview = await previewMarket(harness, `preview after ${targetAsset} hLP cash-boundary exit`);
        harness.assertEqual(`${targetAsset} hLP test shares are fully burned`, await harness.lpBalance("bidder", hlpMint), sharesBefore);
        harness.assertEqual(
          `${targetAsset} hLP funding debt returns to baseline`,
          integer(afterExitPreview[debtPreviewKey].hlpFundingDebt),
          integer(debtBeforePreview[debtPreviewKey].hlpFundingDebt)
        );
        harness.assertEqual(
          `${targetAsset} hLP exit also leaves daily borrow bucket unchanged`,
          stateValue(afterExit, bucketKey),
          stateValue(before, bucketKey)
        );
      }
    },
  },
];

export const POST_GOVERNANCE_MARKET_SCENARIOS: ScenarioDefinition[] = [
  {
    id: "swap.fee-routing",
    async run(harness) {
      const originalFutarchy = await harness.futarchy();
      const originalMarket = await harness.market();
      const routedConfig = { ...originalMarket.config, managerFeeBps: 500 };

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-protocol-revenue",
        label: "set protocol swap revenue share for routing test",
        body: { swapBps: 2_000 },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-config",
        label: "schedule manager swap revenue share for routing test",
        body: { config: routedConfig },
      });
      await harness.timeTravel(0, harness.config.governanceDelaySlots + 10);
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-config",
        label: "apply manager swap revenue share for routing test",
        body: { config: routedConfig },
      });

      const before = await harness.market();
      const previewEvidence = await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/preview-swap",
        label: "preview fee-routed base swap",
        submit: false,
        body: { assetIn: "base", exactAssetIn: "100" },
      });
      const preview = decodePreviewSwapReturnData(previewData(previewEvidence));
      const feeCredit = integer(preview.feeCredit);
      const expectedManager = feeCredit * 500n / 10_000n;
      const expectedProtocol = feeCredit * 2_000n / 10_000n;
      const expectedLp = feeCredit - expectedManager - expectedProtocol;
      const feeVaultBefore = await harness.tokenAccountBalance(
        new PublicKey(before.baseFeeVault),
        harness.config.baseTokenProgram
      );

      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "execute manager protocol and LP fee-routed swap",
        body: { assetIn: "base", exactAssetIn: "100", minAssetOut: "0" },
      });
      const after = await harness.market();
      const feeVaultAfter = await harness.tokenAccountBalance(
        new PublicKey(after.baseFeeVault),
        harness.config.baseTokenProgram
      );
      const managerDelta = stateValue(after, "baseManagerSwapFeeLiability") - stateValue(before, "baseManagerSwapFeeLiability");
      const protocolDelta =
        stateValue(after, "baseProtocolFeeLiability") - stateValue(before, "baseProtocolFeeLiability") +
        stateValue(after, "baseBuybackFeeLiability") - stateValue(before, "baseBuybackFeeLiability");
      const lpDelta =
        stateValue(after, "baseLpSwapFeeLiability") - stateValue(before, "baseLpSwapFeeLiability") +
        stateValue(after, "baseUnallocatedSwapFeeLiability") - stateValue(before, "baseUnallocatedSwapFeeLiability");
      harness.assertEqual("fee vault receives exact previewed credit", feeVaultAfter - feeVaultBefore, feeCredit);
      harness.assertEqual("manager receives exact configured fee liability", managerDelta, expectedManager);
      harness.assertEqual("protocol auction lanes receive exact configured liability", protocolDelta, expectedProtocol);
      harness.assertEqual("LP liabilities receive the complete remainder", lpDelta, expectedLp);
      harness.assertEqual("every fee unit is assigned exactly once", managerDelta + protocolDelta + lpDelta, feeCredit);

      const managerBaseBefore = await harness.tokenBalance(
        "alice",
        harness.config.baseMint,
        harness.config.baseTokenProgram
      );
      const totalManagerClaim = stateValue(after, "baseManagerSwapFeeLiability") +
        stateValue(after, "baseManagerInterestFeeLiability");
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/claim-manager-fees",
        label: "claim all base manager fee liabilities",
        body: { asset: "base" },
      });
      const claimed = await harness.market();
      harness.assertEqual(
        "manager receives exact swap plus interest claim",
        await harness.tokenBalance("alice", harness.config.baseMint, harness.config.baseTokenProgram) - managerBaseBefore,
        totalManagerClaim
      );
      harness.assertEqual("manager swap liability clears", stateValue(claimed, "baseManagerSwapFeeLiability"), 0n);
      harness.assertEqual("manager interest liability clears", stateValue(claimed, "baseManagerInterestFeeLiability"), 0n);
      const emptyClaim = await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/claim-manager-fees",
        label: "reject empty manager fee claim",
        expected: "failure",
        body: { asset: "base" },
      });
      harness.assertEqual("empty manager claim has deterministic error", emptyClaim.errorCode, "AmountZero");

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-config",
        label: "schedule restoration after fee routing test",
        body: { config: originalMarket.config },
      });
      await harness.timeTravel(0, harness.config.governanceDelaySlots + 10);
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-config",
        label: "restore market fee routing configuration",
        body: { config: originalMarket.config },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-protocol-revenue",
        label: "restore protocol swap revenue share",
        body: { swapBps: originalFutarchy.revenueShare.swapBps },
      });
      harness.assertEqual("market fee config restores exactly", (await harness.market()).config, originalMarket.config);
      harness.assertEqual("protocol swap share restores exactly", (await harness.futarchy()).revenueShare.swapBps, originalFutarchy.revenueShare.swapBps);
    },
  },
];
