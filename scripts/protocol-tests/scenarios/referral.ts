import { Keypair, PublicKey } from "@solana/web3.js";

import { decodePreviewBorrowPositionReturnData } from "../../../packages/dusk-sdk/src/preview.js";
import type { TransactionEvidence } from "../types.js";

import { formatUnits, type ProtocolTestHarness, type ScenarioDefinition } from "../harness.js";

const profileClaimPosition = Keypair.generate().publicKey;
const cappedSharePosition = Keypair.generate().publicKey;
const zeroCapPosition = Keypair.generate().publicKey;
const repeatedBorrowPosition = Keypair.generate().publicKey;
const referredLeveragePosition = Keypair.generate().publicKey;
const unlistedPosition = Keypair.generate().publicKey;
const selfReferralPosition = Keypair.generate().publicKey;
const independentReferralPosition = Keypair.generate().publicKey;

function raw(uiAmount: number, decimals: number): bigint {
  return BigInt(uiAmount) * 10n ** BigInt(decimals);
}

function previewData(evidence: TransactionEvidence): [string, BufferEncoding] {
  const data = evidence.simulation.returnData?.data;
  if (!data) throw new Error(`${evidence.label} did not return preview data`);
  return data as [string, BufferEncoding];
}

function integer(value: { toString(): string } | bigint | number): bigint {
  return BigInt(value.toString());
}

function eventValue(event: Record<string, unknown>, camel: string, snake: string): bigint {
  const value = event[camel] ?? event[snake];
  if (value == null) throw new Error(`Referral event is missing ${camel}`);
  return BigInt(String(value));
}

