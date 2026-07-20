import { getAssociatedTokenAddressSync } from "@solana/spl-token";
import { Keypair, PublicKey } from "@solana/web3.js";

import { decodePreviewBorrowPositionReturnData } from "../../../packages/dusk-sdk/src/preview.js";
import type { TransactionEvidence } from "../types.js";

import { formatUnits, type ProtocolTestHarness, type ScenarioDefinition } from "../harness.js";

const feeBoundaryPosition = Keypair.generate().publicKey;
const repeatedBorrowPosition = Keypair.generate().publicKey;
const referredLeveragePosition = Keypair.generate().publicKey;
const selfReferralPosition = Keypair.generate().publicKey;
const secondReferrerPosition = Keypair.generate().publicKey;

function raw(uiAmount: number, decimals: number): bigint {
  return BigInt(uiAmount) * 10n ** BigInt(decimals);
}

function referralFee(principal: bigint, bps: number): bigint {
  return (principal * BigInt(bps) + 9_999n) / 10_000n;
}

function previewData(evidence: TransactionEvidence): [string, BufferEncoding] {
  const data = evidence.simulation.returnData?.data;
  if (!data) throw new Error(`${evidence.label} did not return preview data`);
  return data as [string, BufferEncoding];
}

function integer(value: { toString(): string } | bigint | number): bigint {
  return BigInt(value.toString());
}

function profileAndVault(
  harness: ProtocolTestHarness,
  referrer: PublicKey,
  mint: string,
  tokenProgram: string
) {
  const [profile] = PublicKey.findProgramAddressSync(
    [Buffer.from("referral_profile"), referrer.toBuffer()],
    new PublicKey(harness.config.programId)
  );
  const vault = getAssociatedTokenAddressSync(
    new PublicKey(mint),
    profile,
    true,
    new PublicKey(tokenProgram)
  );
  return { profile, vault };
}

async function positionDebt(
  harness: ProtocolTestHarness,
  wallet: string,
  positionId: PublicKey,
  debtAsset: "base" | "quote",
  label: string
): Promise<bigint> {
  const evidence = await harness.execute({
    wallet,
    endpoint: "/api/v2/fork/tx/preview-borrow-position",
    label,
    submit: false,
    body: { positionId: positionId.toBase58() },
  });
  const preview = decodePreviewBorrowPositionReturnData(previewData(evidence));
  return debtAsset === "base" ? integer(preview.fixedBaseDebt) : integer(preview.fixedQuoteDebt);
}

async function leveragePosition(
  harness: ProtocolTestHarness,
  wallet: string,
  positionId: PublicKey
): Promise<any> {
  const positions = await harness.positions(wallet, positionId);
  const position = positions.find((entry) => entry.eventType === "leverage_position");
  if (!position) throw new Error(`Leverage position ${positionId.toBase58()} was not found`);
  return position.payload;
}

