import { ComputeBudgetProgram, Keypair, Transaction } from "@solana/web3.js";

import { decodePreviewBorrowPositionReturnData } from "../../../packages/dusk-sdk/src/preview.js";
import { formatUnits, type ProtocolTestHarness, type ScenarioDefinition } from "../harness.js";
import type { TransactionEvidence } from "../types.js";

const staleBorrowPositionId = Keypair.generate().publicKey;
const stateMachinePositionIds = {
  alice: Keypair.generate().publicKey,
  bob: Keypair.generate().publicKey,
  trader: Keypair.generate().publicKey,
};

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

async function assertLiveMarketInvariants(
  harness: ProtocolTestHarness,
  label: string
): Promise<void> {
  const market = await harness.market();
  harness.assertTrue(`${label}: base live reserve stays positive`, stateValue(market, "baseReserve") > 0n);
  harness.assertTrue(`${label}: quote live reserve stays positive`, stateValue(market, "quoteReserve") > 0n);
  harness.assertTrue(
    `${label}: base cash never exceeds live reserve plus debt`,
    stateValue(market, "baseCashReserve") <=
      stateValue(market, "baseReserve") +
      stateValue(market, "fixedBaseDebt") +
      stateValue(market, "isolatedBaseDebt")
  );
  harness.assertTrue(
    `${label}: quote cash never exceeds live reserve plus debt`,
    stateValue(market, "quoteCashReserve") <=
      stateValue(market, "quoteReserve") +
      stateValue(market, "fixedQuoteDebt") +
      stateValue(market, "isolatedQuoteDebt")
  );
  harness.assertEqual(
    `${label}: both sides retain one common yLP supply`,
    stateValue(market, "baseReserveYlpSupply"),
    stateValue(market, "quoteReserveYlpSupply")
  );
  await harness.execute({
    wallet: "trader",
    endpoint: "/api/v2/fork/tx/preview-market",
    label: `${label}: protocol market preview validates`,
    submit: false,
    body: {},
  });
}