async function configureReferral(
  harness: ProtocolTestHarness,
  referrer: PublicKey,
  interestShareBps: number,
  active = true
) {
  return harness.execute({
    wallet: "alice",
    endpoint: "/api/v2/fork/tx/configure-referral",
    label: `${active ? "list" : "deactivate"} referral at ${interestShareBps} bps`,
    body: {
      referrer: referrer.toBase58(),
      interestShareBps,
      active,
    },
  });
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

async function repayAll(
  harness: ProtocolTestHarness,
  wallet: string,
  positionId: PublicKey,
  debtAsset: "base" | "quote",
  label: string
) {
  const decimals = debtAsset === "base"
    ? harness.config.baseDecimals
    : harness.config.quoteDecimals;
  const debt = await positionDebt(harness, wallet, positionId, debtAsset, `${label} preview`);
  return harness.execute({
    wallet,
    endpoint: "/api/v2/fork/tx/repay",
    label,
    body: {
      positionId: positionId.toBase58(),
      repayAsset: debtAsset,
      repayAmount: formatUnits(debt, decimals),
    },
  });
}

export const REFERRAL_SCENARIOS: ScenarioDefinition[] = [
  {
    id: "referral.profile-and-claim",
    async run(harness) {
      const referrer = harness.wallet("referrer").publicKey;
      const recipient = harness.wallet("bob").publicKey;
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-protocol-revenue",
        label: "route all realized interest to protocol revenue for referral accounting",
        body: { interestBps: 10_000, maxReferralInterestShareBps: 5_000 },
      });
      await configureReferral(harness, referrer, 5_000);
      await harness.execute({
        wallet: "referrer",
        endpoint: "/api/v2/fork/tx/set-referral-recipient",
        label: "rotate referral recipient to Bob",
        body: { recipient: recipient.toBase58() },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/deposit-collateral",
        label: "deposit collateral for referred interest accrual",
        body: { positionId: profileClaimPosition.toBase58(), marketAsset: "base", depositAmount: "100" },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/borrow",
        label: "bind listed referrer to quote debt",
        body: {
          positionId: profileClaimPosition.toBase58(),
          borrowAsset: "quote",
          borrowAmount: "20",
          minDebtAmountOut: "20",
          minLiquidationCfBps: 0,
          referrer: referrer.toBase58(),
        },
      });
      harness.assertEqual(
        "referral adds no debt surcharge",
        await positionDebt(harness, "alice", profileClaimPosition, "quote", "preview exact referred debt"),
        raw(20, harness.config.quoteDecimals)
      );

      await harness.timeTravel(0, 2_160_000);
      const repayment = await repayAll(
        harness,
        "alice",
        profileClaimPosition,
        "quote",
        "realize referred quote interest"
      );
      const accruedEvents = harness.events(repayment, "ReferralInterestAccrued");
      harness.assertEqual("interest repayment emits one referral accrual", accruedEvents.length, 1);
      const accrued = eventValue(accruedEvents[0].data, "accruedAmount", "accrued_amount");
      harness.assertTrue("referral accrues a positive DAO-interest share", accrued > 0n, accrued);

      const recipientBefore = await harness.tokenBalance(
        "bob",
        harness.config.quoteMint,
        harness.config.quoteTokenProgram
      );
      const claim = await harness.execute({
        wallet: "referrer",
        endpoint: "/api/v2/fork/tx/claim-referral-interest",
        label: "claim accrued quote interest to stored recipient",
        body: { asset: "quote" },
      });
      harness.assertEqual(
        "stored recipient receives the exact accrued amount",
        await harness.tokenBalance("bob", harness.config.quoteMint, harness.config.quoteTokenProgram) - recipientBefore,
        accrued
      );
      harness.assertEqual(
        "claim emits one referral receipt",
        harness.events(claim, "ReferralInterestClaimed").length,
        1
      );
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/withdraw-collateral",
        label: "withdraw collateral after referred debt clears",
        body: {
          positionId: profileClaimPosition.toBase58(),
          marketAsset: "base",
          withdrawAmount: "100",
          minAssetAmountOut: "100",
          minLiquidationCfBps: 0,
        },
      });
    },
  },
  {
    id: "referral.fee-config-boundaries",
    async run(harness) {
      const governance = await harness.futarchy();
      const referrer = harness.wallet("referrer").publicKey;
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-protocol-revenue",
        label: "set 25 percent runtime referral share cap",
        body: { interestBps: 10_000, maxReferralInterestShareBps: 2_500 },
      });
      await configureReferral(harness, referrer, 7_500);

      const invalidCap = await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-protocol-revenue",
        label: "reject runtime referral share cap above 100 percent",
        expected: "failure",
        body: { maxReferralInterestShareBps: 10_001 },
      });
      harness.assertEqual(
        "invalid runtime cap reports referral share error",
        invalidCap.errorCode,
        "InvalidReferralInterestShareBps"
      );
      const invalidProfile = await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/configure-referral",
        label: "reject profile share above 100 percent",
        expected: "failure",
        body: {
          referrer: harness.wallet("bidder").publicKey.toBase58(),
          interestShareBps: 10_001,
          active: true,
        },
      });
      harness.assertEqual(
        "invalid profile share reports referral share error",
        invalidProfile.errorCode,
        "InvalidReferralInterestShareBps"
      );

      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/deposit-collateral",
        label: "deposit collateral for capped referral share",
        body: { positionId: cappedSharePosition.toBase58(), marketAsset: "base", depositAmount: "100" },
      });
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/borrow",
        label: "bind profile whose configured share exceeds runtime cap",
        body: {
          positionId: cappedSharePosition.toBase58(),
          borrowAsset: "quote",
          borrowAmount: "10",
          minDebtAmountOut: "10",
          minLiquidationCfBps: 0,
          referrer: referrer.toBase58(),
        },
      });
      await harness.timeTravel(0, 2_160_000);
      const cappedRepayment = await repayAll(
        harness,
        "bob",
        cappedSharePosition,
        "quote",
        "realize capped referral interest"
      );
      const cappedEvent = harness.events(cappedRepayment, "ReferralInterestAccrued")[0]?.data;
      harness.assertTrue("capped referral repayment emits accrual", Boolean(cappedEvent));
      harness.assertEqual(
        "binding snapshots the runtime-capped share",
        eventValue(cappedEvent, "interestShareBps", "interest_share_bps"),
        2_500n
      );

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-protocol-revenue",
        label: "set runtime referral share cap to zero",
        body: { maxReferralInterestShareBps: 0 },
      });
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/deposit-collateral",
        label: "deposit collateral for zero-cap referral",
        body: { positionId: zeroCapPosition.toBase58(), marketAsset: "base", depositAmount: "100" },
      });
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/borrow",
        label: "bind active referral while runtime cap is zero",
        body: {
          positionId: zeroCapPosition.toBase58(),
          borrowAsset: "quote",
          borrowAmount: "10",
          minDebtAmountOut: "10",
          minLiquidationCfBps: 0,
          referrer: referrer.toBase58(),
        },
      });
      await harness.timeTravel(0, 2_160_000);
      const zeroRepayment = await repayAll(
        harness,
        "bob",
        zeroCapPosition,
        "quote",
        "realize interest while referral cap is zero"
      );
      harness.assertEqual(
        "zero runtime cap emits no positive referral accrual",
        harness.events(zeroRepayment, "ReferralInterestAccrued").length,
        0
      );

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-protocol-revenue",
        label: "restore referral and interest governance settings",
        body: {
          interestBps: Number(governance.revenueShare.interestBps),
          maxReferralInterestShareBps: Number(governance.maxReferralInterestShareBps),
        },
      });
    },
  },
  {
    id: "referral.borrow-and-leverage",
    async run(harness) {
      const referrer = harness.wallet("referrer").publicKey;
      const secondReferrer = harness.wallet("bob").publicKey;
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-protocol-revenue",
        label: "enable protocol interest revenue for debt-binding tests",
        body: { interestBps: 10_000, maxReferralInterestShareBps: 5_000 },
      });
      await configureReferral(harness, referrer, 5_000);
      await configureReferral(harness, secondReferrer, 2_000);
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
          label: draw === 2
            ? "bind referral on the first borrow"
            : "borrow again using the stored referral binding",
          body: {
            positionId: repeatedBorrowPosition.toBase58(),
            borrowAsset: "quote",
            borrowAmount: String(draw),
            minDebtAmountOut: String(draw),
            minLiquidationCfBps: 0,
            ...(draw === 2 ? { referrer: referrer.toBase58() } : {}),
          },
        });
      }
      harness.assertEqual(
        "repeated referred draws store only requested principal",
        await positionDebt(harness, "alice", repeatedBorrowPosition, "quote", "preview repeated referred debt"),
        raw(5, harness.config.quoteDecimals)
      );
      const rebind = await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/borrow",
        label: "reject changing referrer while debt remains open",
        expected: "failure",
        body: {
          positionId: repeatedBorrowPosition.toBase58(),
          borrowAsset: "quote",
          borrowAmount: "1",
          minDebtAmountOut: "1",
          minLiquidationCfBps: 0,
          referrer: secondReferrer.toBase58(),
        },
      });
      harness.assertEqual("debt-side referral binding is immutable", rebind.errorCode, "InvalidReferralProfile");

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/open-leverage",
        label: "open leverage with a listed referrer",
        body: {
          positionId: referredLeveragePosition.toBase58(),
          debtAsset: "quote",
          marginAmount: "10",
          multiplierBps: 20_000,
          minCollateralOut: "0",
          referrer: referrer.toBase58(),
        },
      });
      let leverage = await leveragePosition(harness, "alice", referredLeveragePosition);
      harness.assertEqual(
        "referred leverage open stores exact requested principal",
        BigInt(leverage.debtPrincipal),
        raw(10, harness.config.quoteDecimals)
      );
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/increase-leverage",
        label: "increase leverage without changing its bound referral",
        body: {
          positionId: referredLeveragePosition.toBase58(),
          debtAsset: "quote",
          debtAmount: "2",
          minCollateralOut: "0",
        },
      });
      leverage = await leveragePosition(harness, "alice", referredLeveragePosition);
      harness.assertEqual(
        "leverage increase adds exact principal without a referral surcharge",
        BigInt(leverage.debtPrincipal),
        raw(12, harness.config.quoteDecimals)
      );

      await harness.timeTravel(0, 2_160_000);
      const marginRepayment = await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/add-leverage-margin",
        label: "realize interest on the bound leverage referral",
        body: {
          positionId: referredLeveragePosition.toBase58(),
          debtAsset: "quote",
          amount: "2",
        },
      });
      harness.assertEqual(
        "leverage interest repayment accrues to the bound referral",
        harness.events(marginRepayment, "ReferralInterestAccrued").length,
        1
      );
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/close-leverage",
        label: "close referred leverage with unchanged binding",
        body: { positionId: referredLeveragePosition.toBase58(), debtAsset: "quote", minAmountOut: "0" },
      });
      await repayAll(harness, "alice", repeatedBorrowPosition, "quote", "clear repeated referred debt");
    },
  },
  {
    id: "referral.multi-wallet-and-self",
    async run(harness) {
      const alice = harness.wallet("alice").publicKey;
      const independentReferrerWallet = "liquidator";
      const referrer = harness.wallet(independentReferrerWallet).publicKey;
      const unlisted = harness.wallet("bidder").publicKey;
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-protocol-revenue",
        label: "enable interest sharing for independent referrers",
        body: { interestBps: 10_000, maxReferralInterestShareBps: 5_000 },
      });
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/deposit-collateral",
        label: "deposit collateral before trying an unlisted referral",
        body: { positionId: unlistedPosition.toBase58(), marketAsset: "base", depositAmount: "100" },
      });
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/borrow",
        label: "reject an unlisted referral",
        expected: "failure",
        body: {
          positionId: unlistedPosition.toBase58(),
          borrowAsset: "quote",
          borrowAmount: "1",
          minDebtAmountOut: "1",
          minLiquidationCfBps: 0,
          referrer: unlisted.toBase58(),
        },
      });

      await configureReferral(harness, alice, 5_000);
      await configureReferral(harness, referrer, 3_000);
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/set-referral-recipient",
        label: "set Alice self-referral recipient",
        body: { recipient: alice.toBase58() },
      });
      await harness.execute({
        wallet: independentReferrerWallet,
        endpoint: "/api/v2/fork/tx/set-referral-recipient",
        label: "set independent referral recipient",
        body: { recipient: referrer.toBase58() },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/deposit-collateral",
        label: "deposit quote collateral for self-referred base debt",
        body: { positionId: selfReferralPosition.toBase58(), marketAsset: "quote", depositAmount: "100" },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/borrow",
        label: "open permissioned self-referral",
        body: {
          positionId: selfReferralPosition.toBase58(),
          borrowAsset: "base",
          borrowAmount: "10",
          minDebtAmountOut: "10",
          minLiquidationCfBps: 0,
          referrer: alice.toBase58(),
        },
      });
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/deposit-collateral",
        label: "deposit base collateral for independent quote referral",
        body: { positionId: independentReferralPosition.toBase58(), marketAsset: "base", depositAmount: "100" },
      });
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/borrow",
        label: "open independent permissioned referral",
        body: {
          positionId: independentReferralPosition.toBase58(),
          borrowAsset: "quote",
          borrowAmount: "10",
          minDebtAmountOut: "10",
          minLiquidationCfBps: 0,
          referrer: referrer.toBase58(),
        },
      });

      await harness.timeTravel(0, 2_160_000);
      const selfRepayment = await repayAll(
        harness,
        "alice",
        selfReferralPosition,
        "base",
        "realize self-referral base interest"
      );
      const independentRepayment = await repayAll(
        harness,
        "bob",
        independentReferralPosition,
        "quote",
        "realize independent quote referral interest"
      );
      const selfAccrued = eventValue(
        harness.events(selfRepayment, "ReferralInterestAccrued")[0].data,
        "accruedAmount",
        "accrued_amount"
      );
      const independentAccrued = eventValue(
        harness.events(independentRepayment, "ReferralInterestAccrued")[0].data,
        "accruedAmount",
        "accrued_amount"
      );
      const aliceBaseBefore = await harness.tokenBalance(
        "alice",
        harness.config.baseMint,
        harness.config.baseTokenProgram
      );
      const referrerQuoteBefore = await harness.tokenBalance(
        independentReferrerWallet,
        harness.config.quoteMint,
        harness.config.quoteTokenProgram
      );
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/claim-referral-interest",
        label: "claim self-referral base interest",
        body: { asset: "base" },
      });
      await harness.execute({
        wallet: independentReferrerWallet,
        endpoint: "/api/v2/fork/tx/claim-referral-interest",
        label: "claim independent quote referral interest",
        body: { asset: "quote" },
      });
      harness.assertEqual(
        "self-referral claim is isolated by profile and mint",
        await harness.tokenBalance("alice", harness.config.baseMint, harness.config.baseTokenProgram) - aliceBaseBefore,
        selfAccrued
      );
      harness.assertEqual(
        "independent referral claim is isolated by profile and mint",
        await harness.tokenBalance(
          independentReferrerWallet,
          harness.config.quoteMint,
          harness.config.quoteTokenProgram
        ) - referrerQuoteBefore,
        independentAccrued
      );
    },
  },
];
