/**
 * Vanity addresses for LP mints.
 *
 * Production builds of the program accept an LP mint only if its address ends
 * in `yLP` (the yLP mint) or `hLP` (either hLP mint); the check is an exact,
 * case-sensitive comparison on the base58 string. Rather than grinding
 * keypairs, the mint is created at a `create_with_seed` address, and what is
 * ground is the seed: `sha256(base ‖ seed ‖ Token-2022)` until the encoding
 * ends in the suffix. The creator's own key is the base, so nothing new has
 * to be kept secret and nothing new has to sign.
 *
 * The grinding itself runs on the vanity server (omnipair/vanity-server), the
 * same service the V1 app used for `omLP`. Three characters cost about 200k
 * attempts, well under a second there. Every result is re-derived locally
 * before it is trusted.
 */
import { TOKEN_2022_PROGRAM_ID } from "@solana/spl-token";
import { PublicKey } from "@solana/web3.js";

import { address, type AddressLike } from "./address.js";
import type { MarketLpMintKind } from "./market-bootstrap.js";

/** The address suffix `initialize_market` requires for each LP mint kind. */
export const LP_MINT_ADDRESS_SUFFIX: Readonly<Record<MarketLpMintKind, string>> = {
  ylp: "yLP",
  baseHlp: "hLP",
  quoteHlp: "hLP",
};

export const DEFAULT_VANITY_SERVER_URL = "https://vanity.omnipair.fi";

/** Whether `mint` satisfies the production address rule for `kind`. */
export function hasLpMintAddressSuffix(kind: MarketLpMintKind, mint: AddressLike): boolean {
  return address(mint).toBase58().endsWith(LP_MINT_ADDRESS_SUFFIX[kind]);
}

/** The Token-2022 account address `base` and `seed` create. */
export function deriveLpMintFromSeed(base: AddressLike, seed: string): Promise<PublicKey> {
  return PublicKey.createWithSeed(address(base), seed, TOKEN_2022_PROGRAM_ID);
}

export interface LpMintSeed {
  readonly kind: MarketLpMintKind;
  readonly base: PublicKey;
  readonly seed: string;
  readonly mint: PublicKey;
}

/** What `GET /grind` on the vanity server returns. */
interface VanityGrindResponse {
  address?: unknown;
  seed?: unknown;
  base?: unknown;
  owner?: unknown;
  suffix?: unknown;
  error?: unknown;
}

export interface GrindLpMintSeedOptions {
  kind: MarketLpMintKind;
  /** The creator's key: it signs `createAccountWithSeed`, and it is hashed into the address. */
  base: AddressLike;
  serverUrl?: string;
  /** Overall deadline for one request. The server itself never times out. */
  timeoutMs?: number;
  fetch?: typeof fetch;
}

/**
 * Ask the vanity server for a seed whose Token-2022 address ends in the
 * suffix for `kind`, and verify the answer before returning it.
 */
export async function grindLpMintSeed(options: GrindLpMintSeedOptions): Promise<LpMintSeed> {
  const base = address(options.base);
  const suffix = LP_MINT_ADDRESS_SUFFIX[options.kind];
  const serverUrl = (options.serverUrl ?? DEFAULT_VANITY_SERVER_URL).replace(/\/+$/, "");
  const url = new URL(`${serverUrl}/grind`);
  url.searchParams.set("base", base.toBase58());
  url.searchParams.set("suffix", suffix);
  url.searchParams.set("owner", TOKEN_2022_PROGRAM_ID.toBase58());

  const doFetch = options.fetch ?? globalThis.fetch;
  if (typeof doFetch !== "function") {
    throw new Error("grindLpMintSeed needs a fetch implementation");
  }
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), options.timeoutMs ?? 120_000);
  let body: VanityGrindResponse;
  try {
    const response = await doFetch(url, { signal: controller.signal });
    body = (await response.json().catch(() => ({}))) as VanityGrindResponse;
    if (!response.ok) {
      throw new Error(
        `vanity server ${response.status}: ${typeof body.error === "string" ? body.error : "grind failed"}`
      );
    }
  } finally {
    clearTimeout(timer);
  }

  if (typeof body.seed !== "string" || typeof body.address !== "string") {
    throw new Error("vanity server returned no seed");
  }
  // The server drops a suffix it cannot parse and grinds unconstrained; it
  // echoes what it actually used, so an echo that differs means the result is
  // not what was asked for.
  if (body.suffix !== suffix) {
    throw new Error(`vanity server ground suffix ${JSON.stringify(body.suffix)}, not ${suffix}`);
  }
  if (body.owner !== TOKEN_2022_PROGRAM_ID.toBase58()) {
    throw new Error(`vanity server ground for owner ${String(body.owner)}, not Token-2022`);
  }
  const mint = await deriveLpMintFromSeed(base, body.seed);
  if (mint.toBase58() !== body.address) {
    throw new Error(
      `vanity server address ${body.address} does not derive from base ${base.toBase58()} and seed "${body.seed}"`
    );
  }
  if (!hasLpMintAddressSuffix(options.kind, mint)) {
    throw new Error(`derived mint ${mint.toBase58()} does not end in ${suffix}`);
  }
  return { kind: options.kind, base, seed: body.seed, mint };
}

/** One seed per LP mint a market needs, ground for the same base. */
export async function grindMarketLpMintSeeds(
  options: Omit<GrindLpMintSeedOptions, "kind">
): Promise<Record<MarketLpMintKind, LpMintSeed>> {
  const [ylp, baseHlp, quoteHlp] = await Promise.all([
    grindLpMintSeed({ ...options, kind: "ylp" }),
    grindLpMintSeed({ ...options, kind: "baseHlp" }),
    grindLpMintSeed({ ...options, kind: "quoteHlp" }),
  ]);
  return { ylp, baseHlp, quoteHlp };
}
