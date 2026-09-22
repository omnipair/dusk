import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import * as anchor from "@coral-xyz/anchor";
import { Connection, PublicKey, VersionedTransaction } from "@solana/web3.js";
import {
  ACCOUNT_SIZE,
  AccountType,
  ExtensionType,
  getMintLen,
  TOKEN_2022_PROGRAM_ID,
  TransferFeeConfigLayout,
} from "@solana/spl-token";
import {
  Dusk,
  decodeDuskVirtualBookBatch,
  decodePreviewSwapReturnData,
  combineVirtualBookBatches,
  projectDuskVirtualBook,
  projectDuskVirtualBookCurve,
  virtualCurvePoint,
} from "../dist/index.js";
const BN = Reflect.get(anchor, "BN") ?? Reflect.get(anchor, "default").BN;
const fixture = JSON.parse(
  readFileSync(new URL("./fixtures/virtual-book-batch-devnet-20260920.json", import.meta.url))
);
const slot = fixture.result.context.slot;
const prefix = `Program return: ${fixture.programId} `;
const previews = fixture.result.value.logs
  .filter((line) => line.startsWith(prefix))
  .map((line) => decodePreviewSwapReturnData([line.slice(prefix.length), "base64"]));
const requests = previews.map((preview) => ({
  side: "bids",
  amount: BigInt(preview.exactAssetIn.toString()),
}));
function setup() {
  const connection = new Connection("http://localhost:8899", "confirmed");
  const dusk = new Dusk({ connection, programId: fixture.programId });
  const account = dusk.program.coder.accounts.decode(
    "market",
    Buffer.from(fixture.result.value.accounts[0].data[0], "base64")
  );
  const snapshot = {
    market: fixture.market,
    programId: fixture.programId,
    account,
    slot,
    observedAt: 1000,
  };
  const response = () => structuredClone(fixture.result);
  const decode = (result = response(), inputs = requests) =>
    decodeDuskVirtualBookBatch(dusk.program, snapshot, inputs, result, slot);
  return { dusk, connection, snapshot, response, decode };
}
const close = (actual, expected, tolerance = 1e-8) =>
  assert.ok(Math.abs(actual - expected) < tolerance, `${actual} != ${expected}`);

test("saved devnet batch preserves ordered cumulative inputs and program fees", () => {
  const { decode } = setup();
  const result = decode();
  assert.equal(result.quotes.length, 4);
  assert.deepEqual(
    result.quotes.map((row) => row.preview.exactAssetIn.toString()),
    requests.map((row) => row.amount.toString())
  );
  assert.equal(result.quotes[0].preview.totalFeeDebit.toString(), "1471");
  assert.ok(result.quotes.every((row) => row.outputTransferFee === 0n));
});
for (const [name, alter] of [
  [
    "return program",
    (r) => {
      r.value.returnData.programId = PublicKey.default.toBase58();
    },
  ],
  [
    "old bank",
    (r) => {
      r.context.slot--;
    },
  ],
  [
    "program error",
    (r) => {
      r.value.err = { InstructionError: [2, { Custom: 6000 }] };
    },
  ],
  [
    "truncated logs",
    (r) => {
      r.value.logs = [];
    },
  ],
  [
    "market owner",
    (r) => {
      r.value.accounts[0].owner = PublicKey.default.toBase58();
    },
  ],
  [
    "mint owner",
    (r) => {
      r.value.accounts[1].owner = fixture.programId;
    },
  ],
  [
    "clock",
    (r) => {
      r.value.accounts[4].data[0] = Buffer.alloc(40).toString("base64");
    },
  ],
  [
    "noncanonical encoding",
    (r) => {
      r.value.accounts[0].data[0] += "!";
    },
  ],
])
  test(`batch rejects invalid ${name}`, () => {
    const h = setup(),
      response = h.response();
    alter(response);
    assert.throws(() => h.decode(response));
  });