export const STRESS_SCENARIOS: ScenarioDefinition[] = [
  {
    id: "stress.multi-wallet-state-machine",
    async run(harness) {
      for (const [index, wallet] of (["alice", "bob", "trader"] as const).entries()) {
        const positionId = stateMachinePositionIds[wallet];
        const collateralAsset = index % 2 === 0 ? "base" : "quote";
        const debtAsset = collateralAsset === "base" ? "quote" : "base";
        const decimals = harness.config.baseDecimals;
        const ylpBefore = await harness.lpBalance(wallet, harness.config.ylpMint);

        await harness.execute({
          wallet,
          endpoint: "/api/v2/fork/tx/add-liquidity",
          label: `${wallet} adds state-machine yLP liquidity`,
          body: { baseDepositAmount: "2", quoteDepositAmount: "2", minYlpAmount: "0" },
        });
        await assertLiveMarketInvariants(harness, `${wallet} after yLP add`);

        await harness.execute({
          wallet,
          endpoint: "/api/v2/fork/tx/swap",
          label: `${wallet} executes interleaved ${index % 2 === 0 ? "base" : "quote"} swap`,
          body: {
            assetIn: index % 2 === 0 ? "base" : "quote",
            exactAssetIn: "0.25",
            minAssetOut: "0",
          },
        });
        await assertLiveMarketInvariants(harness, `${wallet} after swap`);

        await harness.execute({
          wallet,
          endpoint: "/api/v2/fork/tx/deposit-collateral",
          label: `${wallet} deposits state-machine collateral`,
          body: { positionId: positionId.toBase58(), marketAsset: collateralAsset, depositAmount: "20" },
        });
        await assertLiveMarketInvariants(harness, `${wallet} after collateral deposit`);

        await harness.execute({
          wallet,
          endpoint: "/api/v2/fork/tx/borrow",
          label: `${wallet} borrows in state-machine sequence`,
          body: {
            positionId: positionId.toBase58(),
            borrowAsset: debtAsset,
            borrowAmount: "1",
            minDebtAmountOut: "1",
            minLiquidationCfBps: 0,
          },
        });
        await assertLiveMarketInvariants(harness, `${wallet} after borrow`);

        await harness.execute({
          wallet,
          endpoint: "/api/v2/fork/tx/repay",
          label: `${wallet} repays state-machine debt`,
          body: { positionId: positionId.toBase58(), repayAsset: debtAsset, repayAmount: "1" },
        });
        await assertLiveMarketInvariants(harness, `${wallet} after repay`);

        await harness.execute({
          wallet,
          endpoint: "/api/v2/fork/tx/withdraw-collateral",
          label: `${wallet} exits state-machine collateral`,
          body: {
            positionId: positionId.toBase58(),
            marketAsset: collateralAsset,
            withdrawAmount: "20",
            minAssetAmountOut: "0",
            minLiquidationCfBps: 0,
          },
        });
        await assertLiveMarketInvariants(harness, `${wallet} after collateral exit`);

        const minted = await harness.lpBalance(wallet, harness.config.ylpMint) - ylpBefore;
        await harness.execute({
          wallet,
          endpoint: "/api/v2/fork/tx/remove-liquidity",
          label: `${wallet} removes state-machine yLP liquidity`,
          body: {
            ylpAmount: formatUnits(minted, decimals),
            minBaseAmountOut: "0",
            minQuoteAmountOut: "0",
          },
        });
        harness.assertEqual(
          `${wallet} yLP balance returns to pre-sequence value`,
          await harness.lpBalance(wallet, harness.config.ylpMint),
          ylpBefore
        );
        await assertLiveMarketInvariants(harness, `${wallet} after yLP exit`);
      }
    },
  },
  {
    id: "stress.concurrent-stale-transactions",
    async run(harness) {
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/deposit-collateral",
        label: "deposit collateral before same-state borrow pair",
        body: {
          positionId: staleBorrowPositionId.toBase58(),
          marketAsset: "base",
          depositAmount: "100",
        },
      });
      const firstBorrow = await harness.buildSignedTransaction("alice", "/api/v2/fork/tx/borrow", {
        positionId: staleBorrowPositionId.toBase58(),
        borrowAsset: "quote",
        borrowAmount: "5",
        minDebtAmountOut: "5",
        minLiquidationCfBps: 0,
      });
      const secondBorrow = await harness.buildSignedTransaction("alice", "/api/v2/fork/tx/borrow", {
        positionId: staleBorrowPositionId.toBase58(),
        borrowAsset: "quote",
        borrowAmount: "6",
        minDebtAmountOut: "6",
        minLiquidationCfBps: 0,
      });
      await harness.executeBuiltTransaction({
        wallet: "alice",
        label: "submit first borrow built from shared pre-state",
        transaction: firstBorrow,
      });
      await harness.executeBuiltTransaction({
        wallet: "alice",
        label: "submit second borrow built from shared pre-state",
        transaction: secondBorrow,
      });
      const positions = await harness.positions("alice", staleBorrowPositionId);
      const position = positions.find((entry) => entry.eventType === "borrow_position")?.payload;
      harness.assertTrue("same-state borrow pair records aggregate debt shares", BigInt(position?.fixedQuoteShares ?? 0) > 0n);
      const debtEvidence = await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/preview-borrow-position",
        label: "preview exact same-state borrow debt",
        submit: false,
        body: { positionId: staleBorrowPositionId.toBase58() },
      });
      const exactDebt = BigInt(
        decodePreviewBorrowPositionReturnData(previewData(debtEvidence)).fixedQuoteDebt.toString()
      );
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/repay",
        label: "repay same-state borrow pair",
        body: {
          positionId: staleBorrowPositionId.toBase58(),
          repayAsset: "quote",
          repayAmount: formatUnits(exactDebt, harness.config.quoteDecimals),
        },
      });
      const repaidEvidence = await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/preview-borrow-position",
        label: "confirm same-state borrow debt is fully repaid",
        submit: false,
        body: { positionId: staleBorrowPositionId.toBase58() },
      });
      harness.assertEqual(
        "same-state borrow cleanup leaves no quote debt",
        BigInt(
          decodePreviewBorrowPositionReturnData(previewData(repaidEvidence)).fixedQuoteDebt.toString()
        ),
        0n
      );
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/withdraw-collateral",
        label: "withdraw same-state borrow fixture collateral",
        body: {
          positionId: staleBorrowPositionId.toBase58(),
          marketAsset: "base",
          withdrawAmount: "100",
          minAssetAmountOut: "0",
          minLiquidationCfBps: 0,
        },
      });

      const ylpBefore = await harness.lpBalance("alice", harness.config.ylpMint);
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/add-liquidity",
        label: "mint yLP before stale double-removal pair",
        body: { baseDepositAmount: "10", quoteDepositAmount: "10", minYlpAmount: "0" },
      });
      const minted = await harness.lpBalance("alice", harness.config.ylpMint) - ylpBefore;
      const removalBody = {
        ylpAmount: formatUnits(minted, harness.config.baseDecimals),
        minBaseAmountOut: "0",
        minQuoteAmountOut: "0",
      };
      const firstRemoval = await harness.buildSignedTransaction(
        "alice",
        "/api/v2/fork/tx/remove-liquidity",
        removalBody
      );
      const staleRemoval = await harness.buildSignedTransaction(
        "alice",
        "/api/v2/fork/tx/remove-liquidity",
        { ...removalBody, minQuoteAmountOut: formatUnits(1n, harness.config.quoteDecimals) }
      );
      await harness.executeBuiltTransaction({
        wallet: "alice",
        label: "submit first full yLP removal from shared pre-state",
        transaction: firstRemoval,
      });
      await harness.executeBuiltTransaction({
        wallet: "alice",
        label: "reject stale second full yLP removal",
        transaction: staleRemoval,
        expected: "failure",
      });
      harness.assertEqual(
        "stale removal cannot burn beyond the shared starting balance",
        await harness.lpBalance("alice", harness.config.ylpMint),
        ylpBefore
      );
    },
  },
  {
    id: "stress.compute-and-account-limits",
    async run(harness) {
      const baseBefore = await harness.lpBalance("trader", harness.config.baseHlpMint);
      const quoteBefore = await harness.lpBalance("trader", harness.config.quoteHlpMint);
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/deposit-single-sided",
        label: "activate base hLP account set for compute profile",
        body: { targetAsset: "base", depositAmount: "10", minHlpAmount: "0" },
      });
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/deposit-single-sided",
        label: "activate quote hLP account set for compute profile",
        body: { targetAsset: "quote", depositAmount: "10", minHlpAmount: "0" },
      });

      const profiled = await harness.buildSignedTransaction("trader", "/api/v2/fork/tx/swap", {
        assetIn: "base",
        exactAssetIn: "10",
        minAssetOut: "0",
      });
      const profile = await harness.simulateBuiltTransaction(profiled);
      harness.assertTrue("active-hLP swap simulation succeeds", profile.succeeds, profile.errorCode);
      harness.assertTrue("active-hLP swap reports compute consumption", (profile.unitsConsumed ?? 0) > 0, profile.unitsConsumed);
      const accountCount = profiled.compileMessage().accountKeys.length;
      const serializedBytes = profiled.serialize().length;
      harness.observe("active-hLP swap resource profile", {
        unitsConsumed: profile.unitsConsumed,
        accountCount,
        serializedBytes,
      });
      harness.assertTrue("active-hLP swap stays below legacy account-key ceiling", accountCount <= 64, accountCount);
      harness.assertTrue("active-hLP swap stays within packet size", serializedBytes <= 1232, serializedBytes);

      await harness.executeBuiltTransaction({
        wallet: "trader",
        label: "execute profiled active-hLP swap",
        transaction: profiled,
      });
      const underBudgeted = await harness.buildSignedTransaction("trader", "/api/v2/fork/tx/swap", {
        assetIn: "quote",
        exactAssetIn: "10",
        minAssetOut: "0",
      });
      const required = await harness.simulateBuiltTransaction(underBudgeted);
      if (!required.succeeds || required.unitsConsumed == null) {
        throw new Error(`Unable to profile mirrored active-hLP swap: ${required.errorCode ?? "unknown"}`);
      }
      underBudgeted.instructions[1] = ComputeBudgetProgram.setComputeUnitLimit({
        units: Math.max(1, required.unitsConsumed - 1),
      });
      underBudgeted.sign(harness.wallet("trader"));
      await harness.executeBuiltTransaction({
        wallet: "trader",
        label: "reject active-hLP swap one compute unit below measured requirement",
        transaction: underBudgeted,
        expected: "failure",
      });

      const baseMinted = await harness.lpBalance("trader", harness.config.baseHlpMint) - baseBefore;
      const quoteMinted = await harness.lpBalance("trader", harness.config.quoteHlpMint) - quoteBefore;
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/withdraw-single-sided",
        label: "withdraw compute-profile base hLP",
        body: {
          targetAsset: "base",
          hlpAmount: formatUnits(baseMinted, harness.config.baseDecimals),
          minTargetAmountOut: "0",
        },
      });
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/withdraw-single-sided",
        label: "withdraw compute-profile quote hLP",
        body: {
          targetAsset: "quote",
          hlpAmount: formatUnits(quoteMinted, harness.config.quoteDecimals),
          minTargetAmountOut: "0",
        },
      });
    },
  },
];
