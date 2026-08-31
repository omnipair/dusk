/**
 * Replay the devnet hLP invariant failure in LiteSVM.
 *
 * A quarter of swaps on the devnet market revert with `BrokenInvariant`
 * (6047) at the hLP reserve identity. Reproducing it in a plain unit test does
 * not work: the instruction runs `market.update()` before the handler, and
 * `update()` calls `Clock::get()`, which does not exist outside the runtime —
 * so a unit test replays the swap against a market the runtime would never
 * have produced, skipping the accrual and EMA refresh that happen in exactly
 * the step being missed.
 *
 * LiteSVM supplies a `Clock`. This loads the **deployed** binary and the
 * captured accounts, sets the clock to the slot the failure was observed at,
 * and sends one swap.
 *
 *   solana program dump <PROGRAM_ID> dusk_devnet.so -u devnet
 *   DUSK_PROGRAM_SO=dusk_devnet.so node --experimental-strip-types \
 *     scripts/devnet/replay_hlp_invariant.ts
 */

import { readFileSync } from "fs";
import { join } from "path";
import { LiteSVM } from "litesvm";
import {
  Keypair,
  PublicKey,
  SYSVAR_INSTRUCTIONS_PUBKEY,
  Transaction,
  TransactionInstruction,
} from "@solana/web3.js";
import {
  TOKEN_2022_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  getAssociatedTokenAddressSync,
} from "@solana/spl-token";
import { createHash } from "crypto";

const FIXTURES = join(
  import.meta.dirname,
  "../../programs/dusk/src/tests/fixtures/devnet-replay",
);

interface Manifest {
  programId: string;
  accounts: Record<string, { address: string; owner: string; lamports: number }>;
  failingCapture: { slot: number; blockTimeUnix: number; file: string };
}

const manifest: Manifest = JSON.parse(
  readFileSync(join(FIXTURES, "manifest.json"), "utf8"),
);

const programSo = process.env.DUSK_PROGRAM_SO;
if (!programSo) {
  console.error(
    "DUSK_PROGRAM_SO must point at the deployed binary:\n" +
      `  solana program dump ${manifest.programId} dusk_devnet.so -u devnet`,
  );
  process.exit(2);
}

const programId = new PublicKey(manifest.programId);
const svm = new LiteSVM();
svm.addProgramFromFile(programId, programSo);

/**
 * Load a captured account exactly as devnet held it — same owner, same
 * lamports, same bytes. Anything reconstructed rather than captured is a
 * variable this replay is trying to eliminate.
 */
function loadCaptured(name: string, file = `${name}.bin`) {
  const entry = manifest.accounts[name];
  if (!entry) throw new Error(`${name} is absent from the manifest`);
  const data = readFileSync(join(FIXTURES, file));
  svm.setAccount(new PublicKey(entry.address), {
    data: new Uint8Array(data),
    executable: false,
    lamports: entry.lamports,
    owner: new PublicKey(entry.owner),
    rentEpoch: 0,
  });
  return new PublicKey(entry.address);
}

// The market is loaded from the failing capture rather than the routine one:
// that is the whole point of the exercise.
const market = loadCaptured("market", manifest.failingCapture.file);
// Everything else the manifest holds, loaded as captured. Listing them by
// name invites exactly the omission that cost a round trip here: the futarchy
// authority was captured and then not loaded, and Anchor reported it as
// uninitialized rather than absent.
for (const name of Object.keys(manifest.accounts)) {
  if (name !== "market") loadCaptured(name);
}

// The clock the failure was observed under. Both halves matter: the borrow
// index advances with slots and the price EMA with the timestamp.
svm.warpToSlot(BigInt(manifest.failingCapture.slot));
const clock = svm.getClock();
clock.slot = BigInt(manifest.failingCapture.slot);
clock.unixTimestamp = BigInt(manifest.failingCapture.blockTimeUnix);
svm.setClock(clock);

const trader = Keypair.generate();
svm.airdrop(trader.publicKey, BigInt(10_000_000_000));

function discriminator(name: string): Buffer {
  return createHash("sha256").update(`global:${name}`).digest().subarray(0, 8);
}

console.log(`program   ${programId.toBase58()}`);
console.log(`market    ${market.toBase58()}`);
console.log(`slot      ${manifest.failingCapture.slot}`);
console.log(`clock     ${manifest.failingCapture.blockTimeUnix}\n`);

/**
 * A trader with a balance, written straight into the ledger.
 *
 * Synthesized rather than captured: the mints are faucet-controlled and
 * minting inside the replay would add a transaction before the one under
 * test. An SPL token account is a fixed 165-byte layout, so writing one is
 * exact rather than approximate.
 */
