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
import { assertLpTokenMetadata, lpTokenNaming, marketLpTokenNaming } from "../dist/lp-naming.js";
import { createHookedLpMintWithSeedInstructions, lpMintLen } from "../dist/market-bootstrap.js";

// Ground locally with SHA-256 with owner=Token-2022 for this base.
const BASE = new PublicKey("FJWRK3XJeVD8njSvTFXyHwP2jkatvBqeVwcNAFe5zVfJ");
const FIXTURES = {
  srlp: { seed: "0000000000dd71b3", address: "5fkEA1zyUU6ms2XexsUkACuFVedYCZ1MrEjrh6X3srLP" },
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

test("naming follows the sr/jr/h scheme within Metaplex limits", () => {
  const naming = marketLpTokenNaming({ baseSymbol: "META", quoteSymbol: "USDC" });
  assert.deepEqual(
    { ...naming.srlp, description: undefined },
    { name: "META/USDC srLP", symbol: "srME/US", description: undefined }
  );
  assert.equal(naming.junior.name, "META/USDC jrLP");
  assert.equal(naming.junior.symbol, "jrME/US");
  assert.equal(naming.baseHlp.name, "META hLP");
  assert.equal(naming.baseHlp.symbol, "hMETA");
  assert.equal(naming.quoteHlp.name, "USDC hLP");
  assert.equal(naming.quoteHlp.symbol, "hUSDC");
  for (const entry of Object.values(naming)) {
    assert.ok(entry.name.length <= 32 && entry.symbol.length <= 10, `${entry.symbol} fits`);
    assert.match(entry.description, /Dusk/);
  }
});

test("pair tickers always use two uppercase characters and names shorten deterministically", () => {
  const srlp = lpTokenNaming({ kind: "srlp", baseSymbol: "cbBTC", quoteSymbol: "USDC" });
  assert.equal(srlp.symbol, "srCB/US");
  assert.equal(srlp.name, "cbBTC/USDC srLP");
  const junior = lpTokenNaming({ kind: "junior", baseSymbol: "JitoSOL", quoteSymbol: "USDC" });
  assert.equal(junior.symbol, "jrJI/US");
  assert.equal(junior.name, "JitoSOL/USDC jrLP");
  const hlp = lpTokenNaming({ kind: "quoteHlp", baseSymbol: "JitoSOL", quoteSymbol: "USDC" });
  assert.equal(hlp.symbol, "hUSDC");
  assert.equal(hlp.name, "USDC hLP");
  const tight = lpTokenNaming({ kind: "srlp", baseSymbol: "  $WIFHAT  ", quoteSymbol: "BONK" });
  assert.equal(tight.symbol, "srWI/BO");
  const long = lpTokenNaming({ kind: "junior", baseSymbol: "ABCDEFGHIJKLMNOPQRST", quoteSymbol: "ZYXWVUTSRQPONMLKJIHG" });
  assert.equal(long.name, "ABCDEFGHIJKLM/ZYXWVUTSRQPON jrLP");
  assert.ok(long.name.length <= 32);
  assert.throws(() => lpTokenNaming({ kind: "srlp", baseSymbol: " ", quoteSymbol: "USDC" }), /empty/);
  assert.throws(() => lpTokenNaming({ kind: "srlp", baseSymbol: "MÉTA", quoteSymbol: "USDC" }), /ASCII/);
});

test("metadata validation matches the on-chain byte limits", () => {
  assert.doesNotThrow(() => assertLpTokenMetadata({ name: "META/USDC jrLP", symbol: "jrME/US", uri: "https://example.test/junior.json" }));
  assert.throws(() => assertLpTokenMetadata({ name: "x".repeat(33), symbol: "jrME/US", uri: "https://example.test" }), /32 bytes/);
  assert.throws(() => assertLpTokenMetadata({ name: "META/USDC jrLP", symbol: "x".repeat(11), uri: "https://example.test" }), /10 bytes/);
  assert.throws(() => assertLpTokenMetadata({ name: "META/USDC jrLP", symbol: "jrME/US", uri: "ipfs://cid" }), /start with http/);
});

test("seed derivation matches the server fixture and the suffix rule", async () => {
  const mint = await deriveLpMintFromSeed(BASE, FIXTURES.srlp.seed);
  assert.equal(mint.toBase58(), FIXTURES.srlp.address);
  assert.ok(hasLpMintAddressSuffix("srlp", mint));
  assert.ok(!hasLpMintAddressSuffix("baseHlp", mint));
});

test("grindLpMintSeed asks for the Token-2022 owner and verifies the answer", async () => {
  const { fetchImpl, calls } = fakeFetch(() => serverResponse("srlp", FIXTURES.srlp));
  const seed = await grindLpMintSeed({ kind: "srlp", base: BASE, serverUrl: "https://vanity.test/", fetch: fetchImpl });
  assert.equal(seed.mint.toBase58(), FIXTURES.srlp.address);
  assert.equal(calls[0].origin + calls[0].pathname, "https://vanity.test/grind");
  assert.equal(calls[0].searchParams.get("suffix"), "srLP");
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
    const { fetchImpl } = fakeFetch(() => serverResponse("srlp", FIXTURES.srlp, overrides));
    await assert.rejects(
      grindLpMintSeed({ kind: "srlp", base: BASE, serverUrl: "https://vanity.test", fetch: fetchImpl }),
      message
    );
  }
});

test("grindMarketLpMintSeeds grinds one seed per mint kind", async () => {
  const { fetchImpl } = fakeFetch((url) =>
    url.searchParams.get("suffix") === "srLP"
      ? serverResponse("srlp", FIXTURES.srlp)
      : serverResponse("baseHlp", FIXTURES.hlp)
  );
  const seeds = await grindMarketLpMintSeeds({ base: BASE, serverUrl: "https://vanity.test", fetch: fetchImpl });
  assert.equal(seeds.srlp.mint.toBase58(), FIXTURES.srlp.address);
  assert.equal(seeds.baseHlp.mint.toBase58(), FIXTURES.hlp.address);
  assert.equal(seeds.quoteHlp.mint.toBase58(), FIXTURES.hlp.address);
});

test("createHookedLpMintWithSeedInstructions needs only the base and payer to sign", async () => {
  const market = new PublicKey(new Uint8Array(32).fill(7));
  const program = new PublicKey(new Uint8Array(32).fill(9));
  const seeded = await createHookedLpMintWithSeedInstructions({
    payer: BASE,
    base: BASE,
    seed: FIXTURES.srlp.seed,
    mint: FIXTURES.srlp.address,
    decimals: 6,
    mintAuthority: market,
    transferHookProgramId: program,
    lamports: 1_000_000,
  });
  assert.equal(seeded.mint.toBase58(), FIXTURES.srlp.address);
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
      payer: BASE, base: BASE, seed: "wrong", mint: FIXTURES.srlp.address, decimals: 6,
      mintAuthority: market, transferHookProgramId: program, lamports: 1,
    }),
    /does not derive/
  );
});

test("junior naming matches the native generation mint without changing srLP or hLP names", () => {
  const naming = lpTokenNaming({ kind: "junior", baseSymbol: "SOL", quoteSymbol: "USDC" });
  assert.equal(naming.name, "SOL/USDC jrLP");
  assert.equal(naming.symbol, "jrSO/US");
  assert.match(naming.description, /first-loss/);
  assert.equal(marketLpTokenNaming({ baseSymbol: "SOL", quoteSymbol: "USDC" }).srlp.symbol, "srSO/US");
});
