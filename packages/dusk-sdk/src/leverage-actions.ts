/** Position actions use the borrowed token as their unit of account on BOTH
 * sides. A reverse position is long the held token against the borrowed token;
 * this leverage is not a dollar-denominated perpetual's short delta.
 */
const BPS = 10_000n;
const NAD = 1_000_000_000n;
const U64_MAX = (1n << 64n) - 1n;
const U128_MAX = (1n << 128n) - 1n;
const ceilDiv = (a: bigint, b: bigint) => (a + b - 1n) / b;

function unsigned(value: bigint, name: string, max = U64_MAX): bigint {
  if (typeof value !== "bigint" || value < 0n || value > max)
    throw new RangeError(`${name} must be an unsigned integer no greater than ${max}`);
  return value;
}

export interface LeverageActionState {
  collateralAmount: bigint;
  /** Indexed debt, including accrued interest, in borrowed-token atoms. */
  debtAmount: bigint;
  /** Mark or EMA value of held collateral in borrowed-token atoms. State the
   * chosen price basis in the UI. This is NOT a realizable close payout. */
  collateralValue: bigint;
}

export function leverageActionMetrics(state: LeverageActionState) {
  for (const [name, value] of Object.entries(state)) unsigned(value, name);
  const equity = state.collateralValue - state.debtAmount;
  return {
    ...state, equity,
    leverageBps: equity > 0n ? ceilDiv(state.collateralValue * BPS, equity) : null,
    debtFree: state.debtAmount === 0n,
    unitOfAccount: "debt-token" as const,
  };
}

/** Matches the program's aggregate-floor-aware debt-share burn. Do not infer
 * a full repayment from a rounded UI debt amount: it can need one more atom. */
export function leverageRepaymentQuote(input: {
  positionShares: bigint; aggregateShares: bigint; borrowIndexNad: bigint; maxCredit: bigint;
}) {
  const { positionShares: shares, aggregateShares: aggregate, borrowIndexNad: index, maxCredit } = input;
  unsigned(shares, "positionShares", U128_MAX); unsigned(aggregate, "aggregateShares", U128_MAX);
  unsigned(index, "borrowIndexNad", U128_MAX); unsigned(maxCredit, "maxCredit");
  if (aggregate < shares || index < NAD) throw new RangeError("Invalid aggregate shares or borrow index");
  const debtAt = (s: bigint) => s * index / NAD;
  const debtBefore = debtAt(shares);
  const fullRepayCredit = debtAt(aggregate) - debtAt(aggregate - shares);
  unsigned(debtBefore, "debtAmount"); unsigned(fullRepayCredit, "fullRepayCredit");
  if (shares === 0n || maxCredit === 0n) return {
    sharesBurned: 0n, cashRepaid: 0n, debtReduced: 0n, debtRemaining: debtBefore, fullRepayCredit,
  };
  let burned = maxCredit * NAD / index;
  if (burned > shares) burned = shares;
  if (burned < shares && debtAt(aggregate) - debtAt(aggregate - burned - 1n) <= maxCredit) burned++;
  const cashRepaid = debtAt(aggregate) - debtAt(aggregate - burned);
  if (burned === 0n || cashRepaid === 0n) throw new RangeError("Repayment is below debt-share granularity");
  const debtRemaining = debtAt(shares - burned);
  return { sharesBurned: burned, cashRepaid, debtReduced: debtBefore - debtRemaining, debtRemaining, fullRepayCredit };
}

/** Exact slice selection used by owner and delegated partial closes. */
export function leverageCloseSlice(input: {
  collateralAmount: bigint; debtShares: bigint; aggregateShares: bigint; borrowIndexNad: bigint; closeBps: number;
}) {
  const { collateralAmount, debtShares, aggregateShares, borrowIndexNad, closeBps } = input;
  unsigned(collateralAmount, "collateralAmount"); unsigned(debtShares, "debtShares", U128_MAX);
  if (!Number.isSafeInteger(closeBps) || closeBps < 1 || closeBps > 10_000 || collateralAmount === 0n)
    throw new RangeError("A close requires collateral and 1..10000 closeBps");
  const collateralDebit = collateralAmount * BigInt(closeBps) / BPS;
  if (collateralDebit === 0n) throw new RangeError("Close amount rounds to zero");
  const sharesToBurn = ceilDiv(debtShares * collateralDebit, collateralAmount);
  if (closeBps < 10_000 && debtShares > 0n && sharesToBurn >= debtShares)
    throw new RangeError("Partial close would clear all debt shares; choose full close or decrease size");
  const { fullRepayCredit: debtRepayment } = leverageRepaymentQuote({
    positionShares: sharesToBurn, aggregateShares, borrowIndexNad, maxCredit: U64_MAX,
  });
  return { closeBps, collateralDebit, sharesToBurn, debtRepayment,
    remainingCollateral: collateralAmount - collateralDebit, remainingDebtShares: debtShares - sharesToBurn };
}

