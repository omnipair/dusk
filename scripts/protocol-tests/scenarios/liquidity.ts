import {
  decodePreviewAddLiquidityReturnData,
} from "../../../packages/dusk-sdk/src/preview.js";
import type { TransactionEvidence } from "../types.js";

import { formatUnits, type ProtocolTestHarness, type ScenarioDefinition } from "../harness.js";

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

function accrued(yieldAccount: any): bigint {
  if (!yieldAccount) return 0n;
  return BigInt(yieldAccount.accruedSwapFeeAmount) + BigInt(yieldAccount.accruedInterestAmount);
}

async function transferLp(
  harness: ProtocolTestHarness,
  wallet: string,
  recipient: string,
  tokenKind: "ylp" | "hlp",
  asset: "base" | "quote",
  amount: bigint,
  decimals: number,
  label: string
): Promise<void> {
  await harness.execute({
    wallet,
    endpoint: "/api/v2/fork/tx/transfer-lp",
    label,
    body: {
      recipient: harness.wallet(recipient).publicKey.toBase58(),
      tokenKind,
      asset,
      amount: formatUnits(amount, decimals),
    },
  });
}

export const LIQUIDITY_SCENARIOS: ScenarioDefinition[] = [
  {
    id: "liquidity.ylp-limiting-side",
    async run(harness) {
      const cases = [
        { base: "10", quote: "1", unused: "unusedBaseAmount" },
        { base: "1", quote: "10", unused: "unusedQuoteAmount" },
      ] as const;

      for (const [index, testCase] of cases.entries()) {
        const previewEvidence = await harness.execute({
          wallet: "trader",
          endpoint: "/api/v2/fork/tx/preview-add-liquidity",
          label: `preview limiting-side deposit ${index + 1}`,
          submit: false,
          body: { baseDepositAmount: testCase.base, quoteDepositAmount: testCase.quote },
        });
        const preview = decodePreviewAddLiquidityReturnData(previewData(previewEvidence));
        const minted = integer(preview.ylpAmount);
        harness.assertTrue(`limiting-side case ${index + 1} mints shares`, minted > 0n, minted);
        harness.assertTrue(
          `limiting-side case ${index + 1} leaves excess input unused`,
          integer(preview[testCase.unused]) > 0n,
          integer(preview[testCase.unused])
        );

        const before = await harness.market();
        const ylpBefore = await harness.lpBalance("trader", harness.config.ylpMint);
        await harness.execute({
          wallet: "trader",
          endpoint: "/api/v2/fork/tx/add-liquidity",
          label: `execute limiting-side deposit ${index + 1}`,
          body: {
            baseDepositAmount: testCase.base,
            quoteDepositAmount: testCase.quote,
            minYlpAmount: formatUnits(minted, harness.config.baseDecimals),
          },
        });
        const after = await harness.market();
        harness.assertEqual(
          `limiting-side case ${index + 1} mint matches preview`,
          await harness.lpBalance("trader", harness.config.ylpMint) - ylpBefore,
          minted
        );
        harness.assertEqual(
          `limiting-side case ${index + 1} base credit matches preview`,
          stateValue(after, "baseReserve") - stateValue(before, "baseReserve"),
          integer(preview.baseReserveCredit)
        );
        harness.assertEqual(
          `limiting-side case ${index + 1} quote credit matches preview`,
          stateValue(after, "quoteReserve") - stateValue(before, "quoteReserve"),
          integer(preview.quoteReserveCredit)
        );
        await harness.execute({
          wallet: "trader",
          endpoint: "/api/v2/fork/tx/remove-liquidity",
          label: `remove limiting-side deposit ${index + 1}`,
          body: {
            ylpAmount: formatUnits(minted, harness.config.baseDecimals),
            minBaseAmountOut: "0",
            minQuoteAmountOut: "0",
          },
        });
        harness.assertEqual(
          `limiting-side case ${index + 1} burns its minted shares`,
          await harness.lpBalance("trader", harness.config.ylpMint),
          ylpBefore
        );
      }
    },
  },
  {
    id: "liquidity.ylp-transfer-checkpoints",
    async run(harness) {
      const decimals = harness.config.baseDecimals;
      const bobAddress = harness.wallet("bob").publicKey.toBase58();
      const traderAddress = harness.wallet("trader").publicKey.toBase58();

      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/add-liquidity",
        label: "initialize Bob yLP token and yield accounts",
        body: { baseDepositAmount: "1", quoteDepositAmount: "1", minYlpAmount: "0" },
      });
      const bobSetupShares = await harness.lpBalance("bob", harness.config.ylpMint);
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/remove-liquidity",
        label: "return Bob to an empty yLP balance",
        body: {
          ylpAmount: formatUnits(bobSetupShares, decimals),
          minBaseAmountOut: "0",
          minQuoteAmountOut: "0",
        },
      });
      harness.assertEqual("Bob starts transfer test with zero yLP", await harness.lpBalance("bob", harness.config.ylpMint), 0n);

      const traderBaseline = await harness.lpBalance("trader", harness.config.ylpMint);
      const initialTraderYield = accrued(await harness.yieldAccount("trader", "base", "ylp"));
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/add-liquidity",
        label: "mint yLP shares for transfer checkpoint test",
        body: { baseDepositAmount: "12", quoteDepositAmount: "12", minYlpAmount: "0" },
      });
      const minted = await harness.lpBalance("trader", harness.config.ylpMint) - traderBaseline;
      harness.assertTrue("transfer test mints divisible yLP balance", minted >= 6n, minted);

      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "accrue base fees before transfer to empty holder",
        body: { assetIn: "base", exactAssetIn: "100", minAssetOut: "0" },
      });
      const first = minted / 3n;
      await transferLp(harness, "trader", "bob", "ylp", "base", first, decimals, "transfer yLP into empty Bob account");

      const traderAfterFirst = await harness.yieldAccount("trader", "base", "ylp");
      const bobAfterFirst = await harness.yieldAccount("bob", "base", "ylp");
      harness.assertTrue(
        "pre-transfer fees remain accrued to sender",
        accrued(traderAfterFirst) > initialTraderYield,
        accrued(traderAfterFirst)
      );
      harness.assertEqual("empty receiver receives no historical fees", accrued(bobAfterFirst), 0n);

      const traderBaseBeforeClaim = await harness.tokenBalance(
        "trader",
        harness.config.baseMint,
        harness.config.baseTokenProgram
      );
      const senderHistoricalClaim = accrued(traderAfterFirst);
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/claim-yield",
        label: "claim sender fees accrued before first transfer",
        body: { asset: "base", tokenKind: "ylp", recipient: traderAddress },
      });
      harness.assertEqual(
        "sender receives exactly its checkpointed historical claim",
        await harness.tokenBalance("trader", harness.config.baseMint, harness.config.baseTokenProgram) - traderBaseBeforeClaim,
        senderHistoricalClaim
      );
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/claim-yield",
        label: "reject historical fee claim by formerly empty receiver",
        expected: "failure",
        body: { asset: "base", tokenKind: "ylp", recipient: bobAddress },
      });

      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "accrue fees while both yLP holders have balances",
        body: { assetIn: "base", exactAssetIn: "100", minAssetOut: "0" },
      });
      const second = minted / 3n;
      await transferLp(harness, "trader", "bob", "ylp", "base", second, decimals, "transfer more yLP into existing Bob account");
      const traderBeforeFull = await harness.yieldAccount("trader", "base", "ylp");
      const bobBeforeFull = await harness.yieldAccount("bob", "base", "ylp");
      harness.assertTrue("sender accrues its second-period fees", accrued(traderBeforeFull) > 0n, accrued(traderBeforeFull));
      harness.assertTrue("existing receiver accrues only its held-balance fees", accrued(bobBeforeFull) > 0n, accrued(bobBeforeFull));

      const remainder = minted - first - second;
      await transferLp(harness, "trader", "bob", "ylp", "base", remainder, decimals, "transfer remaining test yLP without new accrual");
      const traderAfterFull = await harness.yieldAccount("trader", "base", "ylp");
      const bobAfterFull = await harness.yieldAccount("bob", "base", "ylp");
      harness.assertEqual("full transfer does not move sender accrued fees", accrued(traderAfterFull), accrued(traderBeforeFull));
      harness.assertEqual("full transfer does not add sender fees to receiver", accrued(bobAfterFull), accrued(bobBeforeFull));

      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "accrue fees after Bob receives all test yLP",
        body: { assetIn: "base", exactAssetIn: "50", minAssetOut: "0" },
      });
      await transferLp(harness, "bob", "trader", "ylp", "base", 1n, decimals, "checkpoint both holders after final fee period");
      const traderFinalYield = await harness.yieldAccount("trader", "base", "ylp");
      const bobFinalYield = await harness.yieldAccount("bob", "base", "ylp");

      for (const [wallet, account, recipient] of [
        ["trader", traderFinalYield, traderAddress],
        ["bob", bobFinalYield, bobAddress],
      ] as const) {
        const before = await harness.tokenBalance(wallet, harness.config.baseMint, harness.config.baseTokenProgram);
        const expected = accrued(account);
        harness.assertTrue(`${wallet} has final checkpointed yLP yield`, expected > 0n, expected);
        await harness.execute({
          wallet,
          endpoint: "/api/v2/fork/tx/claim-yield",
          label: `claim ${wallet} final yLP yield`,
          body: { asset: "base", tokenKind: "ylp", recipient },
        });
        harness.assertEqual(
          `${wallet} receives exact final yLP claim`,
          await harness.tokenBalance(wallet, harness.config.baseMint, harness.config.baseTokenProgram) - before,
          expected
        );
      }

      await transferLp(harness, "trader", "bob", "ylp", "base", 1n, decimals, "return checkpoint unit to Bob");
      harness.assertEqual("Bob owns exactly the newly minted yLP", await harness.lpBalance("bob", harness.config.ylpMint), minted);
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/remove-liquidity",
        label: "burn all yLP used by transfer checkpoint test",
        body: {
          ylpAmount: formatUnits(minted, decimals),
          minBaseAmountOut: "0",
          minQuoteAmountOut: "0",
        },
      });
      harness.assertEqual("Bob ends transfer test with zero yLP", await harness.lpBalance("bob", harness.config.ylpMint), 0n);
      harness.assertEqual("Trader retains only pre-test yLP", await harness.lpBalance("trader", harness.config.ylpMint), traderBaseline);
    },
  },
  {
    id: "liquidity.yield-recipient-and-claim",
    async run(harness) {
      const trader = harness.wallet("trader").publicKey.toBase58();
      const bob = harness.wallet("bob").publicKey.toBase58();
      const traderShares = await harness.lpBalance("trader", harness.config.ylpMint);
      harness.assertTrue("Trader has yLP for recipient test", traderShares > 0n, traderShares);

      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/set-yield-recipient",
        label: "designate Bob as base yLP yield recipient",
        body: { asset: "base", tokenKind: "ylp", recipient: bob },
      });
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "accrue yield for designated recipient",
        body: { assetIn: "base", exactAssetIn: "100", minAssetOut: "0" },
      });
      const bobBefore = await harness.tokenBalance("bob", harness.config.baseMint, harness.config.baseTokenProgram);
      const traderBefore = await harness.tokenBalance("trader", harness.config.baseMint, harness.config.baseTokenProgram);
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/claim-yield",
        label: "pay yLP yield to designated Bob recipient",
        body: { asset: "base", tokenKind: "ylp", recipient: bob },
      });
      harness.assertTrue(
        "designated recipient receives the claim",
        await harness.tokenBalance("bob", harness.config.baseMint, harness.config.baseTokenProgram) > bobBefore
      );
      harness.assertEqual(
        "yield owner receives no tokens while Bob is designated",
        await harness.tokenBalance("trader", harness.config.baseMint, harness.config.baseTokenProgram),
        traderBefore
      );

      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/set-yield-recipient",
        label: "rotate base yLP yield recipient back to owner",
        body: { asset: "base", tokenKind: "ylp", recipient: trader },
      });
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/claim-yield",
        label: "reject stale former recipient after rotation",
        expected: "failure",
        body: { asset: "base", tokenKind: "ylp", recipient: bob },
      });
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "accrue yield after recipient rotation",
        body: { assetIn: "base", exactAssetIn: "100", minAssetOut: "0" },
      });
      const ownerBefore = await harness.tokenBalance("trader", harness.config.baseMint, harness.config.baseTokenProgram);
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/claim-yield",
        label: "claim rotated yLP yield to owner",
        body: { asset: "base", tokenKind: "ylp", recipient: trader },
      });
      harness.assertTrue(
        "owner receives yield after recipient rotation",
        await harness.tokenBalance("trader", harness.config.baseMint, harness.config.baseTokenProgram) > ownerBefore
      );
    },
  },
  {
    id: "liquidity.yield-rounding-and-empty-claim",
    async run(harness) {
      const trader = harness.wallet("trader").publicKey.toBase58();
      const before = await harness.yieldAccount("trader", "base", "ylp");
      harness.assertEqual("previous scenario leaves no stored base yLP claim", accrued(before), 0n);
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/claim-yield",
        label: "reject repeated empty yLP claim",
        expected: "failure",
        body: { asset: "base", tokenKind: "ylp", recipient: trader },
      });

      const oneRaw = formatUnits(1n, harness.config.baseDecimals);
      const marketBefore = await harness.market();
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "reject swap too small to survive fee and output rounding",
        expected: "failure",
        body: { assetIn: "base", exactAssetIn: oneRaw, minAssetOut: "0" },
      });
      const marketAfter = await harness.market();
      harness.assertEqual("rounded-to-zero swap leaves base reserve unchanged", stateValue(marketAfter, "baseReserve"), stateValue(marketBefore, "baseReserve"));
      harness.assertEqual("rounded-to-zero swap leaves quote reserve unchanged", stateValue(marketAfter, "quoteReserve"), stateValue(marketBefore, "quoteReserve"));
    },
  },
  {
    id: "hlp.transfer-checkpoints",
    async run(harness) {
      const decimals = harness.config.baseDecimals;
      const bobAddress = harness.wallet("bob").publicKey.toBase58();
      const traderAddress = harness.wallet("trader").publicKey.toBase58();

      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/deposit-single-sided",
        label: "initialize Bob base hLP token and yield account",
        body: { targetAsset: "base", depositAmount: "1", minHlpAmount: "0" },
      });
      const bobSetupShares = await harness.lpBalance("bob", harness.config.baseHlpMint);
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/withdraw-single-sided",
        label: "return Bob to empty base hLP balance",
        body: { targetAsset: "base", hlpAmount: formatUnits(bobSetupShares, decimals), minTargetAmountOut: "0" },
      });
      harness.assertEqual("Bob starts hLP transfer test empty", await harness.lpBalance("bob", harness.config.baseHlpMint), 0n);

      const traderBaseline = await harness.lpBalance("trader", harness.config.baseHlpMint);
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/deposit-single-sided",
        label: "mint base hLP for transfer checkpoint test",
        body: { targetAsset: "base", depositAmount: "20", minHlpAmount: "0" },
      });
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "accrue underlying yLP fees for base hLP vault",
        body: { assetIn: "base", exactAssetIn: "100", minAssetOut: "0" },
      });
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/deposit-single-sided",
        label: "checkpoint hLP vault yield before first transfer",
        body: { targetAsset: "base", depositAmount: "1", minHlpAmount: "0" },
      });
      const minted = await harness.lpBalance("trader", harness.config.baseHlpMint) - traderBaseline;
      const first = minted / 3n;
      await transferLp(harness, "trader", "bob", "hlp", "base", first, decimals, "transfer base hLP into empty Bob account");
      const traderAfterFirst = await harness.yieldAccount("trader", "base", "hlp");
      const bobAfterFirst = await harness.yieldAccount("bob", "base", "hlp");
      harness.assertTrue("hLP sender retains pre-transfer yield", accrued(traderAfterFirst) > 0n, accrued(traderAfterFirst));
      harness.assertEqual("empty hLP receiver gets no historical yield", accrued(bobAfterFirst), 0n);

      const traderBaseBeforeClaim = await harness.tokenBalance("trader", harness.config.baseMint, harness.config.baseTokenProgram);
      const firstClaim = accrued(traderAfterFirst);
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/claim-yield",
        label: "claim hLP sender historical yield",
        body: { asset: "base", tokenKind: "hlp", recipient: traderAddress },
      });
      harness.assertEqual(
        "hLP sender receives exact historical claim",
        await harness.tokenBalance("trader", harness.config.baseMint, harness.config.baseTokenProgram) - traderBaseBeforeClaim,
        firstClaim
      );
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/claim-yield",
        label: "reject empty historical hLP claim",
        expected: "failure",
        body: { asset: "base", tokenKind: "hlp", recipient: bobAddress },
      });

      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "accrue hLP yield while both holders are active",
        body: { assetIn: "base", exactAssetIn: "100", minAssetOut: "0" },
      });
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/deposit-single-sided",
        label: "checkpoint hLP vault yield for existing receiver",
        body: { targetAsset: "base", depositAmount: "1", minHlpAmount: "0" },
      });
      const totalMinted = await harness.lpBalance("trader", harness.config.baseHlpMint) + first - traderBaseline;
      const second = totalMinted / 3n;
      await transferLp(harness, "trader", "bob", "hlp", "base", second, decimals, "transfer base hLP into existing Bob account");
      const traderBeforeFull = await harness.yieldAccount("trader", "base", "hlp");
      const bobBeforeFull = await harness.yieldAccount("bob", "base", "hlp");
      harness.assertTrue("hLP sender accrues second-period yield", accrued(traderBeforeFull) > 0n, accrued(traderBeforeFull));
      harness.assertTrue("existing hLP receiver accrues held-balance yield", accrued(bobBeforeFull) > 0n, accrued(bobBeforeFull));

      const traderCurrent = await harness.lpBalance("trader", harness.config.baseHlpMint);
      const remainder = traderCurrent - traderBaseline;
      await transferLp(harness, "trader", "bob", "hlp", "base", remainder, decimals, "transfer remaining test hLP without new accrual");
      const traderAfterFull = await harness.yieldAccount("trader", "base", "hlp");
      const bobAfterFull = await harness.yieldAccount("bob", "base", "hlp");
      harness.assertEqual("full hLP transfer preserves sender accrued amount", accrued(traderAfterFull), accrued(traderBeforeFull));
      harness.assertEqual("full hLP transfer does not import sender accrual", accrued(bobAfterFull), accrued(bobBeforeFull));

      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "accrue final hLP fee period",
        body: { assetIn: "base", exactAssetIn: "50", minAssetOut: "0" },
      });
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/deposit-single-sided",
        label: "checkpoint final hLP vault fee period",
        body: { targetAsset: "base", depositAmount: "1", minHlpAmount: "0" },
      });
      await transferLp(harness, "bob", "trader", "hlp", "base", 1n, decimals, "checkpoint both hLP holders after final period");

      for (const [wallet, recipient] of [["trader", traderAddress], ["bob", bobAddress]] as const) {
        const account = await harness.yieldAccount(wallet, "base", "hlp");
        const expected = accrued(account);
        const before = await harness.tokenBalance(wallet, harness.config.baseMint, harness.config.baseTokenProgram);
        harness.assertTrue(`${wallet} has final checkpointed hLP yield`, expected > 0n, expected);
        await harness.execute({
          wallet,
          endpoint: "/api/v2/fork/tx/claim-yield",
          label: `claim ${wallet} final hLP yield`,
          body: { asset: "base", tokenKind: "hlp", recipient },
        });
        harness.assertEqual(
          `${wallet} receives exact final hLP claim`,
          await harness.tokenBalance(wallet, harness.config.baseMint, harness.config.baseTokenProgram) - before,
          expected
        );
      }

      await transferLp(harness, "trader", "bob", "hlp", "base", 1n, decimals, "return hLP checkpoint unit to Bob");
      const bobFinalShares = await harness.lpBalance("bob", harness.config.baseHlpMint);
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/withdraw-single-sided",
        label: "withdraw all hLP used by transfer checkpoint test",
        body: { targetAsset: "base", hlpAmount: formatUnits(bobFinalShares, decimals), minTargetAmountOut: "0" },
      });
      harness.assertEqual("Bob ends hLP transfer test empty", await harness.lpBalance("bob", harness.config.baseHlpMint), 0n);
      harness.assertEqual("Trader retains only pre-test hLP", await harness.lpBalance("trader", harness.config.baseHlpMint), traderBaseline);
    },
  },
];
