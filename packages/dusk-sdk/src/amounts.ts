import type { Market } from "./type-aliases.js";

/** Decimal precision of normalized quantities in this market (curve depth,
 * hLP NAV/exposure, and debt/collateral values, including legacy `_nad` fields).
 * Prices, interest rates, per-share ratios, and configured launch buy-size
 * references remain scaled by 1e9.
 * Token balances, deposits, withdrawals, fees, and debt amounts use raw atoms.
 */
export function marketAmountDecimals(
  market: Pick<Market, "baseSide" | "quoteSide">
): number {
  return Math.max(9, market.baseSide.assetDecimals, market.quoteSide.assetDecimals);
}
