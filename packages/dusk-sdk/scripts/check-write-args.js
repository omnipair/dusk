import assert from "node:assert/strict";
import { test } from "node:test";

import anchor from "@coral-xyz/anchor";
import { PublicKey } from "@solana/web3.js";

import IDL from "../dist/idl_v2.js";
import { deriveEventAuthorityAddress } from "../dist/constants.js";
import { DuskInstructionInputError, normalizeInstructionArgs } from "../dist/instruction-args.js";
import { feeParameterUpdate, anchorParameterUpdate } from "../dist/parameters.js";
import { DuskWrite } from "../dist/write.js";

const { BN } = anchor;
const key = (byte) => new PublicKey(new Uint8Array(32).fill(byte));
const owner = key(1);
const market = key(2);
const baseMint = key(4);
const quoteMint = key(5);
const provider = {
  connection: { getAccountInfo: async () => ({ owner: key(9), data: Buffer.alloc(0) }) },
  publicKey: owner,
  wallet: { publicKey: owner },
};
const program = new anchor.Program(IDL, provider);
program.account.market.fetch = async () => ({
  srlpMint: key(7),
  baseSide: { assetMint: baseMint, interestVault: key(15), reserveVault: key(17) },
  quoteSide: { assetMint: quoteMint, interestVault: key(16), reserveVault: key(18) },
  baseHlpVault: { srlpVault: key(13), hlpSupply: new BN(0), residualExposure: new BN(0) },
  quoteHlpVault: { srlpVault: key(14), hlpSupply: new BN(0), residualExposure: new BN(0) },
});
const write = new DuskWrite(program);

const borrowAccounts = {
  market,
  owner,
  debtAssetMint: baseMint,
  collateralAssetMint: quoteMint,
  reserveVault: key(20),
  ownerDebtAccount: key(21),
  borrowPosition: key(22),
  referralPartner: null,
  referralAccrual: null,
};

/** Rejects with a DuskInstructionInputError naming the instruction, the field and the detail. */
async function rejectsInput(promise, instruction, path, detail) {
  await assert.rejects(promise, (error) => {
    assert.ok(error instanceof DuskInstructionInputError, `expected DuskInstructionInputError, got ${error}`);
    assert.equal(error.instruction, instruction);
    assert.equal(error.path, path);
    assert.match(error.message, new RegExp(`^Dusk instruction "${instruction}": `));
    assert.match(error.message, detail);
    return true;
  });
}

// What the generic path sent before this check existed: the arguments handed
// to Anchor unchanged, with every u64 as BN.
async function anchorInstruction(name, args, accounts) {
  return program.methods[name](args)
    .accountsPartial({
      ...accounts,
      eventAuthority: deriveEventAuthorityAddress(program.programId)[0],
      program: program.programId,
    })
    .instruction();
}

test("a correct generic call encodes the same bytes as before, for every integer form", async () => {
  const expected = await anchorInstruction(
    "borrow",
    { borrowAmount: new BN(1_000), minDebtAmountOut: new BN(990), minLiquidationCfBps: 7_500, referrer: null },
    borrowAccounts
  );
  for (const [borrowAmount, minDebtAmountOut] of [
    [new BN(1_000), new BN(990)],
    [1_000n, 990n],
    [1_000, 990],
    ["1000", "990"],
  ]) {
    const args = { borrowAmount, minDebtAmountOut, minLiquidationCfBps: 7_500, referrer: null };
    for (const form of [args, [args]]) {
      const instruction = await write.instruction("borrow", form, { accounts: borrowAccounts });
      assert.deepEqual(instruction.data, expected.data);
      assert.deepEqual(instruction.keys, expected.keys);
    }
  }
});

test("a misspelled field throws instead of encoding zero", async () => {
  await rejectsInput(
    write.instruction(
      "borrow",
      { borrowAmount: 1_000n, minDebtAmountOt: 990n, minLiquidationCfBps: 0, referrer: null },
      { accounts: borrowAccounts }
    ),
    "borrow",
    "minDebtAmountOt",
    /unknown field "minDebtAmountOt"; did you mean "minDebtAmountOut"\?/
  );
});

