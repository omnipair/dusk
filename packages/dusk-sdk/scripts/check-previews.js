import assert from "node:assert/strict";
import { test } from "node:test";
import {
  ComputeBudgetProgram,
  Connection,
  PublicKey,
  SystemProgram,
  Transaction,
  TransactionInstruction,
  VersionedTransaction,
} from "@solana/web3.js";

import { DuskGet, DuskSimulationError } from "../dist/get.js";

const payer = new PublicKey(new Uint8Array(32).fill(1));
const programId = new PublicKey(new Uint8Array(32).fill(2));
const instruction = SystemProgram.transfer({
  fromPubkey: payer,
  toPubkey: programId,
  lamports: 1,
});
const returnData = { programId: programId.toBase58(), data: ["AQ==", "base64"] };

function fixture(value = { err: null, logs: [], returnData }) {
  const calls = [];
  const connection = new Connection("https://rpc.invalid", {
    commitment: "confirmed",
    fetch: async (_url, init) => {
      const request = JSON.parse(init.body);
      calls.push(request);
      assert.equal(
        request.method,
        "simulateTransaction",
        "previews must not request or cache a blockhash"
      );
      return new Response(
        JSON.stringify({
          jsonrpc: "2.0",
          id: request.id,
          result: { context: { slot: 123 }, value },
        })
      );
    },
  });
  const reader = new DuskGet({ programId, provider: { connection, publicKey: payer } });
  return { reader, calls, connection };
}

test("repeated SDK previews use RPC blockhash replacement and preserve instructions", async () => {
  const { reader, calls, connection } = fixture();
  const simulate = connection.simulateTransaction;
  for (let i = 0; i < 12; i++) {
    assert.deepEqual(await reader.simulateReturnData(instruction), returnData);
  }
  assert.equal(calls.length, 12);
  assert.equal(
    connection.simulateTransaction,
    simulate,
    "the SDK must not patch the shared connection"
  );
  const expected = new Transaction({
    feePayer: payer,
    recentBlockhash: PublicKey.default.toBase58(),
  }).add(
    ComputeBudgetProgram.requestHeapFrame({ bytes: 256 * 1024 }),
    ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
    instruction
  );
  for (const call of calls) {
    assert.deepEqual(call.params[1], {
      encoding: "base64",
      commitment: "confirmed",
      sigVerify: false,
      replaceRecentBlockhash: true,
    });
    const sent = VersionedTransaction.deserialize(Buffer.from(call.params[0], "base64"));
    assert.deepEqual(Buffer.from(sent.message.serialize()), expected.serializeMessage());
    assert.ok(sent.signatures.every((signature) => signature.every((byte) => byte === 0)));
  }
});

test("previews honor an explicit commitment, fee payer and compute budgets", async () => {
  const { reader, calls } = fixture();
  await reader.simulateReturnData(instruction, {
    commitment: "finalized",
    feePayer: programId,
    computeUnitLimit: 300_000,
    heapFrameBytes: 64 * 1024,
  });
  assert.equal(calls[0].params[1].commitment, "finalized");
  const sent = VersionedTransaction.deserialize(Buffer.from(calls[0].params[0], "base64"));
  const expected = new Transaction({
    feePayer: programId,
    recentBlockhash: PublicKey.default.toBase58(),
  }).add(
    ComputeBudgetProgram.requestHeapFrame({ bytes: 64 * 1024 }),
    ComputeBudgetProgram.setComputeUnitLimit({ units: 300_000 }),
    instruction
  );
  assert.deepEqual(Buffer.from(sent.message.serialize()), expected.serializeMessage());
});

test("simulation preserves supplied instruction indices and overrides budgets in place", async () => {
  const { reader, calls } = fixture();
  const heap = ComputeBudgetProgram.requestHeapFrame({ bytes: 64 * 1024 });
  const limit = ComputeBudgetProgram.setComputeUnitLimit({ units: 500_000 });
  const supplied = [heap, instruction, limit];
  await reader.simulateWithContext(supplied, { computeUnitLimit: 700_000 });
  const sent = VersionedTransaction.deserialize(Buffer.from(calls[0].params[0], "base64"));
  const expected = new Transaction({ feePayer: payer, recentBlockhash: PublicKey.default.toBase58() })
    .add(heap, instruction, ComputeBudgetProgram.setComputeUnitLimit({ units: 700_000 }));
  assert.deepEqual(Buffer.from(sent.message.serialize()), expected.serializeMessage());
  assert.equal(supplied[2], limit, "do not mutate the caller's execution plan");
  await assert.rejects(reader.simulateWithContext([heap, heap, instruction]), /Duplicate compute-budget/);
  await assert.rejects(reader.simulateWithContext([instruction], { preserveInstructionIndices: true }),
    /requires planned heap and compute budgets/);
});

