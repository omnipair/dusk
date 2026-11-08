import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { PublicKey } from "@solana/web3.js";
import { marketLpTokenNaming } from "../../packages/dusk-sdk/dist/index.js";
import { publishLpMetadata, type PublishLpMetadataParams } from "./publish.ts";
import { pinataConfigFromEnv, pinFile } from "./pinata.ts";

const key = (n: number) => new PublicKey(new Uint8Array(32).fill(n));
const endpoint = "https://app.example/api/lp-metadata";
function fixture() {
  const outDir = mkdtempSync(join(tmpdir(), "dusk-lp-metadata-"));
  const naming = marketLpTokenNaming({ baseSymbol: "META", quoteSymbol: "USDC" });
  const params: PublishLpMetadataParams = { connection: {} as PublishLpMetadataParams["connection"], network: "devnet", market: key(1), baseMint: key(2), quoteMint: key(3),
    baseSymbol: "META", quoteSymbol: "USDC", mints: { srlp: key(4), baseHlp: key(5), quoteHlp: key(6), junior: key(7) }, naming,
    pinata: { metadataEndpoint: endpoint, gateway: "https://ipfs.omnipair.fi" }, outDir };
  const uris = Object.fromEntries(Object.keys(params.mints).map(kind => [kind, `https://ipfs.omnipair.fi/ipfs/${kind}`]));
  return { params, naming, uris, outDir };
}

test("uses the constrained pair API, verifies every document and preserves base/quote roles", async () => {
  const f = fixture(), previous = globalThis.fetch;
  let requests = 0;
  globalThis.fetch = async (input, init) => {
    const url = String(input);
    if (url === endpoint) {
      requests++;
      assert.deepEqual(JSON.parse(String(init?.body)), { network: "devnet", base: f.params.baseMint.toBase58(), quote: f.params.quoteMint.toBase58() });
      return Response.json({ uris: f.uris, naming: f.naming });
    }
    const kind = Object.keys(f.uris).find(kind => f.uris[kind] === url)!;
    assert.ok(kind, "only the returned document URIs are fetched");
    return Response.json({ ...f.naming[kind as keyof typeof f.naming], image: "https://ipfs.omnipair.fi/ipfs/image" });
  };
  try {
    const result = await publishLpMetadata(f.params);
    assert.equal(requests, 1);
    assert.equal(result.baseHlp.json.name, "META hLP");
    assert.equal(result.quoteHlp.json.symbol, "hUSDC");
    assert.equal(result.junior?.uri, f.uris.junior);
    assert.equal(JSON.parse(readFileSync(join(f.outDir, "srlp.json"), "utf8")).name, "META/USDC srLP");
  } finally { globalThis.fetch = previous; rmSync(f.outDir, { recursive: true, force: true }); }
});

for (const mismatch of ["response", "document"] as const) test(`rejects a ticker mismatch in the ${mismatch}`, async () => {
  const f = fixture(), previous = globalThis.fetch;
  globalThis.fetch = async input => {
    if (String(input) === endpoint) return Response.json({ uris: f.uris, naming: { ...f.naming, ...(mismatch === "response" ? { srlp: { name: "wrong", symbol: "wrong" } } : {}) } });
    return Response.json({ ...f.naming.srlp, name: "wrong", image: "https://ipfs.omnipair.fi/ipfs/image" });
  };
  try { await assert.rejects(publishLpMetadata(f.params), mismatch === "response" ? /ticker mismatch/ : /document mismatch/); }
  finally { globalThis.fetch = previous; rmSync(f.outDir, { recursive: true, force: true }); }
});

test("reports unavailable metadata without requesting unrestricted upload credentials", async () => {
  const f = fixture(), previous = globalThis.fetch;
  globalThis.fetch = async input => { assert.equal(String(input), endpoint); return Response.json({ error: "Unavailable" }, { status: 503 }); };
  try { await assert.rejects(publishLpMetadata(f.params), /503: Unavailable/); }
  finally { globalThis.fetch = previous; rmSync(f.outDir, { recursive: true, force: true }); }
});

test("defaults to the app's pair API and reserves arbitrary standalone uploads for JWT holders", async () => {
  const config = pinataConfigFromEnv({});
  assert.equal(config.metadataEndpoint, "https://beta.omnipair.fi/api/lp-metadata");
  assert.equal(pinataConfigFromEnv({ PINATA_JWT: "fixture" }).metadataEndpoint, undefined);
  await assert.rejects(pinFile(config, { name: "arbitrary.json", bytes: new Uint8Array(), contentType: "application/json", keyvalues: {} }), /Standalone file uploads require PINATA_JWT/);
});
