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

/** The lookup table must have been extended in an earlier slot. Includes the
 * initial oracle policy in the same creation instruction. Metadata lengths and
 * setup instructions must fit Solana's packet limit after address compression. */
export function marketCreationV0Transaction(params: {
  payer: PublicKey;
  recentBlockhash: string;
  initializeMarketInstruction: TransactionInstruction;
  /** Idempotent setup, such as creating the team treasury WSOL account. */
  setupInstructions?: readonly TransactionInstruction[];
  lookupTable: AddressLookupTableAccount;
  computeUnitLimit?: number;
}): VersionedTransaction {
  const instructions = [
    ComputeBudgetProgram.setComputeUnitLimit({ units: params.computeUnitLimit ?? 800_000 }),
    ...(params.setupInstructions ?? []),
    params.initializeMarketInstruction,
  ];
  const message = new TransactionMessage({
    payerKey: params.payer,
    recentBlockhash: params.recentBlockhash,
    instructions,
  }).compileToV0Message([params.lookupTable]);
  const tx = new VersionedTransaction(message);
  try {
    if (tx.serialize().length > 1232) throw new RangeError("packet size");
  } catch (error) {
    if (!(error instanceof RangeError)) throw error;
    throw new Error("market creation exceeds transaction size; shorten LP metadata or add lookup-table addresses");
  }
  return tx;
}

/** Prepare addresses for a maximum-size parameter proposal. Submit the
 * create/extend instructions and wait one slot before using the table. */
export function parameterProposalLookupTablePlan(params: {
  payer: PublicKey;
  recentSlot: number;
  proposalInstruction: TransactionInstruction;
}) {
  return marketCreationLookupTablePlan({payer: params.payer, recentSlot: params.recentSlot,
    initializeMarketInstruction: params.proposalInstruction});
}

/** All eight parameter families plus maximum metadata need address lookup
 * tables. The wire-size check includes signatures and compute instructions. */
export function parameterProposalV0Transaction(params: {
  payer: PublicKey;
  recentBlockhash: string;
  proposalInstruction: TransactionInstruction;
  lookupTables: readonly AddressLookupTableAccount[];
}): VersionedTransaction {
  const message = new TransactionMessage({payerKey: params.payer, recentBlockhash: params.recentBlockhash,
    instructions: [ComputeBudgetProgram.setComputeUnitLimit({units:1_400_000}), params.proposalInstruction],
  }).compileToV0Message([...params.lookupTables]);
  const tx = new VersionedTransaction(message);
  if (tx.serialize().length > 1232) throw new Error("proposal exceeds transaction size; add lookup-table addresses");
  return tx;
}
