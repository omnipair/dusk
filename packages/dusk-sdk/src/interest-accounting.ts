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
function rate(bps: number): bigint {
  if (!Number.isSafeInteger(bps) || bps < 0 || bps > 10_000) throw new RangeError("Invalid protocol interest rate");
  return BigInt(bps);
}

/** Accounting amounts only: this does not establish hLP settlement feasibility
 * or authorize a harvest. Inputs must come from the same accrued market state.
 * Only the effective transfer fee is charged on the custody transfer.
 */
export function quoteInterestAdvanceAmounts(input: {
  unpaidPublicInterest: bigint;
  outstandingGrossAdvance: bigint;
  reserveCash: bigint;
  maxGross: bigint;
  protocolBps: number;
  transferFee: TransferFeeNotice;
}) {
  const { unpaidPublicInterest: unpaid, outstandingGrossAdvance: advanced, reserveCash, maxGross } = input;
  amount(unpaid, U128_MAX);
  for (const value of [advanced, reserveCash, maxGross]) amount(value);
  if (advanced > unpaid) throw new RangeError("Advance exceeds unpaid interest");
  const bps = rate(input.protocolBps);
  const available = unpaid - advanced;
  const grossAdvance = [available, reserveCash, maxGross].reduce((a, b) => a < b ? a : b);
  const protocolEscrow = grossAdvance * bps / 10_000n;
  const lpVaultDebit = grossAdvance - protocolEscrow;
  const lpVaultCredit = transferFeeCredit(lpVaultDebit, input.transferFee, "effective");
  return { grossAdvance, protocolEscrow, lpVaultDebit, lpVaultCredit, transferFee: lpVaultDebit - lpVaultCredit };
}

/** Allocate the measured custody credit after repayment. Already-funded LP
 * interest is omitted from freshGrossInterest, so it cannot be paid twice.
 * The current protocol rate applies only to the fresh part; the escrowed part
 * is protocol revenue after its proportional share of the transfer fee.
 */
export function allocateRepaidInterest(input: {
  freshGrossInterest: bigint;
  frozenProtocolGross: bigint;
  measuredVaultCredit: bigint;
  currentProtocolBps: number;
}) {
  const { freshGrossInterest: fresh, frozenProtocolGross: frozen, measuredVaultCredit: credit } = input;
  for (const value of [fresh, frozen, credit]) amount(value);
  const bps = rate(input.currentProtocolBps);
  const grossTransfer = fresh + frozen;
  amount(grossTransfer);
  if (credit > grossTransfer) throw new RangeError("Custody credit exceeds the transfer");
  const frozenProtocolCredit = grossTransfer === 0n ? 0n : credit * frozen / grossTransfer;
  const freshCredit = credit - frozenProtocolCredit;
  const freshProtocolCredit = freshCredit * bps / 10_000n;
  const protocolCredit = frozenProtocolCredit + freshProtocolCredit;
  return { grossTransfer, frozenProtocolCredit, freshProtocolCredit, protocolCredit, lpCredit: credit - protocolCredit };
}
