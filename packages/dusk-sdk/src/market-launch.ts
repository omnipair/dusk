import {
  AddressLookupTableAccount,
  AddressLookupTableProgram,
  ComputeBudgetProgram,
  PublicKey,
  TransactionMessage,
  VersionedTransaction,
  type TransactionInstruction,
} from "@solana/web3.js";

/** Prepare a lookup table for Dusk's atomic market and three-metadata CPI. */
export function marketCreationLookupTablePlan(params: {
  payer: PublicKey;
  recentSlot: number;
  initializeMarketInstruction: TransactionInstruction;
}) {
  const [createInstruction, address] = AddressLookupTableProgram.createLookupTable({
    authority: params.payer,
    payer: params.payer,
    recentSlot: params.recentSlot,
  });
  const seen = new Set<string>();
  const addresses = [
    ...params.initializeMarketInstruction.keys
      .filter((key) => !key.isSigner)
      .map((key) => key.pubkey),
    params.initializeMarketInstruction.programId,
  ].filter((key) => {
    const encoded = key.toBase58();
    if (seen.has(encoded)) return false;
    seen.add(encoded);
    return true;
  });
  const extendInstructions = [];
  for (let offset = 0; offset < addresses.length; offset += 20) {
    extendInstructions.push(AddressLookupTableProgram.extendLookupTable({
      lookupTable: address,
      authority: params.payer,
      payer: params.payer,
      addresses: addresses.slice(offset, offset + 20),
    }));
  }
  return { address, createInstruction, extendInstructions, addresses };
}

/** The lookup table must have been extended in an earlier slot. */
export function marketCreationV0Transaction(params: {
  payer: PublicKey;
  recentBlockhash: string;
  initializeMarketInstruction: TransactionInstruction;
  lookupTable: AddressLookupTableAccount;
  computeUnitLimit?: number;
}): VersionedTransaction {
  const instructions = [
    ComputeBudgetProgram.setComputeUnitLimit({ units: params.computeUnitLimit ?? 800_000 }),
    params.initializeMarketInstruction,
  ];
  const message = new TransactionMessage({
    payerKey: params.payer,
    recentBlockhash: params.recentBlockhash,
    instructions,
  }).compileToV0Message([params.lookupTable]);
  return new VersionedTransaction(message);
}
