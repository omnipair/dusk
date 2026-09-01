import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { Keypair, PublicKey, Transaction, TransactionInstruction } from "@solana/web3.js";
import { LiteSVM } from "litesvm";

const EXPECTED_SHA256 = "08672b4c1d665c79b007d72e19d98d07a6d522232410d39d82f33e2670d53800";
const EXPECTED_CODE = Buffer.from("b7000000010000009500000000000000", "hex");
const EM_SBF = 0x107;

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const programPath = resolve(repoRoot, "target/deploy/dusk_emergency_halt.so");
const elf = readFileSync(programPath);

assert.equal(elf.subarray(0, 4).toString("hex"), "7f454c46", "ELF magic");
assert.equal(elf[4], 2, "64-bit ELF class");
assert.equal(elf[5], 1, "little-endian ELF encoding");
assert.equal(elf.readUInt16LE(16), 3, "shared-object ELF type");
assert.equal(elf.readUInt16LE(18), EM_SBF, "Solana SBF machine type");
assert.ok(elf.length <= 512, `halt artifact exceeds one 512-byte budget: ${elf.length}`);

const entry = Number(elf.readBigUInt64LE(24));
assert.deepEqual(elf.subarray(entry, entry + EXPECTED_CODE.length), EXPECTED_CODE, "halt opcodes");

const programHeaderOffset = Number(elf.readBigUInt64LE(32));
assert.equal(elf.readUInt16LE(56), 1, "program-header count");
assert.equal(elf.readUInt32LE(programHeaderOffset), 1, "loadable segment type");
assert.equal(elf.readUInt32LE(programHeaderOffset + 4), 5, "read/execute-only segment flags");

const digest = createHash("sha256").update(elf).digest("hex");
assert.equal(digest, EXPECTED_SHA256, "reproducible halt artifact SHA-256");

const svm = new LiteSVM().withSigverify(false).withBlockhashCheck(false);
const programId = new PublicKey(new Uint8Array(32).fill(7));
const payer = Keypair.generate();
const untouchedAccount = new PublicKey(new Uint8Array(32).fill(9));
const untouchedData = Uint8Array.from([1, 3, 3, 7]);

svm.addProgram(programId, new Uint8Array(elf));
svm.airdrop(payer.publicKey, 1_000_000_000n);
svm.setAccount(untouchedAccount, {
  lamports: 1_000_000,
  data: untouchedData,
  owner: programId,
  executable: false,
  rentEpoch: 0,
});

const transaction = new Transaction().add(
  new TransactionInstruction({
    programId,
    keys: [{ pubkey: untouchedAccount, isSigner: false, isWritable: true }],
    data: Buffer.alloc(64, 0xa5),
  }),
);
transaction.feePayer = payer.publicKey;
transaction.recentBlockhash = svm.latestBlockhash();
transaction.sign(payer);

const result = svm.sendTransaction(transaction);
assert.equal(typeof result.err, "function", "halt invocation must fail");
assert.match(result.err().toString(), /InstructionErrorCustom \{ code: 1 \}/, "custom halt error");
assert.equal(BigInt(result.meta().computeUnitsConsumed()), 2n, "halt compute units");

const accountAfter = svm.getAccount(untouchedAccount);
assert.ok(accountAfter, "writable account remains present");
assert.equal(accountAfter.lamports, 1_000_000, "writable account lamports remain unchanged");
assert.deepEqual(Buffer.from(accountAfter.data), Buffer.from(untouchedData), "writable account data remains unchanged");

console.log(`Emergency halt artifact verified: ${elf.length} bytes, SHA-256 ${digest}, Custom(1), 2 CU.`);
