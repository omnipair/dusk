import { Keypair, PublicKey } from "@solana/web3.js";

import { decodePreviewBorrowPositionReturnData } from "../../../packages/dusk-sdk/src/preview.js";
import {
  formatUnits,
  MutationOutcomeUncertainError,
  type ProtocolTestHarness,
  type ScenarioDefinition,
} from "../harness.js";
import type { TransactionEvidence } from "../types.js";

const bidPositionId = Keypair.generate().publicKey;
const floorPositionId = Keypair.generate().publicKey;
const badDebtPositionId = Keypair.generate().publicKey;
const NAD = 1_000_000_000n;
const BPS_DENOMINATOR = 10_000n;

function ceilDiv(value: bigint, denominator: bigint): bigint {
  return (value + denominator - 1n) / denominator;
}

function normalizeToNad(amount: bigint, decimals: number): bigint {
  if (decimals === 9) return amount;
  if (decimals < 9) return amount * 10n ** BigInt(9 - decimals);
  return amount / 10n ** BigInt(decimals - 9);
}

function denormalizeFromNadCeil(amount: bigint, decimals: number): bigint {
  if (decimals === 9) return amount;
  if (decimals < 9) return ceilDiv(amount, 10n ** BigInt(9 - decimals));
  return amount * 10n ** BigInt(decimals - 9);
}

function collateralForRepay(
  repayAmount: bigint,
  debtDecimals: number,
  collateralDecimals: number,
  totalPenaltyBps: number,
  debtPerCollateralPriceNad: bigint
): bigint {
  const debtWithPenalty = ceilDiv(
    repayAmount * (BPS_DENOMINATOR + BigInt(totalPenaltyBps)),
    BPS_DENOMINATOR
  );
  const debtValueNad = normalizeToNad(debtWithPenalty, debtDecimals);
  const collateralAmountNad = ceilDiv(debtValueNad * NAD, debtPerCollateralPriceNad);
  return denormalizeFromNadCeil(collateralAmountNad, collateralDecimals);
}

function minimumRepayToExhaustCollateral(
  maxRepayAmount: bigint,
  collateralAmount: bigint,
  debtDecimals: number,
  collateralDecimals: number,
  totalPenaltyBps: number,
  debtPerCollateralPriceNad: bigint
): bigint | null {
  if (
    collateralForRepay(
      maxRepayAmount,
      debtDecimals,
      collateralDecimals,
      totalPenaltyBps,
      debtPerCollateralPriceNad
    ) < collateralAmount
  ) {
    return null;
  }
  let low = 1n;
  let high = maxRepayAmount;
  while (low < high) {
    const mid = (low + high) / 2n;
    if (
      collateralForRepay(
        mid,
        debtDecimals,
        collateralDecimals,
        totalPenaltyBps,
        debtPerCollateralPriceNad
      ) >= collateralAmount
    ) {
      high = mid;
    } else {
      low = mid + 1n;
    }
  }
  return low;
}

function eventAmount(data: Record<string, { toString(): string }>, key: string): bigint {
  const value = data[key];
  if (value === undefined) throw new Error(`Liquidation event does not expose ${key}`);
  return BigInt(value.toString());
}

