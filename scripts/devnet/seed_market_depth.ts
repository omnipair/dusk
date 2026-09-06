/**
 * Restore market depth on devnet.
 *
 * Leverage refuses to open when unwinding the position would move the price
 * more than `LEVERAGE_MAX_UNWIND_IMPACT_BPS` (2%), so a shallow market fails
 * `open_leverage` with `LeverageUnwindImpactTooHigh` while every other flow
 * still passes. That reads as a leverage bug and is really a depth problem.
 *
 * Mints from the faucet and adds liquidity until each side reaches the target.
 * Deposits in equal amounts so the pool is not pushed off parity.
 *
 *   TARGET=50000 node --experimental-strip-types scripts/devnet/seed_market_depth.ts
 */
import {
  ASSOCIATED_TOKEN_PROGRAM_ID,
  TOKEN_2022_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  createAssociatedTokenAccountIdempotentInstruction,
  getAssociatedTokenAddressSync,
} from "@solana/spl-token";
import {
  ComputeBudgetProgram, Connection, Keypair, PublicKey, SystemProgram,
  Transaction, TransactionInstruction,
} from "@solana/web3.js";
import { AnchorProvider, Wallet } from "@coral-xyz/anchor";
import { createHash } from "crypto";
import { readFileSync } from "fs";
import { homedir } from "os";
import { join } from "path";
import { Dusk } from "../../packages/dusk-sdk/dist/index.js";

const API = process.env.DUSK_API_URL ?? "https://dusk-api-production-291f.up.railway.app";
const RPC = process.env.DUSK_RPC_URL ?? "https://api.devnet.solana.com";
const FAUCET_PROGRAM_ID =
  process.env.DUSK_FAUCET_PROGRAM_ID ?? "EMmV9HKeQndxFd4duqp65rUSjikVWCPakBH1UjJJ32dz";
/** Per-transaction deposit, kept small enough to stay inside one budget. */
const CHUNK = 5_000n;

const discriminator = (name: string) =>
  createHash("sha256").update(`global:${name}`).digest().subarray(0, 8);

function faucetMint(owner: PublicKey, mint: PublicKey, amount: bigint): TransactionInstruction {
  const programId = new PublicKey(FAUCET_PROGRAM_ID);
  const [authority] = PublicKey.findProgramAddressSync(
    [Buffer.from("faucet_authority"), programId.toBuffer()], programId);
  const data = Buffer.alloc(8);
  data.writeBigUInt64LE(amount);
  return new TransactionInstruction({
    data: Buffer.concat([discriminator("faucet_mint"), data]),
    keys: [
      { isSigner: true, isWritable: true, pubkey: owner },
      { isSigner: false, isWritable: false, pubkey: owner },
      { isSigner: false, isWritable: false, pubkey: authority },
      { isSigner: false, isWritable: true, pubkey: getAssociatedTokenAddressSync(mint, owner) },
      { isSigner: false, isWritable: true, pubkey: mint },
      { isSigner: false, isWritable: false, pubkey: SystemProgram.programId },
      { isSigner: false, isWritable: false, pubkey: TOKEN_PROGRAM_ID },
      { isSigner: false, isWritable: false, pubkey: ASSOCIATED_TOKEN_PROGRAM_ID },
    ],
    programId,
  });
}

