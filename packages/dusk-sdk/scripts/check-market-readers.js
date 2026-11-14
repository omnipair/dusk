import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { Connection, PublicKey } from "@solana/web3.js";
import { Dusk, DuskUnsupportedMarketLayoutError, MARKET_LAYOUT_VERSION } from "../dist/index.js";

// The derived fixture holds a complete Market account at the current layout.
// Each case rewrites only its version byte, so every reader sees otherwise
// valid current-format bytes.
const fixture = JSON.parse(
  readFileSync(new URL("./fixtures/virtual-book-batch-layout17.json", import.meta.url))
);
const original = Buffer.from(fixture.result.value.accounts[0].data[0], "base64");
const programId = new PublicKey(fixture.programId);
const marketKey = new PublicKey(fixture.market);
const UNSUPPORTED = [0, MARKET_LAYOUT_VERSION - 1, MARKET_LAYOUT_VERSION + 1, 255];

function setup() {
  const connection = new Connection("http://localhost:8899", "confirmed");
  const dusk = new Dusk({ connection, programId: fixture.programId });
  const layout = dusk.program.coder.accounts.accountLayouts.get("market").layout;
  const current = dusk.program.coder.accounts.decode("market", original);
  const accountAt = (version) => {
    // The account coder's scratch buffer is smaller than a Market, so the
    // layout re-encodes the account over a copy of its own bytes.
    const data = Buffer.from(original);
    layout.encode({ ...current, version }, data, 8);
    return { data, executable: false, owner: programId, lamports: 1_000_000, rentEpoch: 0 };
  };
  const serve = (...versions) => {
    const accounts = versions.map(accountAt);
    connection.getAccountInfo = async () => accounts[0];
    connection.getAccountInfoAndContext = async () => ({
      context: { slot: fixture.result.context.slot },
      value: accounts[0],
    });
    connection.getProgramAccounts = async () =>
      accounts.map((account, index) => ({
        pubkey: index === 0 ? marketKey : PublicKey.unique(),
        account,
      }));
  };
  return { dusk, serve };
}

const unsupported = (version) => (error) => {
  assert.ok(error instanceof DuskUnsupportedMarketLayoutError, String(error));
  assert.equal(error.version, version);
  assert.equal(error.expected, MARKET_LAYOUT_VERSION);
  assert.match(error.message, /Unsupported Dusk market layout/);
  return true;
};

test("typed market reads return markets at the current layout", async () => {
  const { dusk, serve } = setup();
  serve(MARKET_LAYOUT_VERSION);
  assert.equal((await dusk.get.market(marketKey)).version, MARKET_LAYOUT_VERSION);
  assert.equal((await dusk.get.programAccount("market", marketKey)).version, MARKET_LAYOUT_VERSION);
  assert.equal((await dusk.operator.market(marketKey)).version, MARKET_LAYOUT_VERSION);
  const listed = await dusk.get.allMarkets();
  assert.equal(listed.length, 1);
  assert.ok(listed[0].publicKey.equals(marketKey));
  assert.equal(listed[0].account.version, MARKET_LAYOUT_VERSION);
});

test("single, generic and operator market reads reject every other layout", async () => {
  const { dusk, serve } = setup();
  for (const version of UNSUPPORTED) {
    serve(version);
    await assert.rejects(dusk.get.market(marketKey), unsupported(version));
    await assert.rejects(dusk.get.programAccount("market", marketKey), unsupported(version));
    await assert.rejects(dusk.operator.market(marketKey), unsupported(version));
  }
});

test("market listings reject an unsupported layout anywhere in the list", async () => {
  const { dusk, serve } = setup();
  for (const version of UNSUPPORTED) {
    serve(version);
    await assert.rejects(dusk.get.allMarkets(), unsupported(version));
    serve(MARKET_LAYOUT_VERSION, version);
    await assert.rejects(dusk.get.allMarkets(), unsupported(version));
  }
});

test("instruction builders reject a market at another layout before using it", async () => {
  const { dusk, serve } = setup();
  for (const version of UNSUPPORTED) {
    serve(version);
    await assert.rejects(dusk.write.hlpRemainingAccounts(marketKey), unsupported(version));
    await assert.rejects(
      dusk.tranches.rollIncome({ market: marketKey, payer: PublicKey.unique(), bookIndex: 0 }),
      unsupported(version)
    );
  }
});

test("raw account reads stay raw", async () => {
  const { dusk, serve } = setup();
  for (const version of UNSUPPORTED) {
    serve(version);
    const raw = await dusk.get.accountInfo(marketKey);
    assert.equal(dusk.program.coder.accounts.decode("market", raw.data).version, version);
  }
});
