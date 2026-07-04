import type { AnchorProvider, Program } from "@coral-xyz/anchor";
import type { Connection } from "@solana/web3.js";

import { type AddressLike } from "./address.js";
import { DuskGet } from "./get.js";
import { DuskIndexerClient, type FetchLike } from "./indexer.js";
import { createDuskProgram } from "./program.js";
import type { OmnipairV2 } from "./types_v2.js";
import { DuskWrite } from "./write.js";

export interface DuskOptions {
  program?: Program<OmnipairV2>;
  provider?: AnchorProvider;
  connection?: Connection;
  programId?: AddressLike;
  feePayer?: AddressLike;
  indexerBaseUrl?: string;
  fetch?: FetchLike;
}

export class Dusk {
  readonly program: Program<OmnipairV2>;
  readonly get: DuskGet;
  readonly write: DuskWrite;
  readonly fetch: DuskIndexerClient;

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
  }
}
