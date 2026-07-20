import { getAssociatedTokenAddressSync } from "@solana/spl-token";
import { Keypair, LAMPORTS_PER_SOL, PublicKey } from "@solana/web3.js";

import {
  decodePreviewAddLiquidityReturnData,
  decodePreviewBorrowCapacityReturnData,
  decodePreviewBorrowPositionReturnData,
  decodePreviewMarketReturnData,
  decodePreviewSwapReturnData,
} from "../../../packages/dusk-sdk/src/preview.js";
import type { TransactionEvidence } from "../types.js";

import { formatUnits, type ProtocolTestHarness, type ScenarioDefinition } from "../harness.js";

const lendingPositionId = Keypair.generate().publicKey;
const emptyPositionId = Keypair.generate().publicKey;
const previewPositionId = Keypair.generate().publicKey;
const boundaryPositionId = Keypair.generate().publicKey;
const referralPositionId = Keypair.generate().publicKey;

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

export const BASELINE_SCENARIOS: ScenarioDefinition[] = [
  {
    id: "system.bootstrap-clean",
    fatal: true,
    async run(harness) {
      const market = await harness.market();
      harness.observe("fork config", harness.config);
      harness.assertEqual("program id matches market config", harness.config.programId, "358bjJKXWxeAXAzteX1xTgyd9JNnjtzW8fnwCS8Da1mv");
      harness.assertEqual("market address matches config", market.marketAddress, harness.config.market);
      if (harness.config.fixtureMode === "token2022-fees") {
        harness.assertTrue("transfer-fee fixture starts with positive base reserve", stateValue(market, "baseReserve") > 0n);
        harness.assertTrue("transfer-fee fixture starts with positive quote reserve", stateValue(market, "quoteReserve") > 0n);
        harness.assertTrue("transfer-fee fixture starts with positive yLP supply", stateValue(market, "baseReserveYlpSupply") > 0n);
        harness.assertEqual(
          "transfer-fee fixture keeps one common yLP supply",
          stateValue(market, "baseReserveYlpSupply"),
          stateValue(market, "quoteReserveYlpSupply")
        );
      } else {
        harness.assertEqual("initial base reserve", stateValue(market, "baseReserve"), raw(100_000, market.baseDecimals));
        harness.assertEqual("initial quote reserve", stateValue(market, "quoteReserve"), raw(100_000, market.quoteDecimals));
        if (harness.config.fixtureMode === "mixed-decimals") {
          harness.assertTrue("mixed-decimal fixture starts with positive yLP supply", stateValue(market, "baseReserveYlpSupply") > 0n);
          harness.assertEqual(
            "mixed-decimal fixture keeps one common yLP supply",
            stateValue(market, "baseReserveYlpSupply"),
            stateValue(market, "quoteReserveYlpSupply")
          );
        } else {
          harness.assertEqual("initial base yLP supply", stateValue(market, "baseReserveYlpSupply"), raw(100_000, market.baseDecimals));
          harness.assertEqual("initial quote yLP supply", stateValue(market, "quoteReserveYlpSupply"), raw(100_000, market.quoteDecimals));
        }
      }
      harness.assertEqual("initial base debt", stateValue(market, "fixedBaseDebt"), 0n);
      harness.assertEqual("initial quote debt", stateValue(market, "fixedQuoteDebt"), 0n);
      harness.assertEqual("initial base global-health contribution", stateValue(market, "globalHealthBaseContributionForQuoteDebt"), 0n);
      harness.assertEqual("initial quote global-health contribution", stateValue(market, "globalHealthQuoteContributionForBaseDebt"), 0n);
      harness.assertEqual("market is open", market.reduceOnly, false);
    },
  },
  {
    id: "system.real-wallet-funding",
    async run(harness) {
      for (const wallet of ["alice", "bob", "trader", "referrer", "liquidator", "bidder"]) {
        await harness.fundWallet(wallet);
        harness.assertTrue(`${wallet} has transaction fees`, await harness.solBalance(wallet) >= 19 * LAMPORTS_PER_SOL);
        harness.assertEqual(
          `${wallet} base funding`,
          await harness.tokenBalance(wallet, harness.config.baseMint, harness.config.baseTokenProgram),
          raw(1_000, harness.config.baseDecimals)
        );
        harness.assertEqual(
          `${wallet} quote funding`,
          await harness.tokenBalance(wallet, harness.config.quoteMint, harness.config.quoteTokenProgram),
          raw(1_000, harness.config.quoteDecimals)
        );
      }
    },
  },
  {
    id: "liquidity.ylp-balanced-roundtrip",
    async run(harness) {
      const before = await harness.market();
      const ylpBefore = await harness.lpBalance("trader", harness.config.ylpMint);
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/add-liquidity",
        label: "add balanced yLP liquidity",
        body: { baseDepositAmount: "10", quoteDepositAmount: "10", minYlpAmount: "0" },
      });
      const afterAdd = await harness.market();
      const ylpAfterAdd = await harness.lpBalance("trader", harness.config.ylpMint);
      const minted = ylpAfterAdd - ylpBefore;
      harness.assertTrue("balanced deposit mints yLP", minted > 0n, minted);
      harness.assertEqual("base reserve receives deposit", stateValue(afterAdd, "baseReserve") - stateValue(before, "baseReserve"), raw(10, harness.config.baseDecimals));
      harness.assertEqual("quote reserve receives deposit", stateValue(afterAdd, "quoteReserve") - stateValue(before, "quoteReserve"), raw(10, harness.config.quoteDecimals));
      harness.assertEqual("base yLP supply delta matches wallet mint", stateValue(afterAdd, "baseReserveYlpSupply") - stateValue(before, "baseReserveYlpSupply"), minted);
      harness.assertEqual("quote yLP supply delta matches wallet mint", stateValue(afterAdd, "quoteReserveYlpSupply") - stateValue(before, "quoteReserveYlpSupply"), minted);

      const burn = minted / 2n;
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/remove-liquidity",
        label: "remove half of newly minted yLP",
        body: {
          ylpAmount: formatUnits(burn, harness.config.baseDecimals),
          minBaseAmountOut: "0",
          minQuoteAmountOut: "0",
        },
      });
      const afterRemove = await harness.market();
      const ylpAfterRemove = await harness.lpBalance("trader", harness.config.ylpMint);
      harness.assertEqual("requested yLP amount is burned", ylpAfterAdd - ylpAfterRemove, burn);
      harness.assertTrue("base reserve decreases on removal", stateValue(afterRemove, "baseReserve") < stateValue(afterAdd, "baseReserve"));
      harness.assertTrue("quote reserve decreases on removal", stateValue(afterRemove, "quoteReserve") < stateValue(afterAdd, "quoteReserve"));
      harness.assertTrue("partial roundtrip leaves positive added liquidity", stateValue(afterRemove, "baseReserve") > stateValue(before, "baseReserve"));
    },
  },
  {
    id: "liquidity.ylp-slippage-rejected",
    async run(harness) {
      const before = await harness.market();
      const ylpBefore = await harness.lpBalance("trader", harness.config.ylpMint);
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/add-liquidity",
        label: "reject impossible minimum yLP output",
        expected: "failure",
        body: { baseDepositAmount: "1", quoteDepositAmount: "1", minYlpAmount: "1000000" },
      });
      const after = await harness.market();
      harness.assertEqual("failed add leaves base reserve unchanged", stateValue(after, "baseReserve"), stateValue(before, "baseReserve"));
      harness.assertEqual("failed add leaves quote reserve unchanged", stateValue(after, "quoteReserve"), stateValue(before, "quoteReserve"));
      harness.assertEqual("failed add leaves wallet yLP unchanged", await harness.lpBalance("trader", harness.config.ylpMint), ylpBefore);
    },
  },
  {
    id: "swap.bidirectional",
    async run(harness) {
      const before = await harness.market();
      const traderBaseBefore = await harness.tokenBalance("trader", harness.config.baseMint, harness.config.baseTokenProgram);
      const traderQuoteBefore = await harness.tokenBalance("trader", harness.config.quoteMint, harness.config.quoteTokenProgram);
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "swap base for quote",
        body: { assetIn: "base", exactAssetIn: "1", minAssetOut: "0" },
      });
      const afterBaseIn = await harness.market();
      const traderBaseAfterBaseIn = await harness.tokenBalance("trader", harness.config.baseMint, harness.config.baseTokenProgram);
      const traderQuoteAfterBaseIn = await harness.tokenBalance("trader", harness.config.quoteMint, harness.config.quoteTokenProgram);
      harness.assertEqual("base input debited exactly", traderBaseBefore - traderBaseAfterBaseIn, raw(1, harness.config.baseDecimals));
      harness.assertTrue("quote output credited", traderQuoteAfterBaseIn > traderQuoteBefore);
      harness.assertTrue("base reserve rises", stateValue(afterBaseIn, "baseReserve") > stateValue(before, "baseReserve"));
      harness.assertTrue("quote reserve falls", stateValue(afterBaseIn, "quoteReserve") < stateValue(before, "quoteReserve"));

      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "swap quote for base",
        body: { assetIn: "quote", exactAssetIn: "0.5", minAssetOut: "0" },
      });
      const afterQuoteIn = await harness.market();
      const traderBaseAfterQuoteIn = await harness.tokenBalance("trader", harness.config.baseMint, harness.config.baseTokenProgram);
      const traderQuoteAfterQuoteIn = await harness.tokenBalance("trader", harness.config.quoteMint, harness.config.quoteTokenProgram);
      harness.assertEqual("quote input debited exactly", traderQuoteAfterBaseIn - traderQuoteAfterQuoteIn, raw(1, harness.config.quoteDecimals) / 2n);
      harness.assertTrue("base output credited", traderBaseAfterQuoteIn > traderBaseAfterBaseIn);
      harness.assertTrue("quote reserve rises on reverse swap", stateValue(afterQuoteIn, "quoteReserve") > stateValue(afterBaseIn, "quoteReserve"));
      harness.assertTrue("base reserve falls on reverse swap", stateValue(afterQuoteIn, "baseReserve") < stateValue(afterBaseIn, "baseReserve"));
    },
  },
  {
    id: "swap.slippage-rejected",
    async run(harness) {
      const before = await harness.market();
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "reject impossible swap output floor",
        expected: "failure",
        body: { assetIn: "base", exactAssetIn: "0.1", minAssetOut: "1000000" },
      });
      const after = await harness.market();
      harness.assertEqual("failed swap leaves base reserve unchanged", stateValue(after, "baseReserve"), stateValue(before, "baseReserve"));
      harness.assertEqual("failed swap leaves quote reserve unchanged", stateValue(after, "quoteReserve"), stateValue(before, "quoteReserve"));
    },
  },
  {
    id: "lending.quote-lifecycle",
    async run(harness) {
      const aliceBaseBefore = await harness.tokenBalance("alice", harness.config.baseMint, harness.config.baseTokenProgram);
      const aliceQuoteBefore = await harness.tokenBalance("alice", harness.config.quoteMint, harness.config.quoteTokenProgram);
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/deposit-collateral",
        label: "deposit base collateral",
        body: { positionId: lendingPositionId.toBase58(), marketAsset: "base", depositAmount: "100" },
      });
      let positions = await harness.positions("alice", lendingPositionId);
      harness.assertEqual("one borrow position exists", positions.length, 1);
      harness.assertEqual("base collateral is recorded", BigInt(positions[0].payload.baseCollateral), raw(100, harness.config.baseDecimals));
      harness.assertEqual("collateral tokens leave owner", aliceBaseBefore - await harness.tokenBalance("alice", harness.config.baseMint, harness.config.baseTokenProgram), raw(100, harness.config.baseDecimals));

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/borrow",
        label: "borrow quote against base collateral",
        body: { positionId: lendingPositionId.toBase58(), borrowAsset: "quote", borrowAmount: "10", minDebtAmountOut: "10", minLiquidationCfBps: 0 },
      });
      positions = await harness.positions("alice", lendingPositionId);
      harness.assertTrue("quote debt shares are recorded", BigInt(positions[0].payload.fixedQuoteShares) > 0n, positions[0].payload.fixedQuoteShares);
      harness.assertEqual("borrow proceeds reach owner", await harness.tokenBalance("alice", harness.config.quoteMint, harness.config.quoteTokenProgram) - aliceQuoteBefore, raw(10, harness.config.quoteDecimals));

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/repay",
        label: "repay quote debt in full",
        body: { positionId: lendingPositionId.toBase58(), repayAsset: "quote", repayAmount: "10" },
      });
      positions = await harness.positions("alice", lendingPositionId);
      harness.assertEqual("quote debt shares clear", BigInt(positions[0].payload.fixedQuoteShares), 0n);

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/withdraw-collateral",
        label: "withdraw idle base collateral",
        body: { positionId: lendingPositionId.toBase58(), marketAsset: "base", withdrawAmount: "100", minAssetAmountOut: "100", minLiquidationCfBps: 0 },
      });
      positions = await harness.positions("alice", lendingPositionId);
      harness.assertEqual("base collateral clears", BigInt(positions[0].payload.baseCollateral), 0n);
      harness.assertEqual("collateral roundtrip returns exact base", await harness.tokenBalance("alice", harness.config.baseMint, harness.config.baseTokenProgram), aliceBaseBefore);
    },
  },
  {
    id: "lending.borrow-without-collateral-rejected",
    async run(harness) {
      const before = await harness.market();
      const bobQuoteBefore = await harness.tokenBalance("bob", harness.config.quoteMint, harness.config.quoteTokenProgram);
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/borrow",
        label: "reject borrow from an empty position",
        expected: "failure",
        body: { positionId: emptyPositionId.toBase58(), borrowAsset: "quote", borrowAmount: "1", minDebtAmountOut: "1", minLiquidationCfBps: 0 },
      });
      const after = await harness.market();
      harness.assertEqual("failed borrow leaves quote debt unchanged", stateValue(after, "fixedQuoteDebt"), stateValue(before, "fixedQuoteDebt"));
      harness.assertEqual("failed borrow gives Bob no proceeds", await harness.tokenBalance("bob", harness.config.quoteMint, harness.config.quoteTokenProgram), bobQuoteBefore);
    },
  },
  {
    id: "referral.profile-and-claim",
    async run(harness) {
      const referrer = harness.wallet("referrer");
      const bob = harness.wallet("bob");
      const trader = harness.wallet("trader");
      const [referralProfile] = PublicKey.findProgramAddressSync(
        [Buffer.from("referral_profile"), referrer.publicKey.toBuffer()],
        new PublicKey(harness.config.programId)
      );
      const referralVault = getAssociatedTokenAddressSync(
        new PublicKey(harness.config.quoteMint),
        referralProfile,
        true,
        new PublicKey(harness.config.quoteTokenProgram)
      );

      await harness.execute({
        wallet: "referrer",
        endpoint: "/api/v2/fork/tx/set-referral-recipient",
        label: "create referral profile with Bob recipient",
        body: { recipient: bob.publicKey.toBase58() },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/deposit-collateral",
        label: "deposit collateral for referred borrow",
        body: { positionId: referralPositionId.toBase58(), marketAsset: "base", depositAmount: "100" },
      });

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/borrow",
        label: "reject referred borrow above client fee cap",
        expected: "failure",
        body: {
          positionId: referralPositionId.toBase58(),
          borrowAsset: "quote",
          borrowAmount: "10",
          minDebtAmountOut: "10",
          minLiquidationCfBps: 0,
          referrer: referrer.publicKey.toBase58(),
          maxAcceptableReferralFeeBps: 9,
        },
      });
      harness.assertEqual("rejected referred borrow accrues no fee", await harness.tokenAccountBalance(referralVault, harness.config.quoteTokenProgram), 0n);

      const aliceQuoteBefore = await harness.tokenBalance("alice", harness.config.quoteMint, harness.config.quoteTokenProgram);
      const bobQuoteBefore = await harness.tokenBalance("bob", harness.config.quoteMint, harness.config.quoteTokenProgram);
      const traderQuoteBefore = await harness.tokenBalance("trader", harness.config.quoteMint, harness.config.quoteTokenProgram);
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/borrow",
        label: "execute 10 bps referred borrow",
        body: {
          positionId: referralPositionId.toBase58(),
          borrowAsset: "quote",
          borrowAmount: "10",
          minDebtAmountOut: "10",
          minLiquidationCfBps: 0,
          referrer: referrer.publicKey.toBase58(),
          maxAcceptableReferralFeeBps: 10,
        },
      });
      const fee = (raw(10, harness.config.quoteDecimals) * 10n + 9_999n) / 10_000n;
      harness.assertEqual("borrower receives requested principal only", await harness.tokenBalance("alice", harness.config.quoteMint, harness.config.quoteTokenProgram) - aliceQuoteBefore, raw(10, harness.config.quoteDecimals));
      harness.assertEqual("referral vault receives ceiling fee", await harness.tokenAccountBalance(referralVault, harness.config.quoteTokenProgram), fee);

      const positionEvidence = await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/preview-borrow-position",
        label: "preview referred gross debt",
        submit: false,
        body: { positionId: referralPositionId.toBase58() },
      });
      const position = decodePreviewBorrowPositionReturnData(previewData(positionEvidence));
      harness.assertEqual("gross debt includes referral fee", integer(position.fixedQuoteDebt), raw(10, harness.config.quoteDecimals) + fee);

      await harness.execute({
        wallet: "referrer",
        endpoint: "/api/v2/fork/tx/set-referral-recipient",
        label: "rotate referral recipient to trader",
        body: { recipient: trader.publicKey.toBase58() },
      });
      await harness.execute({
        wallet: "referrer",
        endpoint: "/api/v2/fork/tx/claim-referral-fees",
        label: "claim accrued referral fee after recipient rotation",
        body: { asset: "quote", recipient: trader.publicKey.toBase58() },
      });
      harness.assertEqual("rotated recipient receives accrued fee", await harness.tokenBalance("trader", harness.config.quoteMint, harness.config.quoteTokenProgram) - traderQuoteBefore, fee);
      harness.assertEqual("old recipient receives nothing", await harness.tokenBalance("bob", harness.config.quoteMint, harness.config.quoteTokenProgram), bobQuoteBefore);
      harness.assertEqual("claim drains referral vault", await harness.tokenAccountBalance(referralVault, harness.config.quoteTokenProgram), 0n);
      await harness.execute({
        wallet: "referrer",
        endpoint: "/api/v2/fork/tx/claim-referral-fees",
        label: "reject empty referral claim",
        expected: "failure",
        body: { asset: "quote", recipient: trader.publicKey.toBase58() },
      });

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/repay",
        label: "repay referred gross debt",
        body: { positionId: referralPositionId.toBase58(), repayAsset: "quote", repayAmount: formatUnits(integer(position.fixedQuoteDebt), harness.config.quoteDecimals) },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/withdraw-collateral",
        label: "withdraw referred borrow collateral",
        body: { positionId: referralPositionId.toBase58(), marketAsset: "base", withdrawAmount: "100", minAssetAmountOut: "100", minLiquidationCfBps: 0 },
      });
    },
  },
  {
    id: "hlp.base-roundtrip",
    async run(harness) {
      const before = await harness.lpBalance("trader", harness.config.baseHlpMint);
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/deposit-single-sided",
        label: "deposit base hLP",
        body: { targetAsset: "base", depositAmount: "5", minHlpAmount: "0" },
      });
      const afterDeposit = await harness.lpBalance("trader", harness.config.baseHlpMint);
      const minted = afterDeposit - before;
      harness.assertTrue("base hLP is minted", minted > 0n, minted);
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/withdraw-single-sided",
        label: "withdraw all newly minted base hLP",
        body: { targetAsset: "base", hlpAmount: formatUnits(minted, harness.config.baseDecimals), minTargetAmountOut: "0" },
      });
      harness.assertEqual("base hLP roundtrip burns minted shares", await harness.lpBalance("trader", harness.config.baseHlpMint), before);
    },
  },
  {
    id: "hlp.quote-roundtrip",
    async run(harness) {
      const before = await harness.lpBalance("trader", harness.config.quoteHlpMint);
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/deposit-single-sided",
        label: "deposit quote hLP",
        body: { targetAsset: "quote", depositAmount: "5", minHlpAmount: "0" },
      });
      const afterDeposit = await harness.lpBalance("trader", harness.config.quoteHlpMint);
      const minted = afterDeposit - before;
      harness.assertTrue("quote hLP is minted", minted > 0n, minted);
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/withdraw-single-sided",
        label: "withdraw all newly minted quote hLP",
        body: { targetAsset: "quote", hlpAmount: formatUnits(minted, harness.config.quoteDecimals), minTargetAmountOut: "0" },
      });
      harness.assertEqual("quote hLP roundtrip burns minted shares", await harness.lpBalance("trader", harness.config.quoteHlpMint), before);
    },
  },
  {
    id: "hlp.deposit-slippage-rejected",
    async run(harness) {
      const baseHlpBefore = await harness.lpBalance("trader", harness.config.baseHlpMint);
      const marketBefore = await harness.market();
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/deposit-single-sided",
        label: "reject impossible minimum hLP output",
        expected: "failure",
        body: { targetAsset: "base", depositAmount: "1", minHlpAmount: "1000000" },
      });
      const marketAfter = await harness.market();
      harness.assertEqual("failed hLP deposit leaves wallet shares unchanged", await harness.lpBalance("trader", harness.config.baseHlpMint), baseHlpBefore);
      harness.assertEqual("failed hLP deposit leaves base reserve unchanged", stateValue(marketAfter, "baseReserve"), stateValue(marketBefore, "baseReserve"));
      harness.assertEqual("failed hLP deposit leaves quote reserve unchanged", stateValue(marketAfter, "quoteReserve"), stateValue(marketBefore, "quoteReserve"));
    },
  },
  {
    id: "preview.all-vs-execution",
    async run(harness) {
      const marketBefore = await harness.market();
      const marketEvidence = await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/preview-market",
        label: "preview current market",
        submit: false,
        body: {},
      });
      const marketPreview = decodePreviewMarketReturnData(previewData(marketEvidence));
      harness.assertEqual("market preview base reserve matches account", integer(marketPreview.base.liveReserve), stateValue(marketBefore, "baseReserve"));
      harness.assertEqual("market preview quote reserve matches account", integer(marketPreview.quote.liveReserve), stateValue(marketBefore, "quoteReserve"));

      const liquidityEvidence = await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/preview-add-liquidity",
        label: "preview asymmetric liquidity deposit",
        submit: false,
        body: { baseDepositAmount: "2", quoteDepositAmount: "3" },
      });
      const liquidityPreview = decodePreviewAddLiquidityReturnData(previewData(liquidityEvidence));
      const ylpBefore = await harness.lpBalance("trader", harness.config.ylpMint);
      const stateBeforeAdd = await harness.market();
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/add-liquidity",
        label: "execute previewed asymmetric liquidity deposit",
        body: { baseDepositAmount: "2", quoteDepositAmount: "3", minYlpAmount: "0" },
      });
      const stateAfterAdd = await harness.market();
      const ylpAfter = await harness.lpBalance("trader", harness.config.ylpMint);
      harness.assertEqual("previewed yLP mint matches execution", ylpAfter - ylpBefore, integer(liquidityPreview.ylpAmount));
      harness.assertEqual("previewed base reserve credit matches execution", stateValue(stateAfterAdd, "baseReserve") - stateValue(stateBeforeAdd, "baseReserve"), integer(liquidityPreview.baseReserveCredit));
      harness.assertEqual("previewed quote reserve credit matches execution", stateValue(stateAfterAdd, "quoteReserve") - stateValue(stateBeforeAdd, "quoteReserve"), integer(liquidityPreview.quoteReserveCredit));
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/remove-liquidity",
        label: "remove preview comparison liquidity",
        body: { ylpAmount: formatUnits(integer(liquidityPreview.ylpAmount), harness.config.baseDecimals), minBaseAmountOut: "0", minQuoteAmountOut: "0" },
      });

      const swapEvidence = await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/preview-swap",
        label: "preview base to quote swap",
        submit: false,
        body: { assetIn: "base", exactAssetIn: "1" },
      });
      const swapPreview = decodePreviewSwapReturnData(previewData(swapEvidence));
      const quoteBeforeSwap = await harness.tokenBalance("trader", harness.config.quoteMint, harness.config.quoteTokenProgram);
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "execute previewed base to quote swap",
        body: { assetIn: "base", exactAssetIn: "1", minAssetOut: "0" },
      });
      const quoteAfterSwap = await harness.tokenBalance("trader", harness.config.quoteMint, harness.config.quoteTokenProgram);
      harness.assertEqual("previewed swap output matches execution", quoteAfterSwap - quoteBeforeSwap, integer(swapPreview.amountOut));

      const capacityEvidence = await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/preview-borrow-capacity",
        label: "preview quote borrow capacity",
        submit: false,
        body: { collateralAsset: "base", collateralAmount: "100", projectedBorrowAmount: "10", withReferral: false },
      });
      const capacityPreview = decodePreviewBorrowCapacityReturnData(previewData(capacityEvidence));
      harness.assertTrue("borrow preview exposes positive capacity", integer(capacityPreview.maxBorrowAmount) > 0n, capacityPreview.maxBorrowAmount);
      harness.assertEqual("borrow preview keeps requested principal", integer(capacityPreview.projectedBorrowAmount), raw(10, harness.config.quoteDecimals));

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/deposit-collateral",
        label: "deposit collateral for preview comparison",
        body: { positionId: previewPositionId.toBase58(), marketAsset: "base", depositAmount: "100" },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/borrow",
        label: "execute previewed quote borrow",
        body: { positionId: previewPositionId.toBase58(), borrowAsset: "quote", borrowAmount: "10", minDebtAmountOut: "10", minLiquidationCfBps: 0 },
      });
      const positionEvidence = await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/preview-borrow-position",
        label: "preview executed borrow position",
        submit: false,
        body: { positionId: previewPositionId.toBase58() },
      });
      const positionPreview = decodePreviewBorrowPositionReturnData(previewData(positionEvidence));
      harness.assertEqual("position preview debt matches capacity projection", integer(positionPreview.fixedQuoteDebt), integer(capacityPreview.projectedDebtAmount));
      harness.assertEqual("position preview collateral matches deposit", integer(positionPreview.baseCollateral), raw(100, harness.config.baseDecimals));
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/repay",
        label: "repay preview comparison debt",
        body: { positionId: previewPositionId.toBase58(), repayAsset: "quote", repayAmount: formatUnits(integer(positionPreview.fixedQuoteDebt), harness.config.quoteDecimals) },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/withdraw-collateral",
        label: "withdraw preview comparison collateral",
        body: { positionId: previewPositionId.toBase58(), marketAsset: "base", withdrawAmount: "100", minAssetAmountOut: "100", minLiquidationCfBps: 0 },
      });
    },
  },
  {
    id: "preview.monotonicity",
    async run(harness) {
      let previousCapacity = 0n;
      for (const collateral of [1, 10, 100, 1000]) {
        const evidence = await harness.execute({
          wallet: "alice",
          endpoint: "/api/v2/fork/tx/preview-borrow-capacity",
          label: `preview capacity for ${collateral} base collateral`,
          submit: false,
          body: { collateralAsset: "base", collateralAmount: String(collateral), withReferral: false },
        });
        const preview = decodePreviewBorrowCapacityReturnData(previewData(evidence));
        const capacity = integer(preview.maxBorrowAmount);
        harness.assertTrue(`capacity is monotonic at ${collateral} base`, capacity >= previousCapacity, { previousCapacity, capacity });
        previousCapacity = capacity;
      }
    },
  },
  {
    id: "lending.dynamic-ltv-boundary",
    async run(harness) {
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/deposit-collateral",
        label: "deposit collateral at dynamic LTV boundary",
        body: { positionId: boundaryPositionId.toBase58(), marketAsset: "base", depositAmount: "100" },
      });
      const capacityEvidence = await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/preview-borrow-capacity",
        label: "preview exact dynamic LTV boundary",
        submit: false,
        body: { collateralAsset: "base", collateralAmount: "100", withReferral: false },
      });
      const capacity = decodePreviewBorrowCapacityReturnData(previewData(capacityEvidence));
      const maxBorrow = integer(capacity.maxBorrowAmount);
      harness.assertTrue("dynamic LTV preview returns a usable maximum", maxBorrow > 0n, maxBorrow);
      harness.observe("dynamic LTV boundary", {
        maxBorrow,
        maxCfBps: capacity.maxCfBps,
        liquidationCfBps: capacity.liquidationCfBps,
        maxDebtByHealth: capacity.maxDebtByHealth,
        maxDebtByCash: capacity.maxDebtByCash,
        maxDebtByDailyLimit: capacity.maxDebtByDailyLimit,
      });

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/borrow",
        label: "reject one raw unit above dynamic LTV capacity",
        expected: "failure",
        body: {
          positionId: boundaryPositionId.toBase58(),
          borrowAsset: "quote",
          borrowAmount: formatUnits(maxBorrow + 1n, harness.config.quoteDecimals),
          minDebtAmountOut: formatUnits(maxBorrow + 1n, harness.config.quoteDecimals),
          minLiquidationCfBps: 0,
        },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/borrow",
        label: "borrow exact dynamic LTV capacity",
        body: {
          positionId: boundaryPositionId.toBase58(),
          borrowAsset: "quote",
          borrowAmount: formatUnits(maxBorrow, harness.config.quoteDecimals),
          minDebtAmountOut: formatUnits(maxBorrow, harness.config.quoteDecimals),
          minLiquidationCfBps: 0,
        },
      });
      const positionEvidence = await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/preview-borrow-position",
        label: "preview debt at exact dynamic LTV capacity",
        submit: false,
        body: { positionId: boundaryPositionId.toBase58() },
      });
      const position = decodePreviewBorrowPositionReturnData(previewData(positionEvidence));
      harness.assertEqual("executed debt equals previewed boundary", integer(position.fixedQuoteDebt), maxBorrow);
      harness.assertEqual("stored liquidation CF equals underwriting preview", Number(position.quoteLiquidationCfBps), Number(capacity.liquidationCfBps));
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/repay",
        label: "repay dynamic LTV boundary debt",
        body: { positionId: boundaryPositionId.toBase58(), repayAsset: "quote", repayAmount: formatUnits(integer(position.fixedQuoteDebt), harness.config.quoteDecimals) },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/withdraw-collateral",
        label: "withdraw dynamic LTV boundary collateral",
        body: { positionId: boundaryPositionId.toBase58(), marketAsset: "base", withdrawAmount: "100", minAssetAmountOut: "100", minLiquidationCfBps: 0 },
      });
    },
  },
  {
    id: "invariant.post-baseline-solvency",
    async run(harness) {
      const market = await harness.market();
      harness.observe("final market state", market.state);
      harness.assertTrue("base reserve remains positive", stateValue(market, "baseReserve") > 0n);
      harness.assertTrue("quote reserve remains positive", stateValue(market, "quoteReserve") > 0n);
      harness.assertEqual("user base debt returns to zero", stateValue(market, "fixedBaseDebt"), 0n);
      harness.assertEqual("user quote debt returns to zero", stateValue(market, "fixedQuoteDebt"), 0n);
      harness.assertTrue("base yLP supply does not exceed reserve by an impossible amount", stateValue(market, "baseReserveYlpSupply") > 0n);
      harness.assertTrue("quote yLP supply does not exceed reserve by an impossible amount", stateValue(market, "quoteReserveYlpSupply") > 0n);
    },
  },
];
