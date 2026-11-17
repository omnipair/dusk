import assert from "node:assert/strict";
import { test } from "node:test";
import {
  accrueGrowthQ64, borrowDebtBreakdown, claimableInterest, harvestableInterest, hlpFundingInterest,
  interestGrowthIndexQ64, marketInterestLedger, quoteProtocolInterestShare,
} from "../dist/interest-accounting.js";
import { quoteReferralInterestShare } from "../dist/referral.js";
import { transferFeeCredit, transferFeeNotice, transferInverseFee } from "../dist/transfer-fees.js";

const zero = transferFeeNotice(null, 1n);
test("public borrower debt breakdown reads position principal, not the market ratio", () => {
  const market = { debt: { baseBorrowIndexNad: 1_100_000_000n, quoteBorrowIndexNad: 1_000_000_000n,
    fixedBasePrincipal: 2_000n } };
  const alice = { fixedBaseShares: 1_000n, fixedQuoteShares: 0n, fixedBasePrincipal: 1_000n, fixedQuotePrincipal: 0n };
  assert.deepEqual(borrowDebtBreakdown(alice, market, "base"), { debt: 1_100n, principal: 1_000n, accruedInterest: 100n });
});
test("hLP funding interest reads the opposite vault's indexed debt above principal", () => {
  // The base-target vault borrows quote; the quote-target vault borrows base.
  const market = {
    debt: { baseBorrowIndexNad: 1_100_000_000n, quoteBorrowIndexNad: 1_300_000_000n },
    baseHlpVault: { debtShares: 3_000n, debtPrincipal: 3_000n },
    quoteHlpVault: { debtShares: 1_000n, debtPrincipal: 1_000n },
  };
  assert.equal(hlpFundingInterest(market, "base"), 100n);
  assert.equal(hlpFundingInterest(market, "quote"), 900n);
  assert.equal(hlpFundingInterest({ ...market, quoteHlpVault: { debtShares: 1_000n, debtPrincipal: 1_200n } }, "base"), 0n);
  assert.equal(hlpFundingInterest({ ...market, quoteHlpVault: { debtShares: 0n, debtPrincipal: 0n } }, "base"), 0n);
  assert.throws(() => hlpFundingInterest({ ...market, quoteHlpVault: { debtShares: 1n << 64n, debtPrincipal: 0n } }, "base"));
});

const Q64 = 1n << 64n;

test("growth accrual mirrors accrue_fee_liability_with_remainder", () => {
  // Whole and fractional limbs: 3 holders' units at 2.5 atoms per unit.
  assert.deepEqual(accrueGrowthQ64({ balance: 3n, growthIndexQ64: 5n * Q64 / 2n, checkpointQ64: 0n, remainderQ64: 0n }),
    { amount: 7n, remainderQ64: Q64 / 2n });
  // The carried remainder completes an atom.
  assert.deepEqual(accrueGrowthQ64({ balance: 1n, growthIndexQ64: Q64 / 2n, checkpointQ64: 0n, remainderQ64: Q64 / 2n }),
    { amount: 1n, remainderQ64: 0n });
  // No balance or no index change keeps the remainder.
  assert.deepEqual(accrueGrowthQ64({ balance: 0n, growthIndexQ64: Q64, checkpointQ64: 0n, remainderQ64: 7n }), { amount: 0n, remainderQ64: 7n });
  assert.deepEqual(accrueGrowthQ64({ balance: 5n, growthIndexQ64: Q64, checkpointQ64: Q64, remainderQ64: 7n }), { amount: 0n, remainderQ64: 7n });
  // Splitting one index move across checkpoints never pays more than one move.
  const whole = accrueGrowthQ64({ balance: 1_000_003n, growthIndexQ64: 12_345_678_901_234n, checkpointQ64: 0n, remainderQ64: 0n });
  const first = accrueGrowthQ64({ balance: 1_000_003n, growthIndexQ64: 5_000_000_000_000n, checkpointQ64: 0n, remainderQ64: 0n });
  const second = accrueGrowthQ64({ balance: 1_000_003n, growthIndexQ64: 12_345_678_901_234n, checkpointQ64: 5_000_000_000_000n,
    remainderQ64: first.remainderQ64 });
  assert.equal(first.amount + second.amount, whole.amount);
  assert.equal(second.remainderQ64, whole.remainderQ64);
  assert.throws(() => accrueGrowthQ64({ balance: 1n << 64n, growthIndexQ64: Q64, checkpointQ64: 0n, remainderQ64: 0n }));
});

test("claimable interest is the stored amount plus the lane index change, junior escrow included", () => {
  const holder = { tokenKind: 0, escrowBalance: 40n, accruedInterestAmount: 11n, interestCheckpointQ64: Q64, interestRemainderQ64: 0n };
  // srLP ignores escrow: 100 units at 0.5 atoms per unit.
  assert.equal(claimableInterest({ yieldAccount: holder, lpBalance: 100n, growthIndexQ64: 3n * Q64 / 2n }), 61n);
  // Junior escrow receipts keep earning: 140 units at 0.5 atoms.
  assert.equal(claimableInterest({ yieldAccount: { ...holder, tokenKind: 2 }, lpBalance: 100n, growthIndexQ64: 3n * Q64 / 2n }), 81n);
  // A checkpoint at the index leaves only the stored amount.
  assert.equal(claimableInterest({ yieldAccount: holder, lpBalance: 100n, growthIndexQ64: Q64 }), 11n);
  const market = {
    baseSide: { reserves: { cashReserve: 50n } },
    quoteSide: { reserves: { cashReserve: 500n } },
  };
  assert.equal(harvestableInterest({ claimable: 61n, market, asset: "base" }), 50n);
  assert.equal(harvestableInterest({ claimable: 61n, market, asset: "quote" }), 61n);
});

