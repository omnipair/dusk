import {
  getAssociatedTokenAddressSync,
  TOKEN_2022_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
} from "@solana/spl-token";
import { Keypair, PublicKey } from "@solana/web3.js";

import {
  decodePreviewAddLiquidityReturnData,
  decodePreviewBorrowPositionReturnData,
  decodePreviewSwapReturnData,
} from "../../../packages/dusk-sdk/src/preview.js";
import { formatUnits, type ProtocolTestHarness, type ScenarioDefinition } from "../harness.js";
import type { TransactionEvidence } from "../types.js";

const token2022BorrowPositionId = Keypair.generate().publicKey;
const token2022LeveragePositionId = Keypair.generate().publicKey;
const mixedBorrowPositionId = Keypair.generate().publicKey;
const mixedLeveragePositionId = Keypair.generate().publicKey;

function raw(uiAmount: number, decimals: number): bigint {
  return BigInt(uiAmount) * 10n ** BigInt(decimals);
}

function integer(value: { toString(): string } | bigint | number): bigint {
  return BigInt(value.toString());
}

function previewData(evidence: TransactionEvidence): [string, BufferEncoding] {
  const data = evidence.simulation.returnData?.data;
  if (!data) throw new Error(`${evidence.label} did not return preview data`);
  return data as [string, BufferEncoding];
}