export const REFERRAL_SCENARIOS: ScenarioDefinition[] = [
  {
    id: "referral.fee-config-boundaries",
    async run(harness) {
      const governance = await harness.futarchy();
      const alice = harness.wallet("alice").publicKey.toBase58();
      const referrer = harness.wallet("referrer").publicKey;
      harness.assertEqual("referral boundary runs under Alice governance", governance.authority, alice);
      const { vault } = profileAndVault(
        harness,
        referrer,
        harness.config.quoteMint,
        harness.config.quoteTokenProgram
      );
      await harness.execute({
        wallet: "referrer",
        endpoint: "/api/v2/fork/tx/set-referral-recipient",
        label: "set referrer as boundary-test recipient",
        body: { recipient: referrer.toBase58() },
      });
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/deposit-collateral",
        label: "deposit collateral for referral fee boundaries",
        body: { positionId: feeBoundaryPosition.toBase58(), marketAsset: "base", depositAmount: "100" },
      });

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-protocol-revenue",
        label: "set zero referral origination fee",
        body: { referralOriginationFeeBps: 0 },
      });
      const zeroBefore = await harness.tokenAccountBalance(vault, harness.config.quoteTokenProgram);
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/borrow",
        label: "execute referred borrow at zero bps",
        body: {
          positionId: feeBoundaryPosition.toBase58(),
          borrowAsset: "quote",
          borrowAmount: "1",
          minDebtAmountOut: "1",
          minLiquidationCfBps: 0,
          referrer: referrer.toBase58(),
          maxAcceptableReferralFeeBps: 0,
        },
      });
      harness.assertEqual("zero-bps referral creates no vault credit", await harness.tokenAccountBalance(vault, harness.config.quoteTokenProgram), zeroBefore);
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/repay",
        label: "repay zero-bps referred borrow",
        body: { positionId: feeBoundaryPosition.toBase58(), repayAsset: "quote", repayAmount: "1" },
      });

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-protocol-revenue",
        label: "set standard 10 bps referral fee",
        body: { referralOriginationFeeBps: 10 },
      });
      const rejected = await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/borrow",
        label: "reject 10 bps fee above client 9 bps cap",
        expected: "failure",
        body: {
          positionId: feeBoundaryPosition.toBase58(),
          borrowAsset: "quote",
          borrowAmount: "1",
          minDebtAmountOut: "1",
          minLiquidationCfBps: 0,
          referrer: referrer.toBase58(),
          maxAcceptableReferralFeeBps: 9,
        },
      });
      harness.assertEqual("client referral cap returns deterministic slippage", rejected.errorCode, "ReferralFeeSlippageExceeded");

      const stale = await harness.buildSignedTransaction("bob", "/api/v2/fork/tx/borrow", {
        positionId: feeBoundaryPosition.toBase58(),
        borrowAsset: "quote",
        borrowAmount: "1",
        minDebtAmountOut: "1",
        minLiquidationCfBps: 0,
        referrer: referrer.toBase58(),
        maxAcceptableReferralFeeBps: 10,
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-protocol-revenue",
        label: "raise referral fee to 25 bps after client signs",
        body: { referralOriginationFeeBps: 25 },
      });
      const staleResult = await harness.simulateBuiltTransaction(stale);
      harness.assertEqual("stale 10-bps transaction is rejected after governance change", staleResult.succeeds, false);
      harness.assertEqual("stale transaction reports referral slippage", staleResult.errorCode, "ReferralFeeSlippageExceeded");

      const before25 = await harness.tokenAccountBalance(vault, harness.config.quoteTokenProgram);
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/borrow",
        label: "execute referred borrow at hard-cap 25 bps",
        body: {
          positionId: feeBoundaryPosition.toBase58(),
          borrowAsset: "quote",
          borrowAmount: "1",
          minDebtAmountOut: "1",
          minLiquidationCfBps: 0,
          referrer: referrer.toBase58(),
          maxAcceptableReferralFeeBps: 25,
        },
      });
      const expected25 = referralFee(raw(1, harness.config.quoteDecimals), 25);
      harness.assertEqual("25-bps vault credit uses ceiling fee", await harness.tokenAccountBalance(vault, harness.config.quoteTokenProgram) - before25, expected25);
      const gross25 = await positionDebt(harness, "bob", feeBoundaryPosition, "quote", "preview gross 25-bps boundary debt");
      harness.assertEqual("25-bps gross debt includes fee", gross25, raw(1, harness.config.quoteDecimals) + expected25);

      const invalid26 = await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-protocol-revenue",
        label: "reject referral fee above compile-time hard cap",
        expected: "failure",
        body: { referralOriginationFeeBps: 26 },
      });
      harness.assertEqual("26 bps is rejected by hard cap", invalid26.errorCode, "InvalidReferralFeeBps");
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/repay",
        label: "repay gross 25-bps boundary debt",
        body: {
          positionId: feeBoundaryPosition.toBase58(),
          repayAsset: "quote",
          repayAmount: formatUnits(gross25, harness.config.quoteDecimals),
        },
      });
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/withdraw-collateral",
        label: "withdraw referral boundary collateral",
        body: {
          positionId: feeBoundaryPosition.toBase58(),
          marketAsset: "base",
          withdrawAmount: "100",
          minAssetAmountOut: "100",
          minLiquidationCfBps: 0,
        },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-protocol-revenue",
        label: "restore standard 10 bps referral fee",
        body: { referralOriginationFeeBps: governance.referralOriginationFeeBps },
      });
      harness.assertEqual("referral fee configuration restores", (await harness.futarchy()).referralOriginationFeeBps, governance.referralOriginationFeeBps);
    },
  },
  {
    id: "referral.borrow-and-leverage",
    async run(harness) {
      const referrer = harness.wallet("referrer").publicKey;
      const { vault } = profileAndVault(harness, referrer, harness.config.quoteMint, harness.config.quoteTokenProgram);
      const feeBps = Number((await harness.futarchy()).referralOriginationFeeBps);
      const vaultBefore = await harness.tokenAccountBalance(vault, harness.config.quoteTokenProgram);
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/deposit-collateral",
        label: "deposit collateral for repeated referred draws",
        body: { positionId: repeatedBorrowPosition.toBase58(), marketAsset: "base", depositAmount: "100" },
      });
      for (const draw of [2, 3]) {
        await harness.execute({
          wallet: "alice",
          endpoint: "/api/v2/fork/tx/borrow",
          label: `charge referral fee on ${draw}-token draw`,
          body: {
            positionId: repeatedBorrowPosition.toBase58(),
            borrowAsset: "quote",
            borrowAmount: String(draw),
            minDebtAmountOut: String(draw),
            minLiquidationCfBps: 0,
            referrer: referrer.toBase58(),
            maxAcceptableReferralFeeBps: feeBps,
          },
        });
      }
      const drawFees = referralFee(raw(2, harness.config.quoteDecimals), feeBps)
        + referralFee(raw(3, harness.config.quoteDecimals), feeBps);
      harness.assertEqual("every borrow increase credits a fee", await harness.tokenAccountBalance(vault, harness.config.quoteTokenProgram) - vaultBefore, drawFees);
      const borrowDebt = await positionDebt(harness, "alice", repeatedBorrowPosition, "quote", "preview repeated referred debt");
      const vaultBeforeRepay = await harness.tokenAccountBalance(vault, harness.config.quoteTokenProgram);
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/repay",
        label: "repay without referral charge",
        body: { positionId: repeatedBorrowPosition.toBase58(), repayAsset: "quote", repayAmount: formatUnits(borrowDebt, harness.config.quoteDecimals) },
      });
      harness.assertEqual("repay does not credit referral vault", await harness.tokenAccountBalance(vault, harness.config.quoteTokenProgram), vaultBeforeRepay);
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/withdraw-collateral",
        label: "withdraw repeated-draw collateral",
        body: { positionId: repeatedBorrowPosition.toBase58(), marketAsset: "base", withdrawAmount: "100", minAssetAmountOut: "100", minLiquidationCfBps: 0 },
      });

      const beforeOpen = await harness.tokenAccountBalance(vault, harness.config.quoteTokenProgram);
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/open-leverage",
        label: "charge referral fee on leverage open",
        body: {
          positionId: referredLeveragePosition.toBase58(),
          debtAsset: "quote",
          marginAmount: "10",
          multiplierBps: 20_000,
          minCollateralOut: "0",
          referrer: referrer.toBase58(),
          maxAcceptableReferralFeeBps: feeBps,
        },
      });
      let leverage = await leveragePosition(harness, "alice", referredLeveragePosition);
      const openRequestedPrincipal = raw(10, harness.config.quoteDecimals);
      const openFee = referralFee(openRequestedPrincipal, feeBps);
      harness.assertEqual("leverage open stores gross principal", BigInt(leverage.debtPrincipal), openRequestedPrincipal + openFee);
      harness.assertEqual("leverage open immediately credits referrer", await harness.tokenAccountBalance(vault, harness.config.quoteTokenProgram) - beforeOpen, openFee);

      const beforeIncrease = await harness.tokenAccountBalance(vault, harness.config.quoteTokenProgram);
      const principalBeforeIncrease = BigInt(leverage.debtPrincipal);
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/increase-leverage",
        label: "charge referral fee on leverage increase",
        body: {
          positionId: referredLeveragePosition.toBase58(),
          debtAsset: "quote",
          debtAmount: "2",
          minCollateralOut: "0",
          referrer: referrer.toBase58(),
          maxAcceptableReferralFeeBps: feeBps,
        },
      });
      leverage = await leveragePosition(harness, "alice", referredLeveragePosition);
      const increaseFee = referralFee(raw(2, harness.config.quoteDecimals), feeBps);
      harness.assertEqual("leverage increase stores requested principal plus fee", BigInt(leverage.debtPrincipal) - principalBeforeIncrease, raw(2, harness.config.quoteDecimals) + increaseFee);
      harness.assertEqual("leverage increase immediately credits referrer", await harness.tokenAccountBalance(vault, harness.config.quoteTokenProgram) - beforeIncrease, increaseFee);

      const beforeClose = await harness.tokenAccountBalance(vault, harness.config.quoteTokenProgram);
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/close-leverage",
        label: "close leverage without referral charge",
        body: { positionId: referredLeveragePosition.toBase58(), debtAsset: "quote", minAmountOut: "0" },
      });
      harness.assertEqual("leverage close does not credit referral vault", await harness.tokenAccountBalance(vault, harness.config.quoteTokenProgram), beforeClose);

      const beforeHlp = await harness.tokenAccountBalance(vault, harness.config.quoteTokenProgram);
      const hlpBefore = await harness.lpBalance("trader", harness.config.baseHlpMint);
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/deposit-single-sided",
        label: "open internal hLP funding without referral",
        body: { targetAsset: "base", depositAmount: "5", minHlpAmount: "0" },
      });
      const hlpMinted = await harness.lpBalance("trader", harness.config.baseHlpMint) - hlpBefore;
      harness.assertEqual("internal hLP debt never credits referral vault", await harness.tokenAccountBalance(vault, harness.config.quoteTokenProgram), beforeHlp);
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/withdraw-single-sided",
        label: "close internal hLP funding without referral",
        body: { targetAsset: "base", hlpAmount: formatUnits(hlpMinted, harness.config.baseDecimals), minTargetAmountOut: "0" },
      });
    },
  },
  {
    id: "referral.multi-wallet-and-self",
    async run(harness) {
      const feeBps = Number((await harness.futarchy()).referralOriginationFeeBps);
      const alice = harness.wallet("alice").publicKey;
      const bob = harness.wallet("bob").publicKey;
      const referrer = harness.wallet("referrer").publicKey;
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/set-referral-recipient",
        label: "create Alice self-referral profile",
        body: { recipient: alice.toBase58() },
      });
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/set-referral-recipient",
        label: "create independent Bob referral profile",
        body: { recipient: bob.toBase58() },
      });
      const aliceBaseVault = profileAndVault(harness, alice, harness.config.baseMint, harness.config.baseTokenProgram).vault;
      const referrerQuoteVault = profileAndVault(harness, referrer, harness.config.quoteMint, harness.config.quoteTokenProgram).vault;

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/deposit-collateral",
        label: "deposit quote collateral for self-referred base debt",
        body: { positionId: selfReferralPosition.toBase58(), marketAsset: "quote", depositAmount: "100" },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/borrow",
        label: "execute Alice self-referred base borrow",
        body: {
          positionId: selfReferralPosition.toBase58(),
          borrowAsset: "base",
          borrowAmount: "2",
          minDebtAmountOut: "2",
          minLiquidationCfBps: 0,
          referrer: alice.toBase58(),
          maxAcceptableReferralFeeBps: feeBps,
        },
      });
      const selfFee = referralFee(raw(2, harness.config.baseDecimals), feeBps);
      harness.assertEqual("self-referral accrues in Alice base vault", await harness.tokenAccountBalance(aliceBaseVault, harness.config.baseTokenProgram), selfFee);

      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/deposit-collateral",
        label: "deposit base collateral for independent quote referral",
        body: { positionId: secondReferrerPosition.toBase58(), marketAsset: "base", depositAmount: "100" },
      });
      const quoteVaultBefore = await harness.tokenAccountBalance(referrerQuoteVault, harness.config.quoteTokenProgram);
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/borrow",
        label: "execute independent referrer quote borrow",
        body: {
          positionId: secondReferrerPosition.toBase58(),
          borrowAsset: "quote",
          borrowAmount: "3",
          minDebtAmountOut: "3",
          minLiquidationCfBps: 0,
          referrer: referrer.toBase58(),
          maxAcceptableReferralFeeBps: feeBps,
        },
      });
      const independentFee = referralFee(raw(3, harness.config.quoteDecimals), feeBps);
      harness.assertEqual("independent quote referral remains isolated by mint and profile", await harness.tokenAccountBalance(referrerQuoteVault, harness.config.quoteTokenProgram) - quoteVaultBefore, independentFee);

      const aliceBaseBefore = await harness.tokenBalance("alice", harness.config.baseMint, harness.config.baseTokenProgram);
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/claim-referral-fees",
        label: "claim Alice self-referral base fees",
        body: { asset: "base", recipient: alice.toBase58() },
      });
      harness.assertEqual("Alice receives exact self-referral fee", await harness.tokenBalance("alice", harness.config.baseMint, harness.config.baseTokenProgram) - aliceBaseBefore, selfFee);

      const referrerQuoteBefore = await harness.tokenBalance("referrer", harness.config.quoteMint, harness.config.quoteTokenProgram);
      const referrerVaultBalance = await harness.tokenAccountBalance(referrerQuoteVault, harness.config.quoteTokenProgram);
      await harness.execute({
        wallet: "referrer",
        endpoint: "/api/v2/fork/tx/claim-referral-fees",
        label: "claim aggregate referrer quote fees",
        body: { asset: "quote", recipient: referrer.toBase58() },
      });
      harness.assertEqual("referrer receives its entire per-mint aggregate vault", await harness.tokenBalance("referrer", harness.config.quoteMint, harness.config.quoteTokenProgram) - referrerQuoteBefore, referrerVaultBalance);

      const selfDebt = await positionDebt(harness, "alice", selfReferralPosition, "base", "preview self-referred gross debt");
      const secondDebt = await positionDebt(harness, "bob", secondReferrerPosition, "quote", "preview independent gross debt");
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/repay",
        label: "repay self-referred gross base debt",
        body: { positionId: selfReferralPosition.toBase58(), repayAsset: "base", repayAmount: formatUnits(selfDebt, harness.config.baseDecimals) },
      });
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/repay",
        label: "repay independent gross quote debt",
        body: { positionId: secondReferrerPosition.toBase58(), repayAsset: "quote", repayAmount: formatUnits(secondDebt, harness.config.quoteDecimals) },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/withdraw-collateral",
        label: "withdraw self-referral quote collateral",
        body: { positionId: selfReferralPosition.toBase58(), marketAsset: "quote", withdrawAmount: "100", minAssetAmountOut: "100", minLiquidationCfBps: 0 },
      });
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/withdraw-collateral",
        label: "withdraw independent referral base collateral",
        body: { positionId: secondReferrerPosition.toBase58(), marketAsset: "base", withdrawAmount: "100", minAssetAmountOut: "100", minLiquidationCfBps: 0 },
      });
    },
  },
];
