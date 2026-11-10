import { MIN_AMOUNT_DECIMALS, NINE_DECIMAL_AMOUNT_LAYOUT_VERSION } from "./constants.js";
import type { Market } from "./type-aliases.js";

/** Decimal precision of normalized quantities in this market (curve depth,
 * hLP NAV/exposure, and debt/collateral values, including legacy `_nad` fields).
 * Markets created at the nine-decimal amount layout keep a nine-decimal minimum.
 * Prices, interest rates, per-share ratios, and configured launch buy-size
 * references remain scaled by 1e9.
 * Token balances, deposits, withdrawals, fees, and debt amounts use raw atoms.
 */
export function marketAmountDecimals(
  market: Pick<Market, "version" | "baseSide" | "quoteSide">
): number {
  const minimum = market.version === NINE_DECIMAL_AMOUNT_LAYOUT_VERSION ? 9 : MIN_AMOUNT_DECIMALS;
  return Math.max(minimum, market.baseSide.assetDecimals, market.quoteSide.assetDecimals);
}
