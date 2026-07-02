# @omnipair/program-interface

TypeScript interface for the Omnipair Dusk program.

## Install

```bash
npm install @omnipair/program-interface
# or
yarn add @omnipair/program-interface
```

## Program

```typescript
import * as anchor from "@coral-xyz/anchor";
import type { OmnipairV2 } from "@omnipair/program-interface";
import { IDL, PROGRAM_ID } from "@omnipair/program-interface";

const connection = new anchor.web3.Connection(
  process.env.ANCHOR_PROVIDER_URL ?? "https://api.mainnet-beta.solana.com",
  "confirmed"
);
const wallet = new anchor.Wallet(anchor.web3.Keypair.generate());
const provider = new anchor.AnchorProvider(connection, wallet, {
  commitment: "confirmed",
});

const program = new anchor.Program<OmnipairV2>(IDL, PROGRAM_ID, provider);
```

`IDL_V2` and `OMNIPAIR_V2_PROGRAM_ID` are exported as explicit aliases for
integrations that prefer generation-qualified names.

## Market PDA

Dusk markets are standalone accounts. Pass the same `paramsHash` supplied to
the on-chain `initialize` instruction.

```typescript
import { PublicKey } from "@solana/web3.js";
import { deriveMarketAddress } from "@omnipair/program-interface";

const baseMint = new PublicKey("So11111111111111111111111111111111111111112");
const quoteMint = new PublicKey("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
const paramsHash = new Uint8Array(32);

const [marketPda, marketBump] = deriveMarketAddress(baseMint, quoteMint, paramsHash);
console.log("market:", marketPda.toBase58(), "bump:", marketBump);

const market = await program.account.market.fetch(marketPda);
console.log("base mint:", market.baseMint.toBase58());
console.log("quote mint:", market.quoteMint.toBase58());
```

## Exports

### IDL

- `IDL`: Dusk program IDL.
- `IDL_V2`: alias for `IDL`.

### Types

- `OmnipairV2`: generated Anchor program type.
- `Market`, `BorrowPosition`, `YieldAccount`, `FutarchyAuthority`.
- Dusk event aliases including `MarketCreated`, `LiquidityAdded`,
  `LiquidityRemoved`, `HlpOpened`, `HlpClosed`, `HlpRebalanced`,
  `SwapExecuted`, `YieldClaimed`, and liquidation/protocol auction events.

### Constants

- `PROGRAM_ID`: Dusk program ID.
- `OMNIPAIR_V2_PROGRAM_ID`: alias for `PROGRAM_ID`.
- `DUSK_PROGRAM_ID`: alias for `PROGRAM_ID`.
- `TOKEN_METADATA_PROGRAM_ID`.
- `SEEDS`.

### Utilities

- `deriveMarketAddress(baseMint, quoteMint, paramsHash)`.
- `deriveMarketV2Address(baseMint, quoteMint, paramsHash)`.
- `deriveFutarchyAuthorityAddress()`.
- `deriveMarketReserveVaultAddress(market, reserveMint)`.
- `deriveMarketCollateralVaultAddress(market, collateralMint)`.
- `deriveMarketFeeVaultAddress(market, feeMint)`.
- `deriveMarketInterestVaultAddress(market, interestMint)`.
- `deriveBorrowPositionAddress(market, positionId)`.
- `deriveLeveragePositionAddress(market, positionId)`.
- `deriveYieldAccountAddress(market, owner, assetMint, tokenKind)`.
- `deriveInsuranceAddress(market, assetMint)`.
- Token-2022 transfer-hook validation and extra-account-meta helpers.

## ESM Compatibility

This package ships strict ESM-compatible output. Relative module specifiers
include `.js` extensions in emitted files.

## License

MIT
