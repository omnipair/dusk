import { Keypair, PublicKey } from "@solana/web3.js";

import { decodePreviewBorrowPositionReturnData } from "../../../packages/dusk-sdk/src/preview.js";
import {
  formatUnits,
  type ProtocolTestHarness,
  type ScenarioDefinition,
} from "../harness.js";
import type { TransactionEvidence } from "../types.js";

const bidPositionId = Keypair.generate().publicKey;
const floorPositionId = Keypair.generate().publicKey;
const badDebtPositionId = Keypair.generate().publicKey;
const BPS_DENOMINATOR = 10_000n;

function eventAmount(data: Record<string, { toString(): string }>, key: string): bigint {
  const value = data[key];
  if (value === undefined) throw new Error(`Liquidation event does not expose ${key}`);
  return BigInt(value.toString());
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

async function maximumExternalBid(
  harness: ProtocolTestHarness,
  positionId: PublicKey,
  debtUpperBound: bigint
): Promise<bigint> {
  const body = (amount: bigint) => ({
    positionId: positionId.toBase58(),
    debtAsset: "quote",
    repayAmount: formatUnits(amount, harness.config.quoteDecimals),
    minCollateralOut: "0",
  });
  let low = 0n;
  let high = debtUpperBound;
  while (low + 1n < high) {
    const middle = (low + high) / 2n;
    if ((await harness.probe("bidder", "/api/v2/fork/tx/bid-liquidation-auction", body(middle))).succeeds) {
      low = middle;
    } else {
      high = middle;
    }
  }
  return low;
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
        label: "reject internal backstop before the external-bid window expires",
        expected: "failure",
        body: {
          positionId: floorPositionId.toBase58(),
          debtAsset: "quote",
          minCallerBountyOut: "0",
        },
      });

      const bidderBaseBefore = await harness.tokenBalance(
        "bidder",
        harness.config.baseMint,
        harness.config.baseTokenProgram
      );
      const bidDebtBefore = BigInt(
        (await previewPosition(harness, "bidder", bidPositionId, "preview debt before first external bid"))
          .fixedQuoteDebt.toString()
      );
      const bidRepayAmount = await maximumExternalBid(harness, bidPositionId, bidDebtBefore);
      harness.assertTrue("active external auction exposes a positive repay cap", bidRepayAmount > 0n, {
        bidDebtBefore,
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
      await harness.timeTravel(301, 1_000);
      await harness.execute({
        wallet: "liquidator",
        endpoint: "/api/v2/fork/tx/settle-liquidation-auction-floor",
        label: "internally unwind the partial-bid residual at expiry",
        body: {
          positionId: bidPositionId.toBase58(),
          debtAsset: "quote",
          minCallerBountyOut: "0",
        },
      });
      const bidPositions = await harness.positions("alice", bidPositionId);
      const bidPosition = bidPositions.find((entry) => entry.eventType === "borrow_position");
      harness.assertEqual("internal backstop clears auction timestamp", BigInt(bidPosition.payload.auctionStartTime), 0n);
      harness.assertEqual("internal backstop clears auction debt asset", bidPosition.payload.auctionDebtAsset, null);

      const liquidatorBaseBefore = await harness.tokenBalance(
        "liquidator",
        harness.config.baseMint,
        harness.config.baseTokenProgram
      );
      await harness.execute({
        wallet: "liquidator",
        endpoint: "/api/v2/fork/tx/settle-liquidation-auction-floor",
        label: "permissionlessly unwind the untouched loan at the floor",
        body: {
          positionId: floorPositionId.toBase58(),
          debtAsset: "quote",
          minCallerBountyOut: "0",
        },
      });
      harness.assertTrue(
        "floor settlement pays the caller bounty",
        await harness.tokenBalance("liquidator", harness.config.baseMint, harness.config.baseTokenProgram) > liquidatorBaseBefore
      );
      const floorClosed = await previewPosition(harness, "bob", floorPositionId, "preview internally closed floor loan");
      harness.assertEqual("floor backstop clears all quote debt", BigInt(floorClosed.fixedQuoteDebt.toString()), 0n);
      harness.assertEqual("floor backstop consumes all base collateral", BigInt(floorClosed.baseCollateral.toString()), 0n);

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

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/deposit-collateral",
        label: "deposit quote collateral for bad-debt loan",
        body: {
          positionId: badDebtPositionId.toBase58(),
          marketAsset: "quote",
          depositAmount: "12",
        },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/borrow",
        label: "borrow base for bad-debt loan",
        body: {
          positionId: badDebtPositionId.toBase58(),
          borrowAsset: "base",
          borrowAmount: "6",
          minDebtAmountOut: "6",
          minLiquidationCfBps: 0,
        },
      });

      await harness.fundWallet("trader", "1000000", "1000000");
      const traderBaseBeforeShock = await harness.tokenBalance(
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
      for (let attempt = 1; attempt <= 5; attempt += 1) {
        liquidationPreview = await previewPosition(
          harness,
          "liquidator",
          badDebtPositionId,
          `preview collateral exhaustion attempt ${attempt}`
        );
        const debt = BigInt(liquidationPreview.fixedBaseDebt.toString());
        const collateral = BigInt(liquidationPreview.quoteCollateral.toString());
        harness.observe(`bad-debt eligibility attempt ${attempt}`, {
          debt,
          collateral,
          isLiquidatable: liquidationPreview.baseDebt.isLiquidatable,
          insuranceAvailable,
        });
        if (liquidationPreview.baseDebt.isLiquidatable) break;
        await harness.timeTravel(1, 1_000);
        await harness.execute({
          wallet: "trader",
          endpoint: "/api/v2/fork/tx/swap",
          label: `advance bad-debt EMA attempt ${attempt + 1}`,
          body: { assetIn: "quote", exactAssetIn: "0.001", minAssetOut: "0" },
        });
      }
      if (!liquidationPreview?.baseDebt.isLiquidatable) {
        throw new Error("Deeply insolvent position did not become liquidatable");
      }

      const debtBeforeSettlement = BigInt(liquidationPreview.fixedBaseDebt.toString());
      const collateralBeforeSettlement = BigInt(liquidationPreview.quoteCollateral.toString());
      harness.assertTrue("bad-debt fixture has debt beyond available insurance", debtBeforeSettlement > insuranceAvailable, {
        debtBeforeSettlement,
        insuranceAvailable,
      });

      await harness.execute({
        wallet: "liquidator",
        endpoint: "/api/v2/fork/tx/trigger-liquidation-auction",
        label: "trigger deeply insolvent base-debt auction",
        body: { positionId: badDebtPositionId.toBase58(), debtAsset: "base" },
      });
      await harness.timeTravel(301, 1_000);

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
        label: "permissionlessly unwind bad debt with automatic insurance and socialization",
        body: {
          positionId: badDebtPositionId.toBase58(),
          debtAsset: "base",
          minCallerBountyOut: "0",
        },
      });

      const liquidationEvents = harness.events(settlement, "BorrowPositionLiquidated");
      harness.assertEqual("bad-debt settlement emits one liquidation receipt", liquidationEvents.length, 1);
      const receipt = liquidationEvents[0].data as Record<string, { toString(): string }>;
      const repaid = eventAmount(receipt, "repaid_amount");
      const collateralSeized = eventAmount(receipt, "collateral_seized");
      const collateralToLiquidator = eventAmount(receipt, "collateral_to_liquidator");
      const insuranceDrawn = eventAmount(receipt, "insurance_drawn");
      const socializedLoss = eventAmount(receipt, "socialized_loss");
      harness.assertTrue("internal concentrated unwind repays part of the debt", repaid > 0n, repaid);
      harness.assertTrue("automatic insurance draw stays within available insurance", insuranceDrawn <= insuranceAvailable, {
        insuranceDrawn,
        insuranceAvailable,
      });
      harness.assertTrue("insolvent internal unwind automatically socializes the remainder", socializedLoss > 0n, socializedLoss);
      harness.assertEqual("receipt closes all position debt", eventAmount(receipt, "remaining_debt"), 0n);
      harness.assertEqual(
        "receipt consumes the complete collateral position",
        collateralSeized,
        collateralBeforeSettlement
      );
      harness.assertEqual(
        "floor caller bounty is 0.5% of seized collateral",
        collateralToLiquidator,
        collateralSeized * 50n / BPS_DENOMINATOR
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
      harness.assertEqual(
        "base insurance token-vault debit matches receipt",
        baseInsuranceVaultBefore - baseInsuranceVaultAfter,
        insuranceDrawn
      );
      harness.assertEqual(
        "base insurance accounting debit matches receipt",
        stateValue(marketBeforeSettlement, "baseInsuranceAvailable") -
          stateValue(marketAfterSettlement, "baseInsuranceAvailable"),
        insuranceDrawn
      );
      harness.assertEqual(
        "internal backstop does not fund collateral-side insurance",
        quoteInsuranceVaultAfter - quoteInsuranceVaultBefore,
        0n
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
        "debt-side vault movement contains only insurance credit and realized interest",
        reserveCredit + interestPaid,
        insuranceDrawn
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

      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "reverse deep quote shock after bad-debt settlement",
        body: {
          assetIn: "base",
          exactAssetIn: formatUnits(shockBaseOut, harness.config.baseDecimals),
          minAssetOut: "0",
        },
      });
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "checkpoint restored spot after bad-debt settlement",
        body: { assetIn: "base", exactAssetIn: "0.001", minAssetOut: "0" },
      });
      await harness.timeTravel(1, 1_000);
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "persist restored EMA after bad-debt settlement",
        body: { assetIn: "base", exactAssetIn: "0.001", minAssetOut: "0" },
      });
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