function fundedTokenAccount(mint: PublicKey, owner: PublicKey, amount: bigint): PublicKey {
  const address = getAssociatedTokenAddressSync(mint, owner, true);
  const data = Buffer.alloc(165);
  mint.toBuffer().copy(data, 0);
  owner.toBuffer().copy(data, 32);
  data.writeBigUInt64LE(amount, 64);
  // state = Initialized; leaving it zero makes the token program reject it.
  data.writeUInt8(1, 108);
  svm.setAccount(address, {
    data: new Uint8Array(data),
    executable: false,
    lamports: Number(svm.minimumBalanceForRentExemption(165n)),
    owner: TOKEN_PROGRAM_ID,
    rentEpoch: 0,
  });
  return address;
}

const baseMint = new PublicKey(manifest.accounts.base_mint.address);
const quoteMint = new PublicKey(manifest.accounts.quote_mint.address);
const traderBase = fundedTokenAccount(baseMint, trader.publicKey, 1_000_000_000n);
const traderQuote = fundedTokenAccount(quoteMint, trader.publicKey, 1_000_000_000n);

const [futarchyAuthority] = PublicKey.findProgramAddressSync(
  [Buffer.from("futarchy_authority")],
  programId,
);
const [eventAuthority] = PublicKey.findProgramAddressSync(
  [Buffer.from("__event_authority")],
  programId,
);

/** `SwapArgs { exact_asset_in: u64, min_asset_out: u64 }`, borsh. */
function swapArgs(exactIn: bigint, minOut: bigint): Buffer {
  const data = Buffer.alloc(16);
  data.writeBigUInt64LE(exactIn, 0);
  data.writeBigUInt64LE(minOut, 8);
  return Buffer.concat([discriminator("swap"), data]);
}

/** Pubkeys at the layout offsets the keeper generator produces. */
function hlpPrefix(): PublicKey[] {
  const captured = readFileSync(join(FIXTURES, manifest.failingCapture.file));
  const at = (offset: number) => new PublicKey(captured.subarray(offset, offset + 32));
  return [
    at(9), // ylp_mint
    at(1_751), // base_hlp_vault.ylp_vault
    at(2_135), // quote_hlp_vault.ylp_vault
    at(170), // base_side.interest_vault
    at(675), // quote_side.interest_vault
  ];
}

const meta = (pubkey: PublicKey, isWritable = false, isSigner = false) => ({
  isSigner,
  isWritable,
  pubkey,
});

const instruction = new TransactionInstruction({
  data: swapArgs(5_000_000n, 0n),
  keys: [
    meta(market, true),
    meta(futarchyAuthority),
    meta(trader.publicKey, true, true),
    meta(baseMint),
    meta(quoteMint),
    meta(new PublicKey(manifest.accounts.base_reserve_vault.address), true),
    meta(new PublicKey(manifest.accounts.quote_reserve_vault.address), true),
    meta(traderBase, true),
    meta(traderQuote, true),
    meta(SYSVAR_INSTRUCTIONS_PUBKEY),
    meta(TOKEN_PROGRAM_ID),
    meta(TOKEN_2022_PROGRAM_ID),
    meta(eventAuthority),
    meta(programId),
    // The hLP settlement prefix, in the order the program checks it. Read
    // from the captured market rather than the manifest so the keys are
    // exactly the ones that market names — the quote yLP vault does not exist
    // on chain, and the program key-matches the prefix rather than
    // deserializing it, so an address with no account behind it is correct.
    ...hlpPrefix().map((pubkey) => meta(pubkey, true)),
  ],
  programId,
});

const transaction = new Transaction().add(instruction);
transaction.recentBlockhash = svm.latestBlockhash();
transaction.feePayer = trader.publicKey;
transaction.sign(trader);

const result = svm.sendTransaction(transaction);
const asAny = result as unknown as {
  meta?: () => { logs: () => string[] };
  logs?: () => string[];
  err?: () => unknown;
  toString?: () => string;
};
let logs: string[] = [];
try {
  logs = asAny.meta?.().logs() ?? asAny.logs?.() ?? [];
} catch {
  logs = [];
}
console.log(String(asAny.toString?.() ?? result).slice(0, 300));
for (const line of logs.slice(-12)) console.log("  ", line.slice(0, 150));
// The whole point: 6047 here means the devnet failure reproduces off chain,
// and the identity can then be instrumented locally as often as needed.
process.exit(logs.some((l) => l.includes("6047") || l.includes("BrokenInvariant")) ? 0 : 1);