function absoluteDifference(left: bigint, right: bigint): bigint {
  return left >= right ? left - right : right - left;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
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

async function previewPosition(
  harness: ProtocolTestHarness,
  wallet: string,
  positionId: PublicKey,
  label: string
) {
  const evidence = await harness.execute({
    wallet,
    endpoint: "/api/v2/fork/tx/preview-borrow-position",
    label,
    submit: false,
    body: { positionId: positionId.toBase58() },
  });
  return decodePreviewBorrowPositionReturnData(previewData(evidence));
}

async function maximumFloorSettlement(
  harness: ProtocolTestHarness,
  positionId: PublicKey,
  debtUpperBound: bigint
): Promise<bigint> {
  const body = (amount: bigint) => ({
    positionId: positionId.toBase58(),
    debtAsset: "quote",
    repayAmount: formatUnits(amount, harness.config.quoteDecimals),
    minCollateralOut: "0",
    maxInsuranceDraw: "0",
    maxSocializedLoss: "0",
  });
  let low = 0n;
  let high = debtUpperBound + 1n;
  while (low + 1n < high) {
    const middle = (low + high) / 2n;
    if ((await harness.probe("liquidator", "/api/v2/fork/tx/settle-liquidation-auction-floor", body(middle))).succeeds) {
      low = middle;
    } else {
      high = middle;
    }
  }
  return low;
}

async function settleAuctionWithinCashOnlyBound(
  harness: ProtocolTestHarness,
  liquidatorWallet: string,
  ownerWallet: string,
  positionId: PublicKey
): Promise<{ residualDebt: bigint; auctionStillActive: boolean }> {
  let startingDebt: bigint | null = null;
  for (let attempt = 1; attempt <= 6; attempt += 1) {
    const positions = await harness.positions(ownerWallet, positionId);
    const position = positions.find((entry) => entry.eventType === "borrow_position");
    if (!position) throw new Error(`Borrow position ${positionId.toBase58()} was not found`);
    if (BigInt(position.payload.auctionStartTime) === 0n) {
      const preview = await previewPosition(
        harness,
        liquidatorWallet,
        positionId,
        "preview floor-settled residual healthy debt"
      );
      const residualDebt = BigInt(preview.fixedQuoteDebt.toString());
      if (startingDebt !== null) {
        harness.assertTrue(
          "cash-only floor settlement reduces borrower debt before restoring health",
          residualDebt < startingDebt,
          { startingDebt, residualDebt }
        );
      }
      return { residualDebt, auctionStillActive: false };
    }

    const preview = await previewPosition(
      harness,
      liquidatorWallet,
      positionId,
      `preview floor-settlement cap ${attempt}`
    );
    const debt = BigInt(preview.fixedQuoteDebt.toString());
    startingDebt ??= debt;
    const maxRepayAmount = await maximumFloorSettlement(harness, positionId, debt);
    const oneBasisPointOfDebt = ceilDiv(debt, BPS_DENOMINATOR);
    if (maxRepayAmount <= oneBasisPointOfDebt) {
      harness.assertTrue(
        "cash-only floor settlement reduces debt before reaching its stored-floor pricing boundary",
        debt < startingDebt,
        { startingDebt, debt, maxRepayAmount, oneBasisPointOfDebt }
      );
      harness.observe("cash-only floor settlement reached its stored-floor pricing boundary", {
        attempt,
        debt,
        maxRepayAmount,
        oneBasisPointOfDebt,
      });
      return { residualDebt: debt, auctionStillActive: true };
    }
    harness.assertTrue("active floor settlement exposes a positive repay cap", maxRepayAmount > 0n, {
      debt: preview.fixedQuoteDebt,
      referencePriceMaxRepayAmount: preview.quoteDebt.maxRepayAmount,
      auctionFloorMaxRepayAmount: maxRepayAmount,
    });
    await harness.execute({
      wallet: liquidatorWallet,
      endpoint: "/api/v2/fork/tx/settle-liquidation-auction-floor",
      label: `externally funded floor settlement attempt ${attempt}`,
      body: {
        positionId: positionId.toBase58(),
        debtAsset: "quote",
        repayAmount: formatUnits(maxRepayAmount, harness.config.quoteDecimals),
        minCollateralOut: "0",
        maxInsuranceDraw: "0",
        maxSocializedLoss: "0",
      },
    });
  }
  const residualPreview = await previewPosition(
    harness,
    liquidatorWallet,
    positionId,
    "preview bounded cash-only floor-settlement residual debt"
  );
  const residualDebt = BigInt(residualPreview.fixedQuoteDebt.toString());
  harness.assertTrue(
    "bounded cash-only floor settlements reduce borrower debt",
    startingDebt !== null && residualDebt < startingDebt,
    { startingDebt, residualDebt }
  );
  return { residualDebt, auctionStillActive: true };
}

async function repayResidualDebt(
  harness: ProtocolTestHarness,
  ownerWallet: string,
  positionId: PublicKey,
  residualDebt: bigint
): Promise<void> {
  harness.assertTrue("cash-only liquidation preserves residual owner debt", residualDebt > 0n, residualDebt);
  const repayMax = await harness.tokenBalance(
    ownerWallet,
    harness.config.quoteMint,
    harness.config.quoteTokenProgram,
  );
  harness.assertTrue(
    `${ownerWallet} quote balance covers residual auction debt`,
    repayMax >= residualDebt,
    { repayMax, residualDebt },
  );
  await harness.execute({
    wallet: ownerWallet,
    endpoint: "/api/v2/fork/tx/repay",
    label: `repay ${ownerWallet} residual debt after auction settlements`,
    body: {
      positionId: positionId.toBase58(),
      repayAsset: "quote",
      repayAmount: formatUnits(repayMax, harness.config.quoteDecimals),
    },
  });
  const preview = await previewPosition(harness, ownerWallet, positionId, `preview ${ownerWallet} repaid loan`);
  harness.assertEqual("borrower can fully repay residual auction debt", BigInt(preview.fixedQuoteDebt.toString()), 0n);
}

async function withdrawRemainingCollateral(
  harness: ProtocolTestHarness,
  wallet: string,
  positionId: PublicKey
): Promise<void> {
  const positions = await harness.positions(wallet, positionId);
  const position = positions.find((entry) => entry.eventType === "borrow_position");
  if (!position) throw new Error(`Borrow position ${positionId.toBase58()} was not found`);
  const remainingCollateral = BigInt(position.payload.baseCollateral);
  if (remainingCollateral === 0n) return;
  await harness.execute({
    wallet,
    endpoint: "/api/v2/fork/tx/withdraw-collateral",
    label: `withdraw ${wallet} collateral left after auction`,
    body: {
      positionId: positionId.toBase58(),
      marketAsset: "base",
      withdrawAmount: formatUnits(remainingCollateral, harness.config.baseDecimals),
      minAssetAmountOut: "0",
      minLiquidationCfBps: 0,
    },
  });
}

async function cleanupBadDebtScenario(
  harness: ProtocolTestHarness,
  traderBaseBeforeShock: bigint | null,
): Promise<void> {
  const errors: string[] = [];
  let mutationOutcomeUncertain = false;
  const attempt = async (
    label: string,
    action: () => Promise<void>,
    mutates = true,
  ): Promise<void> => {
    if (mutates && mutationOutcomeUncertain) {
      errors.push(`${label}: skipped after an uncertain cleanup mutation`);
      return;
    }
    try {
      await action();
    } catch (error) {
      errors.push(`${label}: ${errorMessage(error)}`);
      if (error instanceof MutationOutcomeUncertainError) {
        mutationOutcomeUncertain = true;
      }
    }
  };

  let reversedShock = false;
  if (traderBaseBeforeShock !== null) {
    await attempt("reverse bad-debt price shock", async () => {
      const traderBaseNow = await harness.tokenBalance(
        "trader",
        harness.config.baseMint,
        harness.config.baseTokenProgram,
      );
      const traderBaseGained = traderBaseNow > traderBaseBeforeShock
        ? traderBaseNow - traderBaseBeforeShock
        : 0n;
      harness.observe("bad-debt cleanup trader base delta", {
        traderBaseBeforeShock,
        traderBaseNow,
        traderBaseGained,
      });
      if (traderBaseGained === 0n) return;
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "reverse remaining deep quote shock during bad-debt cleanup",
        body: {
          assetIn: "base",
          exactAssetIn: formatUnits(traderBaseGained, harness.config.baseDecimals),
          minAssetOut: "0",
        },
      });
      reversedShock = true;
    });
    if (reversedShock) await attempt("checkpoint restored bad-debt spot", async () => {
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "checkpoint restored spot during bad-debt cleanup",
        body: { assetIn: "base", exactAssetIn: "0.001", minAssetOut: "0" },
      });
    });
    if (reversedShock) await attempt("advance restored bad-debt EMA", async () => {
      await harness.timeTravel(1, 1_000);
    });
    if (reversedShock) await attempt("persist restored bad-debt EMA", async () => {
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "persist restored EMA during bad-debt cleanup",
        body: { assetIn: "base", exactAssetIn: "0.001", minAssetOut: "0" },
      });
    });
  }

  await attempt("repay remaining bad-debt fixture debt", async () => {
    const storedPosition = (await harness.positions("alice", badDebtPositionId)).find(
      (entry) => entry.eventType === "borrow_position",
    );
    if (!storedPosition) return;
    const position = await previewPosition(
      harness,
      "alice",
      badDebtPositionId,
      "preview Alice bad-debt cleanup debt",
    );
    const debt = BigInt(position.fixedBaseDebt.toString());
    if (debt === 0n) return;
    const repayMax = await harness.tokenBalance(
      "alice",
      harness.config.baseMint,
      harness.config.baseTokenProgram,
    );
    if (repayMax === 0n) {
      throw new Error(`Alice has no base balance to repay ${debt} raw debt atoms`);
    }
    harness.observe("bad-debt cleanup repayment bound", { debt, repayMax });
    await harness.execute({
      wallet: "alice",
      endpoint: "/api/v2/fork/tx/repay",
      label: "repay Alice base debt during bad-debt cleanup",
      body: {
        positionId: badDebtPositionId.toBase58(),
        repayAsset: "base",
        repayAmount: formatUnits(repayMax, harness.config.baseDecimals),
      },
    });
  });

  await attempt("withdraw remaining bad-debt fixture collateral", async () => {
    const position = (await harness.positions("alice", badDebtPositionId)).find(
      (entry) => entry.eventType === "borrow_position",
    );
    if (!position) return;
    const quoteCollateral = BigInt(position.payload.quoteCollateral);
    if (quoteCollateral === 0n) return;
    await harness.execute({
      wallet: "alice",
      endpoint: "/api/v2/fork/tx/withdraw-collateral",
      label: "withdraw Alice quote collateral during bad-debt cleanup",
      body: {
        positionId: badDebtPositionId.toBase58(),
        marketAsset: "quote",
        withdrawAmount: formatUnits(quoteCollateral, harness.config.quoteDecimals),
        minAssetAmountOut: "0",
        minLiquidationCfBps: 0,
      },
    });
  });

  await attempt("verify bad-debt fixture cleanup", async () => {
    const storedPosition = (await harness.positions("alice", badDebtPositionId)).find(
      (entry) => entry.eventType === "borrow_position",
    );
    if (!storedPosition) {
      harness.observe("bad-debt cleanup position account", "closed");
      return;
    }
    const position = await previewPosition(
      harness,
      "alice",
      badDebtPositionId,
      "verify cleaned bad-debt fixture position",
    );
    harness.assertEqual(
      "bad-debt cleanup leaves zero base debt",
      BigInt(position.fixedBaseDebt.toString()),
      0n,
    );
    harness.assertEqual(
      "bad-debt cleanup leaves zero quote collateral",
      BigInt(position.quoteCollateral.toString()),
      0n,
    );
  }, false);

  if (errors.length > 0) {
    throw new Error(`Bad-debt scenario cleanup was incomplete: ${errors.join("; ")}`);
  }
}

