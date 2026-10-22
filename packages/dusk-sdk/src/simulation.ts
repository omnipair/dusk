import type { Program } from "@coral-xyz/anchor";
import {
  ComputeBudgetProgram,
  PublicKey,
  Transaction,
  VersionedTransaction,
  TransactionMessage,
  type AddressLookupTableAccount,
  type Commitment,
  type TransactionInstruction,
  type RpcResponseAndContext,
  type SimulatedTransactionResponse,
} from "@solana/web3.js";
import { address, DEFAULT_READONLY_PUBLIC_KEY, type AddressLike } from "./address.js";
import type { Dusk } from "./types_v2.js";

export interface SimulateOptions {
  feePayer?: AddressLike;
  commitment?: Commitment;
  /** Defaults to 1,400,000 units to accommodate heavier native previews. */
  computeUnitLimit?: number;
  /** Defaults to 256 KiB for native preview intermediate state. */
  heapFrameBytes?: number;
}

export interface PreviewSimulationOptions extends SimulateOptions {
  /** Require a complete budget prefix when other programs bind instruction
   * indices. Dusk flash-liquidation plans are detected automatically. */
  preserveInstructionIndices?: boolean;
  /** Optional active lookup tables for larger position-action transactions. */
  lookupTables?: AddressLookupTableAccount[];
  /** Include CPI events when previewing an actual state-changing instruction. */
  innerInstructions?: boolean;
  /** Reject a bank older than this observed slot. */
  minContextSlot?: number;
  /** Post-simulation accounts, in exactly this order and from the same bank. */
  accounts?: readonly AddressLike[];
  signal?: AbortSignal;
  /** Bounds the caller's wait; the transport may finish an in-flight RPC later. */
  timeoutMs?: number;
  /** Set false when simulating an instruction that succeeds without return data. */
  requireReturnData?: boolean;
}

/** Keeps the actual RPC slot, logs and account snapshots beside return data. */
export type DuskPreviewSimulation = RpcResponseAndContext<SimulatedTransactionResponse> & {
  /** Request start, so slow simulation never makes old data appear fresh. */
  observedAt: number;
  accountKeys: readonly PublicKey[];
};

export class DuskSimulationError extends Error {
  constructor(
    message: string,
    readonly simulation: RpcResponseAndContext<SimulatedTransactionResponse>
  ) {
    super(message);
    this.name = "DuskSimulationError";
  }
}

export class DuskPreviewTimeoutError extends Error {
  constructor() {
    super("Dusk preview timed out");
    this.name = "DuskPreviewTimeoutError";
  }
}