test("simulation errors, absent return data and another program's data still fail", async () => {
  for (const [value, message] of [
    [
      { err: { InstructionError: [0, { Custom: 6000 }] }, logs: ["Program error"] },
      "Dusk simulation failed",
    ],
    [{ err: null, logs: [] }, "Dusk simulation did not return data"],
    [
      { err: null, logs: [], returnData: { ...returnData, programId: payer.toBase58() } },
      "Dusk simulation returned data from a different program",
    ],
  ]) {
    const { reader } = fixture(value);
    await assert.rejects(reader.simulateReturnData(instruction), (error) => {
      assert.ok(error instanceof DuskSimulationError);
      assert.equal(error.message, message);
      assert.deepEqual(error.simulation.value, value);
      return true;
    });
  }
});

test("a state-changing instruction may be simulated without return data", async () => {
  const { reader } = fixture({ err: null, logs: [] });
  const result = await reader.simulateWithContext([instruction], { requireReturnData: false });
  assert.equal(result.context.slot, 123);
});

test("context simulations retain actual slots, ordered account snapshots and request time", async () => {
  const accounts = [
    {
      owner: programId.toBase58(),
      executable: false,
      lamports: 1,
      data: ["AQ==", "base64"],
      rentEpoch: 0,
    },
  ];
  const { reader, calls } = fixture({ err: null, logs: ["preview"], returnData, accounts });
  const start = Date.now();
  const result = await reader.simulateWithContext([instruction], {
    minContextSlot: 122,
    accounts: [payer],
  });
  assert.equal(result.context.slot, 123);
  assert.deepEqual(result.value.accounts, accounts);
  assert.deepEqual(result.value.logs, ["preview"]);
  assert.ok(result.observedAt >= start && result.observedAt <= Date.now());
  assert.equal(calls[0].params[1].minContextSlot, 122);
  assert.deepEqual(calls[0].params[1].accounts, {
    encoding: "base64",
    addresses: [payer.toBase58()],
  });
});

test("context simulations reject stale banks and missing account snapshots", async () => {
  const { reader } = fixture();
  await assert.rejects(reader.simulateWithContext([instruction], { minContextSlot: 124 }), /stale/);
  await assert.rejects(
    reader.simulateWithContext([instruction], { accounts: [payer] }),
    /incomplete accounts/
  );
  await assert.rejects(
    reader.simulateWithContext([instruction], { minContextSlot: NaN }),
    /minimum slot/
  );
  await assert.rejects(reader.simulateWithContext([instruction], { timeoutMs: 0 }), /timeout/);
});

test("hung preview simulations can time out or abort without accepting a late result", async (t) => {
  const { reader, connection } = fixture();
  t.mock.method(connection, "simulateTransaction", () => new Promise(() => {}));
  await assert.rejects(reader.simulateWithContext([instruction], { timeoutMs: 10 }), {
    name: "DuskPreviewTimeoutError",
  });
  const controller = new AbortController();
  const result = reader.simulateWithContext([instruction], { signal: controller.signal });
  controller.abort();
  await assert.rejects(result, { name: "AbortError" });
  await assert.rejects(reader.simulateWithContext([instruction], { signal: controller.signal }), {
    name: "AbortError",
  });
});

