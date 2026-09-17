import assert from "node:assert/strict";
import { test } from "node:test";
import {
  ComputeBudgetProgram,
  Connection,
  PublicKey,
  SystemProgram,
  Transaction,
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
      assert.equal(request.method, "simulateTransaction", "previews must not request or cache a blockhash");
      return new Response(JSON.stringify({
        jsonrpc: "2.0",
        id: request.id,
        result: { context: { slot: 123 }, value },
      }));
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
  assert.equal(connection.simulateTransaction, simulate, "the SDK must not patch the shared connection");
  const expected = new Transaction({ feePayer: payer, recentBlockhash: PublicKey.default.toBase58() }).add(
    ComputeBudgetProgram.requestHeapFrame({ bytes: 256 * 1024 }),
    ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
    instruction
  );
  for (const call of calls) {
    assert.deepEqual(call.params[1], {
      encoding: "base64", commitment: "confirmed", sigVerify: false, replaceRecentBlockhash: true,
    });
    const sent = VersionedTransaction.deserialize(Buffer.from(call.params[0], "base64"));
    assert.deepEqual(Buffer.from(sent.message.serialize()), expected.serializeMessage());
    assert.ok(sent.signatures.every((signature) => signature.every((byte) => byte === 0)));
  }
});

test("previews honor an explicit commitment, fee payer and compute budgets", async () => {
  const { reader, calls } = fixture();
  await reader.simulateReturnData(instruction, {
    commitment: "finalized", feePayer: programId, computeUnitLimit: 300_000, heapFrameBytes: 64 * 1024,
  });
  assert.equal(calls[0].params[1].commitment, "finalized");
  const sent = VersionedTransaction.deserialize(Buffer.from(calls[0].params[0], "base64"));
  const expected = new Transaction({ feePayer: programId, recentBlockhash: PublicKey.default.toBase58() }).add(
    ComputeBudgetProgram.requestHeapFrame({ bytes: 64 * 1024 }),
    ComputeBudgetProgram.setComputeUnitLimit({ units: 300_000 }),
    instruction
  );
  assert.deepEqual(Buffer.from(sent.message.serialize()), expected.serializeMessage());
});

test("simulation errors, absent return data and another program's data still fail", async () => {
  for (const [value, message] of [
    [{ err: { InstructionError: [0, { Custom: 6000 }] }, logs: ["Program error"] }, "Dusk simulation failed"],
    [{ err: null, logs: [] }, "Dusk simulation did not return data"],
    [{ err: null, logs: [], returnData: { ...returnData, programId: payer.toBase58() } }, "Dusk simulation returned data from a different program"],
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
