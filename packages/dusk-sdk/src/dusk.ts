import type { AnchorProvider, Program } from "@coral-xyz/anchor";
import type { Connection } from "@solana/web3.js";

import { type AddressLike } from "./address.js";
import { DuskGet } from "./get.js";
import { DuskIndexerClient, type FetchLike } from "./indexer.js";
import { createDuskProgram } from "./program.js";
import type { Dusk as DuskIdl } from "./types_v2.js";
import { DuskWrite } from "./write.js";
import {
  createLeverageDelegateProgram,
  DuskLeverageOrders,
} from "./delegate.js";

export interface DuskOptions {
  program?: Program<DuskIdl>;
  provider?: AnchorProvider;
  connection?: Connection;
  programId?: AddressLike;
  feePayer?: AddressLike;
  indexerBaseUrl?: string;
  fetch?: FetchLike;
  leverageDelegateProgramId?: AddressLike;
}

export class Dusk {
  readonly program: Program<DuskIdl>;
  readonly get: DuskGet;
  readonly write: DuskWrite;
  readonly fetch: DuskIndexerClient;
  /** Conditional orders, which live in the delegate program. */
  readonly orders: DuskLeverageOrders;

  constructor(options: DuskOptions = {}) {
    this.program =
      options.program ??
      createDuskProgram({
        provider: options.provider,
        connection: options.connection,
        programId: options.programId,
      });

    this.get = new DuskGet(this.program, options.feePayer);
    this.write = new DuskWrite(this.program);
    this.fetch = new DuskIndexerClient({
      baseUrl: options.indexerBaseUrl,
      fetch: options.fetch,
    });
    this.orders = new DuskLeverageOrders(
      createLeverageDelegateProgram({
        provider: this.program.provider as AnchorProvider,
        programId: options.leverageDelegateProgramId,
      })
    );
  }
}
