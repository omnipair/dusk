import assert from "node:assert/strict";
import { test } from "node:test";

import anchor from "@coral-xyz/anchor";
import { TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID } from "@solana/spl-token";
import { PublicKey, SystemProgram } from "@solana/web3.js";

import DELEGATE_IDL from "../dist/idl_delegate.js";
import { DUSK_PROGRAM_ID, deriveEventAuthorityAddress, deriveLeveragePositionAddress } from "../dist/constants.js";
import { createLeverageDelegateProgram, deriveLeverageOrderAddress, DuskLeverageOrders } from "../dist/delegate.js";
import { DuskInstructionInputError, normalizeInstructionArgs } from "../dist/instruction-args.js";
import { deriveProtectionOrderAddress, DuskProtectionOrders, PROTECTION_ACTION } from "../dist/protection.js";

const { BN } = anchor;
const key = (byte) => new PublicKey(new Uint8Array(32).fill(byte));
const owner = key(1);
const market = key(2);
const positionId = key(3);
const debtMint = key(4);
const collateralMint = key(5);
const provider = {
  connection: { getAccountInfo: async () => null },
  publicKey: owner,
  wallet: { publicKey: owner },
};
const program = createLeverageDelegateProgram({ provider });
const orders = new DuskLeverageOrders(program);
const leveragePosition = deriveLeveragePositionAddress(market, owner, positionId)[0];

/** Rejects with a DuskInstructionInputError naming the delegate instruction, the field and the detail. */
async function rejectsInput(promise, instruction, path, detail) {
  await assert.rejects(promise, (error) => {
    assert.ok(error instanceof DuskInstructionInputError, `expected DuskInstructionInputError, got ${error}`);
    assert.equal(error.instruction, instruction);
    assert.equal(error.path, path);
    assert.match(error.message, new RegExp(`^Leverage delegate instruction "${instruction}": `));
    assert.match(error.message, detail);
    return true;
  });
}

function assertSameInstruction(actual, expected) {
  assert.ok(actual.programId.equals(expected.programId));
  assert.deepEqual(actual.data, expected.data);
  assert.deepEqual(actual.keys, expected.keys);
}

test("createOrderInstruction encodes the same bytes as before, for every integer form", async () => {
  const order = deriveLeverageOrderAddress(leveragePosition, owner, 7n, program.programId)[0];
  // What the client sent before this check existed: BN arguments through
  // Anchor, with the same accounts.
  const expected = await program.methods
    .createLeverageOrder({ orderId: new BN(7), kind: 2, triggerPriceNad: new BN("1500000000"), closeBps: 5_000, minOwnerPayout: new BN(42) })
    .accounts({ market, leveragePosition, order, owner, systemProgram: SystemProgram.programId })
    .remainingAccounts([])
    .instruction();
  for (const [orderId, triggerPriceNad] of [
    [7n, 1_500_000_000n],
    [7, 1_500_000_000],
    ["7", "1500000000"],
  ]) {
    const instruction = await orders.createOrderInstruction({
      market,
      owner,
      positionId,
      orderId,
      kind: "stopLoss",
      triggerPriceNad,
      closeBps: 5_000,
      minOwnerPayout: 42n,
    });
    assertSameInstruction(instruction, expected);
  }
});

test("createOrderInstruction rejects an unknown kind instead of encoding kind 0", async () => {
  const params = { market, owner, positionId, orderId: 1n, minOwnerPayout: 1n, triggerPriceNad: 1n, closeBps: 10_000 };
  await rejectsInput(
    orders.createOrderInstruction({ ...params, kind: "takeprofit" }),
    "createLeverageOrder",
    "kind",
    /"kind" must be "takeProfit" or "stopLoss", received string "takeprofit"/
  );
  await rejectsInput(orders.createOrderInstruction(params), "createLeverageOrder", "kind", /received undefined/);
  // Inherited Object members are not order kinds either.
  await rejectsInput(orders.createOrderInstruction({ ...params, kind: "toString" }), "createLeverageOrder", "kind", /received string "toString"/);
});