/** Link a held-token size input to percentage without closing more than asked.
 * The returned actual debit may be smaller because on-chain percentages use bps. */
export function leverageClosePercentage(collateralAmount: bigint, requestedDebit: bigint) {
  unsigned(collateralAmount, "collateralAmount"); unsigned(requestedDebit, "requestedDebit");
  if (collateralAmount === 0n || requestedDebit === 0n || requestedDebit > collateralAmount)
    throw new RangeError("Requested close must be within the position");
  const closeBps = Number(requestedDebit * BPS / collateralAmount);
  if (closeBps === 0) throw new RangeError("Requested size is below one basis point of the position");
  return { closeBps, collateralDebit: collateralAmount * BigInt(closeBps) / BPS };
}

export type LeveragePurchaseQuote = (netDebtInput: bigint) => Promise<bigint>;

/** Link target leverage OR additional held-token size to borrowing. The default
 * target is CURRENT WHOLE-position leverage, never leverage of just the added
 * slice. marginCredit is net wallet funding after its transfer fee.
 *
 * quoteCollateralCredit must be monotone and use one fixed curve snapshot,
 * swap fees and the collateral transfer fee. Borrowed input is internal: do
 * not apply a wallet input transfer fee to it. Cache/local quotes are preferred
 * over a separate RPC per search iteration. This is a sizing estimate at the
 * supplied price basis, not an admission check. Simulate the final instruction
 * with previewLeverageAction for actual post-execution IM/MM, debt and payout.
 */
export async function planLeverageIncrease(input: {
  state: LeverageActionState; marginCredit?: bigint; maxBorrowAmount: bigint;
  targetLeverageBps?: bigint; additionalCollateral?: bigint; quoteCollateralCredit: LeveragePurchaseQuote;
}) {
  const before = leverageActionMetrics(input.state);
  if (before.equity <= 0n || before.collateralAmount === 0n) throw new RangeError("Position must have positive equity and collateral");
  const marginCredit = unsigned(input.marginCredit ?? 0n, "marginCredit");
  let hi = unsigned(input.maxBorrowAmount, "maxBorrowAmount");
  if (marginCredit + hi > U64_MAX) throw new RangeError("Combined input exceeds u64");
  if (input.additionalCollateral !== undefined && input.targetLeverageBps !== undefined)
    throw new Error("Select a target leverage OR an additional collateral size");
  const target = input.targetLeverageBps ?? before.leverageBps!;
  unsigned(target, "targetLeverageBps");
  if (target < BPS) throw new RangeError("Asset-relative leverage must be at least 1x");
  type Candidate = { debtAmount: bigint; marginCredit: bigint; collateralCredit: bigint;
    after: ReturnType<typeof leverageActionMetrics> };
  const cache = new Map<bigint, Candidate>();
  async function candidate(debtAmount: bigint): Promise<Candidate> {
    const cached = cache.get(debtAmount); if (cached) return cached;
    const total = marginCredit + debtAmount;
    const collateralCredit = total === 0n ? 0n : unsigned(await input.quoteCollateralCredit(total), "collateralCredit");
    const collateralAmount = unsigned(before.collateralAmount + collateralCredit, "resultingCollateral");
    const collateralValue = unsigned(collateralAmount * before.collateralValue / before.collateralAmount, "resultingValue");
    const result = { debtAmount, marginCredit, collateralCredit,
      after: leverageActionMetrics({ collateralAmount, collateralValue, debtAmount: unsigned(before.debtAmount + debtAmount, "resultingDebt") }) };
    cache.set(debtAmount, result); return result;
  }
  let lo = 0n;
  if (input.additionalCollateral !== undefined) {
    const wanted = unsigned(input.additionalCollateral, "additionalCollateral");
    if (wanted === 0n || (await candidate(lo)).collateralCredit > wanted)
      throw new RangeError("Size is smaller than the wallet-funded purchase");
    if ((await candidate(hi)).collateralCredit < wanted) throw new RangeError("Size exceeds the borrowing bound");
    while (lo < hi) {
      const mid = (lo + hi) / 2n;
      if ((await candidate(mid)).collateralCredit >= wanted) hi = mid; else lo = mid + 1n;
    }
  } else {
    const fits = (c: Awaited<ReturnType<typeof candidate>>) => c.after.equity > 0n && c.after.collateralValue * BPS <= target * c.after.equity;
    if (!fits(await candidate(0n))) throw new RangeError("Target requires more margin or a separate decrease-size action");
    while (lo < hi) {
      const mid = (lo + hi + 1n) / 2n;
      if (fits(await candidate(mid))) lo = mid; else hi = mid - 1n;
    }
  }
  return { ...await candidate(lo), targetLeverageBps: target,
    atBorrowingBound: lo === input.maxBorrowAmount, requiresSimulation: true as const };
}

