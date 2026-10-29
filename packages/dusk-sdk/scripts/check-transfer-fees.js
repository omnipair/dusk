import assert from "node:assert/strict";
import { test } from "node:test";
import { readFileSync } from "node:fs";
import { PublicKey, SYSVAR_CLOCK_PUBKEY } from "@solana/web3.js";
import { TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID, TransferFeeConfigLayout } from "@solana/spl-token";
import { transferFeeNotice, transferFeeCredit, fetchTransferFeeNotices } from "../dist/transfer-fees.js";
const vectors = JSON.parse(readFileSync(new URL("fixtures/transfer-fee-notice.json", import.meta.url)));
const fee = ([epoch, transferFeeBasisPoints, maximumFee]) => ({ epoch: BigInt(epoch), transferFeeBasisPoints, maximumFee: BigInt(maximumFee) });
test("fee notice vectors: current vs admission vs final-notice liquidation", () => {
  for (const v of vectors) {
    const notice = transferFeeNotice({ olderTransferFee: fee(v.older), newerTransferFee: fee(v.newer) }, BigInt(v.epoch));
    assert.equal(notice.phase, v.phase, v.name);
    for (const mode of ["effective", "admission", "liquidation"]) assert.equal(transferFeeCredit(BigInt(v.gross), notice, mode).toString(), v[mode], v.name);
  }
  assert.equal(transferFeeCredit(1n, transferFeeNotice(null, 1n)), 1n);
  assert.throws(() => transferFeeNotice(null, -1n));
  assert.throws(() => transferFeeCredit(1n << 64n, transferFeeNotice(null, 1n)));
});
test("coherent RPC snapshot invalidates on epoch, schedule and authority changes", async () => {
  const mint = new PublicKey(new Uint8Array(32).fill(7));
  const zero = PublicKey.default;
  const data = Buffer.alloc(278); data[45] = 1; data[165] = 1; data.writeUInt16LE(1, 166); data.writeUInt16LE(108, 168);
  const config = { transferFeeConfigAuthority: mint, withdrawWithheldAuthority: zero, withheldAmount: 0n, olderTransferFee: fee(vectors[0].older), newerTransferFee: fee(vectors[0].newer) };
  const encode = () => TransferFeeConfigLayout.encode(config, data.subarray(170)); encode();
  const clock = Buffer.alloc(40); clock.writeBigUInt64LE(100n); clock.writeBigUInt64LE(10n, 16);
  const info = (data, owner) => ({ data, owner, executable: false, lamports: 1 });
  let values = [info(data, TOKEN_2022_PROGRAM_ID), info(clock, new PublicKey("Sysvar1111111111111111111111111111111111111"))];
  const connection = { getMultipleAccountsInfoAndContext: async (keys, options) => {
    assert.ok(keys[1].equals(SYSVAR_CLOCK_PUBKEY)); assert.equal(options.commitment, "confirmed"); assert.equal(options.minContextSlot, 90);
    return { context: { slot: 100 }, value: values };
  } };
  const read = () => fetchTransferFeeNotices(connection, [mint], { minContextSlot: 90 });
  const first = (await read()).mints[0]; assert.equal(first.phase, "notice"); assert.equal(first.nextRecheckEpoch, 11n);
  clock.writeBigUInt64LE(11n, 16);
  const final = (await read()).mints[0]; assert.equal(final.phase, "finalNotice"); assert.notEqual(first.refreshKey, final.refreshKey); assert.equal(final.nextRecheckEpoch, 12n);
  config.newerTransferFee = { ...config.olderTransferFee, epoch: 13n }; encode();
  const canceled = (await read()).mints[0]; assert.notEqual(canceled.refreshKey, final.refreshKey); assert.equal(transferFeeCredit(1000n, canceled, "liquidation"), 970n);
  config.transferFeeConfigAuthority = zero; encode(); assert.equal((await read()).mints[0].feeAuthority, null);
  values[0].owner = zero; await assert.rejects(read, /owner/);
  const legacy = Buffer.alloc(82); legacy[45] = 1; values[0] = info(legacy, TOKEN_PROGRAM_ID);
  assert.equal((await read()).mints[0].phase, "none");
  clock.writeBigUInt64LE(99n); await assert.rejects(read, /Clock/);
  values[1] = null; await assert.rejects(read, /Clock/);
});
