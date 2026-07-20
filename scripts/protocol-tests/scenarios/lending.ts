import { Keypair } from "@solana/web3.js";

import {
  decodePreviewBorrowCapacityReturnData,
  decodePreviewBorrowPositionReturnData,
  decodePreviewMarketReturnData,
} from "../../../packages/dusk-sdk/src/preview.js";
import type { TransactionEvidence } from "../types.js";

import { formatUnits, type ProtocolTestHarness, type ScenarioDefinition } from "../harness.js";

const baseLifecyclePosition = Keypair.generate().publicKey;
const aliceGlobalPosition = Keypair.generate().publicKey;
const bobGlobalPosition = Keypair.generate().publicKey;
const globalAdmissionPosition = Keypair.generate().publicKey;
const dailyQuotePosition = Keypair.generate().publicKey;
const dailyBasePosition = Keypair.generate().publicKey;
const interestPosition = Keypair.generate().publicKey;
const withdrawBoundaryPosition = Keypair.generate().publicKey;
const splitTwoPositions = [Keypair.generate().publicKey, Keypair.generate().publicKey];
const splitFourPositions = [
  Keypair.generate().publicKey,
  Keypair.generate().publicKey,
  Keypair.generate().publicKey,
  Keypair.generate().publicKey,
];
const stagedTwoPosition = Keypair.generate().publicKey;
const stagedFourPosition = Keypair.generate().publicKey;

function raw(uiAmount: number, decimals: number): bigint {
  return BigInt(uiAmount) * 10n ** BigInt(decimals);
}

