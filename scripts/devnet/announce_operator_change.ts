/** Announce one operator-authorized devnet bundle. Execution remains
 * permissionless after the full junior notice plus one protocol day.
 * node --experimental-strip-types scripts/devnet/announce_operator_change.ts
 */
import {
  ComputeBudgetProgram, Connection, Keypair, PublicKey, Transaction,
  TransactionInstruction,
} from "@solana/web3.js";
import { AnchorProvider, Wallet } from "@coral-xyz/anchor";
import { createHash } from "crypto";
import { readFileSync } from "fs";
import { homedir } from "os";
import { join } from "path";
import { Dusk, dailyBorrowLimitParameterUpdate } from "../../packages/dusk-sdk/dist/index.js";

const API = process.env.DUSK_API_URL ?? "https://dusk-api-production-291f.up.railway.app";
const RPC = process.env.DUSK_RPC_URL ?? "https://api.devnet.solana.com";

async function main() {
  const keypair = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(readFileSync(
    process.env.DUSK_KEYPAIR ?? join(homedir(), ".config/solana/id.json"), "utf8"))));
  const connection = new Connection(RPC, "confirmed");
  const config = (await (await fetch(`${API}/api/dusk/v1/config`)).json()).data;
  if (!config) throw new Error("deployment config unavailable");
  const provider = new AnchorProvider(connection, new Wallet(keypair), { commitment: "confirmed" });
  const dusk = new Dusk({ programId: new PublicKey(config.programId), provider });

  const owner = keypair.publicKey;
  const market = new PublicKey(process.env.MARKET ?? config.primaryMarket);

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

  // The description is content-addressed: the chain stores the hash and length,
  // and the URI is where a reader fetches the text. Nothing fetches it here.
  const description = "Raise the daily borrow limit so the devnet acceptance matrix can exercise operator changes end to end.";
  const bytes = Buffer.from(description, "utf8");

  const created = await dusk.operator.announce({
    market,
    operator: owner,
    payer: owner,
    updates: [dailyBorrowLimitParameterUpdate(Number(process.env.MAX_DAILY_BORROW_BPS ?? "2500"))],
    metadata: {
      version: 1,
      title: "Devnet: raise the daily borrow limit",
      descriptionUri: "https://omnipair.fi/proposals/devnet-daily-borrow-limit.md",
      descriptionSha256: Array.from(createHash("sha256").update(bytes).digest()),
      descriptionLen: bytes.length,
    },
  });
  console.log(`create  ${await send([created.instruction])}`);
  console.log(`proposal ${created.proposal.toBase58()}`);

  const stored = await dusk.operator.proposal(created.proposal);
  console.log(`executable after slot ${stored.executeAfterSlot}`);
  console.log(`deadline slot         ${stored.deadlineSlot}`);
  console.log(`digest verified       ${await dusk.operator.verifyDigest(stored)}`);

}
main().catch((e) => { console.error(e); process.exit(1); });