test("order ids and triggers are checked before an address is derived from them", async () => {
  const params = { market, owner, positionId, kind: "takeProfit", orderId: 1n, minOwnerPayout: 1n, triggerPriceNad: 1n, closeBps: 10_000 };
  await rejectsInput(orders.createOrderInstruction({ ...params, orderId: "1.5" }), "createLeverageOrder", "orderId", /unsigned 64-bit integer .*received string "1.5"/);
  await rejectsInput(orders.createOrderInstruction({ ...params, orderId: -1n }), "createLeverageOrder", "orderId", /between 0 and 18446744073709551615, received -1/);
  await rejectsInput(orders.createOrderInstruction({ ...params, orderId: 2n ** 64n }), "createLeverageOrder", "orderId", /between 0 and 18446744073709551615/);
  await rejectsInput(orders.createOrderInstruction({ ...params, triggerPriceNad: undefined }), "createLeverageOrder", "triggerPriceNad", /missing field "triggerPriceNad"/);
  await rejectsInput(orders.createOrderInstruction({ ...params, minOwnerPayout: undefined }), "createLeverageOrder", "minOwnerPayout", /missing field "minOwnerPayout"/);
  await rejectsInput(orders.cancelOrderInstruction({ owner, market, positionId, orderId: 2 ** 53 }), "cancelLeverageOrder", "orderId", /received number 9007199254740992/);
});

test("closeBps keeps its range check", async () => {
  const params = { market, owner, positionId, kind: "takeProfit", orderId: 1n, minOwnerPayout: 1n, triggerPriceNad: 1n };
  for (const closeBps of [0, 10_001, 1.5]) {
    await assert.rejects(orders.createOrderInstruction({ ...params, closeBps }), /closeBps must be between 1 and 10000/);
  }
});

test("cancelOrderInstruction encodes the same bytes as before, however the order is located", async () => {
  const order = deriveLeverageOrderAddress(leveragePosition, owner, 9n, program.programId)[0];
  const expected = await program.methods.cancelLeverageOrder({ orderId: new BN(9) }).accounts({ order, owner }).instruction();
  for (const located of [{ order }, { leveragePosition }, { market, positionId }]) {
    for (const orderId of [9n, 9, "9"]) {
      assertSameInstruction(await orders.cancelOrderInstruction({ owner, orderId, ...located }), expected);
    }
  }
  await assert.rejects(orders.cancelOrderInstruction({ owner, orderId: 9n }), /needs `order`, or `leveragePosition`/);
});

const entryArgs = {
  orderId: 11n,
  positionId,
  debtAsset: 1,
  depositAmount: 1_000_000n,
  protocolFeeDepositAmount: 1_000n,
  minMarginAmount: 990_000n,
  executorBounty: 5_000n,
  multiplierBps: 30_000n,
  limitPriceNad: 2_000_000_000n,
  minCollateralOut: 1n,
  expiryUnixTimestamp: 1_900_000_000n,
  referrer: null,
};
const entryAccounts = {
  market,
  debtMint,
  collateralMint,
  ownerFundingAccount: key(6),
  fundingVault: key(7),
};

test("the generic path builds entry orders exactly as raw Anchor did", async () => {
  const order = PublicKey.findProgramAddressSync(
    [Buffer.from("leverage_entry_order"), market.toBuffer(), owner.toBuffer(), new BN(11).toArrayLike(Buffer, "le", 8)],
    program.programId
  )[0];
  const hook = [{ pubkey: key(8), isSigner: false, isWritable: false }];
  const bnArgs = Object.fromEntries(
    Object.entries(entryArgs).map(([name, value]) => [name, typeof value === "bigint" ? new BN(value.toString()) : value])
  );
  const expected = await program.methods
    .createLeverageEntryOrder(bnArgs)
    .accountsStrict({
      ...entryAccounts,
      order,
      owner,
      tokenProgram: TOKEN_PROGRAM_ID,
      token2022Program: TOKEN_2022_PROGRAM_ID,
      systemProgram: SystemProgram.programId,
    })
    .remainingAccounts(hook)
    .instruction();
  // Anchor derives the order PDA and fills the signer and programs.
  const built = await orders.instruction("createLeverageEntryOrder", entryArgs, { accounts: entryAccounts, remainingAccounts: hook });
  assertSameInstruction(built, expected);
  // A left-out Option encodes as None, the same as null.
  const { referrer, ...withoutReferrer } = entryArgs;
  assert.equal(referrer, null);
  assertSameInstruction(
    await orders.instruction("createLeverageEntryOrder", withoutReferrer, { accounts: entryAccounts, remainingAccounts: hook }),
    expected
  );
});