export async function simulatePreviewWithContext(
  program: Program<Dusk>,
  instructions: readonly TransactionInstruction[],
  options: PreviewSimulationOptions = {}
): Promise<DuskPreviewSimulation> {
  options.signal?.throwIfAborted();
  const timeoutMs = options.timeoutMs ?? 10_000;
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs <= 0)
    throw new Error("Invalid preview timeout");
  if (
    options.minContextSlot !== undefined &&
    (!Number.isSafeInteger(options.minContextSlot) || options.minContextSlot < 0)
  )
    throw new Error("Invalid preview minimum slot");
  if (!instructions.length) throw new Error("A preview requires an instruction");
  // A complete flash plan already has its budgets and absolute settle index.
  // Ordinary previews still need a budget prefix: placing budgets after the
  // preview overwrites Solana's final return-data program ID.
  const simulationInstructions = [...instructions];
  const missingBudgets: TransactionInstruction[] = [];
  const flashDiscriminators = program.idl?.instructions.filter(ix =>
    ix.name === "beginFlashLiquidation" || ix.name === "beginHlpFlash").map(ix => Buffer.from(ix.discriminator)) ?? [];
  const bindsIndices = options.preserveInstructionIndices || instructions.some(ix =>
    ix.programId.equals(program.programId) && flashDiscriminators.some(tag => ix.data.subarray(0, 8).equals(tag)));
  for (const [tag, explicit, budget] of [
    [1, options.heapFrameBytes, ComputeBudgetProgram.requestHeapFrame({ bytes: options.heapFrameBytes ?? 256 * 1024 })],
    [2, options.computeUnitLimit, ComputeBudgetProgram.setComputeUnitLimit({ units: options.computeUnitLimit ?? 1_400_000 })],
  ] as const) {
    const matches = simulationInstructions.flatMap((ix, index) =>
      ix.programId.equals(ComputeBudgetProgram.programId) && ix.data[0] === tag ? [index] : []);
    if (matches.length > 1) throw new Error("Duplicate compute-budget instruction in preview plan");
    if (!matches.length) missingBudgets.push(budget);
    else if (explicit !== undefined) simulationInstructions[matches[0]!] = budget;
  }
  if (bindsIndices && missingBudgets.length)
    throw new Error("Index-bound simulation requires planned heap and compute budgets; use buildFlash or include them before binding indices");
  const transaction = new Transaction().add(...missingBudgets, ...simulationInstructions);
  transaction.feePayer = address(
    options.feePayer ?? program.provider.publicKey ?? DEFAULT_READONLY_PUBLIC_KEY
  );
  transaction.recentBlockhash = PublicKey.default.toBase58();
  const message = options.lookupTables?.length
    ? new TransactionMessage({ payerKey: transaction.feePayer, recentBlockhash: transaction.recentBlockhash,
        instructions: transaction.instructions }).compileToV0Message(options.lookupTables)
    : transaction.compileMessage();
  const wire = new VersionedTransaction(message);
  const keys = message.version === "legacy" ? message.getAccountKeys()
    : message.getAccountKeys({ addressLookupTableAccounts: options.lookupTables ?? [] });
  const accountKeys = Array.from({ length: keys.length }, (_, index) => keys.get(index)!);
  if (wire.serialize().length > 1232)
    throw new Error("Preview exceeds the transaction packet limit");
  const accounts = options.accounts?.map((account) => address(account).toBase58());
  const observedAt = Date.now();
  let timer: ReturnType<typeof setTimeout> | undefined;
  let cancel = () => {};
  const deadline = new Promise<never>((_, reject) => {
    cancel = () =>
      reject(options.signal?.reason ?? new DOMException("Preview cancelled", "AbortError"));
    options.signal?.addEventListener("abort", cancel, { once: true });
    timer = setTimeout(() => reject(new DuskPreviewTimeoutError()), timeoutMs);
  });
  try {
    const connection = program.provider.connection;
    const simulation = await Promise.race([
      connection.simulateTransaction(wire, {
        commitment: options.commitment ?? connection.commitment,
        sigVerify: false,
        replaceRecentBlockhash: true,
        ...(options.innerInstructions ? { innerInstructions: true } : {}),
        ...(options.minContextSlot === undefined ? {} : { minContextSlot: options.minContextSlot }),
        ...(accounts === undefined
          ? {}
          : { accounts: { encoding: "base64", addresses: accounts } }),
      }),
      deadline,
    ]);
    options.signal?.throwIfAborted();
    if (simulation.value.err) throw new DuskSimulationError("Dusk simulation failed", simulation);
    if (options.requireReturnData !== false && !simulation.value.returnData)
      throw new DuskSimulationError("Dusk simulation did not return data", simulation);
    if (options.requireReturnData !== false && simulation.value.returnData && simulation.value.returnData.programId !== program.programId.toBase58())
      throw new DuskSimulationError(
        "Dusk simulation returned data from a different program",
        simulation
      );
    if (
      !Number.isSafeInteger(simulation.context.slot) ||
      simulation.context.slot < (options.minContextSlot ?? 0)
    )
      throw new DuskSimulationError("Dusk simulation returned a stale or invalid slot", simulation);
    if (accounts && simulation.value.accounts?.length !== accounts.length)
      throw new DuskSimulationError("Dusk simulation returned incomplete accounts", simulation);
    return { ...simulation, observedAt, accountKeys };
  } finally {
    clearTimeout(timer);
    options.signal?.removeEventListener("abort", cancel);
  }
}
