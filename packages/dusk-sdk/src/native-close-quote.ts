/// <reference lib="dom" />
/** Local native-close pricing from the program's shared Rust transitions. */

interface QuoteExports {
  memory: WebAssembly.Memory;
  quote_alloc(length: number): number;
  quote_free(pointer: number, length: number): void;
  quote_prepare(
    marketPointer: number, marketLength: number,
    positionPointer: number, positionLength: number,
    slot: bigint, timestamp: bigint
  ): number;
  quote_debt_amount(): bigint;
  quote_amount_out(collateralIn: bigint): number;
  quote_last_amount_out(): bigint;
}

let compiledQuote: Promise<WebAssembly.Module> | undefined;

async function quoteModule(): Promise<WebAssembly.Module> {
  compiledQuote ??= (async () => {
    // This resolves to dist from both the packaged JS and the source-based
    // LiteSVM harness, which runs TypeScript directly.
    const url = new URL("../dist/native-close-quote.wasm", import.meta.url);
    const bytes = url.protocol === "file:"
      ? await (await import("node:fs/promises")).readFile(url)
      : await (async () => {
          const response = await fetch(url);
          if (!response.ok) throw new Error(`Could not load native-close quote: ${response.status}`);
          return response.arrayBuffer();
        })();
    return WebAssembly.compile(bytes);
  })().catch((error) => {
    compiledQuote = undefined;
    throw error;
  });
  return compiledQuote;
}

/** A new instance keeps concurrent quotes from overwriting each other's prepared market. */
export async function createNativeCloseQuote(
  marketData: Uint8Array,
  positionData: Uint8Array,
  slot: bigint,
  timestamp: bigint
): Promise<{ debtAmount: bigint; amountOut(collateralIn: bigint): bigint | "liquidity-limited" | "insufficient" }> {
  const module = await quoteModule();
  const imports: WebAssembly.Imports = {};
  for (const dependency of WebAssembly.Module.imports(module)) {
    const group = (imports[dependency.module] ??= {});
    group[dependency.name] = () => {
      throw new Error(`Unexpected native-close quote import: ${dependency.module}.${dependency.name}`);
    };
  }
  const exports = new WebAssembly.Instance(module, imports).exports as unknown as QuoteExports;
  const marketPointer = exports.quote_alloc(marketData.length);
  const positionPointer = exports.quote_alloc(positionData.length);
  try {
    new Uint8Array(exports.memory.buffer).set(marketData, marketPointer);
    new Uint8Array(exports.memory.buffer).set(positionData, positionPointer);
    if (exports.quote_prepare(
      marketPointer, marketData.length,
      positionPointer, positionData.length,
      slot, timestamp
    ) !== 0) throw new Error("Native-close market or position cannot be quoted at this slot");
  } finally {
    exports.quote_free(marketPointer, marketData.length);
    exports.quote_free(positionPointer, positionData.length);
  }
  return {
    // WebAssembly exposes i64 results as signed BigInts, including Rust u64s.
    debtAmount: BigInt.asUintN(64, exports.quote_debt_amount()),
    amountOut(collateralIn) {
      const status = exports.quote_amount_out(collateralIn);
      if (status === 1) return "liquidity-limited";
      if (status === 2) return "insufficient";
      if (status !== 0) throw new Error("Native-close quote failed for this collateral amount");
      return BigInt.asUintN(64, exports.quote_last_amount_out());
    },
  };
}
