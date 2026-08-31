/**
 * Open a leverage position on devnet.
 *
 * The leverage keeper has nothing to discover until one exists, and unlike a
 * borrow position there is no way to create one as a side effect of ordinary
 * testing. This opens a small one so the keeper's discovery, account
 * assembly and refusal path can be exercised against real state.
 *
 *   node --experimental-strip-types scripts/devnet/open_leverage.ts
 */

import {
  ASSOCIATED_TOKEN_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  getAssociatedTokenAddressSync,
} from "@solana/spl-token";
import {
  ComputeBudgetProgram,
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

import { Dusk } from "../../packages/dusk-sdk/dist/index.js";

const API =
  process.env.DUSK_API_URL ?? "https://dusk-api-production-291f.up.railway.app";
const RPC = process.env.DUSK_RPC_URL ?? "https://api.devnet.solana.com";
const FAUCET_PROGRAM_ID =
  process.env.DUSK_FAUCET_PROGRAM_ID ??
  "EMmV9HKeQndxFd4duqp65rUSjikVWCPakBH1UjJJ32dz";

const MARGIN = 100n;
/** Multipliers to try, largest first; the first the market accepts wins. */
const MULTIPLIERS = [30_000n, 20_000n, 15_000n, 12_000n];

function discriminator(name: string): Buffer {
  return createHash("sha256").update(`global:${name}`).digest().subarray(0, 8);
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

async function main() {
  const keypair = Keypair.fromSecretKey(
    Uint8Array.from(
      JSON.parse(
        readFileSync(
          process.env.DUSK_KEYPAIR ?? join(homedir(), ".config/solana/id.json"),
          "utf8",
        ),
      ),
    ),
  );
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
  const unit = 10n ** BigInt(config.baseDecimals);

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

  await send([faucetMint(keypair.publicKey, baseMint, MARGIN * 4n * unit)]);

  const positionId = Keypair.generate().publicKey;
  for (const multiplierBps of MULTIPLIERS) {
    try {
      const signature = await send([
        ComputeBudgetProgram.setComputeUnitLimit({ units: 600_000 }),
        await dusk.write.buildOpenLeverageInstruction({
          collateralMint: baseMint,
          debtAsset: "quote",
          debtMint: quoteMint,
          marginAmount: (MARGIN * unit).toString(),
          market,
          minCollateralOut: "0",
          multiplierBps: multiplierBps.toString(),
          owner: keypair.publicKey,
          positionId,
        }),
      ]);
      console.log(
        `opened at ${Number(multiplierBps) / 10_000}x on ${MARGIN} base margin  ${signature}`,
      );
      return;
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      const anchor = message.match(/Error Code: (\w+)/)?.[1];
      console.log(
        `  ${Number(multiplierBps) / 10_000}x refused (${anchor ?? message.replace(/\s+/g, " ").slice(0, 70)})`,
      );
    }
  }
  throw new Error("no multiplier was accepted");
}

main().catch((error) => {
  console.error(`FAILED: ${error instanceof Error ? error.message : error}`);
  process.exit(1);
});
