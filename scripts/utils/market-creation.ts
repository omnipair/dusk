import { Connection, Keypair, Transaction, type TransactionInstruction } from "@solana/web3.js";
import {
  marketCreationLookupTablePlan,
  marketCreationV0Transaction,
} from "../../packages/dusk-sdk/src/market-launch.ts";
import { oraclePolicy } from "../../packages/dusk-sdk/dist/oracle.js";

/** Optional JSON uses the SDK's OraclePolicyInput field names and feed IDs. */
export function marketCreationOracleFromEnv() {
  const value = process.env.DUSK_ORACLE_POLICY_JSON;
  return value ? oraclePolicy(JSON.parse(value)) : null;
}

/** Submit the three-metadata market initializer through a fresh v0 lookup table. */
export async function sendAtomicMarketCreation(params: {
  connection: Connection;
  payer: Keypair;
  instruction: TransactionInstruction;
}): Promise<{ signature: string; lookupTable: string }> {
  const { connection, payer, instruction } = params;
  // Public RPC endpoints can load-balance consecutive requests across nodes at
  // different confirmed banks. A confirmed slot from a leading node may not
  // exist in the SlotHashes sysvar of the node that simulates the create-table
  // transaction. Finalized context is stable across those nodes, and
  // minContextSlot prevents the write from being simulated behind it.
  const lookupContext = await connection.getLatestBlockhashAndContext("finalized");
  const plan = marketCreationLookupTablePlan({
    payer: payer.publicKey,
    recentSlot: lookupContext.context.slot,
    initializeMarketInstruction: instruction,
  });
  const sendPreparation = async (setup: TransactionInstruction) => {
    const blockhash = await connection.getLatestBlockhash("confirmed");
    const tx = new Transaction({ feePayer: payer.publicKey, recentBlockhash: blockhash.blockhash }).add(setup);
    tx.sign(payer);
    const signature = await connection.sendRawTransaction(tx.serialize(), {
      preflightCommitment: "confirmed",
      minContextSlot: lookupContext.context.slot,
      maxRetries: 10,
    });
    const confirmation = await connection.confirmTransaction({ ...blockhash, signature }, "confirmed");
    if (confirmation.value.err) throw new Error(`lookup table setup failed: ${JSON.stringify(confirmation.value.err)}`);
  };
  await sendPreparation(plan.createInstruction);
  for (const extend of plan.extendInstructions) await sendPreparation(extend);

  // Newly added addresses cannot be looked up until the following slot.
  const extensionSlot = await connection.getSlot("confirmed");
  const deadline = Date.now() + 45_000;
  while (await connection.getSlot("confirmed") <= extensionSlot) {
    if (Date.now() > deadline) throw new Error("lookup table did not activate within 45 seconds");
    await new Promise((resolve) => setTimeout(resolve, 400));
  }
  const lookupTable = (await connection.getAddressLookupTable(plan.address, { commitment: "confirmed" })).value;
  if (!lookupTable || lookupTable.state.addresses.length !== plan.addresses.length) {
    throw new Error("market creation lookup table is incomplete");
  }
  const blockhash = await connection.getLatestBlockhash("confirmed");
  const tx = marketCreationV0Transaction({
    payer: payer.publicKey,
    recentBlockhash: blockhash.blockhash,
    initializeMarketInstruction: instruction,
    lookupTable,
  });
  tx.sign([payer]);
  const signature = await connection.sendRawTransaction(tx.serialize(), { preflightCommitment: "confirmed" });
  const confirmation = await connection.confirmTransaction({ ...blockhash, signature }, "confirmed");
  if (confirmation.value.err) throw new Error(`market creation failed: ${JSON.stringify(confirmation.value.err)}`);
  return { signature, lookupTable: plan.address.toBase58() };
}
