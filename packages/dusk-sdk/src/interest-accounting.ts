import { transferFeeCredit, type TransferFeeNotice } from "./transfer-fees.js";
import type { BorrowPosition, Market } from "./type-aliases.js";

const U64_MAX = (1n << 64n) - 1n;
const U128_MAX = (1n << 128n) - 1n;
const NAD = 1_000_000_000n;

/** Position-local public principal and indexed debt, in raw token atoms.
 * Read the position and market from the same slot. The market's fixed
 * principal is an aggregate and must not be used to split this loan's payoff
 * or insured loss. A one-atom share-floor phase can leave recorded principal
 * briefly above displayed position debt, so accrued interest is floored at 0.
 */
export function borrowDebtBreakdown(
  position: Pick<BorrowPosition, "fixedBaseShares" | "fixedQuoteShares" | "fixedBasePrincipal" | "fixedQuotePrincipal">,
  market: Pick<Market, "debt">,
  asset: "base" | "quote",
) {
  const shares = BigInt((asset === "base" ? position.fixedBaseShares : position.fixedQuoteShares).toString());
  const principal = BigInt((asset === "base" ? position.fixedBasePrincipal : position.fixedQuotePrincipal).toString());
  const index = BigInt((asset === "base" ? market.debt.baseBorrowIndexNad : market.debt.quoteBorrowIndexNad).toString());
  amount(shares, U128_MAX);
  amount(principal);
  amount(index, U128_MAX);
  if (index < NAD) throw new RangeError("Invalid borrow index");
  const debt = shares * index / NAD;
  amount(debt, U128_MAX);
  return { debt, principal, accruedInterest: debt > principal ? debt - principal : 0n };
}
function amount(value: bigint, max = U64_MAX): void {
  if (value < 0n || value > max) throw new RangeError("Interest amount is out of range");
}
/** Fund LP income using the historical unpaid fee book. Protocol and operator
 * fees remain unpaid until borrower cash arrives. No current fee rate is used.
 * Inputs must describe the same checkpointed market state. */
export function quoteInterestAdvanceAmounts(input: {
  unpaidPublicInterest: bigint;
  unpaidFeeInterest: bigint;
  outstandingGrossAdvance: bigint;
  outstandingRevenueEscrow: bigint;
  reserveCash: bigint;
  maxGross: bigint;
  transferFee: TransferFeeNotice;
}) {
  const { unpaidPublicInterest: unpaid, unpaidFeeInterest: fees,
    outstandingGrossAdvance: advanced, outstandingRevenueEscrow: escrow, reserveCash, maxGross } = input;
  amount(unpaid, U128_MAX);
  for (const value of [fees, advanced, escrow, reserveCash, maxGross]) amount(value);
  if (advanced > unpaid || escrow > advanced || escrow > fees || fees > unpaid
    || fees - escrow > unpaid - advanced) throw new RangeError("Inconsistent unpaid interest/advance");
  const available = unpaid - advanced;
  const grossAdvance = [available, reserveCash, maxGross].reduce((a, b) => a < b ? a : b);
  const revenueEscrow = available === 0n ? 0n : grossAdvance * (fees - escrow) / available;
  const lpVaultDebit = grossAdvance - revenueEscrow;
  const lpVaultCredit = transferFeeCredit(lpVaultDebit, input.transferFee, "effective");
  return { grossAdvance, revenueEscrow, lpVaultDebit, lpVaultCredit, transferFee: lpVaultDebit - lpVaultCredit };
}

/** Allocate measured custody credit against the frozen settlement budget.
 * grossTransfer excludes LP yield already funded by an advance. pendingFeeGross
 * includes both protocol and operator receivables. Recipient claims are settled
 * by the on-chain fee book, including fractional cash from earlier payments. */
export function allocateRepaidInterest(input: {
  grossTransfer: bigint;
  pendingFeeGross: bigint;
  measuredVaultCredit: bigint;
}) {
  const { grossTransfer, pendingFeeGross: fees, measuredVaultCredit: credit } = input;
  for (const value of [grossTransfer, fees, credit]) amount(value);
  if (credit > grossTransfer || fees > grossTransfer) throw new RangeError("Custody credit or fees exceed settlement");
  const revenueCredit = grossTransfer === 0n ? 0n : credit * fees / grossTransfer;
  return { grossTransfer, revenueCredit, lpCredit: credit - revenueCredit };
}
