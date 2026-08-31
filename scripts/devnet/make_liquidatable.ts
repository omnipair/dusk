/**
 * Manufacture a liquidatable position on devnet.
 *
 * A liquidation keeper cannot be proven by a healthy market. Every pass over a
 * solvent book takes the same path — discover, simulate, decline — and never
 * touches the code that signs, submits and confirms. Until one position is
 * genuinely underwater, "the keeper sent nothing" and "the keeper cannot send"
 * look identical.
 *
 * The margin needed is small by construction. A position may borrow up to 95%
 * of its liquidation ratio (`LTV_BUFFER_BPS` is 500), so one borrowed to the
 * limit is about 5% of collateral value away from liquidation. This borrows to
 * the limit and then walks the price down in small steps, asking the program
 * after each one, rather than dumping size and hoping.
 *
 * Liquidatability is never computed here. The script simulates the very
 * instruction the keeper sends and believes the answer, because a second
 * implementation of the solvency rule would be free to disagree with the first.
 *
 *   node --experimental-strip-types scripts/devnet/make_liquidatable.ts
 */

import {
  ASSOCIATED_TOKEN_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  getAssociatedTokenAddressSync,
} from "@solana/spl-token";
import {
  Connection,
  Keypair,
  PublicKey,
  SystemProgram,
  Transaction,
  TransactionInstruction,
} from "@solana/web3.js";
import { AnchorProvider, Wallet } from "@coral-xyz/anchor";
import { createHash } from "crypto";
import { readFileSync } from "fs";
import { homedir } from "os";
import { join } from "path";

import {
  Dusk,
  deriveBorrowPositionAddress,
} from "../../packages/dusk-sdk/dist/index.js";

const API =
  process.env.DUSK_API_URL ?? "https://dusk-api-production-291f.up.railway.app";
const RPC = process.env.DUSK_RPC_URL ?? "https://api.devnet.solana.com";
const FAUCET_PROGRAM_ID =
  process.env.DUSK_FAUCET_PROGRAM_ID ??
  "EMmV9HKeQndxFd4duqp65rUSjikVWCPakBH1UjJJ32dz";

/** Collateral for the test position, in whole base tokens. */
const COLLATERAL = 1_000n;
/** Base sold per step while walking the price down. */
const PRICE_STEP = 2_000n;
/** Give up rather than keep selling into a market that will not move. */
const MAX_STEPS = 12;

function discriminator(name: string): Buffer {
  return createHash("sha256").update(`global:${name}`).digest().subarray(0, 8);
}

function loadKeypair(): Keypair {
  const path =
    process.env.DUSK_KEYPAIR ?? join(homedir(), ".config/solana/id.json");
  return Keypair.fromSecretKey(
    Uint8Array.from(JSON.parse(readFileSync(path, "utf8"))),
  );
}

function faucetMint(
  owner: PublicKey,
  mint: PublicKey,
  amount: bigint,
): TransactionInstruction {
  const programId = new PublicKey(FAUCET_PROGRAM_ID);
  const [authority] = PublicKey.findProgramAddressSync(
    [Buffer.from("faucet_authority"), programId.toBuffer()],
    programId,
  );
  const data = Buffer.alloc(8);
  data.writeBigUInt64LE(amount);
  return new TransactionInstruction({
    data: Buffer.concat([discriminator("faucet_mint"), data]),
    keys: [
      { isSigner: true, isWritable: true, pubkey: owner },
      { isSigner: false, isWritable: false, pubkey: owner },
      { isSigner: false, isWritable: false, pubkey: authority },
      {
        isSigner: false,
        isWritable: true,
        pubkey: getAssociatedTokenAddressSync(mint, owner),
      },
      { isSigner: false, isWritable: true, pubkey: mint },
      { isSigner: false, isWritable: false, pubkey: SystemProgram.programId },
      { isSigner: false, isWritable: false, pubkey: TOKEN_PROGRAM_ID },
      {
        isSigner: false,
        isWritable: false,
        pubkey: ASSOCIATED_TOKEN_PROGRAM_ID,
      },
    ],
    programId,
  });
}

/** The keeper's instruction, rebuilt here so the probe tests the real thing. */
function startLiquidationAuction(
  programId: PublicKey,
  market: PublicKey,
  position: PublicKey,
  debtMint: PublicKey,
): TransactionInstruction {
  return new TransactionInstruction({
    data: discriminator("start_liquidation_auction"),
    keys: [
      { isSigner: false, isWritable: true, pubkey: market },
      { isSigner: false, isWritable: true, pubkey: position },
      { isSigner: false, isWritable: false, pubkey: debtMint },
    ],
    programId,
  });
}

async function send(
  connection: Connection,
  payer: Keypair,
  instructions: TransactionInstruction[],
): Promise<string> {
  const transaction = new Transaction().add(...instructions);
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
  return signature;
}

