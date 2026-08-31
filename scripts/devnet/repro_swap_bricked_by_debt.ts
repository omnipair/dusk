/**
 * Reproduction: an outstanding borrow disables every swap within seconds.
 *
 * Borrow a few hundred quote against collateral and the market keeps working
 * for one slot or two. Roughly fifteen seconds later every swap — any size,
 * any direction, any trader — reverts with `BrokenInvariant` (6047) at the
 * hLP reserve-identity check, and keeps reverting until the debt is repaid.
 *
 * The check tolerates three atoms of drift:
 *
 *   const MAX_CONCENTRATED_HLP_LIVE_DUST_ATOMS: u128 = 3;
 *   require!(identity_base_live.abs_diff(final_base_live_reserve) <= 3 && ...)
 *
 * Three atoms is the right bound for what its comment describes: three
 * independently floored quantities, each off by at most one. It is not a bound
 * on interest, which accrues against principal and passes three atoms quickly
 * once the principal is more than trivial. That is why the failure looks
 * size-dependent from the outside and is really time-dependent: five quote of
 * debt stays under the tolerance for a long time, four hundred crosses it in
 * about fifteen seconds.
 *
 * The decisive evidence that this is accrual rather than the borrow itself:
 * `borrow` and `swap` submitted in the *same* transaction succeed at every
 * size tried, up to and including the largest the market will lend. No time
 * passes between them, so nothing accrues.
 *
 *   node --experimental-strip-types scripts/devnet/repro_swap_bricked_by_debt.ts
 *
 * The script repays what it borrows before exiting, so the market is left as
 * it was found.
 */

import { getAssociatedTokenAddressSync } from "@solana/spl-token";
import {
  Connection,
  Keypair,
  PublicKey,
  Transaction,
  TransactionInstruction,
} from "@solana/web3.js";
import { AnchorProvider, Wallet } from "@coral-xyz/anchor";
import { readFileSync } from "fs";
import { homedir } from "os";
import { join } from "path";

import { Dusk } from "../../packages/dusk-sdk/dist/index.js";

const API =
  process.env.DUSK_API_URL ?? "https://dusk-api-production-291f.up.railway.app";
const RPC = process.env.DUSK_RPC_URL ?? "https://api.devnet.solana.com";
/** Large enough that accrued interest passes three atoms in seconds. */
const BORROW = 400n;
const PROBE_INTERVAL_MS = 15_000;
const PROBES = 8;

function loadKeypair(): Keypair {
  const path =
    process.env.DUSK_KEYPAIR ?? join(homedir(), ".config/solana/id.json");
  return Keypair.fromSecretKey(
    Uint8Array.from(JSON.parse(readFileSync(path, "utf8"))),
  );
}

async function main() {
  const keypair = loadKeypair();
  const connection = new Connection(RPC, "confirmed");
  const config = (await (await fetch(`${API}/api/dusk/v1/config`)).json()).data;

  const dusk = new Dusk({
    programId: new PublicKey(config.programId),
    provider: new AnchorProvider(connection, new Wallet(keypair), {
      commitment: "confirmed",
    }),
  });
  const market = new PublicKey(config.primaryMarket);
  const baseMint = new PublicKey(config.baseMint);
  const quoteMint = new PublicKey(config.quoteMint);
  const unit = 10n ** BigInt(config.quoteDecimals);

  const send = async (instructions: TransactionInstruction[]) => {
    const transaction = new Transaction().add(...instructions);
    const blockhash = await connection.getLatestBlockhash("confirmed");
    transaction.recentBlockhash = blockhash.blockhash;
    transaction.feePayer = keypair.publicKey;
    transaction.sign(keypair);
    const signature = await connection.sendRawTransaction(
      transaction.serialize(),
      { preflightCommitment: "confirmed" },
    );
    const result = await connection.confirmTransaction(
      { ...blockhash, signature },
      "confirmed",
    );
    if (result.value.err) throw new Error(JSON.stringify(result.value.err));
    return signature;
  };

  const swapError = async (): Promise<string | null> => {
    const instruction = await dusk.write.buildSwapInstruction({
      assetInMint: baseMint,
      assetOutMint: quoteMint,
      exactAssetIn: unit.toString(),
      market,
      minAssetOut: "0",
      trader: keypair.publicKey,
    });
    const transaction = new Transaction().add(instruction);
    transaction.recentBlockhash = (
      await connection.getLatestBlockhash("confirmed")
    ).blockhash;
    transaction.feePayer = keypair.publicKey;
    const simulation = await connection.simulateTransaction(transaction);
    if (!simulation.value.err) return null;
    return (
      (simulation.value.logs ?? []).find((line) => line.includes("Error Code")) ??
      JSON.stringify(simulation.value.err)
    ).replace("Program log: AnchorError thrown in programs/dusk/src/", "");
  };

  const positionId = Keypair.generate().publicKey;
  console.log(`swap before borrowing: ${(await swapError()) ?? "ok"}`);

  await send([
    await dusk.write.depositCollateralInstruction({
      assetMint: baseMint,
      depositAmount: (1_000n * unit).toString(),
      market,
      owner: keypair.publicKey,
      ownerAssetAccount: getAssociatedTokenAddressSync(
        baseMint,
        keypair.publicKey,
      ),
      positionId,
    }),
  ]);
  await send([
    await dusk.write.buildBorrowInstruction({
      borrowAmount: (BORROW * unit).toString(),
      collateralAssetMint: baseMint,
      debtAssetMint: quoteMint,
      market,
      minDebtAmountOut: (BORROW * unit).toString(),
      minLiquidationCfBps: 0,
      owner: keypair.publicKey,
      positionId,
    }),
  ]);
  console.log(`borrowed ${BORROW} quote against 1000 base\n`);

  const started = Date.now();
  let broke = false;
  for (let probe = 0; probe < PROBES; probe += 1) {
    const error = await swapError();
    const elapsed = Math.round((Date.now() - started) / 1000);
    console.log(`  +${String(elapsed).padStart(3)}s  swap -> ${error ?? "ok"}`);
    if (error) {
      broke = true;
      break;
    }
    await new Promise((resolve) => setTimeout(resolve, PROBE_INTERVAL_MS));
  }

  for (const amount of [BORROW + 50n, BORROW + 10n, BORROW, 100n, 10n]) {
    try {
      await send([
        await dusk.write.repayInstruction({
          debtAssetMint: quoteMint,
          market,
          owner: keypair.publicKey,
          ownerDebtAccount: getAssociatedTokenAddressSync(
            quoteMint,
            keypair.publicKey,
          ),
          positionId,
          repayAmount: (amount * unit).toString(),
        }),
      ]);
      console.log(`\nrepaid ${amount} quote`);
      break;
    } catch {
      // More than is outstanding; try less.
    }
  }
  console.log(`swap after repaying: ${(await swapError()) ?? "ok"}`);
  process.exit(broke ? 0 : 1);
}

main().catch((error) => {
  console.error(`FAILED: ${error instanceof Error ? error.message : error}`);
  process.exit(2);
});
