import assert from "node:assert/strict";
import { test } from "node:test";
import { PublicKey, Transaction } from "@solana/web3.js";
import { TOKEN_2022_PROGRAM_ID } from "@solana/spl-token";

import {
  LP_MINT_ADDRESS_SUFFIX,
  deriveLpMintFromSeed,
  grindLpMintSeed,
  grindMarketLpMintSeeds,
  hasLpMintAddressSuffix,
} from "../dist/lp-vanity.js";
import { lpTokenNaming, marketLpTokenNaming } from "../dist/lp-naming.js";
import { createHookedLpMintWithSeedInstructions, lpMintLen } from "../dist/market-bootstrap.js";

// Ground on vanity-server 0.5.1 with owner=Token-2022 for this base.
const BASE = new PublicKey("FJWRK3XJeVD8njSvTFXyHwP2jkatvBqeVwcNAFe5zVfJ");
const FIXTURES = {
  ylp: { seed: "CAFVyJTUFnDKsF1R", address: "6SZ2QSA2D3RCdxDGE8UniJGf5riF1i9czvN53vcCryLP" },
  hlp: { seed: "v9JI55Ir6NP3fMRy", address: "36V21WhkSc48ZD1kkGQmECyYwvSMexhtCw2FqKprehLP" },
};

function serverResponse(kind, fixture, overrides = {}) {
  return {
    address: fixture.address,
    seed: fixture.seed,
    seed_bytes: Array.from(Buffer.from(fixture.seed)),
    base: BASE.toBase58(),
    owner: TOKEN_2022_PROGRAM_ID.toBase58(),
    prefix: null,
    suffix: LP_MINT_ADDRESS_SUFFIX[kind],
    case_insensitive: false,
    attempts: 1,
    duration_seconds: 0.01,
    attempts_per_second: 100,
    ...overrides,
  };
}

function fakeFetch(handler) {
  const calls = [];
  const fetchImpl = async (url) => {
    calls.push(new URL(String(url)));
    const body = handler(new URL(String(url)));
    return {
      ok: body.status === undefined || body.status < 400,
      status: body.status ?? 200,
      json: async () => body,
    };
  };
  return { fetchImpl, calls };
}

test("naming follows the y/h pair scheme within Metaplex limits", () => {
  const naming = marketLpTokenNaming({ baseSymbol: "META", quoteSymbol: "USDC" });
  assert.deepEqual(
    { ...naming.ylp, description: undefined },
    { name: "yMETA/USDC LP", symbol: "yMETA.USDC", description: undefined }
  );
  assert.equal(naming.baseHlp.name, "hMETA");
  assert.equal(naming.baseHlp.symbol, "hMETA.USDC");
  assert.equal(naming.quoteHlp.name, "hUSDC");
  assert.equal(naming.quoteHlp.symbol, "hUSDC.META");
  for (const entry of Object.values(naming)) {
    assert.ok(entry.name.length <= 32 && entry.symbol.length <= 10, `${entry.symbol} fits`);
    assert.match(entry.description, /Dusk/);
  }
});

test("long asset symbols are trimmed longest-first until the symbol fits", () => {
  const ylp = lpTokenNaming({ kind: "ylp", baseSymbol: "cbBTC", quoteSymbol: "USDC" });
  assert.equal(ylp.symbol, "ycbBT.USDC");
  assert.equal(ylp.name, "ycbBTC/USDC LP");
  const hlp = lpTokenNaming({ kind: "quoteHlp", baseSymbol: "JitoSOL", quoteSymbol: "USDC" });
  assert.equal(hlp.symbol, "hUSDC.Jito");
  assert.equal(hlp.name, "hUSDC");
  const tight = lpTokenNaming({ kind: "ylp", baseSymbol: "  $WIFHAT  ", quoteSymbol: "BONK" });
  assert.equal(tight.symbol, "yWIFH.BONK");
  assert.throws(() => lpTokenNaming({ kind: "ylp", baseSymbol: " ", quoteSymbol: "USDC" }), /empty/);
});

