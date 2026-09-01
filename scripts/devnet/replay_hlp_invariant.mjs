/**
 * Replay the devnet hLP invariant failure against the *deployed* binary.
 *
 * About a quarter of devnet swaps revert with `BrokenInvariant` (6047) at the
 * hLP reserve identity. This loads the exact deployed program, the captured
 * market state from the moment a swap simulation reverted, the real clock, and
 * sends one swap — then sweeps accrual, because elapsed slots since the market
 * was last touched is what varies between attempts on chain.
 *
 * **Requires litesvm 1.x.** The version pinned for the smoke suite (0.8.0)
 * refuses the deployed artifact: it is SBFv3 (`e_machine` 247, `e_flags` 3)
 * and 0.8.0 does not accept that target. Upgrading the whole project is a
 * separate question, so this runs from its own install:
 *
 *   mkdir -p /tmp/replay && cd /tmp/replay
 *   npm init -y && npm install litesvm@1 @solana/kit
 *   solana program dump JA8Zxxm4t4zopBL8e3dQQXWfQ3a5pBUPY9Sp9RnybV2X dusk.so -u devnet
 *   DUSK_PROGRAM_SO=dusk.so node <path-to-this-file>
 *
 * Result so far: the swap **succeeds** at every accrual point sampled, from
 * zero to two hundred thousand slots. Binary, state and clock are all
 * faithful, so whatever triggers the failure on chain is not among them —
 * see the fixtures README for what that leaves.
 */

import { readFileSync } from "fs";
import { join } from "path";
import { LiteSVM } from "litesvm";
import {
  address,
  appendTransactionMessageInstruction,
  compileTransaction,
  createKeyPairSignerFromBytes,
  createTransactionMessage,
  getProgramDerivedAddress,
  pipe,
  setTransactionMessageFeePayerSigner,
  signTransactionMessageWithSigners,
} from "@solana/kit";
import { createHash } from "crypto";

const FIXTURES =
  process.env.DUSK_REPLAY_FIXTURES ??
  new URL("../../programs/dusk/src/tests/fixtures/devnet-replay/", import.meta.url).pathname;
const manifest = JSON.parse(readFileSync(join(FIXTURES, "manifest.json"), "utf8"));
const programId = address(manifest.programId);
const soPath = process.env.DUSK_PROGRAM_SO;

// FeatureSet.allEnabled() is deliberately NOT used: it fails to build the SBF
// VM at all ("Invalid memory region at index 4"), so every attempt "fails"
// without the program ever running. That reported as a 401/401 reproduction
// until the logs were read.
const svm = new LiteSVM().withSigverify(false).withBlockhashCheck(false);
svm.addProgram(programId, new Uint8Array(readFileSync(soPath)));

function load(name, file = `${name}.bin`) {
  const entry = manifest.accounts[name];
  const data = readFileSync(join(FIXTURES, file));
  svm.setAccount({
    address: address(entry.address),
    data: new Uint8Array(data),
    executable: false,
    lamports: BigInt(entry.lamports),
    programAddress: address(entry.owner),
    space: BigInt(data.length),
  });
  return address(entry.address);
}

const market = load("market", manifest.failingCapture.file);
for (const name of Object.keys(manifest.accounts)) if (name !== "market") load(name);

svm.warpToSlot(BigInt(manifest.failingCapture.slot));
const clock = svm.getClock();
clock.slot = BigInt(manifest.failingCapture.slot);
clock.unixTimestamp = BigInt(manifest.failingCapture.blockTimeUnix);
svm.setClock(clock);

// The real trader, not a generated one: their token accounts are captured
// rather than synthesized, so the signer has to be the owner those accounts
// name or the token program rejects the transfer.
const secret = JSON.parse(
  readFileSync(process.env.DUSK_KEYPAIR ?? `${process.env.HOME}/.config/solana/id.json`, "utf8"),
);
const signer = await createKeyPairSignerFromBytes(new Uint8Array(secret));
svm.airdrop(signer.address, 10_000_000_000n);

function discriminator(name) {
  return createHash("sha256").update(`global:${name}`).digest().subarray(0, 8);
}

const captured = readFileSync(join(FIXTURES, manifest.failingCapture.file));
const at = (offset) => address(bs58(captured.subarray(offset, offset + 32)));

function bs58(bytes) {
  const A = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
  let n = BigInt("0x" + Buffer.from(bytes).toString("hex"));
  let out = "";
  while (n > 0n) { out = A[Number(n % 58n)] + out; n /= 58n; }
  return out;
}

const [futarchyAuthority] = await getProgramDerivedAddress({
  programAddress: programId,
  seeds: [Buffer.from("futarchy_authority")],
});
const [eventAuthority] = await getProgramDerivedAddress({
  programAddress: programId,
  seeds: [Buffer.from("__event_authority")],
});