function stateValue(market: Awaited<ReturnType<ProtocolTestHarness["market"]>>, key: string): bigint {
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

async function previewPosition(
  harness: ProtocolTestHarness,
  wallet: string,
  positionId: { toBase58(): string },
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

async function previewMarket(harness: ProtocolTestHarness, label: string, submit = false) {
  const evidence = await harness.execute({
    wallet: "trader",
    endpoint: "/api/v2/fork/tx/preview-market",
    label,
    submit,
    body: {},
  });
  return decodePreviewMarketReturnData(previewData(evidence));
}

async function maximumAdditionalQuoteBorrow(
  harness: ProtocolTestHarness,
  positionId: { toBase58(): string }
): Promise<bigint> {
  const body = (amount: bigint) => ({
    positionId: positionId.toBase58(),
    borrowAsset: "quote",
    borrowAmount: formatUnits(amount, harness.config.quoteDecimals),
    minDebtAmountOut: formatUnits(amount, harness.config.quoteDecimals),
    minLiquidationCfBps: 0,
  });
  let low = 0n;
  let high = raw(200, harness.config.quoteDecimals);
  while ((await harness.probe("alice", "/api/v2/fork/tx/borrow", body(high))).succeeds) {
    high *= 2n;
  }
  while (low + 1n < high) {
    const middle = (low + high) / 2n;
    if ((await harness.probe("alice", "/api/v2/fork/tx/borrow", body(middle))).succeeds) low = middle;
    else high = middle;
  }
  return low;
}

async function repayAndWithdraw(
  harness: ProtocolTestHarness,
  wallet: string,
  positionId: { toBase58(): string },
  debtAsset: "base" | "quote",
  collateralAsset: "base" | "quote",
  collateralAmount: bigint
): Promise<void> {
  const position = await previewPosition(harness, wallet, positionId, `preview ${wallet} debt for cleanup`);
  const debt = debtAsset === "base" ? integer(position.fixedBaseDebt) : integer(position.fixedQuoteDebt);
  const debtDecimals = debtAsset === "base" ? harness.config.baseDecimals : harness.config.quoteDecimals;
  const collateralDecimals = collateralAsset === "base" ? harness.config.baseDecimals : harness.config.quoteDecimals;
  if (debt > 0n) {
    await harness.execute({
      wallet,
      endpoint: "/api/v2/fork/tx/repay",
      label: `repay ${wallet} ${debtAsset} debt for cleanup`,
      body: {
        positionId: positionId.toBase58(),
        repayAsset: debtAsset,
        repayAmount: formatUnits(debt, debtDecimals),
      },
    });
  }
  if (collateralAmount > 0n) {
    await harness.execute({
      wallet,
      endpoint: "/api/v2/fork/tx/withdraw-collateral",
      label: `withdraw ${wallet} ${collateralAsset} collateral for cleanup`,
      body: {
        positionId: positionId.toBase58(),
        marketAsset: collateralAsset,
        withdrawAmount: formatUnits(collateralAmount, collateralDecimals),
        minAssetAmountOut: "0",
        minLiquidationCfBps: 0,
      },
    });
  }
}

export const LENDING_SCENARIOS: ScenarioDefinition[] = [
  {
    id: "lending.base-lifecycle",
    async run(harness) {
      const quoteBefore = await harness.tokenBalance("bob", harness.config.quoteMint, harness.config.quoteTokenProgram);
      const baseBefore = await harness.tokenBalance("bob", harness.config.baseMint, harness.config.baseTokenProgram);
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/deposit-collateral",
        label: "deposit quote collateral for base debt",
        body: { positionId: baseLifecyclePosition.toBase58(), marketAsset: "quote", depositAmount: "100" },
      });
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/borrow",
        label: "borrow base against quote collateral",
        body: {
          positionId: baseLifecyclePosition.toBase58(),
          borrowAsset: "base",
          borrowAmount: "10",
          minDebtAmountOut: "10",
          minLiquidationCfBps: 0,
        },
      });
      const position = await previewPosition(harness, "bob", baseLifecyclePosition, "preview mirrored base debt position");
      harness.assertEqual("mirrored lifecycle records base debt", integer(position.fixedBaseDebt), raw(10, harness.config.baseDecimals));
      harness.assertEqual(
        "base borrow proceeds reach Bob",
        await harness.tokenBalance("bob", harness.config.baseMint, harness.config.baseTokenProgram) - baseBefore,
        raw(10, harness.config.baseDecimals)
      );
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/repay",
        label: "repay mirrored base debt",
        body: { positionId: baseLifecyclePosition.toBase58(), repayAsset: "base", repayAmount: "10" },
      });
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/withdraw-collateral",
        label: "withdraw mirrored quote collateral",
        body: {
          positionId: baseLifecyclePosition.toBase58(),
          marketAsset: "quote",
          withdrawAmount: "100",
          minAssetAmountOut: "100",
          minLiquidationCfBps: 0,
        },
      });
      harness.assertEqual(
        "quote collateral roundtrip returns exactly",
        await harness.tokenBalance("bob", harness.config.quoteMint, harness.config.quoteTokenProgram),
        quoteBefore
      );
    },
  },
  {
    id: "lending.global-health-non-locking",
    async run(harness) {
      await harness.fundWallet("trader", "50000", "0", 0);
      for (const [wallet, positionId, borrowAmount] of [
        ["alice", aliceGlobalPosition, "20"],
        ["bob", bobGlobalPosition, "60"],
      ] as const) {
        await harness.execute({
          wallet,
          endpoint: "/api/v2/fork/tx/deposit-collateral",
          label: `deposit ${wallet} collateral for global-health test`,
          body: { positionId: positionId.toBase58(), marketAsset: "base", depositAmount: "100" },
        });
        await harness.execute({
          wallet,
          endpoint: "/api/v2/fork/tx/borrow",
          label: `underwrite ${wallet} at stored terms`,
          body: {
            positionId: positionId.toBase58(),
            borrowAsset: "quote",
            borrowAmount,
            minDebtAmountOut: borrowAmount,
            minLiquidationCfBps: 0,
          },
        });
      }
      const bobBefore = await previewPosition(harness, "bob", bobGlobalPosition, "record Bob stored liquidation CF");
      const bobCf = Number(bobBefore.quoteLiquidationCfBps);
      harness.assertTrue("Bob receives a stored liquidation CF", bobCf > 0, bobCf);

      await repayAndWithdraw(
        harness,
        "alice",
        aliceGlobalPosition,
        "quote",
        "base",
        raw(100, harness.config.baseDecimals)
      );
      const bobAfterAliceExit = await previewPosition(harness, "bob", bobGlobalPosition, "preview Bob after Alice exits");
      harness.assertEqual("Alice exit does not float Bob stored CF", Number(bobAfterAliceExit.quoteLiquidationCfBps), bobCf);

      const traderQuoteBefore = await harness.tokenBalance("trader", harness.config.quoteMint, harness.config.quoteTokenProgram);
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "devalue base collateral until aggregate admission is unsafe",
        body: { assetIn: "base", exactAssetIn: "40000", minAssetOut: "0" },
      });
      const quoteReceived = await harness.tokenBalance("trader", harness.config.quoteMint, harness.config.quoteTokenProgram) - traderQuoteBefore;
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/deposit-collateral",
        label: "deposit new collateral while aggregate health is stressed",
        body: { positionId: globalAdmissionPosition.toBase58(), marketAsset: "base", depositAmount: "100" },
      });
      const blocked = await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/borrow",
        label: "pause new borrowing below aggregate admission floor",
        expected: "failure",
        body: {
          positionId: globalAdmissionPosition.toBase58(),
          borrowAsset: "quote",
          borrowAmount: "1",
          minDebtAmountOut: "1",
          minLiquidationCfBps: 0,
        },
      });
      harness.assertEqual("aggregate stress fails with market-health guard", blocked.errorCode, "InsufficientMarketHealth");
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/withdraw-collateral",
        label: "debt-free Alice exits despite another stressed borrower",
        body: {
          positionId: globalAdmissionPosition.toBase58(),
          marketAsset: "base",
          withdrawAmount: "100",
          minAssetAmountOut: "100",
          minLiquidationCfBps: 0,
        },
      });
      const bobStressed = await previewPosition(harness, "bob", bobGlobalPosition, "preview Bob under aggregate stress");
      harness.assertEqual("aggregate stress still does not float Bob CF", Number(bobStressed.quoteLiquidationCfBps), bobCf);

      await repayAndWithdraw(
        harness,
        "bob",
        bobGlobalPosition,
        "quote",
        "base",
        raw(100, harness.config.baseDecimals)
      );
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "reverse global-health price stress",
        body: { assetIn: "quote", exactAssetIn: formatUnits(quoteReceived, harness.config.quoteDecimals), minAssetOut: "0" },
      });
      const final = await harness.market();
      harness.assertEqual("global-health quote debt cleans up", stateValue(final, "fixedQuoteDebt"), 0n);
      harness.assertEqual("global-health contribution cleans up", stateValue(final, "globalHealthBaseContributionForQuoteDebt"), 0n);
    },
  },
  {
    id: "lending.position-splitting",
    async run(harness) {
      const oneShotEvidence = await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/preview-borrow-capacity",
        label: "preview one-shot 100-base capacity",
        submit: false,
        body: { collateralAsset: "base", collateralAmount: "100", withReferral: false },
      });
      const oneShotCapacity = integer(
        decodePreviewBorrowCapacityReturnData(previewData(oneShotEvidence)).maxBorrowAmount
      );

      const executeStaged = async (
        positionId: typeof stagedTwoPosition,
        stages: number,
        collateral: number,
        label: string
      ): Promise<bigint> => {
        let totalCapacity = 0n;
        for (let index = 0; index < stages; index += 1) {
          await harness.execute({
            wallet: "alice",
            endpoint: "/api/v2/fork/tx/deposit-collateral",
            label: `deposit ${label} collateral ${index + 1}`,
            body: { positionId: positionId.toBase58(), marketAsset: "base", depositAmount: String(collateral) },
          });
          const capacity = await maximumAdditionalQuoteBorrow(harness, positionId);
          harness.assertTrue(`${label} stage ${index + 1} has positive capacity`, capacity > 0n, capacity);
          await harness.execute({
            wallet: "alice",
            endpoint: "/api/v2/fork/tx/borrow",
            label: `borrow ${label} capacity ${index + 1}`,
            body: {
              positionId: positionId.toBase58(),
              borrowAsset: "quote",
              borrowAmount: formatUnits(capacity, harness.config.quoteDecimals),
              minDebtAmountOut: formatUnits(capacity, harness.config.quoteDecimals),
              minLiquidationCfBps: 0,
            },
          });
          totalCapacity += capacity;
        }
        await repayAndWithdraw(
          harness,
          "alice",
          positionId,
          "quote",
          "base",
          raw(stages * collateral, harness.config.baseDecimals)
        );
        return totalCapacity;
      };

      const executeSplit = async (
        positions: typeof splitTwoPositions | typeof splitFourPositions,
        collateral: number,
        label: string
      ): Promise<bigint> => {
        let totalCapacity = 0n;
        for (const [index, positionId] of positions.entries()) {
          await harness.execute({
            wallet: "alice",
            endpoint: "/api/v2/fork/tx/deposit-collateral",
            label: `deposit ${label} collateral ${index + 1}`,
            body: { positionId: positionId.toBase58(), marketAsset: "base", depositAmount: String(collateral) },
          });
          const evidence = await harness.execute({
            wallet: "alice",
            endpoint: "/api/v2/fork/tx/preview-borrow-capacity",
            label: `preview sequential ${label} capacity ${index + 1}`,
            submit: false,
            body: { collateralAsset: "base", collateralAmount: String(collateral), withReferral: false },
          });
          const capacity = integer(decodePreviewBorrowCapacityReturnData(previewData(evidence)).maxBorrowAmount);
          harness.assertTrue(`${label} position ${index + 1} has positive capacity`, capacity > 0n, capacity);
          await harness.execute({
            wallet: "alice",
            endpoint: "/api/v2/fork/tx/borrow",
            label: `borrow sequential ${label} capacity ${index + 1}`,
            body: {
              positionId: positionId.toBase58(),
              borrowAsset: "quote",
              borrowAmount: formatUnits(capacity, harness.config.quoteDecimals),
              minDebtAmountOut: formatUnits(capacity, harness.config.quoteDecimals),
              minLiquidationCfBps: 0,
            },
          });
          totalCapacity += capacity;
        }
        for (const positionId of positions) {
          await repayAndWithdraw(
            harness,
            "alice",
            positionId,
            "quote",
            "base",
            raw(collateral, harness.config.baseDecimals)
          );
        }
        return totalCapacity;
      };

      const stagedTwoCapacity = await executeStaged(stagedTwoPosition, 2, 50, "two-stage single position");
      await harness.timeTravel(0, 216_010);
      const twoWayCapacity = await executeSplit(splitTwoPositions, 50, "two-way split");
      await harness.timeTravel(0, 216_010);
      const stagedFourCapacity = await executeStaged(stagedFourPosition, 4, 25, "four-stage single position");
      await harness.timeTravel(0, 216_010);
      const fourWayCapacity = await executeSplit(splitFourPositions, 25, "four-way split");
      const gainBps = (capacity: bigint, control: bigint): bigint => capacity <= control
        ? 0n
        : ((capacity - control) * 10_000n + control - 1n) / control;
      const twoWayGainBps = gainBps(twoWayCapacity, stagedTwoCapacity);
      const fourWayGainBps = gainBps(fourWayCapacity, stagedFourCapacity);
      const maximumBoundedGainBps = 10n;
      harness.assertTrue(
        "two-way position split cannot materially beat equivalent staged borrowing",
        twoWayGainBps <= maximumBoundedGainBps,
        { stagedTwoCapacity, twoWayCapacity, twoWayGainBps, maximumBoundedGainBps }
      );
      harness.assertTrue(
        "four-way position split cannot materially beat equivalent staged borrowing",
        fourWayGainBps <= maximumBoundedGainBps,
        { stagedFourCapacity, fourWayCapacity, fourWayGainBps, maximumBoundedGainBps }
      );
      harness.observe("sequential split capacities", {
        oneShotCapacity,
        stagedTwoCapacity,
        twoWayCapacity,
        stagedFourCapacity,
        fourWayCapacity,
        twoWayGainBps,
        fourWayGainBps,
      });
    },
  },
  {
    id: "lending.daily-borrow-limits",
    async run(harness) {
      const before = await previewMarket(harness, "preview initial daily borrow limits");
      const baseLimit = integer(before.base.dailyBorrowLimit);
      const quoteLimit = integer(before.quote.dailyBorrowLimit);
      harness.assertTrue("base daily limit is bounded by live cash", baseLimit <= integer(before.base.cashReserve), baseLimit);
      harness.assertTrue("quote daily limit is bounded by live cash", quoteLimit <= integer(before.quote.cashReserve), quoteLimit);

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/deposit-collateral",
        label: "deposit base collateral for quote daily-limit debit",
        body: { positionId: dailyQuotePosition.toBase58(), marketAsset: "base", depositAmount: "100" },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/borrow",
        label: "consume quote daily borrow bucket",
        body: {
          positionId: dailyQuotePosition.toBase58(),
          borrowAsset: "quote",
          borrowAmount: "10",
          minDebtAmountOut: "10",
          minLiquidationCfBps: 0,
        },
      });
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/deposit-collateral",
        label: "deposit quote collateral for base daily-limit debit",
        body: { positionId: dailyBasePosition.toBase58(), marketAsset: "quote", depositAmount: "100" },
      });
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/borrow",
        label: "consume base daily borrow bucket",
        body: {
          positionId: dailyBasePosition.toBase58(),
          borrowAsset: "base",
          borrowAmount: "10",
          minDebtAmountOut: "10",
          minLiquidationCfBps: 0,
        },
      });
      const consumed = await previewMarket(harness, "preview consumed daily borrow buckets");
      harness.assertTrue("quote borrowing reduces quote daily headroom", integer(consumed.quote.dailyBorrowRemaining) < integer(before.quote.dailyBorrowRemaining));
      harness.assertTrue("base borrowing reduces base daily headroom", integer(consumed.base.dailyBorrowRemaining) < integer(before.base.dailyBorrowRemaining));

      await harness.fundWallet("trader", "1000", "1000");
      const ylpBefore = await harness.lpBalance("trader", harness.config.ylpMint);
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/add-liquidity",
        label: "grow K proportionally during active daily buckets",
        body: { baseDepositAmount: "100", quoteDepositAmount: "100", minYlpAmount: "0" },
      });
      const minted = await harness.lpBalance("trader", harness.config.ylpMint) - ylpBefore;
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "change one-sided inventory during active daily buckets",
        body: { assetIn: "base", exactAssetIn: "100", minAssetOut: "0" },
      });
      const changed = await previewMarket(harness, "preview K-based limits after liquidity and inventory changes");
      for (const [asset, side] of [["base", changed.base], ["quote", changed.quote]] as const) {
        const limit = integer(side.dailyBorrowLimit);
        const depthRaw = integer(side.conservativeDepthNad) / 10n ** BigInt(9 - (asset === "base" ? harness.config.baseDecimals : harness.config.quoteDecimals));
        const expectedDepthCap = depthRaw * BigInt(Number((await harness.market()).config.maxDailyBorrowBps)) / 10_000n;
        harness.assertTrue(`${asset} daily limit never exceeds conservative K depth`, limit <= expectedDepthCap, { limit, expectedDepthCap });
        harness.assertTrue(`${asset} daily remaining never exceeds current cash`, integer(side.dailyBorrowRemaining) <= integer(side.cashReserve));
      }

      await harness.timeTravel(0, 216_010);
      const decayed = await previewMarket(harness, "submit daily-bucket decay after one day", true);
      harness.assertEqual("base daily bucket fully decays after one day", integer(decayed.base.dailyBorrowRemaining), integer(decayed.base.dailyBorrowLimit));
      harness.assertEqual("quote daily bucket fully decays after one day", integer(decayed.quote.dailyBorrowRemaining), integer(decayed.quote.dailyBorrowLimit));

      await repayAndWithdraw(harness, "alice", dailyQuotePosition, "quote", "base", raw(100, harness.config.baseDecimals));
      await repayAndWithdraw(harness, "bob", dailyBasePosition, "base", "quote", raw(100, harness.config.quoteDecimals));
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/remove-liquidity",
        label: "remove proportional liquidity used by daily-limit test",
        body: { ylpAmount: formatUnits(minted, harness.config.baseDecimals), minBaseAmountOut: "0", minQuoteAmountOut: "0" },
      });
      harness.assertEqual("daily-limit liquidity shares clean up", await harness.lpBalance("trader", harness.config.ylpMint), ylpBefore);
    },
  },
  {
    id: "lending.interest-growth",
    async run(harness) {
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/deposit-collateral",
        label: "deposit collateral for interest growth",
        body: { positionId: interestPosition.toBase58(), marketAsset: "base", depositAmount: "100" },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/borrow",
        label: "borrow principal for interest growth",
        body: {
          positionId: interestPosition.toBase58(),
          borrowAsset: "quote",
          borrowAmount: "20",
          minDebtAmountOut: "20",
          minLiquidationCfBps: 0,
        },
      });
      const initial = await previewPosition(harness, "alice", interestPosition, "record principal and stored CF before time");
      const initialDebt = integer(initial.fixedQuoteDebt);
      const initialCf = Number(initial.quoteLiquidationCfBps);
      const interestVaultBefore = await harness.tokenAccountBalance(
        new (await import("@solana/web3.js")).PublicKey((await harness.market()).quoteInterestVault),
        harness.config.quoteTokenProgram
      );

      await harness.timeTravel(0, 2_160_000);
      await previewMarket(harness, "accrue ten days of borrow interest", true);
      const grown = await previewPosition(harness, "alice", interestPosition, "preview debt after interest accrual");
      const grownDebt = integer(grown.fixedQuoteDebt);
      harness.assertTrue("debt grows above principal with time", grownDebt > initialDebt, { initialDebt, grownDebt });
      harness.assertEqual("other users and time do not float stored CF", Number(grown.quoteLiquidationCfBps), initialCf);

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/repay",
        label: "reject one raw unit over full live debt",
        expected: "failure",
        body: {
          positionId: interestPosition.toBase58(),
          repayAsset: "quote",
          repayAmount: formatUnits(grownDebt + 1n, harness.config.quoteDecimals),
        },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/repay",
        label: "partially repay interest-bearing debt",
        body: { positionId: interestPosition.toBase58(), repayAsset: "quote", repayAmount: "5" },
      });
      const partial = await previewPosition(harness, "alice", interestPosition, "preview partially repaid debt");
      harness.assertTrue("partial repay leaves positive lower debt", integer(partial.fixedQuoteDebt) > 0n && integer(partial.fixedQuoteDebt) < grownDebt);
      harness.assertEqual("partial repay preserves stored CF", Number(partial.quoteLiquidationCfBps), initialCf);
      harness.assertTrue(
        "realized interest is routed to the interest vault",
        await harness.tokenAccountBalance(
          new (await import("@solana/web3.js")).PublicKey((await harness.market()).quoteInterestVault),
          harness.config.quoteTokenProgram
        ) > interestVaultBefore
      );

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/repay",
        label: "repay all remaining interest-bearing debt",
        body: {
          positionId: interestPosition.toBase58(),
          repayAsset: "quote",
          repayAmount: formatUnits(integer(partial.fixedQuoteDebt), harness.config.quoteDecimals),
        },
      });
      const cleared = await previewPosition(harness, "alice", interestPosition, "preview cleared interest position");
      harness.assertEqual("full repay clears debt", integer(cleared.fixedQuoteDebt), 0n);
      harness.assertEqual("full repay clears stored quote CF", Number(cleared.quoteLiquidationCfBps), 0);
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/withdraw-collateral",
        label: "withdraw collateral after interest repayment",
        body: {
          positionId: interestPosition.toBase58(),
          marketAsset: "base",
          withdrawAmount: "100",
          minAssetAmountOut: "100",
          minLiquidationCfBps: 0,
        },
      });
    },
  },
  {
    id: "lending.withdraw-health-protection",
    async run(harness) {
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/deposit-collateral",
        label: "deposit collateral for withdrawal boundary",
        body: { positionId: withdrawBoundaryPosition.toBase58(), marketAsset: "base", depositAmount: "100" },
      });
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/borrow",
        label: "borrow debt for withdrawal boundary",
        body: {
          positionId: withdrawBoundaryPosition.toBase58(),
          borrowAsset: "quote",
          borrowAmount: "50",
          minDebtAmountOut: "50",
          minLiquidationCfBps: 0,
        },
      });
      const initial = await previewPosition(harness, "bob", withdrawBoundaryPosition, "record withdrawal-boundary CF");
      const storedCf = Number(initial.quoteLiquidationCfBps);
      const staleGuard = await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/withdraw-collateral",
        label: "reject withdrawal below user minimum liquidation CF",
        expected: "failure",
        body: {
          positionId: withdrawBoundaryPosition.toBase58(),
          marketAsset: "base",
          withdrawAmount: formatUnits(1n, harness.config.baseDecimals),
          minAssetAmountOut: "0",
          minLiquidationCfBps: storedCf + 1,
        },
      });
      harness.assertEqual("withdrawal stale guard reports slippage", staleGuard.errorCode, "SlippageExceeded");

      let low = 1n;
      let high = raw(100, harness.config.baseDecimals);
      while (low < high) {
        const mid = (low + high + 1n) / 2n;
        const probe = await harness.probe("bob", "/api/v2/fork/tx/withdraw-collateral", {
          positionId: withdrawBoundaryPosition.toBase58(),
          marketAsset: "base",
          withdrawAmount: formatUnits(mid, harness.config.baseDecimals),
          minAssetAmountOut: "0",
          minLiquidationCfBps: 0,
        });
        if (probe.succeeds) low = mid;
        else high = mid - 1n;
      }
      const maxWithdraw = low;
      harness.observe("maximum safe collateral withdrawal", maxWithdraw);
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/withdraw-collateral",
        label: "reject one raw unit above safe withdrawal",
        expected: "failure",
        submit: false,
        body: {
          positionId: withdrawBoundaryPosition.toBase58(),
          marketAsset: "base",
          withdrawAmount: formatUnits(maxWithdraw + 1n, harness.config.baseDecimals),
          minAssetAmountOut: "0",
          minLiquidationCfBps: 0,
        },
      });
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/withdraw-collateral",
        label: "execute exact maximum safe withdrawal",
        body: {
          positionId: withdrawBoundaryPosition.toBase58(),
          marketAsset: "base",
          withdrawAmount: formatUnits(maxWithdraw, harness.config.baseDecimals),
          minAssetAmountOut: "0",
          minLiquidationCfBps: 0,
        },
      });
      const after = await previewPosition(harness, "bob", withdrawBoundaryPosition, "preview exact withdrawal boundary");
      harness.assertEqual("withdrawal stores the newly underwritten CF", Number(after.quoteLiquidationCfBps), storedCf);
      await repayAndWithdraw(
        harness,
        "bob",
        withdrawBoundaryPosition,
        "quote",
        "base",
        raw(100, harness.config.baseDecimals) - maxWithdraw
      );
    },
  },
];
