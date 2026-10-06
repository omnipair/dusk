import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { Program } from "@coral-xyz/anchor";
import BN from "bn.js";
import { test } from "node:test";
import { PublicKey, TransactionInstruction } from "@solana/web3.js";
import { MintLayout, TOKEN_PROGRAM_ID } from "@solana/spl-token";

import { DuskGet, DuskSimulationError } from "../dist/get.js";
import IDL from "../dist/idl_v2.js";
import { createNativeCloseQuote } from "../dist/native-close-quote.js";
import { findMinimumNativeCloseCollateralIn, nativeCloseCredit, nativeCloseGrossForCredit } from "../dist/native-close.js";
import { deriveLeveragePositionAddress } from "../dist/constants.js";
import { DuskWrite } from "../dist/write.js";

const owner = new PublicKey(new Uint8Array(32).fill(1));
const marketKey = new PublicKey(new Uint8Array(32).fill(2));
const positionId = new PublicKey(new Uint8Array(32).fill(3));
const collateralMint = new PublicKey(new Uint8Array(32).fill(4));
const debtMint = new PublicKey(new Uint8Array(32).fill(5));
const programId = new PublicKey(new Uint8Array(32).fill(6));

test("native close search finds the least input with a monotone quote", async () => {
  const result = await findMinimumNativeCloseCollateralIn({
    maxCollateralIn: 2_000n,
    canClose: async (amount) => amount >= 995n,
  });
  assert.equal(result, 995n);
});

test("native close search finds repayment below a liquidity-limited maximum", async () => {
  const result = await findMinimumNativeCloseCollateralIn({
    maxCollateralIn: 2_000n,
    canClose: async (amount) => amount < 995n ? false : amount <= 1_200n ? true : "liquidity-limited",
  });
  assert.equal(result, 995n);
});

test("native close search keeps fee-tier boundaries when the maximum is liquidity-limited", async () => {
  const result = await findMinimumNativeCloseCollateralIn({
    maxCollateralIn: 2_000n,
    feeTiers: { referenceNad: 1_000n, incrementBps: 100, maxFeeBps: 500, collateralDecimals: 9 },
    canClose: async (amount) => {
      const output = amount <= 1_000n ? amount : amount - 10n;
      return output < 995n ? false : output <= 1_200n ? true : "liquidity-limited";
    },
  });
  assert.equal(result, 995n);
});

test("native close search rejects a debt-covering amount that is still liquidity-limited", async () => {
  await assert.rejects(
    findMinimumNativeCloseCollateralIn({
      maxCollateralIn: 2_000n,
      canClose: async (amount) => amount < 995n ? false : "liquidity-limited",
    }),
    /No executable collateral sale/
  );
});

test("native close search checks earlier launch fee tiers before searching atoms", async () => {
  const probes = [];
  const result = await findMinimumNativeCloseCollateralIn({
    maxCollateralIn: 2_000n,
    feeTiers: {
      referenceNad: 1_000n,
      incrementBps: 100,
      maxFeeBps: 500,
      collateralDecimals: 9,
    },
    canClose: async (amount) => {
      probes.push(amount);
      return (amount <= 1_000n ? amount : amount - 10n) >= 995n;
    },
  });
  assert.equal(result, 995n);
  assert.ok(probes.includes(1_000n));
  assert.equal((1_005n - 10n) >= 995n, true);
});

test("native close search can repay below a fee jump even when the maximum fails", async () => {
  const result = await findMinimumNativeCloseCollateralIn({
    maxCollateralIn: 1_001n,
    feeTiers: {
      referenceNad: 1_000n,
      incrementBps: 100,
      maxFeeBps: 500,
      collateralDecimals: 9,
    },
    canClose: async (amount) => (amount <= 1_000n ? amount : amount - 10n) >= 995n,
  });
  assert.equal(result, 995n);
});

