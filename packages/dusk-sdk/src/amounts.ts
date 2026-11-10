import { MARKET_LAYOUT_VERSION, MIN_AMOUNT_DECIMALS } from "./constants.js";
import type { Market } from "./type-aliases.js";

/** Decimal precision of normalized quantities in this market (curve depth,
 * hLP NAV/exposure, and debt/collateral values, including legacy `_nad` fields).
 * Prices, interest rates, per-share ratios, and configured launch buy-size
 * references remain scaled by 1e9.
 * Token balances, deposits, withdrawals, fees, and debt amounts use raw atoms.
 * Throws for a market at any layout other than `MARKET_LAYOUT_VERSION`, whose
 * quantities this scale does not describe.
 */
export function marketAmountDecimals(
  market: Pick<Market, "version" | "baseSide" | "quoteSide">
): number {
  if (market.version !== MARKET_LAYOUT_VERSION) {
    throw new Error(
      `Unsupported Dusk market layout ${market.version}; expected ${MARKET_LAYOUT_VERSION}`
    );
  }
  return Math.max(MIN_AMOUNT_DECIMALS, market.baseSide.assetDecimals, market.quoteSide.assetDecimals);
}
