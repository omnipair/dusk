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

/** Funding interest the opposite-side hLP vault owes in `asset`: its indexed
 * funding debt less principal, floored at zero. `fund_interest` does not
 * advance the reserve cash that backs it. Read from a market whose interest
 * is accrued to the funding slot. */
export function hlpFundingInterest(
  market: Pick<Market, "debt" | "baseHlpVault" | "quoteHlpVault">,
  asset: "base" | "quote",
): bigint {
  // The quote-target vault borrows base and the base-target vault borrows quote.
  const vault = asset === "base" ? market.quoteHlpVault : market.baseHlpVault;
  const shares = BigInt(vault.debtShares.toString());
  const principal = BigInt(vault.debtPrincipal.toString());
  const index = BigInt((asset === "base" ? market.debt.baseBorrowIndexNad : market.debt.quoteBorrowIndexNad).toString());
  amount(shares, U128_MAX);
  amount(principal);
  amount(index, U128_MAX);
  const debt = shares * index / NAD;
  amount(debt);
  return debt > principal ? debt - principal : 0n;
}

/** Fund LP income from reserve cash, as `fund_interest` does for one side.
 * Once the side's junior income is initialized, the frozen protocol and
 * operator share is the historical unpaid fee book's share of the advance; no
 * current fee rate is used. Before that, the program splits the advance at the
 * current protocol interest rate. Reserve cash owed as hLP funding interest is
 * not advanced. Protocol and operator fees remain unpaid until borrower cash
 * arrives. Inputs must describe the same checkpointed market state. */
export function quoteInterestAdvanceAmounts(input: {
  unpaidPublicInterest: bigint;
  /** The side's unpaid fee book after synchronization. Required once junior
   * income is initialized; unused before. */
  unpaidFeeInterest?: bigint;
  outstandingGrossAdvance: bigint;
  outstandingRevenueEscrow: bigint;
  reserveCash: bigint;
  maxGross: bigint;
  transferFee: TransferFeeNotice;
  /** `side.fees.junior.initialized`; defaults to true. */
  juniorInitialized?: boolean;
  /** `FutarchyAuthority.revenueShare.interestBps`; required while junior
   * income is uninitialized. */
  protocolInterestBps?: number;
  /** See `hlpFundingInterest`; defaults to zero. */
  hlpFundingInterest?: bigint;
}) {
  const { unpaidPublicInterest: unpaid, outstandingGrossAdvance: advanced,
    outstandingRevenueEscrow: escrow, reserveCash, maxGross } = input;
  const fundingInterest = input.hlpFundingInterest ?? 0n;
  amount(unpaid, U128_MAX);
  for (const value of [advanced, escrow, reserveCash, maxGross, fundingInterest]) amount(value);
  if (advanced > unpaid || escrow > advanced) throw new RangeError("Inconsistent unpaid interest/advance");
  const available = unpaid - advanced;
  const spendableCash = reserveCash > fundingInterest ? reserveCash - fundingInterest : 0n;
  const grossAdvance = [available, spendableCash, maxGross].reduce((a, b) => a < b ? a : b);
  let revenueEscrow: bigint;
  if (input.juniorInitialized ?? true) {
    const fees = input.unpaidFeeInterest;
    if (fees === undefined) throw new TypeError("unpaidFeeInterest is required once junior income is initialized");
    amount(fees);
    if (escrow > fees || fees > unpaid || fees - escrow > available) {
      throw new RangeError("Inconsistent unpaid interest/advance");
    }
    revenueEscrow = available === 0n ? 0n : grossAdvance * (fees - escrow) / available;
  } else {
    const bps = input.protocolInterestBps;
    if (bps === undefined || !Number.isInteger(bps) || bps < 0 || bps > 10_000) {
      throw new RangeError("protocolInterestBps must be an integer from 0 to 10000 before junior income is initialized");
    }
    revenueEscrow = grossAdvance * BigInt(bps) / 10_000n;
  }
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