test("native close search keeps searching after the launch fee reaches its cap", async () => {
  const result = await findMinimumNativeCloseCollateralIn({
    maxCollateralIn: 10_000n,
    feeTiers: {
      referenceNad: 1_000n,
      incrementBps: 100,
      maxFeeBps: 100,
      collateralDecimals: 9,
    },
    canClose: async (amount) => (amount <= 1_000n ? amount : amount - 50n) >= 1_200n,
  });
  assert.equal(result, 1_250n);
});

test("native close search maps NAD fee boundaries to raw six-decimal atoms", async () => {
  const result = await findMinimumNativeCloseCollateralIn({
    maxCollateralIn: 2_000_000n,
    feeTiers: {
      referenceNad: 1_000_000_000n,
      incrementBps: 100,
      maxFeeBps: 500,
      collateralDecimals: 6,
    },
    canClose: async (amount) =>
      (amount <= 1_000_000n ? amount : amount - 10_000n) >= 995_000n,
  });
  assert.equal(result, 995_000n);
});

test("native close search matches brute force across launch fee boundaries", async () => {
  for (const reference of [7n, 10n]) for (const increment of [50, 200]) {
    for (const cap of [500, 1_000]) for (const maximum of [11n, 20n, 37n, 57n]) {
      const output = (amount) => {
        const bucket = (amount + reference - 1n) / reference;
        const feeBps = BigInt(Math.min(cap, Number(bucket - 1n) * increment));
        return amount - (amount * feeBps + 9_999n) / 10_000n;
      };
      for (const debt of [1n, 5n, 10n, 15n, 25n, 40n]) {
        const expected = Array.from({ length: Number(maximum) }, (_, index) => BigInt(index + 1))
          .find((amount) => output(amount) >= debt);
        const search = findMinimumNativeCloseCollateralIn({
          maxCollateralIn: maximum,
          feeTiers: { referenceNad: reference, incrementBps: increment,
            maxFeeBps: cap, collateralDecimals: 9 },
          canClose: async (amount) => output(amount) >= debt,
        });
        if (expected === undefined) await assert.rejects(search, /cannot repay/);
        else assert.equal(await search, expected);
      }
    }
  }
});

test("native close search rejects an insufficient cap and honors abort", async () => {
  await assert.rejects(
    findMinimumNativeCloseCollateralIn({ maxCollateralIn: 10n, canClose: async () => false }),
    /cannot repay/
  );
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(
    findMinimumNativeCloseCollateralIn({
      maxCollateralIn: 10n,
      canClose: async () => true,
      signal: controller.signal,
    }),
    { name: "AbortError" }
  );
});

test("native close searches gross input across net launch tiers and capped transfer fees", async () => {
  for (const transferFee of [
    { transferFeeBasisPoints: 300, maximumFee: 100n },
    { transferFeeBasisPoints: 10_000, maximumFee: 10n },
  ]) {
    const output = (gross) => {
      const credit = nativeCloseCredit(gross, transferFee);
      return credit <= 1_000n ? credit : credit - 10n;
    };
    const expected = Array.from({ length: 2_000 }, (_, i) => BigInt(i + 1)).find((n) => output(n) >= 995n);
    const found = await findMinimumNativeCloseCollateralIn({
      maxCollateralIn: 2_000n, transferFee,
      feeTiers: { referenceNad: 1_000n, incrementBps: 100, maxFeeBps: 500, collateralDecimals: 9 },
      canClose: async (gross) => output(gross) >= 995n,
    });
    assert.equal(found, expected);
    for (const net of [0n, 1n, 10n, 995n, 2_000n]) {
      const gross = nativeCloseGrossForCredit(net, transferFee);
      assert.equal(nativeCloseCredit(gross, transferFee), net);
      if (gross > 0n) assert.ok(nativeCloseCredit(gross - 1n, transferFee) < net);
    }
  }
});