async function main() {
  const target = BigInt(process.env.TARGET ?? "50000");
  const keypair = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(readFileSync(
    process.env.DUSK_KEYPAIR ?? join(homedir(), ".config/solana/id.json"), "utf8"))));
  const connection = new Connection(RPC, "confirmed");
  // The API previews every market and fails as a whole when any one of them
  // cannot be previewed — which is exactly the state a freshly created,
  // unseeded market puts it in. So allow every value to come from the
  // environment, or this script cannot fix the thing that broke the API.
  const config = process.env.PROGRAM_ID
    ? {
        programId: process.env.PROGRAM_ID,
        primaryMarket: process.env.MARKET,
        baseMint: process.env.BASE_MINT,
        quoteMint: process.env.QUOTE_MINT,
        ylpMint: process.env.YLP_MINT,
        baseDecimals: Number(process.env.BASE_DECIMALS ?? "6"),
      }
    : (await (await fetch(`${API}/api/dusk/v1/config`)).json()).data;
  if (!config) throw new Error("no deployment config: the API is down and PROGRAM_ID was not set");
  const provider = new AnchorProvider(connection, new Wallet(keypair), { commitment: "confirmed" });
  const dusk = new Dusk({ programId: new PublicKey(config.programId), provider });

  const owner = keypair.publicKey;
  // Defaults to the deployment's primary market. MARKET and YLP_MINT together
  // point it at any other market on the same program — the API's config
  // endpoint only describes the primary one.
  const market = new PublicKey(process.env.MARKET ?? config.primaryMarket);
  const baseMint = new PublicKey(config.baseMint);
  const quoteMint = new PublicKey(config.quoteMint);
  const ylpMint = new PublicKey(process.env.YLP_MINT ?? config.ylpMint);
  const unit = 10n ** BigInt(config.baseDecimals);
  const ata = (mint: PublicKey) =>
    getAssociatedTokenAddressSync(mint, owner, false,
      mint.equals(ylpMint) ? TOKEN_2022_PROGRAM_ID : TOKEN_PROGRAM_ID);

  const send = async (instructions: TransactionInstruction[]) => {
    const tx = new Transaction().add(
      ComputeBudgetProgram.setComputeUnitLimit({ units: 800_000 }), ...instructions);
    const bh = await connection.getLatestBlockhash("confirmed");
    tx.recentBlockhash = bh.blockhash; tx.feePayer = owner; tx.sign(keypair);
    const sig = await connection.sendRawTransaction(tx.serialize(), { preflightCommitment: "confirmed" });
    const r = await connection.confirmTransaction({ ...bh, signature: sig }, "confirmed");
    if (r.value.err) throw new Error(JSON.stringify(r.value.err));
    return sig;
  };

  const depth = async () => {
    const info = await connection.getAccountInfo(market, "confirmed");
    if (!info) throw new Error("market account not found");
    return {
      base: info.data.readBigUInt64LE(202) / unit,
      quote: info.data.readBigUInt64LE(707) / unit,
    };
  };

  let now = await depth();
  console.log(`depth now: ${now.base} base / ${now.quote} quote — target ${target} a side`);
  if (now.base >= target && now.quote >= target) {
    console.log("already at target, nothing to do");
    return;
  }

  await send([
    createAssociatedTokenAccountIdempotentInstruction(
      owner, ata(ylpMint), owner, ylpMint, TOKEN_2022_PROGRAM_ID),
  ]);

  while (now.base < target || now.quote < target) {
    const remaining = target - (now.base < now.quote ? now.base : now.quote);
    const chunk = remaining < CHUNK ? remaining : CHUNK;
    if (chunk <= 0n) break;
    await send([
      faucetMint(owner, baseMint, chunk * unit),
      faucetMint(owner, quoteMint, chunk * unit),
    ]);
    await send([
      await dusk.write.addLiquidityInstruction({
        baseMint, market, owner,
        ownerBaseAccount: ata(baseMint),
        ownerQuoteAccount: ata(quoteMint),
        ownerYlpAccount: ata(ylpMint),
        quoteMint, ylpMint,
        baseDepositAmount: (chunk * unit).toString(),
        quoteDepositAmount: (chunk * unit).toString(),
        minYlpAmount: "0",
      }),
    ]);
    now = await depth();
    console.log(`  +${chunk} a side -> ${now.base} base / ${now.quote} quote`);
  }
  console.log(`\ndepth restored: ${now.base} base / ${now.quote} quote`);
}
main().catch((e) => { console.error(e); process.exit(1); });
