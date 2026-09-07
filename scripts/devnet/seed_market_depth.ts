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
 * The deployed faucet allows one mint per wallet per mint per hour, capped at
 * `MAX_MINT_PER_REQUEST`. So a run gets one mint a side and then works from
 * what it holds: it deposits every token available, reports how far short of
 * the target that leaves the pool, and has to be run again for more. Depositing
 * is unlimited — only acquiring is rate limited.
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
/** `MAX_MINT_PER_REQUEST` in the deployed faucet, in raw atoms. */
const MAX_MINT_RAW = 10_000_000_000n;
/** `FaucetError::CooldownActive`. */
const COOLDOWN_ACTIVE = 6002;

const discriminator = (name: string) =>
  createHash("sha256").update(`global:${name}`).digest().subarray(0, 8);

function faucetMint(owner: PublicKey, mint: PublicKey, amount: bigint): TransactionInstruction {
  const programId = new PublicKey(FAUCET_PROGRAM_ID);
  const [authority] = PublicKey.findProgramAddressSync(
    [Buffer.from("faucet_authority"), programId.toBuffer()], programId);
  // The deployed faucet rate-limits per recipient and mint, so it takes a
  // claim PDA that the eight-account layout predates.
  const [claim] = PublicKey.findProgramAddressSync(
    [Buffer.from("faucet_claim"), owner.toBuffer(), mint.toBuffer()], programId);
  const data = Buffer.alloc(8);
  data.writeBigUInt64LE(amount);
  return new TransactionInstruction({
    data: Buffer.concat([discriminator("faucet_mint"), data]),
    keys: [
      { isSigner: true, isWritable: true, pubkey: owner },
      { isSigner: false, isWritable: false, pubkey: owner },
      { isSigner: false, isWritable: false, pubkey: authority },
      { isSigner: false, isWritable: true, pubkey: getAssociatedTokenAddressSync(mint, owner) },
      { isSigner: false, isWritable: true, pubkey: claim },
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

  const balance = async (mint: PublicKey) => {
    const held = await connection
      .getTokenAccountBalance(ata(mint), "confirmed")
      .catch(() => null);
    return held ? BigInt(held.value.amount) : 0n;
  };

  const shortfall = () =>
    (target - (now.base < now.quote ? now.base : now.quote)) * unit;

  // One attempt per mint. A cooldown is the expected answer on a repeat run,
  // not a failure — the deposit below still has whatever is already held to
  // work with. Preflight spells the code `0x1772` and confirmation spells it
  // `6002`, so match either.
  const acquire = async (mint: PublicKey, want: bigint) => {
    const label = mint.toBase58().slice(0, 8);
    if (want <= 0n) return;
    const ask = want > MAX_MINT_RAW ? MAX_MINT_RAW : want;
    try {
      await send([faucetMint(owner, mint, ask)]);
      console.log(`  minted ${ask / unit} ${label}`);
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      const cooldown = new RegExp(`${COOLDOWN_ACTIVE}|0x${COOLDOWN_ACTIVE.toString(16)}`);
      if (!cooldown.test(message)) throw e;
      console.log(`  ${label}: faucet cooldown active, depositing held balance`);
    }
  };

  await acquire(baseMint, shortfall() - (await balance(baseMint)));
  await acquire(quoteMint, shortfall() - (await balance(quoteMint)));

  // Deposit is not rate limited, so this runs until the tokens are gone or the
  // target is met, whichever comes first.
  for (;;) {
    const held = {
      base: await balance(baseMint),
      quote: await balance(quoteMint),
    };
    const available = held.base < held.quote ? held.base : held.quote;
    const room = shortfall() < available ? shortfall() : available;
    if (room <= 0n) break;
    const chunk = room > CHUNK * unit ? CHUNK * unit : room;
    await send([
      await dusk.write.addLiquidityInstruction({
        baseMint, market, owner,
        ownerBaseAccount: ata(baseMint),
        ownerQuoteAccount: ata(quoteMint),
        ownerYlpAccount: ata(ylpMint),
        quoteMint, ylpMint,
        baseDepositAmount: chunk.toString(),
        quoteDepositAmount: chunk.toString(),
        minYlpAmount: "0",
      }),
    ]);
    now = await depth();
    console.log(`  +${chunk / unit} a side -> ${now.base} base / ${now.quote} quote`);
  }

  if (now.base >= target && now.quote >= target) {
    console.log(`\ndepth restored: ${now.base} base / ${now.quote} quote`);
    return;
  }
  console.log(
    `\ndepth ${now.base} base / ${now.quote} quote — ${shortfall() / unit} a side ` +
      `short of ${target}. The faucet allows ${MAX_MINT_RAW / unit} per mint per ` +
      `hour per wallet, so run this again in an hour to add more.`,
  );
}
main().catch((e) => { console.error(e); process.exit(1); });
