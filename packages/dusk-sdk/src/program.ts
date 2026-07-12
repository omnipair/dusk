import { AnchorProvider, Program } from "@coral-xyz/anchor";
import type { Connection, Transaction, VersionedTransaction } from "@solana/web3.js";

import { address, DEFAULT_READONLY_PUBLIC_KEY, type AddressLike } from "./address.js";
import IDL from "./idl_v2.js";
import type { Dusk } from "./types_v2.js";
import { PROGRAM_ID } from "./constants.js";

export type DuskProgram = Program<Dusk>;

export interface ReadonlyWallet {
  publicKey: ReturnType<typeof address>;
  signTransaction<T extends Transaction | VersionedTransaction>(transaction: T): Promise<T>;
  signAllTransactions<T extends Transaction | VersionedTransaction>(transactions: T[]): Promise<T[]>;
}

export interface DuskProgramOptions {
  provider?: AnchorProvider;
  connection?: Connection;
  programId?: AddressLike;
}

export function duskIdl(programId: AddressLike = PROGRAM_ID): Dusk {
  const idl = JSON.parse(JSON.stringify(IDL)) as Dusk;
  idl.address = address(programId).toBase58() as Dusk["address"];
  return idl;
}

export function createReadonlyProvider(connection: Connection): AnchorProvider {
  const wallet: ReadonlyWallet = {
    publicKey: DEFAULT_READONLY_PUBLIC_KEY,
    async signTransaction<T extends Transaction | VersionedTransaction>(transaction: T): Promise<T> {
      return transaction;
    },
    async signAllTransactions<T extends Transaction | VersionedTransaction>(
      transactions: T[]
    ): Promise<T[]> {
      return transactions;
    },
  };

  return new AnchorProvider(connection, wallet, { commitment: "confirmed" });
}

export function createDuskProgram(options: DuskProgramOptions): DuskProgram {
  const provider =
    options.provider ?? (options.connection ? createReadonlyProvider(options.connection) : undefined);

  if (!provider) {
    throw new Error("Dusk SDK requires either an Anchor provider or a Solana connection.");
  }

  return new Program<Dusk>(duskIdl(options.programId), provider);
}
