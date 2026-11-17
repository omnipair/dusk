import type { BorrowPosition, HlpCohort, HlpVault, JuniorCohort, Market, YieldAccount } from "./type-aliases.js";

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
 * funding debt less principal, floored at zero. It is credited to ordinary
 * srLP and junior holders as it accrues and recorded as the ledger's
 * `fundingRecognized`. Read from a market whose interest is accrued to the
 * funding slot. */
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

/** One asset's accrued-interest ledger as bigints, read from `fees.ledger`.
 * Interest is credited to holders when it accrues, so `credited` can exceed
 * reserve cash; claims are paid from cash as it allows. `deficit` is the
 * part of a loss LP capital could not absorb; while either asset has one,
 * the market refuses new liquidity (`InterestDeficit`). */
export interface MarketInterestLedger {
  /** Fixed and isolated borrower interest outstanding, as last recognized. */
  publicRecognized: bigint;
  /** hLP funding interest outstanding in this asset, as last recognized. */
  fundingRecognized: bigint;
  /** Credited, unclaimed interest of every holder lane, the junior seed and the operator. */
  credited: bigint;
  /** Protocol share of outstanding interest, paid only when borrowers pay. */
  protocolUnpaid: bigint;
  /** The current operator recipient's credited, unclaimed share (part of `credited`). */
  operatorClaimable: bigint;
  /** Protocol interest paid but not yet booked into its revenue liabilities. */
  protocolPending: bigint;
  /** Pool loss beyond LP capital. Never decreases. */
  deficit: bigint;
}

export function marketInterestLedger(market: Pick<Market, "baseSide" | "quoteSide">, asset: "base" | "quote"): MarketInterestLedger {
  const ledger = (asset === "base" ? market.baseSide : market.quoteSide).fees.ledger;
  const read = (value: { toString(): string }) => BigInt(value.toString());
  return {
    publicRecognized: read(ledger.publicRecognized),
    fundingRecognized: read(ledger.fundingRecognized),
    credited: read(ledger.credited),
    protocolUnpaid: read(ledger.protocolUnpaid),
    operatorClaimable: read(ledger.operatorClaimable),
    protocolPending: read(ledger.protocolPending),
    deficit: read(ledger.deficit),
  };
}

/** Protocol share of an interest payment of `interestPaid` atoms, as the
 * program takes it: `floor(paid · protocolUnpaid / interest)` where
 * `interest = publicRecognized + fundingRecognized`, both read from the same
 * accrued market state. The protocol is paid only from interest borrowers
 * actually pay; the rest of the payment stays in reserve cash for holders. */
export function quoteProtocolInterestShare(input: {
  interestPaid: bigint;
  ledger: Pick<MarketInterestLedger, "publicRecognized" | "fundingRecognized" | "protocolUnpaid">;
}): bigint {
  const { interestPaid: paid, ledger } = input;
  amount(paid);
  const interest = ledger.publicRecognized + ledger.fundingRecognized;
  if (ledger.protocolUnpaid < 0n || ledger.protocolUnpaid > interest) throw new RangeError("Inconsistent interest ledger");
  if (paid > interest) throw new RangeError("Payment exceeds outstanding interest");
  return interest === 0n ? 0n : paid * ledger.protocolUnpaid / interest;
}

/** Holder interest lane a yield account checkpoints against. */
export type InterestLaneKind = "srlp" | "junior" | "baseHlp" | "quoteHlp";

/** The Q64 interest growth index a holder of `kind` checkpoints against for
 * revenue `asset`, as harvest reads it: ordinary srLP
 * `fees.interestGrowthIndexQ64`, junior `fees.junior.interestGrowthQ64`, each
 * hLP vault's per-asset index. A retired junior generation or hLP cohort
 * reads its frozen final index instead; pass the decoded cohort. */
