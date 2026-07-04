# @omnipair/dusk-sdk

TypeScript SDK for Omnipair Dusk.

The package exports the generated Anchor IDL/types, PDA helpers, typed preview
decoders, a small write/read facade over the Dusk program, and an indexer client
for historical API data.

## Install

```bash
npm install @omnipair/dusk-sdk
# or
yarn add @omnipair/dusk-sdk
```

## Dusk Client

```typescript
import { AnchorProvider } from "@coral-xyz/anchor";
import { Connection } from "@solana/web3.js";
import { Dusk } from "@omnipair/dusk-sdk";

const connection = new Connection("https://api.mainnet-beta.solana.com", "confirmed");
const provider = new AnchorProvider(connection, wallet, { commitment: "confirmed" });

const dusk = new Dusk({
  provider,
  indexerBaseUrl: "https://api.indexer.omnipair.fi/api/v1",
});
```

The client is intentionally split by source of truth:

- `dusk.write`: Anchor instruction, transaction, and RPC builders.
- `dusk.get`: PDA helpers, direct RPC account fetches, and typed simulation previews.
- `dusk.fetch`: historical/indexed HTTP API methods.

## Write Instructions

```typescript
const ix = await dusk.write.instruction(
  "swap",
  {
    exactAssetIn: amountIn,
    minAssetOut: minAmountOut,
  },
  {
    accounts: {
      market,
      futarchyAuthority,
      trader,
      assetInMint,
      assetOutMint,
      reserveInVault,
      reserveOutVault,
      feeInVault,
      traderAssetInAccount,
      traderAssetOutAccount,
      tokenProgram,
      token2022Program,
      eventAuthority,
      program: dusk.program.programId,
    },
  }
);
```

`write.builder(...)`, `write.transaction(...)`, and `write.rpc(...)` expose the
same generic path for every Dusk instruction in the IDL.

## Get On-Chain State

```typescript
const [market] = dusk.get.pda.market(baseMint, quoteMint, paramsHash);
const account = await dusk.get.market(market);

const swap = await dusk.get.previewSwap({
  market,
  assetInMint: baseMint,
  assetOutMint: quoteMint,
  exactAssetIn: amountIn,
});
```

Preview methods use Solana `simulateTransaction` and decode typed Anchor return
data. They replace the old log-parsing getter workaround.

Available typed previews:

- `previewMarket(market)`.
- `previewSwap({ market, assetInMint, assetOutMint, exactAssetIn })`.
- `previewBorrowCapacity({ market, collateralAssetMint, debtAssetMint, collateralAmount, projectedDebtAmount })`.
- `previewBorrowPosition({ market, borrowPosition })`.

## Fetch Historical Data

```typescript
const pools = await dusk.fetch.pools({ limit: 50, sortBy: "tvl", sortOrder: "desc" });
const activity = await dusk.fetch.poolActivity(market, {
  categories: ["swaps", "liquidity", "lending"],
  limit: 100,
});
const snapshots = await dusk.fetch.userPortfolioSnapshots(owner, "30D");
```

The indexer client wraps the Omnipair `/api/v1` routes for pools, stats, users,
positions, GeckoTerminal, CoinGecko, and CMC-compatible data. Use
`dusk.fetch.request(path, options)` for new or unwrapped endpoints.

## Raw Program Exports

```typescript
import {
  createDuskProgram,
  deriveMarketAddress,
  IDL,
  PROGRAM_ID,
  type OmnipairV2,
} from "@omnipair/dusk-sdk";

const program = createDuskProgram({ provider });
```

`IDL_V2`, `OMNIPAIR_V2_PROGRAM_ID`, and `DUSK_PROGRAM_ID` are exported as
explicit aliases for integrations that prefer generation-qualified names.

## ESM Compatibility

This package ships strict ESM-compatible output. Relative module specifiers
include `.js` extensions in emitted files.

## License

MIT
