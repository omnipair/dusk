import type { DuskVirtualBookSnapshot } from "./market.js";

export interface VirtualBookLevel {
  price: number;
  size: number;
  total: number;
  quoteSize: number;
  quoteTotal: number;
  averagePrice: number;
  impactPercent: number;
  feePercent: number;
  surchargePercent: number;
}
export interface VirtualBookView {
  market: string;
  baseMint: string;
  quoteMint: string;
  mid: number;
  step: number;
  bids: VirtualBookLevel[];
  asks: VirtualBookLevel[];
  slot: number;
  concentrated: boolean;
}

const NAD = 1_000_000_000n;
const n = (value: { toString(): string }) => BigInt(value.toString());
type Layer = { liquidity: bigint; lower: bigint; upper: bigint };

/** Sampling projection of curve.rs::nested_point_at_sqrt_price (math revision 1).
 * Inventory deltas select cumulative inputs for native swap previews. These
 * pre-fee samples are never rendered as the user-facing book.
 */
export function virtualCurvePoint(tail: bigint, layers: readonly Layer[], sqrt: bigint) {
  if (tail <= 0n || sqrt <= 0n) throw new Error("Invalid depth curve");
  let base = (tail * NAD) / sqrt,
    quote = (tail * sqrt) / NAD;
  for (const { liquidity: l, lower, upper } of layers) {
    if (l < 0n || lower <= 0n || upper <= lower) throw new Error("Invalid depth range");
    const s = sqrt < lower ? lower : sqrt > upper ? upper : sqrt;
    base += (l * NAD) / s - (l * NAD) / upper;
    quote += (l * s) / NAD - (l * lower) / NAD;
  }
  return { base, quote };
}

export function projectDuskVirtualBookCurve(
  snapshot: DuskVirtualBookSnapshot | null,
  groupingBps = 10,
  count = 12
): VirtualBookView | null {
  if (!snapshot) return null;
  if (
    ![5, 10, 25, 50, 100].includes(groupingBps) ||
    !Number.isInteger(count) ||
    count < 1 ||
    count > 50
  )
    throw new Error("Invalid depth grouping");
  const { account, preview } = snapshot;
  const c = account.amm.concentratedCurveCache;
  if (c.mathRevision !== 1) throw new Error("Unknown depth curve revision");
  const base = n(preview.amm.ordinaryBaseReserveNad);
  const quote = n(preview.amm.ordinaryQuoteReserveNad);
  if (base <= 0n || quote <= 0n) return null;
  const concentrated = n(c.concentratedLiquidity);
  let tail = n(c.tailLiquidity);
  if (tail <= 0n || concentrated < 0n) throw new Error("Invalid depth curve");
  if (concentrated === 0n) {
    // curve.rs::effective_reserves/quote_exact_in uses the live reserve
    // product for CPMM. Conservative output rounding can grow that product
    // without changing the cached liquidity. Reconstruct sqrt(x*y) instead
    // of treating the original cache as the current trading invariant.
    const product = base * quote;
    let root = product;
    let next = (root + 1n) / 2n;
    while (next < root) {
      root = next;
      next = (root + product / root) / 2n;
    }
    tail = root;
  }
  const layers: Layer[] =
    concentrated === 0n
      ? []
      : c.fadeWidthBps === 0
        ? [
            {
              liquidity: concentrated,
              lower: n(c.coreLowerSqrtPriceNad),
              upper: n(c.coreUpperSqrtPriceNad),
            },
          ]
        : [
            {
              liquidity: (concentrated + 1n) / 2n,
              lower: n(c.coreLowerSqrtPriceNad),
              upper: n(c.coreUpperSqrtPriceNad),
            },
            {
              liquidity: concentrated / 2n,
              lower: n(c.outerLowerSqrtPriceNad),
              upper: n(c.outerUpperSqrtPriceNad),
            },
          ];
  const point = (sqrt: bigint) => virtualCurvePoint(tail, layers, sqrt);
  // Invert the current ordinary base inventory. Lending EMA / last fill are
  // not the current AMM marginal price and cannot anchor this book.
  let lo = 1n,
    hi = NAD;
  for (let i = 0; point(hi).base > base; i++) {
    if (i > 128) throw new Error("Depth curve is outside supported domain");
    hi *= 2n;
  }
  while (hi - lo > 1n) {
    const middle = (lo + hi) / 2n;
    if (point(middle).base > base) lo = middle;
    else hi = middle;
  }
  const anchor = point(hi);
  // A different curve implementation must fail closed, not draw plausible depth.
  const difference = anchor.quote > quote ? anchor.quote - quote : quote - anchor.quote;
  if (difference > quote / 100_000n + 100n)
    throw new Error("Depth curve does not match the current reserves");
  const mid = (Number(hi) / 1e9) ** 2;
  const step = (mid * groupingBps) / 10_000;
  const cashBase =
    Number(preview.base.cashReserve.toString()) / 10 ** account.baseSide.assetDecimals;
  const cashQuote =
    Number(preview.quote.cashReserve.toString()) / 10 ** account.quoteSide.assetDecimals;
  const cashBaseNad =
    (n(preview.base.cashReserve) * NAD) / 10n ** BigInt(account.baseSide.assetDecimals);
  const cashQuoteNad =
    (n(preview.quote.cashReserve) * NAD) / 10n ** BigInt(account.quoteSide.assetDecimals);
  const side = (direction: 1 | -1) => {
    const levels: VirtualBookLevel[] = [];
    let previousBase = 0,
      previousQuote = 0;
    for (let i = 1; i <= count; i++) {
      let price = mid + direction * step * i;
      if (!(price > 0)) break;
      let targetSqrt = BigInt(Math.max(1, Math.floor(Math.sqrt(price) * 1e9)));
      const exceedsCash = (s: bigint) => {
        const p = point(s);
        return direction === 1
          ? anchor.base - p.base > cashBaseNad
          : anchor.quote - p.quote > cashQuoteNad;
      };
      const clipped = exceedsCash(targetSqrt);
      if (clipped) {
        // Include the final partial bucket instead of hiding executable cash
        // whenever it is smaller than the selected price step.
        let lower = direction === 1 ? hi : targetSqrt;
        let upper = direction === 1 ? targetSqrt : hi;
        while (upper - lower > 1n) {
          const middle = (lower + upper) / 2n;
          if (exceedsCash(middle) === (direction === 1)) upper = middle;
          else lower = middle;
        }
        targetSqrt = direction === 1 ? lower : upper;
        price = (Number(targetSqrt) / 1e9) ** 2;
      }
      const target = point(targetSqrt);
      const total =
        Number(direction === 1 ? anchor.base - target.base : target.base - anchor.base) / 1e9;
      const quoteTotal =
        Number(direction === 1 ? target.quote - anchor.quote : anchor.quote - target.quote) / 1e9;
      // Never show borrowed/virtual inventory as withdrawable output cash.
      if (
        total <= previousBase ||
        quoteTotal <= previousQuote ||
        (direction === 1 && total > cashBase) ||
        (direction === -1 && quoteTotal > cashQuote)
      )
        break;
      const averagePrice = quoteTotal / total;
      levels.push({
        price,
        size: total - previousBase,
        total,
        quoteSize: quoteTotal - previousQuote,
        quoteTotal,
        averagePrice,
        impactPercent: Math.abs(averagePrice / mid - 1) * 100,
        feePercent: 0,
        surchargePercent: 0,
      });
      previousBase = total;
      previousQuote = quoteTotal;
      if (clipped) break;
    }
    return levels;
  };
  if (![mid, step, cashBase, cashQuote].every(Number.isFinite))
    throw new Error("Nonfinite depth values");
  return {
    market: snapshot.market,
    baseMint: account.baseSide.assetMint.toBase58(),
    quoteMint: account.quoteSide.assetMint.toBase58(),
    mid,
    step,
    bids: side(-1),
    asks: side(1),
    slot: snapshot.slot,
    concentrated: concentrated > 0n,
  };
}