test("each holder kind reads its own interest lane, retired cohorts their frozen index", () => {
  const side = (offset) => ({
    fees: { interestGrowthIndexQ64: 10n + offset, junior: { interestGrowthQ64: 20n + offset } },
  });
  const vault = (offset) => ({ baseInterestGrowthIndexQ64: 30n + offset, quoteInterestGrowthIndexQ64: 40n + offset });
  const market = { baseSide: side(0n), quoteSide: side(1n), baseHlpVault: vault(0n), quoteHlpVault: vault(100n) };
  assert.equal(interestGrowthIndexQ64({ market, kind: "srlp", asset: "base" }), 10n);
  assert.equal(interestGrowthIndexQ64({ market, kind: "srlp", asset: "quote" }), 11n);
  assert.equal(interestGrowthIndexQ64({ market, kind: "junior", asset: "quote" }), 21n);
  assert.equal(interestGrowthIndexQ64({ market, kind: "baseHlp", asset: "quote" }), 40n);
  assert.equal(interestGrowthIndexQ64({ market, kind: "quoteHlp", asset: "base" }), 130n);
  const retiredJuniorCohort = { finalIncome: [{ interestGrowthQ64: 7n }, { interestGrowthQ64: 8n }] };
  assert.equal(interestGrowthIndexQ64({ market, kind: "junior", asset: "quote", retiredJuniorCohort }), 8n);
  const retiredHlpCohort = { finalVault: vault(1_000n) };
  assert.equal(interestGrowthIndexQ64({ market, kind: "baseHlp", asset: "base", retiredHlpCohort }), 1_030n);
  assert.throws(() => interestGrowthIndexQ64({ market, kind: "srlp", asset: "base", retiredJuniorCohort }));
  assert.throws(() => interestGrowthIndexQ64({ market, kind: "junior", asset: "base", retiredHlpCohort }));
  assert.throws(() => interestGrowthIndexQ64({ market, kind: "hlp", asset: "base" }));
});

test("the ledger reads as bigints and the protocol share of a payment is pooled and floored", () => {
  const ledger = { publicRecognized: 700n, fundingRecognized: 300n, credited: 900n, protocolUnpaid: 150n,
    operatorClaimable: 90n, protocolPending: 4n, deficit: 0n };
  const market = { baseSide: { fees: { ledger } }, quoteSide: { fees: { ledger: { ...ledger, deficit: 9n } } } };
  assert.deepEqual(marketInterestLedger(market, "base"), ledger);
  assert.equal(marketInterestLedger(market, "quote").deficit, 9n);
  // floor(333 * 150 / 1000) = 49; the whole outstanding interest pays all of Pu.
  assert.equal(quoteProtocolInterestShare({ interestPaid: 333n, ledger }), 49n);
  assert.equal(quoteProtocolInterestShare({ interestPaid: 1_000n, ledger }), 150n);
  assert.equal(quoteProtocolInterestShare({ interestPaid: 0n, ledger: { publicRecognized: 0n, fundingRecognized: 0n, protocolUnpaid: 0n } }), 0n);
  assert.throws(() => quoteProtocolInterestShare({ interestPaid: 1_001n, ledger }));
  assert.throws(() => quoteProtocolInterestShare({ interestPaid: 1n, ledger: { ...ledger, protocolUnpaid: 1_001n } }));
  // The referral partner takes its floored share of that protocol revenue.
  assert.deepEqual(quoteReferralInterestShare({ interestPaid: 333n, protocolInterestRevenue: 49n, interestShareBps: 2_500 }),
    { interestPaid: 333n, protocolInterestRevenue: 49n, interestShareBps: 2_500, referralAmount: 12n });
  assert.throws(() => quoteReferralInterestShare({ interestPaid: 3n, protocolInterestRevenue: 4n, interestShareBps: 0 }));
});

test("inverse transfer fee mirrors SPL calculate_inverse_fee", () => {
  const notice = (transferFeeBasisPoints, maximumFee) => transferFeeNotice({
    olderTransferFee: { epoch: 0n, transferFeeBasisPoints, maximumFee },
    newerTransferFee: { epoch: 0n, transferFeeBasisPoints, maximumFee },
  }, 1n);
  assert.equal(transferInverseFee(0n, notice(100, 100n)), 0n);
  assert.equal(transferInverseFee(792n, notice(100, 100n)), 8n);
  assert.equal(transferInverseFee(1_000n, notice(100, 100n)), 11n);
  assert.equal(transferInverseFee(1n, notice(1, 1_000n)), 1n);
  assert.equal(transferInverseFee(1_000_000n, notice(500, 7n)), 7n);
  assert.equal(transferInverseFee(5n, notice(10_000, 9n)), 9n);
  assert.equal(transferInverseFee(5n, zero), 0n);
  for (const [bps, max] of [[1, 1_000_000n], [50, 1_000_000n], [250, 13n], [9_999, 1n << 40n]]) {
    for (const post of [1n, 2n, 99n, 10_000n, 123_457n]) {
      const fee = transferInverseFee(post, notice(bps, max));
      // The grossed-up transfer delivers at least the intended amount.
      assert.ok(transferFeeCredit(post + fee, notice(bps, max)) >= post, `${bps} ${max} ${post}`);
    }
  }
});