test("order fees use 10 bps, round up, and reject non-u64 values", async () => {
  const { calculateOrderProtocolFee, ORDER_PROTOCOL_FEE_BPS, orderProtocolFeeAccounts } = await import("../dist/delegate.js");
  const { getAssociatedTokenAddressSync, TOKEN_2022_PROGRAM_ID } = await import("@solana/spl-token");
  assert.equal(ORDER_PROTOCOL_FEE_BPS, 10n);
  assert.equal(calculateOrderProtocolFee(0), 0n);
  assert.equal(calculateOrderProtocolFee(1), 1n);
  assert.equal(calculateOrderProtocolFee(1_001), 2n);
  assert.equal(calculateOrderProtocolFee("10000000"), 10000n);
  assert.equal(calculateOrderProtocolFee(0xffffffffffffffffn), 18446744073709552n);
  assert.throws(() => calculateOrderProtocolFee(-1));
  assert.throws(() => calculateOrderProtocolFee(0x10000000000000000n));
  assert.throws(() => calculateOrderProtocolFee(Number.MAX_SAFE_INTEGER + 1));
  assert.equal(orderProtocolFeeAccounts(payer, programId, TOKEN_2022_PROGRAM_ID).feeRecipient.toBase58(),
    getAssociatedTokenAddressSync(programId, payer, true, TOKEN_2022_PROGRAM_ID).toBase58());
});

test("hLP capacity helper binds all mints and preserves native status, ceilings and slot", async () => {
  const { createDuskProgram } = await import("../dist/program.js");
  const { deriveFutarchyAuthorityAddress } = await import("../dist/constants.js");
  const { default: anchor } = await import("@coral-xyz/anchor");
  const baseMint = new PublicKey(new Uint8Array(32).fill(3));
  const quoteMint = new PublicKey(new Uint8Array(32).fill(4));
  const targetHlpMint = new PublicKey(new Uint8Array(32).fill(5));
  const rpcValue = { err: null, logs: [], returnData: null };
  const { connection, calls } = fixture(rpcValue);
  const program = createDuskProgram({ connection, programId });
  const expected = {
    market: payer, targetAsset: { base: {} }, hlpMint: targetHlpMint,
    slot: new anchor.BN(123), epoch: new anchor.BN(9), status: { ready: {} },
    fundingLimitGross: new anchor.BN(5_101), fundingLimitNet: new anchor.BN(5_000),
  };
  const encoded = program.coder.types.encode("hlpDepositCapacityPreview", expected);
  rpcValue.returnData = { programId: programId.toBase58(), data: [encoded.toString("base64"), "base64"] };
  const reader = new DuskGet(program);
  const result = await reader.previewHlpDepositCapacity({
    market: payer, baseMint, quoteMint, targetHlpMint, feePayer: payer,
  });
  assert.deepEqual(result.status, { ready: {} });
  assert.equal(result.slot.toString(), "123");
  assert.equal(result.fundingLimitGross.toString(), "5101");
  assert.equal(result.fundingLimitNet.toString(), "5000");
  const tx = VersionedTransaction.deserialize(Buffer.from(calls[0].params[0], "base64"));
  const ix = tx.message.compiledInstructions.at(-1);
  assert.equal(tx.message.staticAccountKeys[ix.programIdIndex].toBase58(), programId.toBase58());
  assert.deepEqual(Array.from(ix.accountKeyIndexes, (i) => tx.message.staticAccountKeys[i].toBase58()), [
    payer, deriveFutarchyAuthorityAddress(programId)[0], baseMint, quoteMint, targetHlpMint,
  ].map((key) => key.toBase58()));
});


test("hLP flash simulation never inserts budgets ahead of a bound settlement", async () => {
  const { reader, calls } = fixture();
  const discriminator = [1, 3, 5, 7, 2, 4, 6, 8];
  reader.program.idl = { instructions: [{ name: "beginHlpFlash", discriminator }] };
  const begin = new TransactionInstruction({ programId, keys: [], data: Buffer.from(discriminator) });
  await assert.rejects(reader.simulateWithContext([begin]), /requires planned heap and compute budgets/);
  assert.equal(calls.length, 0);
  const instructions = [ComputeBudgetProgram.requestHeapFrame({ bytes: 256 * 1024 }),
    ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }), begin];
  await reader.simulateWithContext(instructions);
  const sent = VersionedTransaction.deserialize(Buffer.from(calls[0].params[0], "base64"));
  assert.deepEqual([...sent.message.compiledInstructions[2].data], discriminator);
  assert.equal(sent.message.compiledInstructions.length, 3);
});
