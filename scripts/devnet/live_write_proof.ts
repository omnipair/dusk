/**
 * Signed-write proof against the live devnet deployment.
 *
 * Everything up to now has been simulation: the SDK builds an instruction, the
 * cluster says it would succeed, and nothing is ever committed. Simulation
 * cannot catch a fee payer that cannot pay, an account the runtime creates
 * only on the real path, or a race with another writer. This script signs and
 * sends, so a green run is evidence that the write path works rather than that
 * it typechecks.
 *
 * It also leaves state behind on purpose. A borrow position is the thing every
 * lending keeper discovers, and until one exists on devnet the keepers have
 * nothing to prove themselves against.
 *
 *   node --experimental-strip-types scripts/devnet/live_write_proof.ts
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

// Imported from the built SDK rather than its sources: this must exercise the
// artifact the app consumes, not a parallel compilation of it.
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

function discriminator(name: string): Buffer {
  return createHash("sha256").update(`global:${name}`).digest().subarray(0, 8);
}

function loadKeypair(): Keypair {
  const path =
    process.env.DUSK_KEYPAIR ?? join(homedir(), ".config/solana/id.json");
  const secret = JSON.parse(readFileSync(path, "utf8"));
  return Keypair.fromSecretKey(Uint8Array.from(secret));
}

interface DeploymentConfig {
  baseDecimals: number;
  baseMint: string;
  primaryMarket: string;
  programId: string;
  quoteDecimals: number;
  quoteMint: string;
}

async function deploymentConfig(): Promise<DeploymentConfig> {
  const response = await fetch(`${API}/api/dusk/v1/config`);
  if (!response.ok) throw new Error(`config: HTTP ${response.status}`);
  const body = (await response.json()) as { data: DeploymentConfig };
  return body.data;
}

/**
 * Mint test tokens. Built by hand for the same reason the app builds it by
 * hand: one instruction with a fixed account list does not justify a second
 * IDL and coder.
 */
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

const steps: { detail: string; name: string; signature?: string }[] = [];

function record(name: string, detail: string, signature?: string) {
  steps.push({ detail, name, signature });
  const suffix = signature ? ` ${signature}` : "";
  console.log(`  ${name.padEnd(22)} ${detail}${suffix}`);
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
    throw new Error(
      `${signature} failed on chain: ${JSON.stringify(result.value.err)}`,
    );
  }
  return signature;
}

async function tokenBalance(
  connection: Connection,
  owner: PublicKey,
  mint: PublicKey,
): Promise<bigint> {
  const address = getAssociatedTokenAddressSync(mint, owner);
  const account = await connection.getAccountInfo(address, "confirmed");
  if (!account) return 0n;
  return account.data.readBigUInt64LE(64);
}

async function main() {
  const keypair = loadKeypair();
  const connection = new Connection(RPC, "confirmed");
  const config = await deploymentConfig();

  console.log(`wallet  ${keypair.publicKey.toBase58()}`);
  console.log(`market  ${config.primaryMarket}`);
  console.log(`program ${config.programId}\n`);

  const lamports = await connection.getBalance(keypair.publicKey, "confirmed");
  if (lamports < 100_000_000) {
    throw new Error(
      `fee payer holds ${lamports / 1e9} SOL; fund it before running`,
    );
  }

  const provider = new AnchorProvider(connection, new Wallet(keypair), {
    commitment: "confirmed",
  });
  const dusk = new Dusk({ programId: new PublicKey(config.programId), provider });

  const market = new PublicKey(config.primaryMarket);
  const baseMint = new PublicKey(config.baseMint);
  const quoteMint = new PublicKey(config.quoteMint);
  const baseUnit = 10n ** BigInt(config.baseDecimals);
  const quoteUnit = 10n ** BigInt(config.quoteDecimals);

  // 1. Faucet. Also the first real signature of the run, so a failure here
  //    separates "cannot sign at all" from "cannot build a protocol write".
  const wantBase = 500n * baseUnit;
  const wantQuote = 500n * quoteUnit;
  const mintSignature = await send(connection, keypair, [
    faucetMint(keypair.publicKey, baseMint, wantBase),
    faucetMint(keypair.publicKey, quoteMint, wantQuote),
  ]);
  record("faucet_mint", "500 base + 500 quote", mintSignature);

  const baseBefore = await tokenBalance(connection, keypair.publicKey, baseMint);
  const quoteBefore = await tokenBalance(
    connection,
    keypair.publicKey,
    quoteMint,
  );

  // 2. Swap through the SDK, the same builder the app calls.
  const swapAmount = 10n * baseUnit;
  const swapInstruction = await dusk.write.buildSwapInstruction({
    assetInMint: baseMint,
    assetOutMint: quoteMint,
    exactAssetIn: swapAmount.toString(),
    market,
    // No floor: this run is proving the write path, and a floor would make a
    // routine price move look like a broken builder.
    minAssetOut: "0",
    trader: keypair.publicKey,
  });
  const swapSignature = await send(connection, keypair, [swapInstruction]);
  const quoteAfterSwap = await tokenBalance(
    connection,
    keypair.publicKey,
    quoteMint,
  );
  record(
    "swap",
    `10 base in, ${Number(quoteAfterSwap - quoteBefore) / Number(quoteUnit)} quote out`,
    swapSignature,
  );

  // 3. Collateral, then borrow. This is the state the lending keepers exist
  //    to watch, so it stays behind after the run.
  // The position id is a fresh keypair's public key, matching the app: a
  // position is addressed by an id the owner picks, not by the owner, so one
  // wallet can hold several.
  const positionId = Keypair.generate().publicKey;
  const collateral = 100n * baseUnit;
  const collateralSignature = await send(connection, keypair, [
    await dusk.write.depositCollateralInstruction({
      assetMint: baseMint,
      depositAmount: collateral.toString(),
      market,
      owner: keypair.publicKey,
      ownerAssetAccount: getAssociatedTokenAddressSync(
        baseMint,
        keypair.publicKey,
      ),
      positionId,
    }),
  ]);
  record("deposit_collateral", "100 base", collateralSignature);

  const borrowAmount = 5n * quoteUnit;
  const borrowSignature = await send(connection, keypair, [
    await dusk.write.buildBorrowInstruction({
      borrowAmount: borrowAmount.toString(),
      collateralAssetMint: baseMint,
      debtAssetMint: quoteMint,
      market,
      minDebtAmountOut: borrowAmount.toString(),
      minLiquidationCfBps: 0,
      owner: keypair.publicKey,
      positionId,
    }),
  ]);
  record("borrow", "5 quote", borrowSignature);

  const [position] = deriveBorrowPositionAddress(market, positionId);
  const positionAccount = await connection.getAccountInfo(position, "confirmed");
  record(
    "borrow_position",
    positionAccount
      ? `${position.toBase58()} (${positionAccount.data.length} bytes)`
      : `${position.toBase58()} MISSING`,
  );
  if (!positionAccount) {
    throw new Error("borrow succeeded but no position account exists");
  }

  const baseAfter = await tokenBalance(connection, keypair.publicKey, baseMint);
  console.log(
    `\nbase ${Number(baseBefore) / Number(baseUnit)} -> ${Number(baseAfter) / Number(baseUnit)}`,
  );
  console.log(`${steps.filter((step) => step.signature).length} signed writes confirmed`);
}

main().catch((error) => {
  console.error(`\nFAILED: ${error instanceof Error ? error.message : error}`);
  process.exit(1);
});