test("batch verifies amount, side, and client program identity", () => {
  const h = setup();
  assert.throws(
    () => h.decode(h.response(), [{ ...requests[0], amount: 1n }, ...requests.slice(1)]),
    /request/
  );
  assert.throws(
    () => h.decode(h.response(), [{ ...requests[0], side: "asks" }, ...requests.slice(1)]),
    /request/
  );
  assert.throws(
    () =>
      decodeDuskVirtualBookBatch(
        h.dusk.program,
        { ...h.snapshot, programId: PublicKey.default.toBase58() },
        requests,
        h.response(),
        slot
      ),
    /mismatch/
  );
});
test("output transfer fees use the simulated epoch exactly once, including maximum fee", () => {
  const h = setup(),
    response = h.response(),
    epoch = h.decode().epoch;
  const data = Buffer.alloc(getMintLen([ExtensionType.TransferFeeConfig]));
  Buffer.from(response.value.accounts[2].data[0], "base64").copy(data, 0, 0, 82);
  data[ACCOUNT_SIZE] = AccountType.Mint;
  data.writeUInt16LE(ExtensionType.TransferFeeConfig, ACCOUNT_SIZE + 1);
  data.writeUInt16LE(TransferFeeConfigLayout.span, ACCOUNT_SIZE + 3);
  TransferFeeConfigLayout.encode(
    {
      transferFeeConfigAuthority: PublicKey.default,
      withdrawWithheldAuthority: PublicKey.default,
      withheldAmount: 0n,
      olderTransferFee: { epoch: 0n, transferFeeBasisPoints: 100, maximumFee: 999999n },
      newerTransferFee: { epoch, transferFeeBasisPoints: 200, maximumFee: 30000n },
    },
    data,
    ACCOUNT_SIZE + 5
  );
  response.value.accounts[2].data[0] = data.toString("base64");
  response.value.accounts[2].owner = TOKEN_2022_PROGRAM_ID.toBase58();
  const result = h.decode(response);
  assert.equal(result.quotes[0].outputTransferFee, 10171n);
  assert.equal(result.quotes[3].outputTransferFee, 30000n);
  assert.equal(result.quotes[0].preview.amountOut.toString(), "508550");
});
test("public batch method uses one unsigned RPC simulation with ordered accounts and slot floor", async (t) => {
  const h = setup();
  const simulate = t.mock.method(h.connection, "simulateTransaction", async (tx, options) => {
    assert.ok(tx instanceof VersionedTransaction);
    assert.ok(tx.signatures.every((signature) => signature.every((byte) => byte === 0)));
    assert.equal(tx.message.compiledInstructions.length, 6);
    assert.equal(options.minContextSlot, slot);
    assert.equal(options.sigVerify, false);
    assert.equal(options.replaceRecentBlockhash, true);
    assert.equal(options.accounts.addresses.length, 5);
    assert.equal(options.accounts.addresses[0], fixture.market);
    return h.response();
  });
  const start = Date.now();
  const result = await h.dusk.get.previewVirtualBookBatch(h.snapshot, requests);
  assert.equal(simulate.mock.callCount(), 1);
  assert.equal(result.slot, slot);
  assert.ok(result.observedAt >= start && result.observedAt <= Date.now());
  assert.equal(result.quotes.length, 4);
  await assert.rejects(h.dusk.get.previewVirtualBookBatch(h.snapshot, []), /one to four/);
  await assert.rejects(
    h.dusk.get.previewVirtualBookBatch(h.snapshot, [...requests, requests[0]]),
    /one to four/
  );
});
for (const kind of ["state", "epoch", "slot spread", "old slot", "start price"])
  test(`combining batches rejects ${kind} changes`, () => {
    const h = setup(),
      first = h.decode(),
      second = h.decode();
    if (kind === "state") second.state += "changed";
    if (kind === "epoch") second.epoch++;
    if (kind === "slot spread") second.slot += 9;
    if (kind === "old slot") second.slot--;
    if (kind === "start price") second.quotes[0].preview.startPriceNad = new BN(1);
    assert.throws(() => combineVirtualBookBatches(h.snapshot, 10, 1, [first, second]), /changed/);
  });
