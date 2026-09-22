import type { Program } from "@coral-xyz/anchor";
import {
  ComputeBudgetProgram,
  PublicKey,
  Transaction,
  VersionedTransaction,
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
  /** Reject a bank older than this observed slot. */
  minContextSlot?: number;
  /** Post-simulation accounts, in exactly this order and from the same bank. */
  accounts?: readonly AddressLike[];
  signal?: AbortSignal;
  /** Bounds the caller's wait; the transport may finish an in-flight RPC later. */
  timeoutMs?: number;
}

/** Keeps the actual RPC slot, logs and account snapshots beside return data. */
export type DuskPreviewSimulation = RpcResponseAndContext<SimulatedTransactionResponse> & {
  /** Request start, so slow simulation never makes old data appear fresh. */
  observedAt: number;
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
  const transaction = new Transaction().add(
    ComputeBudgetProgram.requestHeapFrame({ bytes: options.heapFrameBytes ?? 256 * 1024 }),
    ComputeBudgetProgram.setComputeUnitLimit({ units: options.computeUnitLimit ?? 1_400_000 }),
    ...instructions
  );
  transaction.feePayer = address(
    options.feePayer ?? program.provider.publicKey ?? DEFAULT_READONLY_PUBLIC_KEY
  );
  transaction.recentBlockhash = PublicKey.default.toBase58();
  const wire = new VersionedTransaction(transaction.compileMessage());
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
        ...(options.minContextSlot === undefined ? {} : { minContextSlot: options.minContextSlot }),
        ...(accounts === undefined
          ? {}
          : { accounts: { encoding: "base64", addresses: accounts } }),
      }),
      deadline,
    ]);
    options.signal?.throwIfAborted();
    if (simulation.value.err) throw new DuskSimulationError("Dusk simulation failed", simulation);
    if (!simulation.value.returnData)
      throw new DuskSimulationError("Dusk simulation did not return data", simulation);
    if (simulation.value.returnData.programId !== program.programId.toBase58())
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
    return { ...simulation, observedAt };
  } finally {
    clearTimeout(timer);
    options.signal?.removeEventListener("abort", cancel);
  }
}