// Trader token accounts, written straight in. The mints are faucet-controlled
// and minting would add a transaction ahead of the one under test.
const TOKEN = address("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
const TOKEN22 = address("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");
const ATA = address("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");

async function fundedTokenAccount(mint, owner, amount) {
  const [addr] = await getProgramDerivedAddress({
    programAddress: ATA,
    seeds: [bytesOf(owner), bytesOf(TOKEN), bytesOf(mint)],
  });
  const data = Buffer.alloc(165);
  Buffer.from(bytesOf(mint)).copy(data, 0);
  Buffer.from(bytesOf(owner)).copy(data, 32);
  data.writeBigUInt64LE(amount, 64);
  data.writeUInt8(1, 108);
  svm.setAccount({
    address: addr,
    data: new Uint8Array(data),
    executable: false,
    lamports: 2_039_280n,
    programAddress: TOKEN,
    space: 165n,
  });
  return addr;
}

function bytesOf(addr) {
  const A = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
  let n = 0n;
  for (const ch of addr) n = n * 58n + BigInt(A.indexOf(ch));
  let hex = n.toString(16);
  if (hex.length % 2) hex = "0" + hex;
  const raw = Buffer.from(hex, "hex");
  const out = Buffer.alloc(32);
  raw.copy(out, 32 - raw.length);
  return new Uint8Array(out);
}

const baseMint = address(manifest.accounts.base_mint.address);
const quoteMint = address(manifest.accounts.quote_mint.address);
// Captured, not synthesized. A hand-written SPL layout carries a balance and
// nothing else; the real accounts carry whatever history devnet gave them,
// and that difference was the top of the list of remaining variables.
const traderBase = address(manifest.accounts.trader_base.address);
const traderQuote = address(manifest.accounts.trader_quote.address);

const args = Buffer.alloc(16);
args.writeBigUInt64LE(5_000_000n, 0);
args.writeBigUInt64LE(0n, 8);

const ro = (a) => ({ address: a, role: 0 });
const rw = (a) => ({ address: a, role: 1 });
const signerRw = (a) => ({ address: a, role: 3 });

const instruction = {
  programAddress: programId,
  accounts: [
    rw(market),
    ro(futarchyAuthority),
    signerRw(signer.address),
    ro(baseMint),
    ro(quoteMint),
    rw(address(manifest.accounts.base_reserve_vault.address)),
    rw(address(manifest.accounts.quote_reserve_vault.address)),
    rw(traderBase),
    rw(traderQuote),
    ro(address("Sysvar1nstructions1111111111111111111111111")),
    ro(TOKEN),
    ro(TOKEN22),
    ro(eventAuthority),
    ro(programId),
    // hLP settlement prefix, in the order the program checks it.
    rw(at(9)), rw(at(1751)), rw(at(2135)), rw(at(170)), rw(at(675)),
  ],
  data: new Uint8Array(Buffer.concat([discriminator("swap"), args])),
};

async function attempt(clockSlot) {
  const message = pipe(
    createTransactionMessage({ version: 0 }),
    (m) => setTransactionMessageFeePayerSigner(signer, m),
    (m) => svm.setTransactionMessageLifetimeUsingLatestBlockhash(m),
    (m) => appendTransactionMessageInstruction(instruction, m),
  );
  const signed = await signTransactionMessageWithSigners(message);
  const result = svm.sendTransaction(signed);
  const text = String(result);
  // Only the hLP invariant counts as a reproduction. Any other failure — a VM
  // that will not start, a missing account — is a broken harness wearing the
  // bug's clothes, and counting it produces a confident wrong answer.
  const logs = (() => {
    try { return result.meta?.().logs() ?? result.logs?.() ?? []; } catch { return []; }
  })();
  const joined = logs.join(" ");
  const reproduced = joined.includes("BrokenInvariant") || joined.includes("6047");
  const failed = text.includes("FailedTransactionMetadata");
  if (failed && !reproduced) {
    const why = logs.find((l) => /fail|Error/i.test(l)) ?? text.slice(0, 120);
    return { failed: false, other: why, text };
  }
  return { failed: reproduced, text };
}

// Sweep accrual. The devnet failure is intermittent at about a quarter of
// swaps, and elapsed slots since the market was last touched is what varies
// between one attempt and the next, so this samples that axis directly.
let failures = 0;
const base = Number(manifest.failingCapture.slot);
// Fine sweep: devnet failures recur every ~188 slots with a ~34% duty
// cycle, so exponentially spaced samples walk straight past the window.
const sweep = [];
for (let i = 0; i <= 400; i += 1) sweep.push(i);
for (const elapsed of sweep) {
  // Reload every captured account first. A swap that succeeds *commits*, so
  // without this the second iteration onward runs against post-swap state —
  // and worse, against a market whose last_update_slot is now current, which
  // erases the accrual the sweep exists to vary.
  load("market", manifest.failingCapture.file);
  for (const name of Object.keys(manifest.accounts)) if (name !== "market") load(name);

  const clock = svm.getClock();
  clock.slot = BigInt(base + elapsed);
  clock.unixTimestamp = BigInt(manifest.failingCapture.blockTimeUnix + Math.round(elapsed * 0.4));
  // Devnet's real epoch state at the failing slot. Left at LiteSVM defaults
  // the program sees epoch 0 starting at time 0, and anything deriving a rate
  // or a window from the epoch would behave differently than on chain.
  clock.epoch = 1136n;
  clock.leaderScheduleEpoch = 1137n;
  clock.epochStartTimestamp = 1788149045n;
  svm.setClock(clock);
  svm.expireBlockhash();
  const { failed, other, text } = await attempt(base + elapsed);
  if (other && elapsed === sweep[0]) console.log(`  harness problem, not the bug: ${String(other).slice(0, 110)}`);
  if (failed) {
    failures += 1;
    const code = text.match(/Custom\((\d+)\)/)?.[1] ?? "?";
    if (failures <= 2) {
      const logLine = text.split("\\n").find((l) => l.includes("Error Code")) ?? "";
      console.log(`  +${elapsed} slots -> FAILED`);
      console.log(`     ${text.slice(text.indexOf("logs:"), text.indexOf("logs:") + 900).replace(/\\s+/g, " ").slice(0, 700)}`);
    }
  }
}
console.log(failures > 0 ? `\n${failures} of ${sweep.length} sweep points reproduced the failure` : `\nnone of ${sweep.length} sweep points reproduced it`);