test("an entry order without its fee deposit throws instead of encoding zero", async () => {
  // dusk-webapp PR #98 shipped this call with the fee deposit at zero.
  const { protocolFeeDepositAmount, ...withoutFee } = entryArgs;
  assert.equal(protocolFeeDepositAmount, 1_000n);
  await rejectsInput(
    orders.instruction("createLeverageEntryOrder", withoutFee, { accounts: entryAccounts }),
    "createLeverageEntryOrder",
    "protocolFeeDepositAmount",
    /missing field "protocolFeeDepositAmount"/
  );
  await rejectsInput(
    orders.instruction("createLeverageEntryOrder", { ...withoutFee, protocolFeeDepositAmout: 1_000n }, { accounts: entryAccounts }),
    "createLeverageEntryOrder",
    "protocolFeeDepositAmout",
    /unknown field "protocolFeeDepositAmout"; did you mean "protocolFeeDepositAmount"\?/
  );
  await rejectsInput(
    orders.transaction("createLeverageEntryOrder", { ...entryArgs, debtAsset: "quote" }, { accounts: entryAccounts }),
    "createLeverageEntryOrder",
    "debtAsset",
    /unsigned 8-bit integer .*received string "quote"/
  );
  await rejectsInput(
    orders.instruction("createLeverageEntryOrder", { ...entryArgs, positionId: "not a key" }, { accounts: entryAccounts }),
    "createLeverageEntryOrder",
    "positionId",
    /must be a PublicKey or base58 address/
  );
});

test("an account the delegate instruction does not declare throws", async () => {
  const { ownerFundingAccount, ...rest } = entryAccounts;
  await rejectsInput(
    orders.instruction("createLeverageEntryOrder", entryArgs, { accounts: { ...rest, ownerFundingAcount: ownerFundingAccount } }),
    "createLeverageEntryOrder",
    "ownerFundingAcount",
    /unknown account "ownerFundingAcount"; did you mean "ownerFundingAccount"\?/
  );
  await rejectsInput(
    orders.instruction("executeHlpOrder", { orderId: 1n }, { accounts: { protocolFee: { feeRecipent: key(9) } } }),
    "executeHlpOrder",
    "protocolFee.feeRecipent",
    /unknown account "protocolFee.feeRecipent"; did you mean "feeRecipient"\?/
  );
});

test("the generic path covers cancellations, hLP orders and argument-free instructions", async () => {
  const order = key(10);
  const cancelAccounts = { order, debtMint, fundingVault: key(7), ownerFundingAccount: key(6) };
  const expected = await program.methods
    .cancelLeverageEntryOrder({ orderId: new BN(11), minOwnerRefundOut: new BN(0) })
    .accountsStrict({ ...cancelAccounts, owner, tokenProgram: TOKEN_PROGRAM_ID, token2022Program: TOKEN_2022_PROGRAM_ID })
    .instruction();
  assertSameInstruction(
    await orders.instruction("cancelLeverageEntryOrder", { orderId: 11n, minOwnerRefundOut: 0 }, { accounts: cancelAccounts }),
    expected
  );
  await rejectsInput(
    orders.instruction("cancelLeverageEntryOrder", { orderId: 11n }, { accounts: cancelAccounts }),
    "cancelLeverageEntryOrder",
    "minOwnerRefundOut",
    /missing field "minOwnerRefundOut"/
  );
  await rejectsInput(
    orders.instruction("createHlpOrder", { orderId: 1n, kind: 1, hlpAmount: 1n, triggerNAD: 1n, minTargetAmountOut: 1n }, { accounts: {} }),
    "createHlpOrder",
    "triggerNAD",
    /did you mean "triggerNad"\?/
  );

  const protectionAccounts = { order, lpMint: key(11), custodyLpAccount: key(12), ownerLpAccount: key(13) };
  const cancel = await orders.instruction("cancelProtectionOrder", undefined, { accounts: protectionAccounts });
  assert.deepEqual(cancel.data, program.coder.instruction.encode("cancelProtectionOrder", {}));
  await rejectsInput(
    orders.instruction("cancelProtectionOrder", { orderId: 1n }, { accounts: protectionAccounts }),
    "cancelProtectionOrder",
    "",
    /expects no arguments, received 1/
  );
  assert.throws(() => orders.method("borrow", {}), /^Error: Unknown Leverage delegate instruction: borrow$/);
});

