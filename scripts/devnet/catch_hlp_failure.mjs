/**
 * Catch a live hLP failure and replay that exact state.
 *
 * Simulates swaps against devnet until one reverts, captures **every** account
 * the swap touches at that instant, and immediately replays it locally against
 * the deployed binary. Capturing only the market and reusing vaults from an
 * earlier slot replays a market against balances it never saw, which is not a
 * faithful replay at all.
 *
 *   DUSK_RPC_URL=... DUSK_PROGRAM_SO=dusk.so node scripts/devnet/catch_hlp_failure.mjs
 *
 * Needs litesvm 1.x alongside — see replay_hlp_invariant.mjs for why and how.
 */

// Simulate on devnet until a swap reverts, capture the market at that instant,
// and immediately replay it locally against the deployed binary. If devnet
// says fail and the replay says ok for the *same* state, the trigger is not in
// the state at all.
import { execFileSync } from "child_process";
import { readFileSync, writeFileSync } from "fs";
import { dirname, resolve } from "path";
import { fileURLToPath } from "url";
import { Connection, Keypair, PublicKey, Transaction } from "@solana/web3.js";
import { AnchorProvider, Wallet } from "@coral-xyz/anchor";

const DUSK = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const FIX = `${DUSK}/programs/dusk/src/tests/fixtures/devnet-replay`;
const { Dusk } = await import(`${DUSK}/packages/dusk-sdk/dist/index.js`);

const kp = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(readFileSync(`${process.env.HOME}/.config/solana/id.json`, "utf8"))));
const c = new Connection(process.env.DUSK_RPC_URL, "confirmed");
const cfg = (await (await fetch("https://dusk-api-production-291f.up.railway.app/api/dusk/v1/config")).json()).data;
const dusk = new Dusk({ programId: new PublicKey(cfg.programId), provider: new AnchorProvider(c, new Wallet(kp), { commitment: "confirmed" }) });
const market = new PublicKey(cfg.primaryMarket);

for (let i = 0; i < 60; i += 1) {
  const ix = await dusk.write.buildSwapInstruction({
    assetInMint: new PublicKey(cfg.baseMint), assetOutMint: new PublicKey(cfg.quoteMint),
    exactAssetIn: "5000000", market, minAssetOut: "0", trader: kp.publicKey,
  });
  const tx = new Transaction().add(ix);
  tx.recentBlockhash = (await c.getLatestBlockhash("confirmed")).blockhash;
  tx.feePayer = kp.publicKey;
  const sim = await c.simulateTransaction(tx);
  if (!sim.value.err) { process.stdout.write("."); await new Promise(r => setTimeout(r, 1200)); continue; }

  // Every account, at the same instant. Capturing only the market and reusing
  // vaults from an earlier slot replays a market against balances it never saw
  // — and the swap validates reserve custody against market state, so that
  // mismatch is not cosmetic.
  const manifest = JSON.parse(readFileSync(`${FIX}/manifest.json`, "utf8"));
  const names = Object.keys(manifest.accounts);
  const infos = await c.getMultipleAccountsInfo(
    names.map((n) => new PublicKey(manifest.accounts[n].address)),
    "confirmed",
  );
  for (const [index, name] of names.entries()) {
    const account = infos[index];
    if (!account) continue;
    writeFileSync(`${FIX}/${name === "market" ? "market_failing" : name}.bin`, account.data);
    manifest.accounts[name].lamports = account.lamports;
    manifest.accounts[name].owner = account.owner.toBase58();
  }
  manifest.failingCapture = { slot: sim.context.slot, blockTimeUnix: Number(await c.getBlockTime(sim.context.slot)) || manifest.failingCapture.blockTimeUnix, file: "market_failing.bin" };
  writeFileSync(`${FIX}/manifest.json`, JSON.stringify(manifest, null, 2) + "\n");
  console.log(`\ndevnet FAILED at slot ${sim.context.slot}; captured and replaying`);

  const out = execFileSync("node", ["replay.mjs"], {
    env: { ...process.env, DUSK_REPLAY_FIXTURES: `${FIX}/` },
    encoding: "utf8",
  });
  console.log(out.trim().split("\n").slice(-2).join("\n"));
  process.exit(0);
}
console.log("\nno devnet failure observed");
