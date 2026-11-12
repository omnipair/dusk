import { MARKET_LAYOUT_VERSION } from "./constants.js";
import type { Market } from "./type-aliases.js";

/** A Market account at a layout this SDK does not read. Its stored
 * quantities use another scale, so it is rejected rather than decoded as a
 * current market. */
export class DuskUnsupportedMarketLayoutError extends Error {
  readonly expected = MARKET_LAYOUT_VERSION;

  constructor(readonly version: number) {
    super(`Unsupported Dusk market layout ${version}; expected ${MARKET_LAYOUT_VERSION}`);
    this.name = "DuskUnsupportedMarketLayoutError";
  }
}

/** Returns `market` when it is at `MARKET_LAYOUT_VERSION`, the only layout
 * this SDK reads, and throws `DuskUnsupportedMarketLayoutError` otherwise.
 * Every typed Market read in the SDK applies it. Raw account bytes, such as
 * `DuskGet.accountInfo`, are returned unchecked. */
export function assertCurrentMarketLayout<T extends Pick<Market, "version">>(market: T): T {
  if (market.version !== MARKET_LAYOUT_VERSION) {
    throw new DuskUnsupportedMarketLayoutError(market.version);
  }
  return market;
}
