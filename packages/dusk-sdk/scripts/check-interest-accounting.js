import assert from "node:assert/strict";
import { test } from "node:test";
import { quoteInterestAdvanceAmounts, allocateRepaidInterest, borrowDebtBreakdown, hlpFundingInterest } from "../dist/interest-accounting.js";
import { transferFeeNotice } from "../dist/transfer-fees.js";

const zero = transferFeeNotice(null, 1n);
test("public borrower debt breakdown reads position principal, not the market ratio", () => {
  const market = { debt: { baseBorrowIndexNad: 1_100_000_000n, quoteBorrowIndexNad: 1_000_000_000n,
    fixedBasePrincipal: 2_000n } };
  const alice = { fixedBaseShares: 1_000n, fixedQuoteShares: 0n, fixedBasePrincipal: 1_000n, fixedQuotePrincipal: 0n };
  assert.deepEqual(borrowDebtBreakdown(alice, market, "base"), { debt: 1_100n, principal: 1_000n, accruedInterest: 100n });
});
test("cash funding freezes the split and respects cash, request and unfunded limits", () => {
  const input = { unpaidPublicInterest: 1_000n, outstandingGrossAdvance: 300n, reserveCash: 600n, maxGross: 900n, unpaidFeeInterest: 400n, outstandingRevenueEscrow: 120n, transferFee: zero };
  assert.deepEqual(quoteInterestAdvanceAmounts(input), { grossAdvance: 600n, revenueEscrow: 240n, lpVaultDebit: 360n, lpVaultCredit: 360n, transferFee: 0n });
  assert.equal(quoteInterestAdvanceAmounts({ ...input, reserveCash: 1_000n }).grossAdvance, 700n);
  assert.equal(quoteInterestAdvanceAmounts({ ...input, maxGross: 100n }).grossAdvance, 100n);
  assert.equal(quoteInterestAdvanceAmounts({ ...input, reserveCash: 0n }).grossAdvance, 0n);
  assert.throws(() => quoteInterestAdvanceAmounts({ ...input, outstandingGrossAdvance: 1_001n }));
  assert.throws(() => quoteInterestAdvanceAmounts({ ...input, maxGross: -1n }));
  assert.throws(() => quoteInterestAdvanceAmounts({ ...input, unpaidFeeInterest: 1_001n }));
});

// fund_public_interest freezes split_revenue(gross, protocol bps) until the
// side's junior income is initialized, and the fee book's share afterwards.
test("before junior initialization the advance splits at the current protocol rate", () => {
  const input = { unpaidPublicInterest: 1_000n, outstandingGrossAdvance: 300n, outstandingRevenueEscrow: 60n,
    reserveCash: 600n, maxGross: 900n, transferFee: zero, juniorInitialized: false, protocolInterestBps: 2_000 };
  assert.deepEqual(quoteInterestAdvanceAmounts(input),
    { grossAdvance: 600n, revenueEscrow: 120n, lpVaultDebit: 480n, lpVaultCredit: 480n, transferFee: 0n });
  // floor(999 * 20%) rounds the frozen fee down, as split_revenue does.
  assert.equal(quoteInterestAdvanceAmounts({ ...input, unpaidPublicInterest: 2_000n, reserveCash: 999n, maxGross: 999n })
    .revenueEscrow, 199n);
  assert.equal(quoteInterestAdvanceAmounts({ ...input, protocolInterestBps: 0 }).revenueEscrow, 0n);
  // The uninitialized fee book is not read, even when it lags the escrow.
  assert.equal(quoteInterestAdvanceAmounts({ ...input, unpaidFeeInterest: 0n }).revenueEscrow, 120n);
  // The same state read as initialized follows the fee book instead.
  assert.equal(quoteInterestAdvanceAmounts({ ...input, juniorInitialized: true, unpaidFeeInterest: 400n }).revenueEscrow, 291n);
  for (const protocolInterestBps of [undefined, -1, 1.5, 10_001]) {
    assert.throws(() => quoteInterestAdvanceAmounts({ ...input, protocolInterestBps }), /protocolInterestBps/);
  }
  const { juniorInitialized, ...initialized } = input;
  assert.throws(() => quoteInterestAdvanceAmounts(initialized), /unpaidFeeInterest is required/);
});