export const LIQUIDATION_SCENARIOS: ScenarioDefinition[] = [
  {
    id: "liquidation.auction-lifecycle",
    async run(harness) {
      for (const [wallet, positionId] of [
        ["alice", bidPositionId],
        ["bob", floorPositionId],
      ] as const) {
        await harness.execute({
          wallet,
          endpoint: "/api/v2/fork/tx/deposit-collateral",
          label: `deposit ${wallet} collateral for liquidation auction`,
          body: { positionId: positionId.toBase58(), marketAsset: "base", depositAmount: "100" },
        });
        await harness.execute({
          wallet,
          endpoint: "/api/v2/fork/tx/borrow",
          label: `borrow quote for ${wallet} liquidation auction`,
          body: {
            positionId: positionId.toBase58(),
            borrowAsset: "quote",
            borrowAmount: "50",
            minDebtAmountOut: "50",
            minLiquidationCfBps: 0,
          },
        });
      }

      await harness.execute({
        wallet: "liquidator",
        endpoint: "/api/v2/fork/tx/trigger-liquidation-auction",
        label: "reject auction trigger while loan is healthy",
        expected: "failure",
        body: { positionId: bidPositionId.toBase58(), debtAsset: "quote" },
      });

      await harness.fundWallet("trader", "100000", "100000");
      const marketBeforeShock = await harness.market();
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "move collateral price below liquidation threshold",
        body: { assetIn: "base", exactAssetIn: "35000", minAssetOut: "0" },
      });
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "checkpoint shocked spot price for liquidation EMA",
        body: { assetIn: "base", exactAssetIn: "0.001", minAssetOut: "0" },
      });
      await harness.timeTravel(1, 1_000);
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "persist liquidation EMA after slot advancement",
        body: { assetIn: "base", exactAssetIn: "0.001", minAssetOut: "0" },
      });
      const marketAfterShock = await harness.market();
      harness.assertTrue(
        "loan price shock increases base reserves",
        stateValue(marketAfterShock, "baseLiveReserve") > stateValue(marketBeforeShock, "baseLiveReserve")
      );

      let liquidationPreviews = [] as Awaited<ReturnType<typeof previewPosition>>[];
      for (let attempt = 1; attempt <= 3; attempt += 1) {
        liquidationPreviews = [];
        for (const [wallet, positionId] of [
          ["alice", bidPositionId],
          ["bob", floorPositionId],
        ] as const) {
          liquidationPreviews.push(
            await previewPosition(
              harness,
              wallet,
              positionId,
              `preview ${wallet} liquidation eligibility attempt ${attempt}`
            )
          );
        }
        harness.observe(`liquidation eligibility attempt ${attempt}`, liquidationPreviews.map((preview) => ({
          debt: preview.fixedQuoteDebt,
          collateralValueNad: preview.quoteDebt.collateralValueNad,
          liquidationCfBps: preview.quoteDebt.liquidationCfBps,
          liquidationReferencePriceNad: preview.quoteDebt.liquidationReferencePriceNad,
          liquidationHealthBps: preview.quoteDebt.liquidationHealthBps,
          isLiquidatable: preview.quoteDebt.isLiquidatable,
          maxRepayAmount: preview.quoteDebt.maxRepayAmount,
        })));
        if (liquidationPreviews.every((preview) => preview.quoteDebt.isLiquidatable)) break;
        await harness.timeTravel(1, 1_000);
        await harness.execute({
          wallet: "trader",
          endpoint: "/api/v2/fork/tx/swap",
          label: `persist liquidation EMA attempt ${attempt + 1}`,
          body: { assetIn: "base", exactAssetIn: "0.001", minAssetOut: "0" },
        });
      }
      harness.assertTrue(
        "both shocked loans are liquidatable according to preview",
        liquidationPreviews.every((preview) => preview.quoteDebt.isLiquidatable),
        liquidationPreviews.map((preview) => preview.quoteDebt)
      );
      for (const preview of liquidationPreviews) {
        harness.assertTrue(
          "liquidation close factor caps one settlement below full debt",
          BigInt(preview.quoteDebt.maxRepayAmount.toString()) < BigInt(preview.fixedQuoteDebt.toString()),
          preview.quoteDebt
        );
      }

      for (const positionId of [bidPositionId, floorPositionId]) {
        await harness.execute({
          wallet: "liquidator",
          endpoint: "/api/v2/fork/tx/trigger-liquidation-auction",
          label: `trigger liquidation auction ${positionId.toBase58().slice(0, 6)}`,
          body: { positionId: positionId.toBase58(), debtAsset: "quote" },
        });
        const positions = await harness.positions(
          positionId.equals(bidPositionId) ? "alice" : "bob",
          positionId
        );
        const position = positions.find((entry) => entry.eventType === "borrow_position");
        harness.assertTrue("auction start time is recorded", BigInt(position.payload.auctionStartTime) > 0n);
        harness.assertEqual("auction records its quote debt asset", position.payload.auctionDebtAsset, "quote");
        harness.assertTrue(
          "auction starts above its floor",
          BigInt(position.payload.auctionStartPriceNad) > BigInt(position.payload.auctionFloorPriceNad)
        );
      }

      const wrongAssetBid = await harness.execute({
        wallet: "bidder",
        endpoint: "/api/v2/fork/tx/bid-liquidation-auction",
        label: "reject bid for debt asset other than triggered auction asset",
        expected: "failure",
        body: {
          positionId: bidPositionId.toBase58(),
          debtAsset: "base",
          repayAmount: "1",
          minCollateralOut: "0",
        },
      });
      harness.assertEqual("wrong-side auction bid fails at asset binding", wrongAssetBid.errorCode, "PositionNotLiquidatable");

      await harness.execute({
        wallet: "liquidator",
        endpoint: "/api/v2/fork/tx/settle-liquidation-auction-floor",
        label: "reject external settlement before Dutch auction floor",
        expected: "failure",
        body: {
          positionId: floorPositionId.toBase58(),
          debtAsset: "quote",
          repayAmount: "10",
          minCollateralOut: "0",
          maxInsuranceDraw: "0",
          maxSocializedLoss: "0",
        },
      });

      const bidderBaseBefore = await harness.tokenBalance(
        "bidder",
        harness.config.baseMint,
        harness.config.baseTokenProgram
      );
      const bidPreview = await previewPosition(
        harness,
        "bidder",
        bidPositionId,
        "preview debt before first external bid"
      );
      const bidDebtBefore = BigInt(bidPreview.fixedQuoteDebt.toString());
      const bidMaxRepayAmount = BigInt(bidPreview.quoteDebt.maxRepayAmount.toString());
      // Submit immediately instead of binary-searching a Dutch-auction boundary:
      // wall-clock decay during repeated simulations changes the bid price and
      // can turn an otherwise healthy partial liquidation into collateral exhaustion.
      const bidRepayAmount = 10n ** BigInt(harness.config.quoteDecimals);
      harness.assertTrue("active external auction covers the deterministic partial bid", bidMaxRepayAmount >= bidRepayAmount, {
        bidDebtBefore,
        bidMaxRepayAmount,
        bidRepayAmount,
      });
      await harness.execute({
        wallet: "bidder",
        endpoint: "/api/v2/fork/tx/bid-liquidation-auction",
        label: "submit partial external liquidation bid",
        body: {
          positionId: bidPositionId.toBase58(),
          debtAsset: "quote",
          repayAmount: formatUnits(bidRepayAmount, harness.config.quoteDecimals),
          minCollateralOut: "0",
        },
      });
      const bidAfterPartial = await previewPosition(
        harness,
        "bidder",
        bidPositionId,
        "preview debt after partial external bid"
      );
      harness.assertTrue(
        "partial bid reduces borrower debt",
        BigInt(bidAfterPartial.fixedQuoteDebt.toString()) < bidDebtBefore
      );
      harness.assertTrue(
        "partial bid transfers collateral to bidder",
        await harness.tokenBalance("bidder", harness.config.baseMint, harness.config.baseTokenProgram) > bidderBaseBefore
      );
      await harness.execute({
        wallet: "bidder",
        endpoint: "/api/v2/fork/tx/bid-liquidation-auction",
        label: "reject liquidation bid above remaining debt and collateral",
        expected: "failure",
        body: {
          positionId: bidPositionId.toBase58(),
          debtAsset: "quote",
          repayAmount: "1000",
          minCollateralOut: "0",
        },
      });
      await harness.timeTravel(30, 100);
      const bidSettlement = await settleAuctionWithinCashOnlyBound(
        harness,
        "liquidator",
        "alice",
        bidPositionId
      );
      harness.observe("partial-bid cash-only settlement outcome", bidSettlement);
      await repayResidualDebt(harness, "alice", bidPositionId, bidSettlement.residualDebt);
      const bidPositions = await harness.positions("alice", bidPositionId);
      const bidPosition = bidPositions.find((entry) => entry.eventType === "borrow_position");
      harness.assertEqual("residual repayment clears partial-bid auction timestamp", BigInt(bidPosition.payload.auctionStartTime), 0n);
      harness.assertEqual("residual repayment clears partial-bid auction debt asset", bidPosition.payload.auctionDebtAsset, null);
      await withdrawRemainingCollateral(harness, "alice", bidPositionId);

      const liquidatorBaseBefore = await harness.tokenBalance(
        "liquidator",
        harness.config.baseMint,
        harness.config.baseTokenProgram
      );
      await harness.execute({
        wallet: "liquidator",
        endpoint: "/api/v2/fork/tx/settle-liquidation-auction-floor",
        label: "settle partial loan with external capital at the floor",
        body: {
          positionId: floorPositionId.toBase58(),
          debtAsset: "quote",
          repayAmount: "10",
          minCollateralOut: "0",
          maxInsuranceDraw: "0",
          maxSocializedLoss: "0",
        },
      });
      harness.assertTrue(
        "floor settlement transfers collateral to the external liquidator",
        await harness.tokenBalance("liquidator", harness.config.baseMint, harness.config.baseTokenProgram) > liquidatorBaseBefore
      );
      const floorSettlement = await settleAuctionWithinCashOnlyBound(
        harness,
        "liquidator",
        "bob",
        floorPositionId
      );
      harness.observe("floor cash-only settlement outcome", floorSettlement);
      await repayResidualDebt(harness, "bob", floorPositionId, floorSettlement.residualDebt);
      const floorPositions = await harness.positions("bob", floorPositionId);
      const floorPosition = floorPositions.find((entry) => entry.eventType === "borrow_position");
      harness.assertEqual("residual repayment clears floor auction timestamp", BigInt(floorPosition.payload.auctionStartTime), 0n);
      harness.assertEqual("residual repayment clears floor auction debt asset", floorPosition.payload.auctionDebtAsset, null);
      await withdrawRemainingCollateral(harness, "bob", floorPositionId);

      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "restore pool ratio after loan liquidation auctions",
        body: { assetIn: "quote", exactAssetIn: "25500", minAssetOut: "0" },
      });
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "checkpoint restored spot after loan liquidations",
        body: { assetIn: "quote", exactAssetIn: "0.001", minAssetOut: "0" },
      });
      await harness.timeTravel(1, 1_000);
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "persist restored loan-liquidation EMA",
        body: { assetIn: "quote", exactAssetIn: "0.001", minAssetOut: "0" },
      });
      const marketAfter = await harness.market();
      harness.assertEqual("auction loans leave no fixed quote debt", stateValue(marketAfter, "fixedQuoteDebt"), 0n);
      harness.assertEqual(
        "auction loans leave no quote global-health contribution",
        stateValue(marketAfter, "globalHealthBaseContributionForQuoteDebt"),
        0n
      );
      harness.assertTrue("bidder received at least one raw collateral unit", bidderBaseBefore + 1n <= await harness.tokenBalance("bidder", harness.config.baseMint, harness.config.baseTokenProgram));
      harness.assertTrue("liquidator received at least one raw collateral unit", liquidatorBaseBefore + 1n <= await harness.tokenBalance("liquidator", harness.config.baseMint, harness.config.baseTokenProgram));
    },
  },
  {
    id: "liquidation.bad-debt-and-insurance",
    async run(harness) {
      const marketBeforeLoan = await harness.market();
      const insuranceAvailable = stateValue(marketBeforeLoan, "baseInsuranceAvailable");
      const baseInsuranceVault = new PublicKey(marketBeforeLoan.baseInsuranceVault);
      const quoteInsuranceVault = new PublicKey(marketBeforeLoan.quoteInsuranceVault);
      const quoteCollateralVault = new PublicKey(marketBeforeLoan.quoteCollateralVault);
      const baseReserveVault = new PublicKey(marketBeforeLoan.baseReserveVault);
      const baseInterestVault = new PublicKey(marketBeforeLoan.baseInterestVault);
      harness.assertTrue(
        "normal loan liquidations fund base insurance before the bad-debt test",
        insuranceAvailable > 0n,
        insuranceAvailable
      );
      harness.assertEqual(
        "tracked base insurance equals its token vault",
        await harness.tokenAccountBalance(baseInsuranceVault, harness.config.baseTokenProgram),
        insuranceAvailable
      );

      const baseUnit = 10n ** BigInt(harness.config.baseDecimals);
      const quoteUnit = 10n ** BigInt(harness.config.quoteDecimals);
      const minimumBadDebtBorrow = 6n * baseUnit;
      const insuranceScaledBorrow = insuranceAvailable * 3n;
      const unroundedBadDebtBorrow = insuranceScaledBorrow > minimumBadDebtBorrow
        ? insuranceScaledBorrow
        : minimumBadDebtBorrow;
      const badDebtBorrow = ceilDiv(unroundedBadDebtBorrow, baseUnit) * baseUnit;
      const badDebtCollateral = (badDebtBorrow / baseUnit) * 2n * quoteUnit;
      harness.observe("insurance-scaled bad-debt fixture", {
        insuranceAvailable,
        badDebtBorrow,
        badDebtCollateral,
      });

      let traderBaseBeforeShock: bigint | null = null;
      let primaryFailed = false;
      let primaryError: unknown = null;
      try {
        await harness.execute({
          wallet: "alice",
          endpoint: "/api/v2/fork/tx/deposit-collateral",
          label: "deposit quote collateral for bad-debt loan",
          body: {
            positionId: badDebtPositionId.toBase58(),
            marketAsset: "quote",
            depositAmount: formatUnits(badDebtCollateral, harness.config.quoteDecimals),
          },
        });
        await harness.execute({
          wallet: "alice",
          endpoint: "/api/v2/fork/tx/borrow",
          label: "borrow base for bad-debt loan",
          body: {
            positionId: badDebtPositionId.toBase58(),
            borrowAsset: "base",
            borrowAmount: formatUnits(badDebtBorrow, harness.config.baseDecimals),
            minDebtAmountOut: formatUnits(badDebtBorrow, harness.config.baseDecimals),
            minLiquidationCfBps: 0,
          },
        });

        await harness.fundWallet("trader", "1000000", "1000000");
        traderBaseBeforeShock = await harness.tokenBalance(
          "trader",
          harness.config.baseMint,
          harness.config.baseTokenProgram
        );
        await harness.execute({
          wallet: "trader",
          endpoint: "/api/v2/fork/tx/swap",
          label: "deeply devalue quote collateral for bad-debt stress",
          body: { assetIn: "quote", exactAssetIn: "950000", minAssetOut: "0" },
        });
        const traderBaseAfterShock = await harness.tokenBalance(
          "trader",
          harness.config.baseMint,
          harness.config.baseTokenProgram
        );
        const shockBaseOut = traderBaseAfterShock - traderBaseBeforeShock;
        harness.assertTrue("deep quote swap pays real base output", shockBaseOut > 0n, shockBaseOut);
        await harness.execute({
          wallet: "trader",
          endpoint: "/api/v2/fork/tx/swap",
          label: "checkpoint deeply shocked quote price",
          body: { assetIn: "quote", exactAssetIn: "0.001", minAssetOut: "0" },
        });
        await harness.timeTravel(1, 1_000);
        await harness.execute({
          wallet: "trader",
          endpoint: "/api/v2/fork/tx/swap",
          label: "persist deeply shocked quote-price EMA",
          body: { assetIn: "quote", exactAssetIn: "0.001", minAssetOut: "0" },
        });

        let liquidationPreview: Awaited<ReturnType<typeof previewPosition>> | null = null;
        let repayAmount: bigint | null = null;
        for (let attempt = 1; attempt <= 5; attempt += 1) {
          liquidationPreview = await previewPosition(
            harness,
            "liquidator",
            badDebtPositionId,
            `preview collateral exhaustion attempt ${attempt}`
          );
          const debt = BigInt(liquidationPreview.fixedBaseDebt.toString());
          const collateral = BigInt(liquidationPreview.quoteCollateral.toString());
          const maxRepay = BigInt(liquidationPreview.baseDebt.maxRepayAmount.toString());
          const referencePrice = BigInt(
            liquidationPreview.baseDebt.liquidationReferencePriceNad.toString()
          );
          const candidate = referencePrice > 0n && maxRepay > 0n
            ? minimumRepayToExhaustCollateral(
                maxRepay,
                collateral,
                harness.config.baseDecimals,
                harness.config.quoteDecimals,
                liquidationPreview.baseDebt.totalPenaltyBps,
                referencePrice
              )
            : null;
          harness.observe(`bad-debt eligibility attempt ${attempt}`, {
            debt,
            collateral,
            maxRepay,
            referencePrice,
            totalPenaltyBps: liquidationPreview.baseDebt.totalPenaltyBps,
            isLiquidatable: liquidationPreview.baseDebt.isLiquidatable,
            minimumRepayToExhaustCollateral: candidate,
            insuranceAvailable,
          });
          if (
            liquidationPreview.baseDebt.isLiquidatable &&
            candidate !== null &&
            maxRepay > candidate &&
            maxRepay - candidate >= insuranceAvailable &&
            debt > candidate + insuranceAvailable
          ) {
            repayAmount = candidate;
            break;
          }
          await harness.timeTravel(1, 1_000);
          await harness.execute({
            wallet: "trader",
            endpoint: "/api/v2/fork/tx/swap",
            label: `advance bad-debt EMA attempt ${attempt + 1}`,
            body: { assetIn: "quote", exactAssetIn: "0.001", minAssetOut: "0" },
          });
        }
        if (!liquidationPreview || repayAmount === null) {
          throw new Error("Deeply insolvent position did not expose an insurance-exhausting settlement");
        }

        const debtBeforeSettlement = BigInt(liquidationPreview.fixedBaseDebt.toString());
        const maxRepayAmount = BigInt(liquidationPreview.baseDebt.maxRepayAmount.toString());
        const expectedInsuranceDraw = insuranceAvailable;
        const preAuctionSocializedLossQuote =
          debtBeforeSettlement - repayAmount - expectedInsuranceDraw;
        harness.assertTrue(
          "chosen repay exhausts collateral within the liquidation close factor",
          repayAmount <= maxRepayAmount,
          { repayAmount, maxRepayAmount }
        );
        harness.assertTrue(
          "close-factor headroom can draw every available insurance token",
          maxRepayAmount - repayAmount >= expectedInsuranceDraw,
          { repayAmount, maxRepayAmount, expectedInsuranceDraw }
        );
        harness.assertTrue(
          "insurance exhaustion still leaves debt to socialize",
          preAuctionSocializedLossQuote > 0n,
          preAuctionSocializedLossQuote
        );

        await harness.execute({
          wallet: "liquidator",
          endpoint: "/api/v2/fork/tx/trigger-liquidation-auction",
          label: "trigger deeply insolvent base-debt auction",
          body: { positionId: badDebtPositionId.toBase58(), debtAsset: "base" },
        });
        await harness.timeTravel(30, 100);
        const cappedFailure = await harness.execute({
          wallet: "liquidator",
          endpoint: "/api/v2/fork/tx/settle-liquidation-auction-floor",
          label: "reject bad-debt settlement without socialized-loss consent",
          expected: "failure",
          body: {
            positionId: badDebtPositionId.toBase58(),
            debtAsset: "base",
            repayAmount: formatUnits(repayAmount, harness.config.baseDecimals),
            minCollateralOut: "0",
            maxInsuranceDraw: formatUnits(expectedInsuranceDraw, harness.config.baseDecimals),
            maxSocializedLoss: "0",
          },
        });
        harness.assertEqual(
          "socialized-loss caller cap protects settlement",
          cappedFailure.errorCode,
          "LiquidationSocializationExceeded"
        );

        const refreshedPreview = await previewPosition(
          harness,
          "liquidator",
          badDebtPositionId,
          "refresh bad-debt socialization quote after auction wait",
        );
        const refreshedDebt = BigInt(refreshedPreview.fixedBaseDebt.toString());
        const refreshedSocializedLossQuote =
          refreshedDebt - repayAmount - expectedInsuranceDraw;
        const oneBasisPointOfDebt = ceilDiv(refreshedDebt, BPS_DENOMINATOR);
        const socializationHeadroom = oneBasisPointOfDebt > 1n ? oneBasisPointOfDebt : 1n;
        const socializedLossCap = refreshedSocializedLossQuote + socializationHeadroom;
        harness.assertTrue(
          "refreshed settlement still requires a positive socialized loss",
          refreshedSocializedLossQuote > 0n,
          { refreshedDebt, repayAmount, expectedInsuranceDraw },
        );
        harness.observe("refreshed bad-debt socialization bound", {
          refreshedDebt,
          repayAmount,
          expectedInsuranceDraw,
          refreshedSocializedLossQuote,
          socializationHeadroom,
          socializedLossCap,
        });

        const marketBeforeSettlement = await harness.market();
        const baseInsuranceVaultBefore = await harness.tokenAccountBalance(
          baseInsuranceVault,
          harness.config.baseTokenProgram
        );
        const quoteInsuranceVaultBefore = await harness.tokenAccountBalance(
          quoteInsuranceVault,
          harness.config.quoteTokenProgram
        );
        const quoteCollateralVaultBefore = await harness.tokenAccountBalance(
          quoteCollateralVault,
          harness.config.quoteTokenProgram
        );
        const baseReserveVaultBefore = await harness.tokenAccountBalance(
          baseReserveVault,
          harness.config.baseTokenProgram
        );
        const baseInterestVaultBefore = await harness.tokenAccountBalance(
          baseInterestVault,
          harness.config.baseTokenProgram
        );
        const liquidatorQuoteBefore = await harness.tokenBalance(
          "liquidator",
          harness.config.quoteMint,
          harness.config.quoteTokenProgram
        );
        const settlement = await harness.execute({
          wallet: "liquidator",
          endpoint: "/api/v2/fork/tx/settle-liquidation-auction-floor",
          label: "settle bad debt with exhausted insurance and bounded socialization",
          body: {
            positionId: badDebtPositionId.toBase58(),
            debtAsset: "base",
            repayAmount: formatUnits(repayAmount, harness.config.baseDecimals),
            minCollateralOut: "0",
            maxInsuranceDraw: formatUnits(expectedInsuranceDraw, harness.config.baseDecimals),
            maxSocializedLoss: formatUnits(socializedLossCap, harness.config.baseDecimals),
          },
        });

        const liquidationEvents = harness.events(settlement, "BorrowPositionLiquidated");
        harness.assertEqual("bad-debt settlement emits one liquidation receipt", liquidationEvents.length, 1);
        const receipt = liquidationEvents[0].data as Record<string, { toString(): string }>;
        const repaid = eventAmount(receipt, "repaid_amount");
        const collateralSeized = eventAmount(receipt, "collateral_seized");
        const collateralToLiquidator = eventAmount(receipt, "collateral_to_liquidator");
        const insuranceFunded = collateralSeized - collateralToLiquidator;
        const insuranceDrawn = eventAmount(receipt, "insurance_drawn");
        const socializedLoss = eventAmount(receipt, "socialized_loss");
        harness.assertEqual("receipt records exact liquidator repayment", repaid, repayAmount);
        harness.assertEqual("receipt exhausts available base insurance", insuranceDrawn, expectedInsuranceDraw);
        harness.assertTrue(
          "receipt remains within the caller's finite socialized-loss cap",
          socializedLoss <= socializedLossCap,
          { socializedLoss, socializedLossCap },
        );
        harness.assertTrue(
          "receipt remains within one-basis-point headroom of the refreshed quote",
          absoluteDifference(socializedLoss, refreshedSocializedLossQuote) <= socializationHeadroom,
          { socializedLoss, refreshedSocializedLossQuote, socializationHeadroom },
        );
        harness.assertEqual("receipt closes all position debt", eventAmount(receipt, "remaining_debt"), 0n);
        harness.assertEqual(
          "liquidator and collateral-insurance credits conserve seized collateral",
          collateralToLiquidator + insuranceFunded,
          collateralSeized
        );

        const marketAfterSettlement = await harness.market();
        const baseInsuranceVaultAfter = await harness.tokenAccountBalance(
          baseInsuranceVault,
          harness.config.baseTokenProgram
        );
        const quoteInsuranceVaultAfter = await harness.tokenAccountBalance(
          quoteInsuranceVault,
          harness.config.quoteTokenProgram
        );
        const quoteCollateralVaultAfter = await harness.tokenAccountBalance(
          quoteCollateralVault,
          harness.config.quoteTokenProgram
        );
        const baseReserveVaultAfter = await harness.tokenAccountBalance(
          baseReserveVault,
          harness.config.baseTokenProgram
        );
        const baseInterestVaultAfter = await harness.tokenAccountBalance(
          baseInterestVault,
          harness.config.baseTokenProgram
        );
        const liquidatorQuoteAfter = await harness.tokenBalance(
          "liquidator",
          harness.config.quoteMint,
          harness.config.quoteTokenProgram
        );
        const interestPaid = baseInterestVaultAfter - baseInterestVaultBefore;
        const reserveCredit = baseReserveVaultAfter - baseReserveVaultBefore;
        const aggregateDebtCleared = repaid + insuranceDrawn + socializedLoss;
        const fixedBaseDebtBeforeSettlement = stateValue(
          marketBeforeSettlement,
          "fixedBaseDebt"
        );
        const fixedBaseDebtAfterSettlement = stateValue(
          marketAfterSettlement,
          "fixedBaseDebt"
        );
        harness.assertTrue(
          "settlement does not increase persisted aggregate base debt",
          fixedBaseDebtBeforeSettlement >= fixedBaseDebtAfterSettlement,
          { fixedBaseDebtBeforeSettlement, fixedBaseDebtAfterSettlement }
        );
        const persistedDebtReduction =
          fixedBaseDebtBeforeSettlement - fixedBaseDebtAfterSettlement;
        harness.assertTrue(
          "settlement aggregate clearance includes nonnegative instruction-time accrual",
          aggregateDebtCleared >= persistedDebtReduction,
          { aggregateDebtCleared, persistedDebtReduction }
        );
        const settlementAccrual = aggregateDebtCleared - persistedDebtReduction;
        harness.assertTrue(
          "instruction-time accrual remains within the caller's bounded headroom",
          settlementAccrual <= socializationHeadroom,
          { settlementAccrual, socializationHeadroom }
        );
        harness.assertEqual(
          "base insurance token-vault debit matches receipt",
          baseInsuranceVaultBefore - baseInsuranceVaultAfter,
          insuranceDrawn
        );
        harness.assertEqual("base insurance accounting is exhausted", stateValue(marketAfterSettlement, "baseInsuranceAvailable"), 0n);
        harness.assertEqual("base insurance token vault is exhausted", baseInsuranceVaultAfter, 0n);
        harness.assertEqual(
          "quote insurance token-vault credit matches receipt",
          quoteInsuranceVaultAfter - quoteInsuranceVaultBefore,
          insuranceFunded
        );
        harness.assertEqual(
          "quote insurance state credit matches receipt",
          stateValue(marketAfterSettlement, "quoteInsuranceAvailable") -
            stateValue(marketBeforeSettlement, "quoteInsuranceAvailable"),
          insuranceFunded
        );
        harness.assertEqual(
          "collateral vault debit matches seized collateral",
          quoteCollateralVaultBefore - quoteCollateralVaultAfter,
          collateralSeized
        );
        harness.assertEqual(
          "liquidator token credit matches receipt",
          liquidatorQuoteAfter - liquidatorQuoteBefore,
          collateralToLiquidator
        );
        harness.assertEqual(
          "debt-side vault credits conserve repayment and insurance draw",
          reserveCredit + interestPaid,
          repaid + insuranceDrawn
        );
        harness.assertEqual(
          "cash-reserve credit matches the reserve token vault",
          stateValue(marketAfterSettlement, "baseCashReserve") -
            stateValue(marketBeforeSettlement, "baseCashReserve"),
          reserveCredit
        );
        harness.assertEqual(
          "virtual-reserve write-down plus instruction-time accrual equals socialized loss plus realized interest",
          stateValue(marketBeforeSettlement, "baseLiveReserve") -
            stateValue(marketAfterSettlement, "baseLiveReserve") +
            settlementAccrual,
          socializedLoss + interestPaid
        );
        harness.assertEqual("bad-debt position clears aggregate base debt", stateValue(marketAfterSettlement, "fixedBaseDebt"), 0n);
        harness.assertEqual("bad-debt position clears aggregate base principal", stateValue(marketAfterSettlement, "fixedBasePrincipal"), 0n);
        harness.assertEqual(
          "bad-debt position clears quote global-health contribution",
          stateValue(marketAfterSettlement, "globalHealthQuoteContributionForBaseDebt"),
          0n
        );
        const closedPreview = await previewPosition(
          harness,
          "alice",
          badDebtPositionId,
          "preview closed bad-debt position"
        );
        harness.assertEqual("bad-debt position has zero base debt", BigInt(closedPreview.fixedBaseDebt.toString()), 0n);
        harness.assertEqual("bad-debt position has zero quote collateral", BigInt(closedPreview.quoteCollateral.toString()), 0n);
      } catch (error) {
        primaryFailed = true;
        primaryError = error;
      }

      if (primaryError instanceof MutationOutcomeUncertainError) {
        throw primaryError;
      }
      try {
        await cleanupBadDebtScenario(harness, traderBaseBeforeShock);
      } catch (cleanupError) {
        const originalContext = primaryFailed
          ? ` Original scenario error: ${errorMessage(primaryError)}.`
          : "";
        throw new MutationOutcomeUncertainError(
          `Bad-debt scenario cleanup could not prove a clean state: ${errorMessage(cleanupError)}.${originalContext}`,
        );
      }
      if (primaryFailed) throw primaryError;

      const restoredMarket = await harness.market();
      harness.assertTrue(
        "reversed stress leaves meaningful depth on both sides",
        stateValue(restoredMarket, "baseLiveReserve") > 50n * 10n ** BigInt(harness.config.baseDecimals) &&
          stateValue(restoredMarket, "quoteLiveReserve") > 50n * 10n ** BigInt(harness.config.quoteDecimals),
        restoredMarket.state
      );
    },
  },
];