/** Ask the program whether this position can be put to auction. */
async function isLiquidatable(
  connection: Connection,
  payer: Keypair,
  instruction: TransactionInstruction,
): Promise<{ liquidatable: boolean; detail: string }> {
  const transaction = new Transaction().add(instruction);
  transaction.recentBlockhash = (
    await connection.getLatestBlockhash("confirmed")
  ).blockhash;
  transaction.feePayer = payer.publicKey;
  const simulation = await connection.simulateTransaction(transaction);
  if (!simulation.value.err) return { detail: "accepted", liquidatable: true };
  const logs = (simulation.value.logs ?? [])
    .filter((line) => line.includes("Error Code"))
    .join(" | ");
  return { detail: logs || JSON.stringify(simulation.value.err), liquidatable: false };
}

async function main() {
  const keypair = loadKeypair();
  const connection = new Connection(RPC, "confirmed");
  const config = (await (await fetch(`${API}/api/dusk/v1/config`)).json()).data;

  const programId = new PublicKey(config.programId);
  const market = new PublicKey(config.primaryMarket);
  const baseMint = new PublicKey(config.baseMint);
  const quoteMint = new PublicKey(config.quoteMint);
  const baseUnit = 10n ** BigInt(config.baseDecimals);
  const quoteUnit = 10n ** BigInt(config.quoteDecimals);

  const provider = new AnchorProvider(connection, new Wallet(keypair), {
    commitment: "confirmed",
  });
  const dusk = new Dusk({ programId, provider });

  console.log(`wallet ${keypair.publicKey.toBase58()}`);
  console.log(`market ${market.toBase58()}\n`);

  // Enough to collateralize the position and to move the price afterwards.
  const needed = COLLATERAL + PRICE_STEP * BigInt(MAX_STEPS) + 1_000n;
  await send(connection, keypair, [
    faucetMint(keypair.publicKey, baseMint, needed * baseUnit),
    faucetMint(keypair.publicKey, quoteMint, 10_000n * quoteUnit),
  ]);
  console.log(`minted ${needed} base for collateral and price pressure`);

  const positionId = Keypair.generate().publicKey;
  const [position] = deriveBorrowPositionAddress(market, positionId);

  await send(connection, keypair, [
    await dusk.write.depositCollateralInstruction({
      assetMint: baseMint,
      depositAmount: (COLLATERAL * baseUnit).toString(),
      market,
      owner: keypair.publicKey,
      ownerAssetAccount: getAssociatedTokenAddressSync(
        baseMint,
        keypair.publicKey,
      ),
      positionId,
    }),
  ]);
  console.log(`position ${position.toBase58()} collateralized with ${COLLATERAL} base`);

  // Borrow as much as the program will allow. The ceiling depends on the
  // curve rather than on a constant, so it is found by asking rather than
  // computed: each refusal is information, and the largest accepted amount is
  // the one that leaves the thinnest margin to liquidation.
  let borrowed = 0n;
  for (const attempt of [850n, 820n, 800n, 780n, 750n, 700n, 600n, 500n]) {
    try {
      await send(connection, keypair, [
        await dusk.write.buildBorrowInstruction({
          borrowAmount: (attempt * quoteUnit).toString(),
          collateralAssetMint: baseMint,
          debtAssetMint: quoteMint,
          market,
          minDebtAmountOut: (attempt * quoteUnit).toString(),
          minLiquidationCfBps: 0,
          owner: keypair.publicKey,
          positionId,
        }),
      ]);
      borrowed = attempt;
      console.log(`borrowed ${attempt} quote against ${COLLATERAL} base`);
      break;
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      console.log(`  ${attempt} quote refused (${message.slice(0, 60)})`);
    }
  }
  if (borrowed === 0n) throw new Error("no borrow amount was accepted");

  const probe = startLiquidationAuction(programId, market, position, quoteMint);
  let state = await isLiquidatable(connection, keypair, probe);
  console.log(`\nafter borrowing: ${state.liquidatable ? "LIQUIDATABLE" : state.detail}`);

  // Walk the collateral's price down until the program agrees. Small steps,
  // because the aim is the smallest move that crosses the threshold — this is
  // a shared market and an oversized sale is harder to undo than to avoid.
  let sold = 0n;
  for (let step = 0; step < MAX_STEPS && !state.liquidatable; step += 1) {
    await send(connection, keypair, [
      await dusk.write.buildSwapInstruction({
        assetInMint: baseMint,
        assetOutMint: quoteMint,
        exactAssetIn: (PRICE_STEP * baseUnit).toString(),
        market,
        minAssetOut: "0",
        trader: keypair.publicKey,
      }),
    ]);
    sold += PRICE_STEP;
    state = await isLiquidatable(connection, keypair, probe);
    console.log(
      `  sold ${sold} base -> ${state.liquidatable ? "LIQUIDATABLE" : state.detail.slice(0, 70)}`,
    );
  }

  console.log(
    state.liquidatable
      ? `\nposition ${position.toBase58()} is liquidatable after selling ${sold} base`
      : `\nposition ${position.toBase58()} is still healthy after selling ${sold} base`,
  );
  if (!state.liquidatable) process.exit(1);
}

main().catch((error) => {
  console.error(`\nFAILED: ${error instanceof Error ? error.message : error}`);
  process.exit(1);
});