test("WebAssembly quote matches the shared Rust fixture", async () => {
  const fixture = JSON.parse(readFileSync(new URL("./native-close-fixture.json", import.meta.url)));
  const quote = await createNativeCloseQuote(
    Buffer.from(fixture.market, "hex"), Buffer.from(fixture.position, "hex"), 0n, 0n
  );
  assert.equal(quote.debtAmount, BigInt(fixture.debt));
  assert.equal(quote.amountOut(BigInt(fixture.amount)), BigInt(fixture.output));
  assert.equal(quote.amountOut(1n), "insufficient");
});

async function largeNativeCloseQuote(baseReserve, debt = 100_000_000_000_000_000n) {
  const fixture = JSON.parse(readFileSync(new URL("./native-close-fixture.json", import.meta.url)));
  const decoder = new Program(IDL, { publicKey: null, connection: {} });
  const market = decoder.coder.accounts.decode("market", Buffer.from(fixture.market, "hex"));
  const position = decoder.coder.accounts.decode("leveragePosition", Buffer.from(fixture.position, "hex"));
  const bn = (amount) => new BN(amount.toString());
  const quoteReserve = 1_000_000_000_000_000_000n;
  market.baseSide.assetDecimals = market.quoteSide.assetDecimals = 18;
  market.baseSide.reserves.liveReserve = bn(baseReserve);
  market.baseSide.reserves.cashReserve = bn(baseReserve - debt);
  market.quoteSide.reserves.liveReserve = market.quoteSide.reserves.cashReserve = bn(quoteReserve);
  market.baseSide.shares.ylpSupply = market.quoteSide.shares.ylpSupply = bn(quoteReserve);
  market.debt.isolatedBaseShares = market.debt.isolatedBasePrincipal = bn(debt);
  position.debtShares = position.debtPrincipal = bn(debt);
  position.collateralAmount = bn(2n * quoteReserve);
  // Anchor's encode convenience method has a 1,000-byte scratch buffer;
  // the Market account needs its full layout size.
  const encode = (name, state) => {
    const { layout, discriminator } = decoder.coder.accounts.accountLayouts.get(name);
    const data = Buffer.alloc(layout.span);
    layout.encode(state, data);
    return Buffer.concat([Buffer.from(discriminator), data]);
  };
  return createNativeCloseQuote(encode("market", market), encode("leveragePosition", position), 0n, 0n);
}

test("WebAssembly debt stays unsigned at the i64 and u64 boundaries", async () => {
  const maximum = (1n << 64n) - 1n;
  for (const debt of [(1n << 63n) - 1n, 1n << 63n, maximum]) {
    const quote = await largeNativeCloseQuote(maximum, debt);
    assert.equal(quote.debtAmount, debt);
  }
});

test("WebAssembly swap output remains unsigned across the i64 boundary", async () => {
  const reserve = 1_000_000_000_000_000_000n;
  for (const [base, input, expected] of [
    [2n * ((1n << 63n) - 1n), reserve, (1n << 63n) - 1n],
    [3n * (1n << 62n), 2n * reserve, 1n << 63n],
    [18n * reserve, 2n * reserve, 12n * reserve],
  ]) {
    const quote = await largeNativeCloseQuote(base);
    assert.equal(quote.amountOut(input), expected);
  }
});

test("native close finds smaller repayment when maximum output exceeds i64", async () => {
  const quote = await largeNativeCloseQuote(18_000_000_000_000_000_000n);
  const maximum = 2_000_000_000_000_000_000n;
  const selected = await findMinimumNativeCloseCollateralIn({
    maxCollateralIn: maximum,
    canClose: async (input) => {
      const output = quote.amountOut(input);
      return typeof output === "bigint" ? output >= quote.debtAmount :
        output === "liquidity-limited" ? output : false;
    },
  });
  assert.equal(selected, 5_586_592_178_770_950n);
  assert.ok(quote.amountOut(selected) >= quote.debtAmount);
  assert.ok(quote.amountOut(selected - 1n) < quote.debtAmount);
});

