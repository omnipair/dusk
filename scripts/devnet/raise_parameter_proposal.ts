/**
 * Raise a parameter proposal and carry it as far as the timelock allows.
 *
 * The definition of done includes moving a proposal through timelock to
 * execution, and `dusk-lifecycle-keeper` has never had one to act on. The
 * timelock is `PARAMETER_PROPOSAL_TIMELOCK_SECONDS` — seven days — and a real
 * cluster has no clock to advance, so this does the three steps that can be
 * done now: create, support past the strict majority, and queue. Execution is
 * permissionless afterwards, which is precisely what the lifecycle keeper is
 * for.
 *
 *   node --experimental-strip-types scripts/devnet/raise_parameter_proposal.ts
 */
import { TOKEN_2022_PROGRAM_ID, getAssociatedTokenAddressSync } from "@solana/spl-token";
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
  const ylpMint = new PublicKey(config.ylpMint);
  const ylpAta = getAssociatedTokenAddressSync(ylpMint, owner, true, TOKEN_2022_PROGRAM_ID);

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

  const ylpAccount = await connection.getAccountInfo(ylpAta, "confirmed");
  const held = ylpAccount ? ylpAccount.data.readBigUInt64LE(64) : 0n;
  console.log(`yLP held by proposer: ${held}`);
  if (held === 0n) throw new Error("proposer holds no yLP; it cannot sponsor a proposal");

  // The description is content-addressed: the chain stores the hash and length,
  // and the URI is where a reader fetches the text. Nothing fetches it here.
  const description = "Raise the daily borrow limit so the devnet acceptance matrix can exercise governance end to end.";
  const bytes = Buffer.from(description, "utf8");
  const nonce = BigInt(Date.now());

  const created = await dusk.write.createParameterProposal({
    market,
    proposer: owner,
    nonce,
    update: dailyBorrowLimitParameterUpdate(Number(process.env.MAX_DAILY_BORROW_BPS ?? "2500")),
    metadata: {
      version: 1,
      title: "Devnet: raise the daily borrow limit",
      descriptionUri: "https://omnipair.fi/proposals/devnet-daily-borrow-limit.md",
      descriptionSha256: Array.from(createHash("sha256").update(bytes).digest()),
      descriptionLen: bytes.length,
    },
    // Sponsor with everything held, so the strict majority is reachable in one
    // step; the floor is only 1% but queuing needs more than half.
    initialSupport: held,
    ylpMint,
    holderYlpAccount: ylpAta,
  });
  console.log(`create  ${await send([created.instruction])}`);
  console.log(`proposal ${created.proposal.toBase58()}`);

  // Sponsoring past the strict majority queues the proposal in the same
  // instruction, so queueing again is refused as ProposalNotCollecting. Read
  // the status rather than assuming which of the two happened.
  const fetched = await dusk.program.account.parameterProposal.fetch(created.proposal);
  const status = Object.keys(fetched.status as object)[0];
  if (status === "collecting") {
    const queued = await dusk.write.queueParameterProposal({ market, proposal: created.proposal });
    console.log(`queue   ${await send([queued.instruction])}`);
  } else {
    console.log(`queue   not needed — create left it "${status}"`);
  }

  const final = await dusk.program.account.parameterProposal.fetch(created.proposal);
  const at = (v: unknown) => new Date(Number(v) * 1000).toISOString();
  console.log(`\nstatus            ${Object.keys(final.status as object)[0]}`);
  console.log(`executable after  ${at(final.executeAfter)}`);
  console.log(`deadline          ${at(final.executionDeadline)}`);
  console.log("\nExecution is permissionless once the timelock elapses; dusk-lifecycle-keeper should take it from here.");
}
main().catch((e) => { console.error(e); process.exit(1); });
