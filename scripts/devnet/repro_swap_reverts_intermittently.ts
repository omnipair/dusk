/**
 * Reproduction: swaps fail intermittently, and an outstanding borrow makes it
 * constant.
 *
 * On a market with **no user debt at all**, roughly a third of swap attempts
 * revert with `BrokenInvariant` (6047) at the hLP reserve-identity check in
 * `transitions/liquidity/hlp/engine.rs`. Borrow a few hundred quote and the
 * same check fails on essentially every attempt until the debt is repaid.
 *
 * The check tolerates three atoms of drift:
 *
 *   const MAX_CONCENTRATED_HLP_LIVE_DUST_ATOMS: u128 = 3;
 *   require!(identity_base_live.abs_diff(final_base_live_reserve) <= 3 && ...)
 *
 * Three is the right bound for what its comment describes — three
 * independently floored quantities, each off by at most one. It is not a bound
 * on accrued interest, which grows with principal and elapsed slots and is not
 * carried by the identity being checked. The hLP vault holds debt of its own,
 * so this accrues whether or not anybody has borrowed; a user borrow simply
 * adds principal and pushes the drift past three atoms continuously instead of
 * intermittently.
 *
 * Widening the constant would not fix it. The drift is unbounded in principal
 * and elapsed time, so every constant is eventually too small — the identity
 * has to account for accrual rather than tolerate it.
 *
 * Two measurements are taken: the failure rate at rest, and the failure rate
 * while a borrow stands. The borrow is repaid before exiting, so the market is
 * left as it was found.
 *
 *   node --experimental-strip-types scripts/devnet/repro_swap_bricked_by_debt.ts
 */

import { getAssociatedTokenAddressSync } from "@solana/spl-token";
import {
  ComputeBudgetProgram,
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
/** Large enough that accrued interest passes three atoms continuously. */
const BORROW = 400n;
const PROBE_INTERVAL_MS = 5_000;
const PROBES = 12;

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
    // Borrow and repay both settle hLP as part of their market update and
    // exceed the default 200k budget. Without this they fail as a compute
    // error that looks nothing like the defect being measured.
    const transaction = new Transaction().add(
      ComputeBudgetProgram.setComputeUnitLimit({ units: 800_000 }),
      ...instructions,
    );
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
    // Only a program revert counts. `BlockhashNotFound` is the RPC declining
    // to simulate at all — the program never ran — and counting it as a swap
    // failure inflates every rate this script reports. That mistake is why
    // this repository once recorded "a quarter of swaps revert on an idle
    // market": the pattern was the RPC's blockhash cycle, not the protocol.
    const logs = simulation.value.logs ?? [];
    const reverted = logs.some((line) => line.includes("Error Code"));
    if (!reverted) return null;
    return (
      logs.find((line) => line.includes("Error Code")) ??
      JSON.stringify(simulation.value.err)
    ).replace("Program log: AnchorError thrown in programs/dusk/src/", "");
  };

  /** Sample the swap path repeatedly and report how often it reverts. */
  const failureRate = async (label: string): Promise<number> => {
    let failures = 0;
    for (let probe = 0; probe < PROBES; probe += 1) {
      const error = await swapError();
      if (error) failures += 1;
      process.stdout.write(error ? "x" : ".");
      await new Promise((resolve) => setTimeout(resolve, PROBE_INTERVAL_MS));
    }
    const rate = (failures / PROBES) * 100;
    console.log(`  ${label}: ${failures}/${PROBES} reverted (${rate.toFixed(0)}%)`);
    return rate;
  };

  console.log("sampling the swap path with no borrow outstanding");
  const atRest = await failureRate("at rest");

  const positionId = Keypair.generate().publicKey;
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
  console.log(`\nborrowed ${BORROW} quote against 1000 base`);
  const withDebt = await failureRate("with debt");

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

  console.log(
    `\nat rest ${atRest.toFixed(0)}% of swaps revert; with a ${BORROW} quote borrow, ${withDebt.toFixed(0)}%`,
  );
  // A market that swaps every time is the only passing result.
  process.exit(atRest === 0 && withDebt === 0 ? 0 : 1);
}

main().catch((error) => {
  console.error(`FAILED: ${error instanceof Error ? error.message : error}`);
  process.exit(2);
});