export function interestGrowthIndexQ64(input: {
  market: Pick<Market, "baseSide" | "quoteSide" | "baseHlpVault" | "quoteHlpVault">;
  kind: InterestLaneKind;
  asset: "base" | "quote";
  retiredJuniorCohort?: Pick<JuniorCohort, "finalIncome">;
  retiredHlpCohort?: Pick<HlpCohort, "finalVault">;
}): bigint {
  const { market, kind, asset } = input;
  const side = asset === "base" ? market.baseSide : market.quoteSide;
  const hlpIndex = (vault: Pick<HlpVault, "baseInterestGrowthIndexQ64" | "quoteInterestGrowthIndexQ64">) =>
    BigInt((asset === "base" ? vault.baseInterestGrowthIndexQ64 : vault.quoteInterestGrowthIndexQ64).toString());
  if (kind === "srlp") {
    if (input.retiredJuniorCohort || input.retiredHlpCohort) throw new Error("srLP has no retired cohorts");
    return BigInt(side.fees.interestGrowthIndexQ64.toString());
  }
  if (kind === "junior") {
    if (input.retiredHlpCohort) throw new Error("Junior cannot use an hLP cohort");
    const income = input.retiredJuniorCohort ? input.retiredJuniorCohort.finalIncome[asset === "base" ? 0 : 1]! : side.fees.junior;
    return BigInt(income.interestGrowthQ64.toString());
  }
  if (kind !== "baseHlp" && kind !== "quoteHlp") throw new Error("Invalid yield token kind");
  if (input.retiredJuniorCohort) throw new Error("hLP cannot use a junior cohort");
  if (input.retiredHlpCohort) return hlpIndex(input.retiredHlpCohort.finalVault);
  return hlpIndex(kind === "baseHlp" ? market.baseHlpVault : market.quoteHlpVault);
}

/** Mirror of `accrue_fee_liability_with_remainder`: whole atoms a balance
 * earns as a Q64 growth index moves past its checkpoint, carrying the
 * sub-atom remainder. */
export function accrueGrowthQ64(input: {
  balance: bigint;
  growthIndexQ64: bigint;
  checkpointQ64: bigint;
  remainderQ64: bigint;
}): { amount: bigint; remainderQ64: bigint } {
  const { balance, growthIndexQ64: index, checkpointQ64: checkpoint, remainderQ64: remainder } = input;
  amount(balance);
  amount(index, U128_MAX);
  amount(checkpoint, U128_MAX);
  amount(remainder);
  if (balance === 0n || index <= checkpoint) return { amount: 0n, remainderQ64: remainder };
  const delta = index - checkpoint;
  const fractional = balance * (delta & U64_MAX) + remainder;
  const accrued = balance * (delta >> 64n) + (fractional >> 64n);
  amount(accrued);
  return { amount: accrued, remainderQ64: fractional & U64_MAX };
}

/** Interest a yield account can claim at `growthIndexQ64`, computed as
 * `YieldAccount::accrue` checkpoints it: the stored `accruedInterestAmount`
 * plus the balance's share of the index change since the holder's
 * checkpoint. Junior escrow receipts earn alongside the wallet balance. Pass
 * the LP balance the program reads (the owner's LP token account) and an
 * index from `interestGrowthIndexQ64` read with the account. The index
 * includes interest up to the market's last accrual.
 *
 * A harvest pays `min(claimable, cashReserve)` of the asset's reserve side;
 * the rest stays claimable (`harvestableInterest`). */
export function claimableInterest(input: {
  yieldAccount: Pick<YieldAccount, "tokenKind" | "escrowBalance" | "accruedInterestAmount" | "interestCheckpointQ64" | "interestRemainderQ64">;
  lpBalance: bigint;
  growthIndexQ64: bigint;
}): bigint {
  const holder = input.yieldAccount;
  const escrow = holder.tokenKind === 2 ? BigInt(holder.escrowBalance.toString()) : 0n;
  const balance = input.lpBalance + escrow;
  const accrued = accrueGrowthQ64({
    balance,
    growthIndexQ64: input.growthIndexQ64,
    checkpointQ64: BigInt(holder.interestCheckpointQ64.toString()),
    remainderQ64: BigInt(holder.interestRemainderQ64.toString()),
  }).amount;
  const claimable = BigInt(holder.accruedInterestAmount.toString()) + accrued;
  amount(claimable);
  return claimable;
}

/** Interest a harvest pays now: claimable interest limited by the asset's
 * reserve cash. Unpaid interest stays claimable and is not lost. */
export function harvestableInterest(input: {
  claimable: bigint;
  market: Pick<Market, "baseSide" | "quoteSide">;
  asset: "base" | "quote";
}): bigint {
  const cash = BigInt((input.asset === "base" ? input.market.baseSide : input.market.quoteSide).reserves.cashReserve.toString());
  return input.claimable < cash ? input.claimable : cash;
}