function quote(input, output, surcharge, outputTransferFee = 0n) {
  return {
    slot,
    outputTransferFee,
    preview: {
      ...previews[0],
      exactAssetIn: new BN(input),
      amountOut: new BN(output),
      totalFeeRateNad: new BN(10_000_000 + surcharge),
      divergenceFeeRateNad: new BN(surcharge),
      volatilityFeeRateNad: new BN(0),
    },
  };
}
function book(bids, asks = []) {
  return {
    ...setup().snapshot,
    groupingBps: 10,
    firstQuoteSlot: slot,
    mid: 1,
    quotes: { bids, asks },
  };
}
test("marginal depth reflects nonlinear surcharge without charging cumulative trades twice", () => {
  const value = projectDuskVirtualBook(book([quote(100e6, 98e6, 0), quote(200e6, 180e6, 70e6)]));
  close(value.bids[0].price, 0.98);
  close(value.bids[1].price, 0.82);
  close(value.bids[1].averagePrice, 0.9);
  close(value.bids[1].impactPercent, 10);
  assert.equal(value.bids[1].feePercent, 8);
  assert.equal(value.bids[1].surchargePercent, 7);
  close(
    value.bids.reduce((sum, row) => sum + row.price * row.size, 0),
    180
  );
});
test("asks use net base received and refuse fabricated positive extra depth", () => {
  const value = projectDuskVirtualBook(
    book([], [quote(100e6, 99e6, 0, 1_000_000n), quote(200e6, 182e6, 70e6, 2_000_000n)])
  );
  assert.equal(value.asks[0].total, 98);
  assert.equal(value.asks[1].total, 180);
  close(value.asks[1].price, 100 / 82);
  const data = book([quote(1e6, 990e6, 0), quote(2e6, 990e6, 0)]);
  data.account.quoteSide.assetDecimals = 9;
  const limited = projectDuskVirtualBook(data);
  assert.equal(limited.bids.length, 1);
  close(limited.bids[0].price, 0.99);
  assert.equal(projectDuskVirtualBook(null), null);
});
function curve(concentrated = false) {
  const NAD = 1_000_000_000n,
    tail = 100n * NAD;
  const layers = concentrated
    ? [
        { liquidity: 450n * NAD, lower: 950_000_000n, upper: 1_050_000_000n },
        { liquidity: 450n * NAD, lower: 800_000_000n, upper: 1_200_000_000n },
      ]
    : [];
  const point = virtualCurvePoint(tail, layers, NAD);
  const snapshot = setup().snapshot;
  snapshot.account.baseSide.assetDecimals = 6;
  snapshot.account.quoteSide.assetDecimals = 9;
  snapshot.account.amm.concentratedCurveCache = {
    mathRevision: 1,
    tailLiquidity: new BN(tail.toString()),
    concentratedLiquidity: new BN((concentrated ? 900n * NAD : 0n).toString()),
    fadeWidthBps: concentrated ? 1500 : 0,
    coreLowerSqrtPriceNad: new BN(950_000_000),
    coreUpperSqrtPriceNad: new BN(1_050_000_000),
    outerLowerSqrtPriceNad: new BN(800_000_000),
    outerUpperSqrtPriceNad: new BN(1_200_000_000),
  };
  snapshot.preview = {
    amm: {
      ordinaryBaseReserveNad: new BN(point.base.toString()),
      ordinaryQuoteReserveNad: new BN(point.quote.toString()),
    },
    base: { cashReserve: new BN(1_000_000_000) },
    quote: { cashReserve: new BN("1000000000000") },
  };
  return snapshot;
}
test("curve matches analytical CPMM and concentration across range boundaries", () => {
  const plain = projectDuskVirtualBookCurve(curve(), 100, 50),
    concentrated = projectDuskVirtualBookCurve(curve(true), 100, 50);
  close(plain.asks[5].total, 100 - 100 / Math.sqrt(1.06), 1e-5);
  close(plain.bids[5].averagePrice, Math.sqrt(0.94), 1e-5);
  close(concentrated.asks[0].size / plain.asks[0].size, 10, 1e-4);
  close(concentrated.asks[49].size, plain.asks[49].size, 1e-7);
  close(concentrated.bids[49].size, plain.bids[49].size, 1e-7);
});
test("curve caps output cash and rejects incompatible math or reserve state", () => {
  const limited = curve();
  limited.preview.base.cashReserve = new BN(500_000);
  limited.preview.quote.cashReserve = new BN(0);
  const value = projectDuskVirtualBookCurve(limited, 100);
  assert.equal(value.bids.length, 0);
  close(value.asks.at(-1).total, 0.5, 1e-6);
  const bad = curve(true);
  bad.account.amm.concentratedCurveCache.mathRevision = 2;
  assert.throws(() => projectDuskVirtualBookCurve(bad), /revision/);
  bad.account.amm.concentratedCurveCache.mathRevision = 1;
  bad.preview.amm.ordinaryQuoteReserveNad = new BN(1);
  assert.throws(() => projectDuskVirtualBookCurve(bad), /reserves/);
  assert.throws(() => projectDuskVirtualBookCurve(curve(), 0), /grouping/);
});