test("reserve cash owed as hLP funding interest is not advanced", () => {
  const input = { unpaidPublicInterest: 1_000n, outstandingGrossAdvance: 300n, reserveCash: 600n, maxGross: 900n,
    unpaidFeeInterest: 400n, outstandingRevenueEscrow: 120n, transferFee: zero };
  assert.deepEqual(quoteInterestAdvanceAmounts({ ...input, hlpFundingInterest: 250n }),
    { grossAdvance: 350n, revenueEscrow: 140n, lpVaultDebit: 210n, lpVaultCredit: 210n, transferFee: 0n });
  assert.equal(quoteInterestAdvanceAmounts({ ...input, hlpFundingInterest: 600n }).grossAdvance, 0n);
  assert.equal(quoteInterestAdvanceAmounts({ ...input, hlpFundingInterest: 700n }).grossAdvance, 0n);
  assert.equal(quoteInterestAdvanceAmounts({ ...input, hlpFundingInterest: 250n, maxGross: 100n }).grossAdvance, 100n);
  const early = quoteInterestAdvanceAmounts({ ...input, hlpFundingInterest: 250n, juniorInitialized: false,
    protocolInterestBps: 2_000, outstandingRevenueEscrow: 60n });
  assert.deepEqual([early.grossAdvance, early.revenueEscrow], [350n, 70n]);
  assert.throws(() => quoteInterestAdvanceAmounts({ ...input, hlpFundingInterest: -1n }));

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

test("advance pays the effective mint fee, not its announced future fee", () => {
  const transferFee = transferFeeNotice({
    olderTransferFee: { epoch: 0n, transferFeeBasisPoints: 100, maximumFee: 100n },
    newerTransferFee: { epoch: 2n, transferFeeBasisPoints: 9_000, maximumFee: 1_000n },
  }, 1n);
  assert.deepEqual(quoteInterestAdvanceAmounts({ unpaidPublicInterest: 1_000n, outstandingGrossAdvance: 0n, reserveCash: 1_000n, maxGross: 1_000n, unpaidFeeInterest: 200n, outstandingRevenueEscrow: 0n, transferFee }),
    { grossAdvance: 1_000n, revenueEscrow: 200n, lpVaultDebit: 800n, lpVaultCredit: 792n, transferFee: 8n });
});

test("repayment uses frozen fees rather than current rates", () => {
  const result = allocateRepaidInterest({grossTransfer:200n,pendingFeeGross:200n,measuredVaultCredit:198n});
  assert.equal(result.lpCredit,0n);
  assert.equal(result.revenueCredit,198n);
  const mixed=allocateRepaidInterest({grossTransfer:700n,pendingFeeGross:350n,measuredVaultCredit:693n});
  assert.deepEqual(mixed,{grossTransfer:700n,revenueCredit:346n,lpCredit:347n});
  assert.equal(mixed.revenueCredit+mixed.lpCredit,693n);
  assert.throws(()=>allocateRepaidInterest({grossTransfer:2n,pendingFeeGross:1n,measuredVaultCredit:3n}));
  assert.throws(()=>allocateRepaidInterest({grossTransfer:2n,pendingFeeGross:3n,measuredVaultCredit:2n}));
});

test("u64-sized advances retain integer precision", () => {
  const max = (1n << 64n) - 1n;
  const result = quoteInterestAdvanceAmounts({ unpaidPublicInterest: (1n << 100n), outstandingGrossAdvance: 0n, reserveCash: max, maxGross: max, unpaidFeeInterest: 200n, outstandingRevenueEscrow: 0n, transferFee: zero });
  assert.equal(result.grossAdvance, max);
  assert.equal(result.revenueEscrow + result.lpVaultDebit, max);
});