test("the check names the program from the IDL it is given", () => {
  assert.throws(
    () => normalizeInstructionArgs(program.idl, "fundProtectionOrder", { lpAmount: -1 }),
    (error) => error instanceof DuskInstructionInputError && /^Leverage delegate instruction "fundProtectionOrder": "lpAmount" must be between 0/.test(error.message)
  );
  // An IDL without metadata keeps the Dusk label.
  const { metadata, ...withoutMetadata } = program.idl;
  assert.equal(metadata.name, "leverageDelegate");
  assert.throws(
    () => normalizeInstructionArgs(withoutMetadata, "fundProtectionOrder", { lpAmount: -1 }),
    /^DuskInstructionInputError: Dusk instruction "fundProtectionOrder"/
  );
  // The raw snake_case IDL labels the same way.
  assert.equal(DELEGATE_IDL.metadata.name, "leverage_delegate");
});

const protectionParams = {
  orderId: 21n,
  action: PROTECTION_ACTION.borrowRepay,
  debtAsset: 1,
  lpAmount: 1_000n,
  maxLpPerExecution: 100n,
  maxPaymentPerExecution: 50n,
  triggerHealthBps: 11_000n,
  targetHealthBps: 12_000n,
  minPaymentPerLpNad: 1n,
  keeperFeeBps: 30,
  expiresAt: 1_900_000_000n,
};
const protectionCreateAccounts = {
  market,
  borrowPosition: key(14),
  leveragePosition: null,
  lpMint: key(11),
  baseMint: debtMint,
  quoteMint: collateralMint,
  ownerLpAccount: key(13),
  custodyLpAccount: key(12),
  baseYieldAccount: key(15),
  quoteYieldAccount: key(16),
  owner,
};

test("protection orders encode the same bytes as before and check their inputs", async () => {
  const protection = new DuskProtectionOrders(program);
  const order = deriveProtectionOrderAddress(owner, 21n, program.programId)[0];
  const bnParams = Object.fromEntries(
    Object.entries(protectionParams).map(([name, value]) => [name, typeof value === "bigint" ? new BN(value.toString()) : value])
  );
  const expected = await program.methods
    .createProtectionOrder(bnParams)
    .accountsStrict({
      ...protectionCreateAccounts,
      order,
      duskProgram: DUSK_PROGRAM_ID,
      duskEventAuthority: deriveEventAuthorityAddress()[0],
      token2022Program: TOKEN_2022_PROGRAM_ID,
      systemProgram: SystemProgram.programId,
    })
    .instruction();
  assertSameInstruction(await protection.createInstruction(protectionParams, protectionCreateAccounts), expected);

  // A missing u8 used to be spread through and encoded as 0: the base side.
  const { debtAsset, ...withoutDebtAsset } = protectionParams;
  assert.equal(debtAsset, 1);
  await rejectsInput(
    protection.createInstruction(withoutDebtAsset, protectionCreateAccounts),
    "createProtectionOrder",
    "debtAsset",
    /missing field "debtAsset"/
  );
  await rejectsInput(
    protection.createInstruction({ ...protectionParams, keeperFeeBp: 30 }, protectionCreateAccounts),
    "createProtectionOrder",
    "keeperFeeBp",
    /did you mean "keeperFeeBps"\?/
  );

  const manageAccounts = { order, lpMint: key(11), custodyLpAccount: key(12), ownerLpAccount: key(13), owner };
  const fund = await program.methods
    .fundProtectionOrder({ lpAmount: new BN(5) })
    .accountsStrict({ ...manageAccounts, token2022Program: TOKEN_2022_PROGRAM_ID })
    .instruction();
  assertSameInstruction(await protection.fundInstruction(5n, manageAccounts), fund);
  const { custodyLpAccount, ...withoutCustody } = manageAccounts;
  await rejectsInput(
    protection.cancelInstruction({ ...withoutCustody, custodyLpAcount: custodyLpAccount }),
    "cancelProtectionOrder",
    "custodyLpAcount",
    /did you mean "custodyLpAccount"\?/
  );
});