/** Native cumulative swap quotes are alternatives against unchanged market state.
 * Differences between adjacent cumulative quotes form the displayed price levels;
 * this includes nonlinear fees without charging each bucket as a separate swap.
 */
export function projectDuskVirtualBook(
  snapshot: import("./quotes.js").DuskVirtualBookQuotes | null
): VirtualBookView | null {
  if (!snapshot) return null;
  const { account, quotes, mid, groupingBps } = snapshot;
  const side = (name: "bids" | "asks") => {
    let previousBase = 0,
      previousQuote = 0;
    const rows: VirtualBookLevel[] = [];
    for (const quote of quotes[name]) {
      const input = n(quote.preview.exactAssetIn);
      const output = n(quote.preview.amountOut) - quote.outputTransferFee;
      const total = Number(name === "bids" ? input : output) / 10 ** account.baseSide.assetDecimals;
      const quoteTotal =
        Number(name === "bids" ? output : input) / 10 ** account.quoteSide.assetDecimals;
      const size = total - previousBase;
      const quoteSize = quoteTotal - previousQuote;
      // A larger input that buys no additional output cannot add book depth.
      if (size <= 0 || quoteSize <= 0) break;
      const averagePrice = quoteTotal / total;
      const row = {
        price: quoteSize / size,
        size,
        total,
        quoteSize,
        quoteTotal,
        averagePrice,
        impactPercent: Math.abs(averagePrice / mid - 1) * 100,
        feePercent: Number(quote.preview.totalFeeRateNad.toString()) / 1e7,
        surchargePercent:
          Number(n(quote.preview.divergenceFeeRateNad) + n(quote.preview.volatilityFeeRateNad)) /
          1e7,
      };
      if (!Object.values(row).every(Number.isFinite))
        throw new Error("Nonfinite native depth values");
      rows.push(row);
      previousBase = total;
      previousQuote = quoteTotal;
    }
    return rows;
  };
  return {
    market: snapshot.market,
    baseMint: account.baseSide.assetMint.toBase58(),
    quoteMint: account.quoteSide.assetMint.toBase58(),
    mid,
    step: (mid * groupingBps) / 10_000,
    bids: side("bids"),
    asks: side("asks"),
    slot: snapshot.slot,
    concentrated: n(account.amm.concentratedCurveCache.concentratedLiquidity) > 0n,
  };
}
