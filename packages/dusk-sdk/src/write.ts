import type { Program } from "@coral-xyz/anchor";
import type { AccountMeta, Transaction, TransactionInstruction } from "@solana/web3.js";

import { normalizeAccountKeys } from "./address.js";
import type { OmnipairV2 } from "./types_v2.js";

export type DuskInstructionName = OmnipairV2["instructions"][number]["name"];
export type DuskInstructionArgs = unknown[] | unknown | undefined;
export type DuskAccounts = Record<string, unknown>;

export interface DuskBuildOptions {
  accounts?: DuskAccounts;
  remainingAccounts?: AccountMeta[];
}

type AnchorMethodBuilder = {
  accounts(accounts: DuskAccounts): AnchorMethodBuilder;
  remainingAccounts(accounts: AccountMeta[]): AnchorMethodBuilder;
  instruction(): Promise<TransactionInstruction>;
  transaction(): Promise<Transaction>;
  rpc(): Promise<string>;
};

type AnchorMethods = Record<string, (...args: unknown[]) => AnchorMethodBuilder>;

export class DuskWrite {
  constructor(readonly program: Program<OmnipairV2>) {}

  method(name: DuskInstructionName, args?: DuskInstructionArgs): AnchorMethodBuilder {
    const method = (this.program.methods as unknown as AnchorMethods)[name];
    if (!method) {
      throw new Error(`Unknown Dusk instruction: ${name}`);
    }
    return method(...normalizeArgs(args));
  }

  builder(
    name: DuskInstructionName,
    args?: DuskInstructionArgs,
    options: DuskBuildOptions = {}
  ): AnchorMethodBuilder {
    let builder = this.method(name, args);
    if (options.accounts) {
      builder = builder.accounts(normalizeAccountKeys(options.accounts));
    }
    if (options.remainingAccounts?.length) {
      builder = builder.remainingAccounts(options.remainingAccounts);
    }
    return builder;
  }

  instruction(
    name: DuskInstructionName,
    args?: DuskInstructionArgs,
    options?: DuskBuildOptions
  ): Promise<TransactionInstruction> {
    return this.builder(name, args, options).instruction();
  }

  transaction(
    name: DuskInstructionName,
    args?: DuskInstructionArgs,
    options?: DuskBuildOptions
  ): Promise<Transaction> {
    return this.builder(name, args, options).transaction();
  }

  rpc(name: DuskInstructionName, args?: DuskInstructionArgs, options?: DuskBuildOptions) {
    return this.builder(name, args, options).rpc();
  }
}

function normalizeArgs(args: DuskInstructionArgs): unknown[] {
  if (args === undefined) {
    return [];
  }
  return Array.isArray(args) ? args : [args];
}