/** Find the smallest sale which fully repays debt while retaining collateral.
 * Same-snapshot, monotone quotes must report internal debt-token output after
 * swap fees, before a possible transfer of unused output to the wallet.
 */
export async function planLeverageRepayAll(input: {
  collateralAmount: bigint; maxCollateralDebit: bigint; fullRepayCredit: bigint;
  quoteDebtOutput: (collateralDebit: bigint) => Promise<bigint>;
}) {
  const { collateralAmount, maxCollateralDebit, fullRepayCredit } = input;
  for (const [name, value] of Object.entries({ collateralAmount, maxCollateralDebit, fullRepayCredit })) unsigned(value, name);
  if (fullRepayCredit === 0n || maxCollateralDebit === 0n || maxCollateralDebit >= collateralAmount)
    throw new RangeError("Repay-all requires debt and must leave some collateral");
  let lo = 1n, hi = maxCollateralDebit;
  const quote = async (amount: bigint) => unsigned(await input.quoteDebtOutput(amount), "debtOutput");
  if (await quote(hi) < fullRepayCredit) throw new RangeError("Collateral limit cannot fully repay debt");
  while (lo < hi) { const mid = (lo + hi) / 2n; if (await quote(mid) >= fullRepayCredit) hi = mid; else lo = mid + 1n; }
  const output = await quote(lo);
  return { collateralAmount: lo, minRepayOut: fullRepayCredit,
    maxSurplusOut: output - fullRepayCredit, requiresSimulation: true as const };
}

/** Link a lower whole-position leverage target to the smallest collateral sale.
 * Uses the same sizing-only price basis and fixed-snapshot quote contract as
 * planLeverageIncrease. Exact share repayment determines the refund bound.
 * The supplied maximum must bracket a target-reaching sale; price impact can
 * make a larger endpoint miss even when a smaller sale would work. In that
 * case, narrow the bracket using local quotes rather than inferring insolvency.
 */
export async function planLeverageDecrease(input: {
  state: LeverageActionState; targetLeverageBps: bigint; maxCollateralDebit: bigint;
  positionShares: bigint; aggregateShares: bigint; borrowIndexNad: bigint;
  quoteDebtOutput: (collateralDebit: bigint) => Promise<bigint>;
}) {
  const before = leverageActionMetrics(input.state);
  const target = unsigned(input.targetLeverageBps, "targetLeverageBps");
  let lo = 0n, hi = unsigned(input.maxCollateralDebit, "maxCollateralDebit");
  if (before.leverageBps === null || before.debtFree || target < BPS || target >= before.leverageBps || hi >= before.collateralAmount)
    throw new RangeError("Choose a lower leverage of at least 1x and leave some collateral");
  type Candidate = { collateralAmount: bigint; minRepayOut: bigint; maxSurplusOut: bigint;
    after: ReturnType<typeof leverageActionMetrics> };
  const cache = new Map<bigint, Candidate>();
  async function candidate(debit: bigint): Promise<Candidate> {
    const cached = cache.get(debit); if (cached) return cached;
    const output = debit === 0n ? 0n : unsigned(await input.quoteDebtOutput(debit), "debtOutput");
    let repayment;
    try {
      repayment = leverageRepaymentQuote({ ...input, maxCredit: output });
    } catch (error) {
      if (!(error instanceof RangeError) || !error.message.includes("granularity")) throw error;
      repayment = { cashRepaid: 0n, debtRemaining: before.debtAmount, debtReduced: 0n };
    }
    const collateralAmount = before.collateralAmount - debit;
    const result = { collateralAmount: debit, minRepayOut: repayment.cashRepaid, maxSurplusOut: output - repayment.cashRepaid,
      after: leverageActionMetrics({ collateralAmount, debtAmount: repayment.debtRemaining,
        collateralValue: collateralAmount * before.collateralValue / before.collateralAmount }) };
    cache.set(debit, result); return result;
  }
  const reaches = (c: Candidate) => c.minRepayOut > 0n && c.after.equity > 0n && c.after.collateralValue * BPS <= target * c.after.equity;
  if (!reaches(await candidate(hi))) throw new RangeError("Target leverage is not bracketed at this sale limit; try a smaller limit or inspect local quotes");
  while (lo < hi) { const mid = (lo + hi) / 2n; if (reaches(await candidate(mid))) hi = mid; else lo = mid + 1n; }
  return { ...await candidate(lo), targetLeverageBps: target, requiresSimulation: true as const };
}