test("SDK searches locally and simulates only the selected close", async (t) => {
  const fixture = JSON.parse(readFileSync(new URL("./native-close-fixture.json", import.meta.url)));
  const marketData = Buffer.from(fixture.market, "hex");
  const positionData = Buffer.from(fixture.position, "hex");
  const decoder = new Program(IDL, { publicKey: null, connection: {} });
  const position = decoder.coder.accounts.decode("leveragePosition", positionData);
  const marketKey = position.market;
  const positionId = position.positionId;
  const owner = position.owner;
  const positionKey = new PublicKey(new Uint8Array(32).fill(8));
  const clockData = Buffer.alloc(40);
  clockData.writeBigUInt64LE(123n, 0);
  const account = (data) => ({ data, owner: decoder.programId });
  const mintData = Buffer.alloc(MintLayout.span);
  MintLayout.encode({ mintAuthorityOption: 1, mintAuthority: owner, supply: 100_000n,
    decimals: 9, isInitialized: true, freezeAuthorityOption: 0, freezeAuthority: PublicKey.default }, mintData);
  const mintAccount = { data: mintData, owner: TOKEN_PROGRAM_ID, executable: false, lamports: 1 };
  const clockAccount = { data: clockData,
    owner: new PublicKey("Sysvar1111111111111111111111111111111111111"), executable: false, lamports: 1 };
  const returnedAccounts = [mintAccount, clockAccount].map((info) => ({
    ...info, owner: info.owner.toBase58(), data: [info.data.toString("base64"), "base64"],
  }));
  let reads = 0;
  const connection = {
    async getMultipleAccountsInfoAndContext(keys) {
      reads++;
      assert.deepEqual(keys.map((key) => key.toBase58()).slice(0, 2),
        [marketKey.toBase58(), positionKey.toBase58()]);
      return { context: { slot: 123 },
        value: [account(marketData), account(positionData), mintAccount, clockAccount] };
    },
  };
  const program = new Program(IDL, { publicKey: owner, connection });
  let builds = 0;
  t.mock.method(DuskWrite.prototype, "closeLeverageInstruction", async (params) => {
    builds++;
    assert.equal(params.collateralFunded, true);
    const data = Buffer.alloc(8);
    data.writeBigUInt64LE(BigInt(params.collateralIn));
    return new TransactionInstruction({ programId: program.programId, keys: [], data });
  });
  const reader = new DuskGet(program);
  let simulations = 0;
  reader.simulateWithContext = async ([instruction], options) => {
    simulations++;
    assert.equal(options.requireReturnData, false);
    assert.equal(options.feePayer.toBase58(), owner.toBase58());
    assert.equal(options.minContextSlot, 123);
    const selected = instruction.data.readBigUInt64LE();
    const quote = await createNativeCloseQuote(marketData, positionData, 123n, 0n);
    assert.ok(quote.amountOut(selected) >= quote.debtAmount);
    assert.ok(quote.amountOut(selected - 1n) < quote.debtAmount);
    return { context: { slot: 124 },
      value: { err: null, accounts: returnedAccounts }, observedAt: Date.now() };
  };
  const params = {
    market: marketKey, leveragePosition: positionKey, positionOwner: owner, positionId,
    debtAsset: "base", debtMint, collateralMint,
    ownerDebtAccount: owner, ownerCollateralAccount: owner,
    minAmountOut: 10n,
  };
  const result = await reader.findCollateralLeverageCloseInput(params);
  assert.equal(result.collateralReturned, BigInt(position.collateralAmount.toString()) - result.collateralIn);
  assert.equal(result.observedSlot, 124);
  assert.equal(reads, 1);
  assert.equal(builds, 1);
  assert.equal(simulations, 1);

  reader.simulateWithContext = async () => {
    throw new DuskSimulationError("Dusk simulation failed", {
      context: { slot: 125 },
      value: { err: { InstructionError: [2, { Custom: 6065 }] } },
    });
  };
  await assert.rejects(reader.findCollateralLeverageCloseInput(params), DuskSimulationError);
});
