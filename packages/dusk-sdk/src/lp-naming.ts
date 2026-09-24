/**
 * Names and symbols for LP tokens.
 *
 * The program writes whatever `initialize_lp_metadata` is given, and there is
 * no instruction to change it afterwards, so the strings are decided once,
 * here, for every market. No brand in the name: the token is identified by
 * what it is and which market it belongs to.
 *
 *   yLP        name `yMETA/USDC LP`   symbol `yMETA.USDC`
 *   base hLP   name `hMETA`           symbol `hMETA.USDC`
 *   quote hLP  name `hUSDC`           symbol `hUSDC.META`
 *
 * The hLP symbol leads with the side the token is exposed to, then the other
 * asset of the market. Metaplex caps names at 32 and symbols at 10 bytes, so
 * long asset symbols are trimmed, longest first, until the pair fits.
 */
import type { MarketLpMintKind } from "./market-bootstrap.js";

export const MAX_LP_NAME_LENGTH = 32;
export const MAX_LP_SYMBOL_LENGTH = 10;

export interface LpTokenNaming {
  readonly name: string;
  readonly symbol: string;
  /** For the off-chain metadata JSON; not written on chain. */
  readonly description: string;
}

export interface LpTokenNamingParams {
  kind: MarketLpMintKind;
  baseSymbol: string;
  quoteSymbol: string;
}

function cleanSymbol(symbol: string): string {
  const cleaned = symbol.trim().replace(/^\$+/, "").replace(/\s+/g, "");
  if (cleaned.length === 0) {
    throw new Error("asset symbol is empty");
  }
  return cleaned;
}

/** Trim two symbols, longest first, until their combined length fits. */
function fitPair(first: string, second: string, budget: number): [string, string] {
  let a = first;
  let b = second;
  while (a.length + b.length > budget) {
    if (a.length >= b.length && a.length > 1) {
      a = a.slice(0, -1);
    } else if (b.length > 1) {
      b = b.slice(0, -1);
    } else {
      throw new Error(`cannot fit "${first}" and "${second}" into ${budget} characters`);
    }
  }
  return [a, b];
}

export function lpTokenNaming(params: LpTokenNamingParams): LpTokenNaming {
  const base = cleanSymbol(params.baseSymbol);
  const quote = cleanSymbol(params.quoteSymbol);
  const market = `${base}/${quote}`;

  if (params.kind === "ylp") {
    // `y` + A + `.` + B
    const [a, b] = fitPair(base, quote, MAX_LP_SYMBOL_LENGTH - 2);
    // `y` + A + `/` + B + ` LP`
    const [na, nb] = fitPair(base, quote, MAX_LP_NAME_LENGTH - 5);
    return {
      name: `y${na}/${nb} LP`,
      symbol: `y${a}.${b}`,
      description: `Dusk yLP for the ${market} market: a share of the pooled ${base} and ${quote} liquidity that earns swap fees and lending interest.`,
    };
  }

  const side = params.kind === "baseHlp" ? base : quote;
  const other = params.kind === "baseHlp" ? quote : base;
  const [a, b] = fitPair(side, other, MAX_LP_SYMBOL_LENGTH - 2);
  return {
    name: `h${side.slice(0, MAX_LP_NAME_LENGTH - 1)}`,
    symbol: `h${a}.${b}`,
    description: `Dusk hLP for ${side} in the ${market} market: a hedged, 2x leveraged liquidity share funded from the ${other} side.`,
  };
}

/** Naming for all three LP mints of one market. */
export function marketLpTokenNaming(params: Omit<LpTokenNamingParams, "kind">): Record<MarketLpMintKind, LpTokenNaming> {
  return {
    ylp: lpTokenNaming({ ...params, kind: "ylp" }),
    baseHlp: lpTokenNaming({ ...params, kind: "baseHlp" }),
    quoteHlp: lpTokenNaming({ ...params, kind: "quoteHlp" }),
  };
}