test("CPMM samples current reserves after accumulated rounding changes the cached invariant", () => {
  const snapshot = curve();
  // Public devnet market 45qXCmfQrDxTDYc1k7Xu65Qo3kYHRYUkYmiCQKLvPBhL,
  // previewMarket simulation at slot 501310482, captured 2026-09-20.
  snapshot.account.amm.concentratedCurveCache.tailLiquidity = new BN("1000000004042");
  snapshot.preview.amm.ordinaryBaseReserveNad = new BN("977147409571");
  snapshot.preview.amm.ordinaryQuoteReserveNad = new BN("1023397532586");
  snapshot.preview.base.cashReserve = new BN("1177142099");
  snapshot.account.quoteSide.assetDecimals = 6;
  snapshot.preview.quote.cashReserve = new BN("1232858326");
  const value = projectDuskVirtualBookCurve(snapshot, 10);
  const base = 977.147409571, quote = 1023.397532586;
  close(value.mid, quote / base);
  assert.equal(value.bids.length, 12);
  assert.equal(value.asks.length, 12);
  for (const [i, row] of value.asks.entries()) {
    close(row.total, base * (1 - 1 / Math.sqrt(1 + .001 * (i + 1))), 2e-6);
    close(row.quoteTotal, quote * (Math.sqrt(1 + .001 * (i + 1)) - 1), 2e-6);
  }
  for (const [i, row] of value.bids.entries()) {
    close(row.total, base * (1 / Math.sqrt(1 - .001 * (i + 1)) - 1), 2e-6);
    close(row.quoteTotal, quote * (1 - Math.sqrt(1 - .001 * (i + 1))), 2e-6);
  }
  snapshot.account.amm.concentratedCurveCache.tailLiquidity = new BN("900000000000");
  assert.deepEqual(projectDuskVirtualBookCurve(snapshot, 10), value);
});

test("full curve sampling batches both directions and retains original observation age", async (t) => {
  const h = setup(),
    snapshot = curve();
  snapshot.account.quoteSide.assetDecimals = h.snapshot.account.quoteSide.assetDecimals;
  const sent = [];
  t.mock.method(h.connection, "simulateTransaction", async (tx, options) => {
    const response = h.response();
    const returns = tx.message.compiledInstructions.slice(2).map((ix) => {
      const decoded = h.dusk.program.coder.instruction.decode(Buffer.from(ix.data));
      assert.equal(decoded.name, "previewSwap");
      const mint = tx.message.staticAccountKeys[ix.accountKeyIndexes[2]];
      const fromBase = mint.equals(snapshot.account.baseSide.assetMint);
      const amount = BigInt(decoded.data.args.exactAssetIn.toString());
      sent.push({ fromBase, amount });
      return h.dusk.program.coder.types
        .encode("swapPreview", {
          ...previews[0],
          exactAssetIn: new BN(amount.toString()),
          amountOut: new BN(((amount * 997n) / 1000n).toString()),
          assetIn: fromBase ? { base: {} } : { quote: {} },
          assetOut: fromBase ? { quote: {} } : { base: {} },
          startPriceNad: new BN(1_000_000_000),
        })
        .toString("base64");
    });
    assert.ok(returns.length <= 4);
    assert.equal(options.minContextSlot, slot);
    response.value.logs = returns.map((value) => prefix + value);
    response.value.returnData.data[0] = returns.at(-1);
    return response;
  });
  const quotes = await h.dusk.get.previewVirtualBookQuotes(snapshot);
  assert.equal(quotes.quotes.bids.length, 12);
  assert.equal(quotes.quotes.asks.length, 12);
  assert.equal(quotes.observedAt, snapshot.observedAt);
  assert.equal(quotes.firstQuoteSlot, slot);
  assert.equal(quotes.slot, slot);
  for (const direction of [true, false]) {
    const inputs = sent.filter((row) => row.fromBase === direction);
    assert.ok(inputs.every((row, i) => i === 0 || row.amount > inputs[i - 1].amount));
  }
});