async function previewBorrowPosition(
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

async function maximumRepayDebit(
  harness: ProtocolTestHarness,
  positionId: PublicKey,
  debtAsset: "base" | "quote",
  debt: bigint,
  decimals: number
): Promise<bigint> {
  const body = (amount: bigint) => ({
    positionId: positionId.toBase58(),
    repayAsset: debtAsset,
    repayAmount: formatUnits(amount, decimals),
  });
  let low = 0n;
  let high = debt * 2n + 1n;
  while (low + 1n < high) {
    const middle = (low + high) / 2n;
    if ((await harness.probe("alice", "/api/v2/fork/tx/repay", body(middle))).succeeds) {
      low = middle;
    } else {
      high = middle;
    }
  }
  return low;
}

export const COMPATIBILITY_SCENARIOS: ScenarioDefinition[] = [
  {
    id: "security.token-2022-assets",
    fixtureModes: ["token2022-fees"],
    async run(harness) {
      harness.assertEqual("base pool asset uses Token-2022", harness.config.baseTokenProgram, TOKEN_2022_PROGRAM_ID.toBase58());
      harness.assertEqual("quote pool asset uses Token-2022", harness.config.quoteTokenProgram, TOKEN_2022_PROGRAM_ID.toBase58());

      const liquidityPreviewEvidence = await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/preview-add-liquidity",
        label: "preview Token-2022 transfer-fee liquidity",
        submit: false,
        body: { baseDepositAmount: "10", quoteDepositAmount: "10" },
      });
      const liquidityPreview = decodePreviewAddLiquidityReturnData(previewData(liquidityPreviewEvidence));
      harness.assertTrue("base liquidity transfer fee is detected", integer(liquidityPreview.baseTransferFee) > 0n, liquidityPreview.baseTransferFee);
      harness.assertTrue("quote liquidity transfer fee is detected", integer(liquidityPreview.quoteTransferFee) > 0n, liquidityPreview.quoteTransferFee);
      const ylpBefore = await harness.lpBalance("trader", harness.config.ylpMint);
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/add-liquidity",
        label: "add liquidity with Token-2022 pool assets",
        body: { baseDepositAmount: "10", quoteDepositAmount: "10", minYlpAmount: "0" },
      });
      const minted = await harness.lpBalance("trader", harness.config.ylpMint) - ylpBefore;
      harness.assertEqual("Token-2022 liquidity mint matches preview", minted, integer(liquidityPreview.ylpAmount));
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/remove-liquidity",
        label: "remove Token-2022 pool-asset liquidity",
        body: {
          ylpAmount: formatUnits(minted, harness.config.baseDecimals),
          minBaseAmountOut: "0",
          minQuoteAmountOut: "0",
        },
      });

      const swapPreviewEvidence = await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/preview-swap",
        label: "preview Token-2022 transfer-fee swap",
        submit: false,
        body: { assetIn: "base", exactAssetIn: "10" },
      });
      const swapPreview = decodePreviewSwapReturnData(previewData(swapPreviewEvidence));
      harness.assertTrue("swap preview detects input transfer fee", integer(swapPreview.transferFee) > 0n, swapPreview.transferFee);
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "execute Token-2022 transfer-fee swap",
        body: { assetIn: "base", exactAssetIn: "10", minAssetOut: "0" },
      });

      const referrer = harness.wallet("referrer").publicKey;
      await harness.execute({
        wallet: "referrer",
        endpoint: "/api/v2/fork/tx/set-referral-recipient",
        label: "create referral profile for Token-2022 fee asset",
        body: { recipient: referrer.toBase58() },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/deposit-collateral",
        label: "deposit Token-2022 transfer-fee collateral",
        body: {
          positionId: token2022BorrowPositionId.toBase58(),
          marketAsset: "base",
          depositAmount: "100",
        },
      });
      const borrowPosition = (await harness.positions("alice", token2022BorrowPositionId)).find(
        (entry) => entry.eventType === "borrow_position"
      )?.payload;
      const creditedCollateral = BigInt(borrowPosition?.baseCollateral ?? 0);
      harness.assertTrue("collateral credit is positive after transfer fee", creditedCollateral > 0n, creditedCollateral);
      harness.assertTrue(
        "collateral accounting excludes withheld transfer fee",
        creditedCollateral < raw(100, harness.config.baseDecimals),
        creditedCollateral
      );
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/borrow",
        label: "execute referred borrow with Token-2022 debt asset",
        body: {
          positionId: token2022BorrowPositionId.toBase58(),
          borrowAsset: "quote",
          borrowAmount: "10",
          minDebtAmountOut: "0",
          minLiquidationCfBps: 0,
          referrer: referrer.toBase58(),
          maxAcceptableReferralFeeBps: 25,
        },
      });
      const duskProgram = new PublicKey(harness.config.programId);
      const [referralProfile] = PublicKey.findProgramAddressSync(
        [Buffer.from("referral_profile"), referrer.toBuffer()],
        duskProgram
      );
      const referralVault = getAssociatedTokenAddressSync(
        new PublicKey(harness.config.quoteMint),
        referralProfile,
        true,
        TOKEN_2022_PROGRAM_ID
      );
      harness.assertTrue(
        "Token-2022 referral vault receives net fee credit",
        await harness.tokenAccountBalance(referralVault, harness.config.quoteTokenProgram) > 0n
      );
      const referrerQuoteBefore = await harness.tokenBalance(
        "referrer",
        harness.config.quoteMint,
        harness.config.quoteTokenProgram
      );
      await harness.execute({
        wallet: "referrer",
        endpoint: "/api/v2/fork/tx/claim-referral-fees",
        label: "claim Token-2022 referral fees",
        body: { asset: "quote", recipient: referrer.toBase58() },
      });
      harness.assertEqual(
        "Token-2022 referral claim drains the vault",
        await harness.tokenAccountBalance(referralVault, harness.config.quoteTokenProgram),
        0n
      );
      harness.assertTrue(
        "Token-2022 referral recipient receives net claim",
        await harness.tokenBalance("referrer", harness.config.quoteMint, harness.config.quoteTokenProgram) > referrerQuoteBefore
      );

      const debtBeforeRepay = BigInt(
        (
          await previewBorrowPosition(
            harness,
            "alice",
            token2022BorrowPositionId,
            "preview Token-2022 debt for exact gross repayment"
          )
        ).fixedQuoteDebt.toString()
      );
      const repayDebit = await maximumRepayDebit(
        harness,
        token2022BorrowPositionId,
        "quote",
        debtBeforeRepay,
        harness.config.quoteDecimals
      );
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/repay",
        label: "repay Token-2022 debt with transfer-fee gross-up",
        body: {
          positionId: token2022BorrowPositionId.toBase58(),
          repayAsset: "quote",
          repayAmount: formatUnits(repayDebit, harness.config.quoteDecimals),
        },
      });
      const debtAfterRepay = await previewBorrowPosition(
        harness,
        "alice",
        token2022BorrowPositionId,
        "confirm Token-2022 debt is fully repaid"
      );
      harness.assertEqual("Token-2022 gross repayment clears net debt", integer(debtAfterRepay.fixedQuoteDebt), 0n);
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/withdraw-collateral",
        label: "withdraw Token-2022 collateral after repayment",
        body: {
          positionId: token2022BorrowPositionId.toBase58(),
          marketAsset: "base",
          withdrawAmount: formatUnits(creditedCollateral, harness.config.baseDecimals),
          minAssetAmountOut: "0",
          minLiquidationCfBps: 0,
        },
      });

      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/open-leverage",
        label: "open leverage with Token-2022 transfer-fee assets",
        body: {
          positionId: token2022LeveragePositionId.toBase58(),
          debtAsset: "quote",
          marginAmount: "5",
          multiplierBps: 20_000,
          minCollateralOut: "0",
        },
      });
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/close-leverage",
        label: "close leverage with Token-2022 transfer-fee assets",
        body: {
          positionId: token2022LeveragePositionId.toBase58(),
          debtAsset: "quote",
          minAmountOut: "0",
        },
      });

      const hlpBefore = await harness.lpBalance("trader", harness.config.baseHlpMint);
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/deposit-single-sided",
        label: "deposit base hLP with Token-2022 pool assets",
        body: { targetAsset: "base", depositAmount: "5", minHlpAmount: "0" },
      });
      const hlpMinted = await harness.lpBalance("trader", harness.config.baseHlpMint) - hlpBefore;
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/withdraw-single-sided",
        label: "withdraw base hLP with Token-2022 pool assets",
        body: {
          targetAsset: "base",
          hlpAmount: formatUnits(hlpMinted, harness.config.baseDecimals),
          minTargetAmountOut: "0",
        },
      });
    },
  },
  {
    id: "security.mixed-decimals",
    fixtureModes: ["mixed-decimals"],
    async run(harness) {
      harness.assertEqual(
        "mixed fixture exposes 0 and 9 decimals",
        [harness.config.baseDecimals, harness.config.quoteDecimals].sort((a, b) => a - b),
        [0, 9]
      );
      harness.assertEqual("mixed base asset uses legacy SPL", harness.config.baseTokenProgram, TOKEN_PROGRAM_ID.toBase58());
      harness.assertEqual("mixed quote asset uses legacy SPL", harness.config.quoteTokenProgram, TOKEN_PROGRAM_ID.toBase58());

      const ylpBefore = await harness.lpBalance("trader", harness.config.ylpMint);
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/add-liquidity",
        label: "add liquidity across 0 and 9 decimal assets",
        body: { baseDepositAmount: "10", quoteDepositAmount: "10", minYlpAmount: "0" },
      });
      const minted = await harness.lpBalance("trader", harness.config.ylpMint) - ylpBefore;
      harness.assertTrue("mixed-decimal liquidity mints yLP", minted > 0n, minted);
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/remove-liquidity",
        label: "remove mixed-decimal liquidity",
        body: {
          ylpAmount: formatUnits(minted, harness.config.baseDecimals),
          minBaseAmountOut: "0",
          minQuoteAmountOut: "0",
        },
      });

      const zeroAsset = harness.config.baseDecimals === 0 ? "base" : "quote";
      const nineAsset = zeroAsset === "base" ? "quote" : "base";
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "swap 0-decimal asset without rounding to zero",
        body: { assetIn: zeroAsset, exactAssetIn: "100", minAssetOut: "0" },
      });
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "swap 9-decimal asset in reverse direction",
        body: { assetIn: nineAsset, exactAssetIn: "1", minAssetOut: "0" },
      });

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/deposit-collateral",
        label: "deposit mixed-decimal base collateral",
        body: {
          positionId: mixedBorrowPositionId.toBase58(),
          marketAsset: "base",
          depositAmount: "500",
        },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/borrow",
        label: "borrow mixed-decimal quote debt",
        body: {
          positionId: mixedBorrowPositionId.toBase58(),
          borrowAsset: "quote",
          borrowAmount: "10",
          minDebtAmountOut: "10",
          minLiquidationCfBps: 0,
        },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/repay",
        label: "repay mixed-decimal quote debt",
        body: { positionId: mixedBorrowPositionId.toBase58(), repayAsset: "quote", repayAmount: "10" },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/withdraw-collateral",
        label: "withdraw mixed-decimal base collateral",
        body: {
          positionId: mixedBorrowPositionId.toBase58(),
          marketAsset: "base",
          withdrawAmount: "500",
          minAssetAmountOut: "0",
          minLiquidationCfBps: 0,
        },
      });

      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/open-leverage",
        label: "open mixed-decimal leverage",
        body: {
          positionId: mixedLeveragePositionId.toBase58(),
          debtAsset: "quote",
          marginAmount: "100",
          multiplierBps: 20_000,
          minCollateralOut: "0",
        },
      });
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/close-leverage",
        label: "close mixed-decimal leverage",
        body: { positionId: mixedLeveragePositionId.toBase58(), debtAsset: "quote", minAmountOut: "0" },
      });

      const hlpBefore = await harness.lpBalance("trader", harness.config.baseHlpMint);
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/deposit-single-sided",
        label: "deposit mixed-decimal hLP",
        body: { targetAsset: "base", depositAmount: "10", minHlpAmount: "0" },
      });
      const hlpMinted = await harness.lpBalance("trader", harness.config.baseHlpMint) - hlpBefore;
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/withdraw-single-sided",
        label: "withdraw mixed-decimal hLP",
        body: {
          targetAsset: "base",
          hlpAmount: formatUnits(hlpMinted, harness.config.baseDecimals),
          minTargetAmountOut: "0",
        },
      });
    },
  },
];