test("a missing field throws instead of encoding zero", async () => {
  await rejectsInput(
    write.instruction("borrow", { borrowAmount: 1_000n, minLiquidationCfBps: 0 }, { accounts: borrowAccounts }),
    "borrow",
    "minDebtAmountOut",
    /missing field "minDebtAmountOut"/
  );
  await rejectsInput(
    write.instruction("borrow", { borrowAmount: 1_000n, minDebtAmountOut: undefined, minLiquidationCfBps: 0 }, { accounts: borrowAccounts }),
    "borrow",
    "minDebtAmountOut",
    /missing field "minDebtAmountOut"/
  );
  await rejectsInput(write.instruction("borrow", undefined, { accounts: borrowAccounts }), "borrow", "", /expects 1 argument \(args\), received 0/);
  await rejectsInput(write.instruction("previewMarket", { market }, { accounts: { market } }), "previewMarket", "", /expects no arguments, received 1/);
});

test("integers outside their type, unsafe numbers and non-integers fail with a clear message", async () => {
  const call = (overrides) =>
    write.instruction(
      "borrow",
      { borrowAmount: 1n, minDebtAmountOut: 1n, minLiquidationCfBps: 0, ...overrides },
      { accounts: borrowAccounts }
    );
  await rejectsInput(call({ borrowAmount: -1n }), "borrow", "borrowAmount", /must be between 0 and 18446744073709551615, received -1/);
  await rejectsInput(call({ borrowAmount: 2n ** 64n }), "borrow", "borrowAmount", /must be between 0 and 18446744073709551615/);
  await rejectsInput(call({ borrowAmount: 2 ** 53 }), "borrow", "borrowAmount", /unsigned 64-bit integer .*received number 9007199254740992/);
  await rejectsInput(call({ borrowAmount: 1.5 }), "borrow", "borrowAmount", /received number 1.5/);
  await rejectsInput(call({ borrowAmount: "" }), "borrow", "borrowAmount", /decimal string\), received string ""/);
  await rejectsInput(call({ borrowAmount: "1e3" }), "borrow", "borrowAmount", /received string "1e3"/);
  await rejectsInput(call({ borrowAmount: true }), "borrow", "borrowAmount", /received boolean/);
  await rejectsInput(call({ minLiquidationCfBps: 70_000 }), "borrow", "minLiquidationCfBps", /between 0 and 65535/);
  await rejectsInput(call({ referrer: "not a key" }), "borrow", "referrer", /must be a PublicKey or base58 address/);
});

test("an absent Option field encodes as None, the same as null", async () => {
  const accounts = { market, collateralAssetMint: baseMint, debtAssetMint: quoteMint };
  const withNull = await write.instruction("previewBorrowCapacity", { collateralAmount: 5n, projectedBorrowAmount: null }, { accounts });
  const absent = await write.instruction("previewBorrowCapacity", { collateralAmount: 5n }, { accounts });
  assert.deepEqual(absent.data, withNull.data);
  assert.equal(program.coder.instruction.decode(absent.data).data.args.projectedBorrowAmount, null);
});

test("an account name the instruction does not declare throws", async () => {
  const { ownerDebtAccount, ...rest } = borrowAccounts;
  await rejectsInput(
    write.instruction(
      "borrow",
      { borrowAmount: 1n, minDebtAmountOut: 1n, minLiquidationCfBps: 0 },
      { accounts: { ...rest, ownerDebtAcount: ownerDebtAccount } }
    ),
    "borrow",
    "ownerDebtAcount",
    /unknown account "ownerDebtAcount"; did you mean "ownerDebtAccount"\?/
  );
  await rejectsInput(
    write.instruction("borrow", { borrowAmount: 1n, minDebtAmountOut: 1n, minLiquidationCfBps: 0 }, { accounts: { ...borrowAccounts, owner: 7 } }),
    "borrow",
    "owner",
    /account "owner" must be a PublicKey or base58 address, received number 7/
  );
});

test("enums take exactly one known variant", async () => {
  const accounts = { market, owner, assetMint: baseMint, lpMint: key(24), yieldAccount: key(25) };
  const recipient = key(26);
  const instruction = await write.instruction("setYieldRecipient", { tokenKind: { hlp: {} }, recipient }, { accounts });
  assert.deepEqual(program.coder.instruction.decode(instruction.data).data.args.tokenKind, { hlp: {} });
  await rejectsInput(
    write.instruction("setYieldRecipient", { tokenKind: { hlp: {}, srlp: {} }, recipient }, { accounts }),
    "setYieldRecipient",
    "tokenKind",
    /must name exactly one yieldTokenKind variant \(srlp, hlp, junior\), received hlp, srlp/
  );
  await rejectsInput(
    write.instruction("setYieldRecipient", { tokenKind: { Hlp: {} }, recipient }, { accounts }),
    "setYieldRecipient",
    "tokenKind.Hlp",
    /unknown yieldTokenKind variant "Hlp"; did you mean "hlp"\?/
  );
});

