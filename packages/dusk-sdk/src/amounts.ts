import { MIN_AMOUNT_DECIMALS } from "./constants.js";
import { assertCurrentMarketLayout } from "./market-layout.js";
import type { Market } from "./type-aliases.js";

/** Decimal precision of normalized quantities in this market (curve depth,
 * hLP NAV/exposure, and debt/collateral values, including legacy `_nad` fields).
 * Prices, interest rates, per-share ratios, and configured launch buy-size
 * references remain scaled by 1e9.
 * Token balances, deposits, withdrawals, fees, and debt amounts use raw atoms.
 * Throws `DuskUnsupportedMarketLayoutError` for a market at any layout other
 * than `MARKET_LAYOUT_VERSION`, whose quantities this scale does not describe.
 */
export function marketAmountDecimals(
  market: Pick<Market, "version" | "baseSide" | "quoteSide">
): number {
  assertCurrentMarketLayout(market);
  return Math.max(MIN_AMOUNT_DECIMALS, market.baseSide.assetDecimals, market.quoteSide.assetDecimals);
}
