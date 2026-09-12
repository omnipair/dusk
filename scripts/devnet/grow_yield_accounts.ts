/**
 * Bring every stale yield account on the deployment up to the current layout.
 *
 * `YieldAccount` gained `harvest_authority` -- an `Option<Pubkey>`, so one
 * discriminant byte and thirty-two of key -- and every account created before
 * that deploy is still the old length. Anchor reads the whole struct, so those
 * accounts cannot be opened: harvest, `set_harvest_authority`, single-sided
 * hLP deposits and governance support each fail with
 * `AccountDidNotDeserialize` before their handler runs.
 *
 * `grow_yield_account` is permissionless and idempotent -- an account already
 * at the right size returns Ok -- so this sweeps every one it finds rather
 * than tracking which have been done. The caller pays the rent for the bytes
 * it appends, which is a few thousand lamports each.
 *
 *   node --experimental-strip-types scripts/devnet/grow_yield_accounts.ts
 */

import {
  Connection,
  Keypair,
  PublicKey,
  Transaction,
  TransactionInstruction,
  SystemProgram,
} from "@solana/web3.js";
import { createHash } from "crypto";
import { readFileSync } from "fs";
import { homedir } from "os";
import { join } from "path";

const RPC = process.env.DUSK_RPC_URL ?? "https://api.devnet.solana.com";
const PROGRAM_ID = new PublicKey(
  process.env.DUSK_PROGRAM_ID ?? "JA8Zxxm4t4zopBL8e3dQQXWfQ3a5pBUPY9Sp9RnybV2X",
);
/** How many repairs fit in one transaction comfortably. */
const BATCH = 8;

function discriminator(kind: "global" | "account", name: string): Buffer {
  return createHash("sha256").update(`${kind}:${name}`).digest().subarray(0, 8);
}

function loadKeypair(): Keypair {
  const path =
    process.env.DUSK_KEYPAIR ?? join(homedir(), ".config/solana/id.json");
  return Keypair.fromSecretKey(
    Uint8Array.from(JSON.parse(readFileSync(path, "utf8"))),
  );
}

function growInstruction(payer: PublicKey, yieldAccount: PublicKey) {
  return new TransactionInstruction({
    data: discriminator("global", "grow_yield_account"),
    keys: [
      { isSigner: true, isWritable: true, pubkey: payer },
      { isSigner: false, isWritable: true, pubkey: yieldAccount },
      { isSigner: false, isWritable: false, pubkey: SystemProgram.programId },
    ],
    programId: PROGRAM_ID,
  });
}

async function main() {
  const payer = loadKeypair();
  const connection = new Connection(RPC, "confirmed");

  const accounts = await connection.getProgramAccounts(PROGRAM_ID, {
    filters: [
      {
        memcmp: {
          offset: 0,
          bytes: (await import("bs58")).default.encode(
            discriminator("account", "YieldAccount"),
          ),
        },
      },
    ],
  });
  if (accounts.length === 0) throw new Error("no yield accounts found");

  // The program knows the target size; nothing here does. Restating it
  // would be a second copy of `YieldAccount`'s layout free to disagree with
  // the first, and on this deployment every account is stale, so the
  // largest one on chain is not the answer either. So the sweep is offered
  // to all of them -- the instruction is idempotent and returns Ok for an
  // account already at the right size -- and the sizes afterwards are what
  // proves it worked.
  const sizes = new Map<number, number>();
  for (const a of accounts) {
    const n = a.account.data.length;
    sizes.set(n, (sizes.get(n) ?? 0) + 1);
  }
  console.log(`yield accounts: ${accounts.length}`);
  for (const [size, count] of [...sizes].sort((x, y) => x[0] - y[0])) {
    console.log(`  ${size} bytes: ${count}`);
  }
  const stale = accounts;
  console.log(`\noffering the repair to all ${stale.length}\n`);

  for (let index = 0; index < stale.length; index += BATCH) {
    const slice = stale.slice(index, index + BATCH);
    const transaction = new Transaction().add(
      ...slice.map((entry) => growInstruction(payer.publicKey, entry.pubkey)),
    );
    const { blockhash, lastValidBlockHeight } =
      await connection.getLatestBlockhash("confirmed");
    transaction.recentBlockhash = blockhash;
    transaction.feePayer = payer.publicKey;
    transaction.sign(payer);
    const signature = await connection.sendRawTransaction(
      transaction.serialize(),
      { preflightCommitment: "confirmed" },
    );
    const result = await connection.confirmTransaction(
      { blockhash, lastValidBlockHeight, signature },
      "confirmed",
    );
    if (result.value.err) {
      throw new Error(`${signature}: ${JSON.stringify(result.value.err)}`);
    }
    console.log(`  ${slice.length} repaired  ${signature}`);
  }

  // Believe the chain, not the sends.
  const after = await Promise.all(
    accounts.map(async (entry) => ({
      pubkey: entry.pubkey,
      length: (await connection.getAccountInfo(entry.pubkey))?.data.length ?? 0,
    })),
  );
  const finalSizes = new Set(after.map((entry) => entry.length));
  console.log("\nafter:");
  for (const size of [...finalSizes].sort((x, y) => x - y)) {
    console.log(`  ${size} bytes: ${after.filter((e) => e.length === size).length}`);
  }
  if (finalSizes.size !== 1) {
    console.error("\nyield accounts still disagree on size");
    process.exit(1);
  }
  console.log(`\nall ${after.length} yield accounts agree at ${[...finalSizes][0]} bytes`);
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
