/**
 * Names and symbols for LP tokens.
 *
 * The program writes the names provided at `initialize_market`, and there is
 * no instruction to change them afterwards, so the strings are decided once,
 * here, for every market. No brand in the name: the token is identified by
 * what it is and which market it belongs to.
 *
 *   srLP        name `META/USDC srLP`   symbol `srME.US`
 *   jrLP       name `META/USDC jrLP`  symbol `jrME.US`
 *   base hLP   name `META hLP`        symbol `hMETA`
 *   quote hLP  name `USDC hLP`        symbol `hUSDC`
 *
 * Metaplex caps names at 32 and symbols at 10 bytes. Asset symbols are cleaned
 * and restricted to ASCII before deterministic shortening, so character and
 * byte limits cannot diverge.
 */
import type { MarketLpMintKind } from "./market-bootstrap.js";

export const MAX_LP_NAME_LENGTH = 32;
export const MAX_LP_SYMBOL_LENGTH = 10;
export const MAX_LP_URI_LENGTH = 200;

export interface LpTokenNaming {
  readonly name: string;
  readonly symbol: string;
  /** For the off-chain metadata JSON; not written on chain. */
  readonly description: string;
}

export interface LpTokenNamingParams {
  kind: MarketLpMintKind | "junior";
  baseSymbol: string;
  quoteSymbol: string;
}

function cleanSymbol(symbol: string): string {
  const cleaned = symbol.trim().replace(/^\$+/, "").replace(/\s+/g, "");
  if (cleaned.length === 0) {
    throw new Error("asset symbol is empty");
  }
  if (!/^[\x21-\x7e]+$/.test(cleaned)) {
    throw new Error("asset symbol must contain printable ASCII characters only");
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

  if (params.kind === "srlp" || params.kind === "junior") {
    const junior = params.kind === "junior";
    const symbolPrefix = junior ? "jr" : "sr";
    const nameSuffix = junior ? " jrLP" : " srLP";
    const [na, nb] = fitPair(base, quote, MAX_LP_NAME_LENGTH - 1 - nameSuffix.length);
    const a = base.slice(0, 2).toUpperCase();
    const b = quote.slice(0, 2).toUpperCase();
    return {
      name: `${na}/${nb}${nameSuffix}`,
      symbol: `${symbolPrefix}${a}.${b}`,
      description: junior
        ? `Dusk jrLP for the ${market} market: productive first-loss liquidity with junior income and a notice period before conversion to srLP.`
        : `Dusk srLP for the ${market} market: a senior share of pooled ${base} and ${quote} liquidity, protected by available insurance and junior capital against covered credit losses.`,
    };
  }

  const side = params.kind === "baseHlp" ? base : quote;
  const other = params.kind === "baseHlp" ? quote : base;
  return {
    name: `${side.slice(0, MAX_LP_NAME_LENGTH - 4)} hLP`,
    symbol: `h${side.slice(0, MAX_LP_SYMBOL_LENGTH - 1)}`,
    description: `Dusk hLP for ${side} in the ${market} market: a hedged, 2x leveraged liquidity share funded from the ${other} side.`,
  };
}

/** Naming for all four LP receipt kinds of one market. */
export function marketLpTokenNaming(
  params: Omit<LpTokenNamingParams, "kind">,
): Record<MarketLpMintKind | "junior", LpTokenNaming> {
  return {
    srlp: lpTokenNaming({ ...params, kind: "srlp" }),
    junior: lpTokenNaming({ ...params, kind: "junior" }),
    baseHlp: lpTokenNaming({ ...params, kind: "baseHlp" }),
    quoteHlp: lpTokenNaming({ ...params, kind: "quoteHlp" }),
  };
}

export interface LpTokenMetadataInput {
  name: string;
  symbol: string;
  uri: string;
}

/** Match the on-chain Metaplex input contract before a transaction is built. */
export function assertLpTokenMetadata(metadata: LpTokenMetadataInput): void {
  if (!metadata.name || !/^[\x20-\x7e]+$/.test(metadata.name) || metadata.name.length > MAX_LP_NAME_LENGTH) {
    throw new Error(`LP token name must be printable ASCII and at most ${MAX_LP_NAME_LENGTH} bytes`);
  }
  if (!metadata.symbol || !/^[\x21-\x7e]+$/.test(metadata.symbol) || metadata.symbol.length > MAX_LP_SYMBOL_LENGTH) {
    throw new Error(`LP token symbol must be printable ASCII and at most ${MAX_LP_SYMBOL_LENGTH} bytes`);
  }
  if (
    !metadata.uri.startsWith("http")
    || metadata.uri.length > MAX_LP_URI_LENGTH
    || !/^[\x21-\x7e]+$/.test(metadata.uri)
  ) {
    throw new Error(`LP token URI must start with http and be at most ${MAX_LP_URI_LENGTH} bytes`);
  }
}
