import { Connection, Keypair, Transaction, type TransactionInstruction } from "@solana/web3.js";
import {
  marketCreationLookupTablePlan,
  marketCreationV0Transaction,
} from "../../packages/dusk-sdk/src/market-launch.ts";

/** Submit the three-metadata market initializer through a fresh v0 lookup table. */
export async function sendAtomicMarketCreation(params: {
  connection: Connection;
  payer: Keypair;
  instruction: TransactionInstruction;
}): Promise<{ signature: string; lookupTable: string }> {
  const { connection, payer, instruction } = params;
  const plan = marketCreationLookupTablePlan({
    payer: payer.publicKey,
    recentSlot: await connection.getSlot("confirmed"),
    initializeMarketInstruction: instruction,
  });
  const sendPreparation = async (setup: TransactionInstruction) => {
    const blockhash = await connection.getLatestBlockhash("confirmed");
    const tx = new Transaction({ feePayer: payer.publicKey, recentBlockhash: blockhash.blockhash }).add(setup);
    tx.sign(payer);
    const signature = await connection.sendRawTransaction(tx.serialize(), { preflightCommitment: "confirmed" });
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