test("seed derivation matches the server fixture and the suffix rule", async () => {
  const mint = await deriveLpMintFromSeed(BASE, FIXTURES.ylp.seed);
  assert.equal(mint.toBase58(), FIXTURES.ylp.address);
  assert.ok(hasLpMintAddressSuffix("ylp", mint));
  assert.ok(!hasLpMintAddressSuffix("baseHlp", mint));
});

test("grindLpMintSeed asks for the Token-2022 owner and verifies the answer", async () => {
  const { fetchImpl, calls } = fakeFetch(() => serverResponse("ylp", FIXTURES.ylp));
  const seed = await grindLpMintSeed({ kind: "ylp", base: BASE, serverUrl: "https://vanity.test/", fetch: fetchImpl });
  assert.equal(seed.mint.toBase58(), FIXTURES.ylp.address);
  assert.equal(calls[0].origin + calls[0].pathname, "https://vanity.test/grind");
  assert.equal(calls[0].searchParams.get("suffix"), "yLP");
  assert.equal(calls[0].searchParams.get("owner"), TOKEN_2022_PROGRAM_ID.toBase58());
  assert.equal(calls[0].searchParams.get("base"), BASE.toBase58());
});

test("grindLpMintSeed rejects an unconstrained, wrong-owner, or mismatched result", async () => {
  const cases = [
    [{ suffix: null }, /ground suffix null/],
    [{ owner: "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA" }, /not Token-2022/],
    [{ seed: "somethingelse01" }, /does not derive/],
    [{ status: 400, error: "Invalid base pubkey in query parameter" }, /vanity server 400/],
  ];
  for (const [overrides, message] of cases) {
    const { fetchImpl } = fakeFetch(() => serverResponse("ylp", FIXTURES.ylp, overrides));
    await assert.rejects(
      grindLpMintSeed({ kind: "ylp", base: BASE, serverUrl: "https://vanity.test", fetch: fetchImpl }),
      message
    );
  }
});

test("grindMarketLpMintSeeds grinds one seed per mint kind", async () => {
  const { fetchImpl } = fakeFetch((url) =>
    url.searchParams.get("suffix") === "yLP"
      ? serverResponse("ylp", FIXTURES.ylp)
      : serverResponse("baseHlp", FIXTURES.hlp)
  );
  const seeds = await grindMarketLpMintSeeds({ base: BASE, serverUrl: "https://vanity.test", fetch: fetchImpl });
  assert.equal(seeds.ylp.mint.toBase58(), FIXTURES.ylp.address);
  assert.equal(seeds.baseHlp.mint.toBase58(), FIXTURES.hlp.address);
  assert.equal(seeds.quoteHlp.mint.toBase58(), FIXTURES.hlp.address);
});

test("createHookedLpMintWithSeedInstructions needs only the base and payer to sign", async () => {
  const market = new PublicKey(new Uint8Array(32).fill(7));
  const program = new PublicKey(new Uint8Array(32).fill(9));
  const seeded = await createHookedLpMintWithSeedInstructions({
    payer: BASE,
    base: BASE,
    seed: FIXTURES.ylp.seed,
    mint: FIXTURES.ylp.address,
    decimals: 6,
    mintAuthority: market,
    transferHookProgramId: program,
    lamports: 1_000_000,
  });
  assert.equal(seeded.mint.toBase58(), FIXTURES.ylp.address);
  assert.equal(seeded.instructions.length, 3);
  const signers = new Set(
    seeded.instructions.flatMap((ix) => ix.keys.filter((k) => k.isSigner).map((k) => k.pubkey.toBase58()))
  );
  assert.deepEqual([...signers], [BASE.toBase58()]);
  const create = seeded.instructions[0];
  assert.ok(create.keys.some((k) => k.pubkey.equals(seeded.mint) && k.isWritable && !k.isSigner));
  const tx = new Transaction().add(...seeded.instructions);
  tx.feePayer = BASE;
  tx.recentBlockhash = BASE.toBase58();
  assert.ok(tx.serializeMessage().length < 1232);
  assert.ok(lpMintLen() > 82);
  await assert.rejects(
    createHookedLpMintWithSeedInstructions({
      payer: BASE, base: BASE, seed: "wrong", mint: FIXTURES.ylp.address, decimals: 6,
      mintAuthority: market, transferHookProgramId: program, lamports: 1,
    }),
    /does not derive/
  );
});
