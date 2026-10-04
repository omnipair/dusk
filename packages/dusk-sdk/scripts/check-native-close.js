import assert from "node:assert/strict";
import { test } from "node:test";
import { PublicKey, TransactionInstruction } from "@solana/web3.js";

import { DuskGet, DuskSimulationError } from "../dist/get.js";
import { findMinimumNativeCloseCollateralIn } from "../dist/native-close.js";
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

test("SDK searches exact close simulations and returns the minimum sale", async (t) => {
  let builds = 0;
  t.mock.method(DuskWrite.prototype, "closeLeverageInstruction", async (params) => {
    builds++;
    assert.equal(params.collateralFunded, true);
    assert.equal(params.collateralIn, 1_990n);
    return new TransactionInstruction({ programId, keys: [], data: Buffer.alloc(16) });
  });
  const program = {
    programId,
    provider: { publicKey: owner },
    idl: { errors: [
      { code: 6033, name: "insufficientOutputAmount" },
      { code: 6041, name: "insufficientAmount" },
      { code: 6042, name: "insufficientLiquidity" },
    ] },
    coder: { instruction: { encode(name, args) {
      assert.equal(name, "closeCollateralLeverage");
      assert.equal(args.args.debtAsset, 0);
      const data = Buffer.alloc(16);
      data.writeBigUInt64LE(BigInt(args.args.collateralIn.toString()));
      data.writeBigUInt64LE(BigInt(args.args.minCollateralOut.toString()), 8);
      return data;
    } } },
  };
  const reader = new DuskGet(program);
  reader.leveragePosition = async () => ({
    owner, market: marketKey, positionId, debtAsset: 0,
    fundedCollateralAmount: { toString: () => "100" },
    collateralAmount: { toString: () => "2000" },
  });
  reader.market = async () => ({
    config: { amm: { launchRateLimitAsset: 1,
      launchRateLimitReferenceNad: { toString: () => "1000" },
      launchRateLimitIncrementBps: 100,
      launchRateLimitMaxFeeBps: 500,
    } },
    baseSide: { assetDecimals: 9 }, quoteSide: { assetDecimals: 9 },
  });
  const probes = [];
  reader.simulateWithContext = async ([instruction], options) => {
    assert.equal(options.requireReturnData, false);
    assert.equal(options.feePayer.toBase58(), owner.toBase58());
    const amount = instruction.data.readBigUInt64LE();
    assert.equal(instruction.data.readBigUInt64LE(8), 10n);
    probes.push(amount);
    if ((amount <= 1_000n ? amount : amount - 10n) < 995n) {
      throw new DuskSimulationError("Dusk simulation failed", {
        context: { slot: 123 },
        value: { err: { InstructionError: [2, { Custom: 6041 }] } },
      });
    }
    return { context: { slot: 123 }, value: { err: null }, observedAt: Date.now() };
  };
  const result = await reader.findCollateralLeverageCloseInput({
    market: marketKey, positionOwner: owner, positionId,
    debtAsset: "base", debtMint, collateralMint,
    ownerDebtAccount: owner, ownerCollateralAccount: owner,
    minAmountOut: 10n,
  });
  assert.deepEqual(result, { collateralIn: 995n, collateralReturned: 1_005n, observedSlot: 123 });
  assert.equal(builds, 1);
  assert.ok(probes.includes(1_000n));
  assert.equal(probes.at(-1), 995n, "the selected close is revalidated");

  reader.simulateWithContext = async ([instruction]) => {
    const amount = instruction.data.readBigUInt64LE();
    const output = amount <= 1_000n ? amount : amount - 10n;
    const errorCode = output < 995n ? 6041 : output > 1_200n ? 6042 : undefined;
    if (errorCode !== undefined) throw new DuskSimulationError("Dusk simulation failed", {
      context: { slot: 124 },
      value: { err: { InstructionError: [2, { Custom: errorCode }] } },
    });
    return { context: { slot: 124 }, value: { err: null }, observedAt: Date.now() };
  };
  const liquidityBounded = await reader.findCollateralLeverageCloseInput({
    market: marketKey, positionOwner: owner, positionId,
    debtAsset: "base", debtMint, collateralMint,
    ownerDebtAccount: owner, ownerCollateralAccount: owner,
    minAmountOut: 10n,
  });
  assert.deepEqual(liquidityBounded, { collateralIn: 995n, collateralReturned: 1_005n, observedSlot: 124 });

  reader.simulateWithContext = async () => {
    throw new DuskSimulationError("Dusk simulation failed", {
      context: { slot: 124 },
      value: { err: { InstructionError: [2, { Custom: 6065 }] } },
    });
  };
  await assert.rejects(reader.findCollateralLeverageCloseInput({
    market: marketKey, positionOwner: owner, positionId,
    debtAsset: "base", debtMint, collateralMint,
    ownerDebtAccount: owner, ownerCollateralAccount: owner,
    minAmountOut: 10n,
  }), DuskSimulationError, "unrelated instruction failures must surface to the client");
});
