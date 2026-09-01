/**
 * Every product flow, signed and sent against live devnet.
 *
 * The plan's definition of done is that a person can complete every supported
 * flow through the SDK. Simulation cannot establish that: it misses fee payers
 * that cannot pay, accounts the runtime creates only on the real path, and
 * races with other writers. So each flow here signs and sends, and reports
 * what the chain said.
 *
 * Flows run in dependency order — you cannot remove liquidity you never added,
 * or repay a loan you never took — and a failure is recorded and the run
 * continues, because one broken flow should not hide the state of the other
 * eleven.
 *
 *   node --experimental-strip-types scripts/devnet/live_flow_matrix.ts
 */

import {
  ASSOCIATED_TOKEN_PROGRAM_ID,
  TOKEN_2022_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  createAssociatedTokenAccountIdempotentInstruction,
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

interface Outcome {
  flow: string;
  ok: boolean;
  detail: string;
  /** Attempts spent, when more than one was needed. */
  attempts: number;
}

/**
 * How many times to retry a flow that hit the known hLP invariant defect.
 *
 * That failure is intermittent, so a single attempt cannot distinguish a flow
 * that is broken from one that is merely blocked — and reporting them the same
 * way would bury a real regression under a known defect. Retrying separates
 * them: a flow that eventually succeeds is working and obstructed, one that
 * never does is neither.
 */
const INVARIANT_RETRIES = 6;

const outcomes: Outcome[] = [];

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
  const owner = keypair.publicKey;
  const market = new PublicKey(config.primaryMarket);
  const baseMint = new PublicKey(config.baseMint);
  const quoteMint = new PublicKey(config.quoteMint);
  const ylpMint = new PublicKey(config.ylpMint);
  const unit = 10n ** BigInt(config.baseDecimals);
  // The yLP mint is Token-2022 — it carries the transfer hook the market
  // validates — while the asset mints are legacy. Deriving every associated
  // account under one program silently produces the wrong address for the
  // other, and the failure surfaces as IncorrectProgramId from inside the
  // associated-token program rather than as anything about yLP.
  const tokenProgramFor = (mint: PublicKey) =>
    mint.equals(ylpMint) ? TOKEN_2022_PROGRAM_ID : TOKEN_PROGRAM_ID;
  const ata = (mint: PublicKey, holder: PublicKey = owner) =>
    getAssociatedTokenAddressSync(mint, holder, false, tokenProgramFor(mint));

  const send = async (instructions: TransactionInstruction[]) => {
    const transaction = new Transaction().add(
      ComputeBudgetProgram.setComputeUnitLimit({ units: 600_000 }),
      ...instructions,
    );
    const blockhash = await connection.getLatestBlockhash("confirmed");
    transaction.recentBlockhash = blockhash.blockhash;
    transaction.feePayer = owner;
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

  /**
   * Run one flow. A failure is recorded rather than thrown: the point is a
   * matrix, and stopping at the first red cell tells you least when most is
   * broken.
   */
  const flow = async (name: string, run: () => Promise<string>) => {
    let blocked = 0;
    for (let attempt = 1; ; attempt += 1) {
    try {
      const signature = await run();
      outcomes.push({
        attempts: attempt,
        detail: signature.slice(0, 20),
        flow: name,
        ok: true,
      });
      console.log(
        `  ok    ${name.padEnd(26)} ${signature.slice(0, 20)}${blocked > 0 ? `  (after ${blocked} blocked)` : ""}`,
      );
      return;
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      // web3.js puts the Anchor error in the logs, not the message, and the
      // message alone reads as a generic simulation failure — which is how a
      // named, actionable error gets reported as "something went wrong".
      const logs: string[] =
        (error as { logs?: string[] }).logs ??
        (typeof (error as { getLogs?: unknown }).getLogs === "function" ? [] : []);
      const fromLogs = logs.find((line) => line.includes("Error Code"));
      const anchor =
        message.match(/Error Code: (\w+)/)?.[1] ??
        fromLogs?.match(/Error Code: (\w+)/)?.[1];
      const detail =
        anchor ??
        fromLogs?.replace(/^Program log: /, "").slice(0, 70) ??
        message.replace(/\s+/g, " ").slice(0, 70);
      if (detail === "BrokenInvariant" && blocked < INVARIANT_RETRIES) {
        blocked += 1;
        await new Promise((resolve) => setTimeout(resolve, 4_000));
        continue;
      }
      outcomes.push({ attempts: attempt, detail, flow: name, ok: false });
      console.log(
        `  FAIL  ${name.padEnd(26)} ${detail}${blocked > 0 ? `  (${blocked} retries)` : ""}`,
      );
      return;
    }
    }
  };

  const balance = async (mint: PublicKey): Promise<bigint> => {
    const account = await connection.getAccountInfo(ata(mint), "confirmed");
    return account ? account.data.readBigUInt64LE(64) : 0n;
  };

  console.log(`wallet ${owner.toBase58()}\nmarket ${market.toBase58()}\n`);

  await flow("faucet mint", () =>
    send([
      faucetMint(owner, baseMint, 5_000n * unit),
      faucetMint(owner, quoteMint, 5_000n * unit),
    ]),
  );

  await flow("swap", async () =>
    send([
      await dusk.write.buildSwapInstruction({
        assetInMint: baseMint,
        assetOutMint: quoteMint,
        exactAssetIn: (5n * unit).toString(),
        market,
        minAssetOut: "0",
        trader: owner,
      }),
    ]),
  );

  const liquidityAccounts = {
    baseMint,
    market,
    owner,
    ownerBaseAccount: ata(baseMint),
    ownerQuoteAccount: ata(quoteMint),
    ownerYlpAccount: ata(ylpMint),
    quoteMint,
    ylpMint,
  };

  await flow("add liquidity", async () =>
    send([
      createAssociatedTokenAccountIdempotentInstruction(
        owner,
        ata(ylpMint),
        owner,
        ylpMint,
        TOKEN_2022_PROGRAM_ID,
      ),
      await dusk.write.addLiquidityInstruction({
        ...liquidityAccounts,
        baseDepositAmount: (50n * unit).toString(),
        minYlpAmount: "0",
        quoteDepositAmount: (50n * unit).toString(),
      }),
    ]),
  );

  const ylpHeld = await balance(ylpMint);
  await flow("remove liquidity", async () =>
    send([
      await dusk.write.removeLiquidityInstruction({
        ...liquidityAccounts,
        minBaseAmountOut: "0",
        minQuoteAmountOut: "0",
        ylpAmount: (ylpHeld / 2n).toString(),
      }),
    ]),
  );

  const positionId = Keypair.generate().publicKey;
  await flow("deposit collateral", async () =>
    send([
      await dusk.write.depositCollateralInstruction({
        assetMint: baseMint,
        depositAmount: (200n * unit).toString(),
        market,
        owner,
        ownerAssetAccount: ata(baseMint),
        positionId,
      }),
    ]),
  );

  await flow("borrow", async () =>
    send([
      await dusk.write.buildBorrowInstruction({
        borrowAmount: (20n * unit).toString(),
        collateralAssetMint: baseMint,
        debtAssetMint: quoteMint,
        market,
        minDebtAmountOut: (20n * unit).toString(),
        minLiquidationCfBps: 0,
        owner,
        positionId,
      }),
    ]),
  );

  await flow("repay", async () =>
    send([
      await dusk.write.repayInstruction({
        debtAssetMint: quoteMint,
        market,
        owner,
        ownerDebtAccount: ata(quoteMint),
        positionId,
        repayAmount: (25n * unit).toString(),
      }),
    ]),
  );

  await flow("withdraw collateral", async () =>
    send([
      await dusk.write.withdrawCollateralInstruction({
        assetMint: baseMint,
        market,
        minAssetAmountOut: "0",
        minLiquidationCfBps: 0,
        owner,
        ownerAssetAccount: ata(baseMint),
        positionId,
        withdrawAmount: (100n * unit).toString(),
      }),
    ]),
  );

  const leverageId = Keypair.generate().publicKey;
  await flow("open leverage", async () =>
    send([
      await dusk.write.buildOpenLeverageInstruction({
        collateralMint: baseMint,
        debtAsset: "quote",
        debtMint: quoteMint,
        marginAmount: (50n * unit).toString(),
        market,
        minCollateralOut: "0",
        multiplierBps: "20000",
        owner,
        positionId: leverageId,
      }),
    ]),
  );

  const leverageAccounts = {
    collateralMint: baseMint,
    debtAsset: "quote" as const,
    debtMint: quoteMint,
    market,
    positionId: leverageId,
    positionOwner: owner,
  };

  await flow("add leverage margin", async () =>
    send([
      await dusk.write.addLeverageMarginInstruction({
        ...leverageAccounts,
        amount: (5n * unit).toString(),
        ownerDebtAccount: ata(quoteMint),
      }),
    ]),
  );

  await flow("close leverage", async () =>
    send([
      await dusk.write.closeLeverageInstruction({
        ...leverageAccounts,
        minAmountOut: "0",
        ownerDebtAccount: ata(quoteMint),
      }),
    ]),
  );

  const passed = outcomes.filter((outcome) => outcome.ok).length;
  const obstructed = outcomes.filter(
    (outcome) => outcome.ok && outcome.attempts > 1,
  );
  console.log(`\n${passed}/${outcomes.length} flows signed and confirmed`);
  if (obstructed.length > 0) {
    console.log(
      `${obstructed.length} needed retries past the hLP invariant defect: ` +
        obstructed.map((outcome) => outcome.flow).join(", "),
    );
  }
  const failed = outcomes.filter((outcome) => !outcome.ok);
  if (failed.length > 0) {
    console.log("failing:");
    for (const outcome of failed) {
      console.log(`  ${outcome.flow}: ${outcome.detail}`);
    }
    process.exit(1);
  }
}

main().catch((error) => {
  console.error(`FAILED: ${error instanceof Error ? error.message : error}`);
  process.exit(2);
});