test("protection close fills in both token programs and checks its accounts", async () => {
  const protection = new DuskProtectionOrders(program);
  const accounts = {
    order: deriveProtectionOrderAddress(owner, 21n, program.programId)[0],
    custodyLpAccount: key(12),
    baseYieldAccount: key(15),
    quoteYieldAccount: key(16),
    baseMint: debtMint,
    quoteMint: collateralMint,
    custodyBaseAccount: key(28),
    custodyQuoteAccount: null,
    ownerBaseAccount: key(30),
    ownerQuoteAccount: null,
    owner,
  };
  const expected = await program.methods
    .closeProtectionOrder()
    .accountsStrict({ ...accounts, tokenProgram: TOKEN_PROGRAM_ID, token2022Program: TOKEN_2022_PROGRAM_ID })
    .instruction();
  assertSameInstruction(await protection.closeInstruction(accounts), expected);
  const { custodyLpAccount, ...withoutCustody } = accounts;
  await rejectsInput(
    protection.closeInstruction({ ...withoutCustody, custodyLpAcount: custodyLpAccount }),
    "closeProtectionOrder",
    "custodyLpAcount",
    /did you mean "custodyLpAccount"\?/
  );
});

test("protection execution checks its inputs before reading the order", async () => {
  const protection = new DuskProtectionOrders(program);
  const accounts = {
    protocolFee: { feeRecipient: key(20) },
    order: key(21),
    market,
    futarchyAuthority: key(22),
    borrowPosition: key(14),
    leveragePosition: null,
    positionOwner: owner,
    baseMint: debtMint,
    quoteMint: collateralMint,
    lpMint: key(11),
    srlpMint: key(23),
    baseReserveVault: key(24),
    quoteReserveVault: key(25),
    paymentVault: key(26),
    debtInterestVault: key(27),
    borrowedInterestVault: null,
    hlpSrlpAccount: null,
    custodyLpAccount: key(12),
    custodyBaseAccount: key(28),
    custodyQuoteAccount: key(29),
    baseYieldAccount: key(15),
    quoteYieldAccount: key(16),
    ownerBaseAccount: key(30),
    ownerQuoteAccount: key(31),
    keeperPaymentAccount: key(32),
    referralPartner: null,
    referralAccrual: null,
    keeper: key(33),
  };
  const fetch = program.account.protectionOrder.fetch;
  let reads = 0;
  program.account.protectionOrder.fetch = async () => {
    reads += 1;
    return { action: PROTECTION_ACTION.borrowCollateral };
  };
  try {
    const { keeperPaymentAccount, ...withoutKeeperPayment } = accounts;
    await rejectsInput(
      protection.executeInstruction({ lpAmount: 1n, paymentAmount: 1n }, { ...withoutKeeperPayment, keeperPaymentAcount: keeperPaymentAccount }),
      "executeProtectionOrder",
      "keeperPaymentAcount",
      /did you mean "keeperPaymentAccount"\?/
    );
    await rejectsInput(
      protection.executeInstruction({ lpAmount: 1n, paymentAmout: 1n }, accounts),
      "executeProtectionOrder",
      "paymentAmout",
      /did you mean "paymentAmount"\?/
    );
    assert.equal(reads, 0);

    // Market oracle reads need a live connection; this client's market has none.
    protection.oracleAccounts = async (_market, remaining) => remaining;
    const expected = await program.methods
      .executeProtectionOrder({ lpAmount: new BN(3), paymentAmount: new BN(4) })
      .accountsStrict({
        ...accounts,
        duskProgram: DUSK_PROGRAM_ID,
        duskEventAuthority: deriveEventAuthorityAddress()[0],
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
      })
      .instruction();
    assertSameInstruction(await protection.executeInstruction({ lpAmount: 3n, paymentAmount: "4" }, accounts), expected);
    assert.equal(reads, 1);
  } finally {
    program.account.protectionOrder.fetch = fetch;
  }
});