test("nested structs, tuple variants and fixed arrays are checked by path", () => {
  const profile = feeParameterUpdate({
    baseFeeBps: 30,
    divergenceFeeShareCapBps: 0,
    volatilityFeeShareCapBps: 0,
    divergenceFeeCoefficientNad: 0,
    volatilityFeeCoefficientNad: 0,
    volatilityHalfLifeMs: 60_000,
    volatilityShockCapNad: 0,
    volatilityAccumulatorCapNad: 0,
  }).profile;
  const args = {
    nonce: 1n,
    expectedEpoch: 0n,
    updates: [anchorParameterUpdate({ kind: "fee", profile })],
    metadata: {
      version: 1,
      title: "Lower the fee",
      descriptionUri: "https://example.test/p.md",
      descriptionSha256: Array(32).fill(2),
      descriptionLen: 10,
    },
  };
  const [normalized] = normalizeInstructionArgs(program.idl, "announceOperatorChange", args);
  assert.equal(normalized.nextOperator, null);
  assert.equal(normalized.interestShareBps, null);
  // A tuple variant may be keyed by index or written as a tuple.
  const keyed = program.coder.types.encode("marketParameterUpdate", normalized.updates[0]);
  const [asTuple] = normalizeInstructionArgs(program.idl, "announceOperatorChange", { ...args, updates: [{ fee: [profile] }] });
  assert.deepEqual(program.coder.types.encode("marketParameterUpdate", asTuple.updates[0]), keyed);

  const fails = (input, path, detail) =>
    assert.throws(
      () => normalizeInstructionArgs(program.idl, "announceOperatorChange", input),
      (error) => error instanceof DuskInstructionInputError && error.path === path && detail.test(error.message)
    );
  fails({ ...args, metadata: { ...args.metadata, descriptionSha256: Array(31).fill(2) } },
    "metadata.descriptionSha256", /must hold exactly 32 elements, received 31/);
  fails({ ...args, updates: [{ fee: { 0: { ...profile, baseFeeBp: 1 } } }] }, "updates[0].fee.0.baseFeeBp", /unknown field/);
  const { volatilityHalfLifeMs, ...withoutHalfLife } = profile;
  fails({ ...args, updates: [{ fee: { 0: withoutHalfLife } }] }, "updates[0].fee.0.volatilityHalfLifeMs", /missing field/);
  fails({ ...args, updates: [{ fee: { 1: profile } }] }, "updates[0].fee.1", /tuple fields are keyed 0–0/);
});

test("composite builders check the caller's fields the same way", async () => {
  const swapAccounts = {
    market,
    trader: owner,
    assetInMint: baseMint,
    assetOutMint: quoteMint,
    reserveInVault: key(30),
    reserveOutVault: key(31),
    traderAssetInAccount: key(32),
    traderAssetOutAccount: key(33),
  };
  const swap = await write.swapInstruction({ exactAssetIn: 100n, minAssetOut: "90" }, { market, accounts: swapAccounts });
  const decoded = program.coder.instruction.decode(swap.data).data.args;
  assert.equal(decoded.exactAssetIn.toString(), "100");
  assert.equal(decoded.minAssetOut.toString(), "90");
  await rejectsInput(
    write.swapInstruction({ exactAssetIn: 100n, minAssetOt: 90n }, { market, accounts: swapAccounts }),
    "swap",
    "minAssetOt",
    /unknown field "minAssetOt"; did you mean "minAssetOut"\?/
  );
  await rejectsInput(
    write.referredBorrow(
      { borrowAmount: 10n, minDebtAmountOut: 9n, minLiquidationCfBp: 7_500 },
      { payer: owner, referrer: key(34), market, debtMint: baseMint, accounts: borrowAccounts }
    ),
    "borrow",
    "minLiquidationCfBp",
    /did you mean "minLiquidationCfBps"\?/
  );
  const hlpAccounts = {
    futarchyAuthority: key(35),
    srlpMint: key(7),
    baseReserveVault: key(17),
    quoteReserveVault: key(18),
    ownerTargetAccount: key(36),
    ownerHlpAccount: key(37),
    hlpSrlpAccount: key(38),
  };
  await rejectsInput(
    write.depositSingleSided(
      { depositAmount: 100n },
      { payer: owner, owner, market, targetHlpMint: key(39), baseMint, quoteMint, accounts: hlpAccounts }
    ),
    "depositSingleSided",
    "minHlpAmount",
    /missing field "minHlpAmount"/
  );
});
