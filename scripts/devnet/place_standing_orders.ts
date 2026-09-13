/**
 * Open a small leverage position and leave two conditional orders on it.
 *
 * The acceptance matrix cancels the order it places, so a wallet that has run
 * it still has nothing to look at. This leaves a take-profit and a stop-loss
 * standing so the orders surface has real rows to render, and so the read path
 * is exercised against accounts that actually exist.
 *
 *   node --experimental-strip-types scripts/devnet/place_standing_orders.ts
 */
import { TOKEN_2022_PROGRAM_ID, getAssociatedTokenAddressSync } from "@solana/spl-token";
import {
  ComputeBudgetProgram, Connection, Keypair, PublicKey, Transaction,
  TransactionInstruction,
} from "@solana/web3.js";
import { AnchorProvider, Wallet } from "@coral-xyz/anchor";
import { readFileSync } from "fs";
import { homedir } from "os";
import { join } from "path";
import {
  Dusk,
  DuskLeverageOrders,
  LEVERAGE_DELEGATE_PROGRAM_ID,
  createLeverageDelegateProgram,
} from "../../packages/dusk-sdk/dist/index.js";

const API = process.env.DUSK_API_URL ?? "https://dusk-api-production-291f.up.railway.app";
const RPC = process.env.DUSK_RPC_URL ?? "https://api.devnet.solana.com";
/** CLOSE | CLOSE_SETTLED — the minimum a take-profit needs. */
const APPROVED_ACTIONS = (1 << 0) | (1 << 5);

async function main() {
  const keypair = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(readFileSync(
    process.env.DUSK_KEYPAIR ?? join(homedir(), ".config/solana/id.json"), "utf8"))));
  const connection = new Connection(RPC, "confirmed");
  const config = (await (await fetch(`${API}/api/dusk/v1/config`)).json()).data;
  if (!config) throw new Error("deployment config unavailable");
  const provider = new AnchorProvider(connection, new Wallet(keypair), { commitment: "confirmed" });
  const dusk = new Dusk({ programId: new PublicKey(config.programId), provider });

  const owner = keypair.publicKey;
  const market = new PublicKey(config.primaryMarket);
  const baseMint = new PublicKey(config.baseMint);
  const quoteMint = new PublicKey(config.quoteMint);
  const unit = 10n ** BigInt(config.quoteDecimals);

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

  // Small enough to stay well inside LEVERAGE_MAX_UNWIND_IMPACT_BPS at current
  // depth; the point is to have a position, not a large one.
  const positionId = Keypair.generate().publicKey;
  console.log(`position ${positionId.toBase58()}`);
  console.log(`open      ${await send([
    await dusk.write.buildOpenLeverageInstruction({
      collateralMint: baseMint, debtAsset: "quote", debtMint: quoteMint,
      marginAmount: (20n * unit).toString(), market, minCollateralOut: "0",
      multiplierBps: "20000", owner, positionId,
    }),
  ])}`);
  console.log(`delegate  ${await send([
    await dusk.write.buildCreateLeverageDelegationInstruction({
      approvedActions: APPROVED_ACTIONS, debtAsset: "quote",
      delegatedProgram: LEVERAGE_DELEGATE_PROGRAM_ID, market, owner, positionId,
    }),
  ])}`);

  const orders = new DuskLeverageOrders(createLeverageDelegateProgram({ provider }));
  const base = BigInt(Date.now());
  for (const [kind, triggerNad, closeBps, label] of [
    ["takeProfit", 2n * 10n ** 9n, 4_000, "take profit, closes 40%"],
    ["stopLoss", 5n * 10n ** 8n, 10_000, "stop loss, closes 100%"],
  ] as const) {
    const signature = await send([
      await orders.createOrderInstruction({
        closeBps, kind, market, orderId: base + BigInt(kind === "takeProfit" ? 1 : 2),
        owner, positionId, triggerCloseoutPriceNad: triggerNad.toString(),
      }),
    ]);
    console.log(`${label.padEnd(24)} ${signature}`);
  }
  console.log("\nboth orders left standing — the orders surface should now show two rows.");
}
main().catch((e) => { console.error(e); process.exit(1); });
