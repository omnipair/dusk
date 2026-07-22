# @omnipair/dusk-sdk

TypeScript SDK for Omnipair Dusk (v2).

A `Dusk` instance is an enriched Anchor program facade. It exposes the raw
Anchor program through `dusk.program`, alongside typed on-chain reads and
previews through `dusk.get`, transaction builders through `dusk.write`, and
indexed historical data through `dusk.fetch`.

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

### Referral Interest Sharing

Futarchy first lists a referrer and configures its share of realized protocol
interest revenue:

```typescript
const configureTx = await dusk.write.configureReferralTransaction({
  authoritySigner: futarchySigner.publicKey,
  referrer,
  interestShareBps: 2_500,
  active: true,
});
```

The listed referrer may then designate the wallet that receives claims:

```typescript
const profileTx = await dusk.write.setReferralRecipientTransaction({
  authority: referrer.publicKey,
  recipient,
});
```

The referred-action builders derive the profile and its per-market, per-mint
accrual account, initialize the accrual idempotently, and compose setup with the
debt-opening instruction:

```typescript
const { transaction, referralProfile, referralAccrual } =
  await dusk.write.referredBorrow(
    {
      borrowAmount,
      minDebtAmountOut,
      minLiquidationCfBps,
    },
    {
      payer: borrower,
      referrer,
      market,
      debtMint,
      accounts: borrowAccounts,
    }
  );
```

`referredOpenLeverage(...)` provides the equivalent leverage-opening flow.
Existing borrow debt sides and leverage positions retain their bound profile on
later debt increases. The program snapshots the profile share, capped by the
current runtime maximum, when the binding is created. Deactivation or later
rate/cap updates affect new bindings only. Referral does not change requested
principal, position debt, interest, health, or liquidation terms.

When interest is realized, the profile accrues a governed share of the DAO's
interest revenue. Claims always pay a token account owned by the profile's
current recipient, and the SDK resolves Token-2022 transfer-hook accounts:

```typescript
const claimTx = await dusk.write.claimReferralInterestTransaction({
  authority: referrer,
  market,
  mint: debtMint,
  recipientTokenAccount,
});
```

`referralBindingInterestShareBps(...)` computes the admission-time capped share.
Pass that stored share to `quoteReferralInterestShare(...)` to mirror the
on-chain floor rounding for realized interest.

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
- `previewBorrowCapacity({ market, collateralAssetMint, debtAssetMint, collateralAmount, projectedBorrowAmount })`.
- `previewBorrowPosition({ market, borrowPosition })`.

`previewBorrowCapacity` exposes both the health-limited result of the on-chain
binary search and the final limit after cash and daily-borrow constraints:

```typescript
const capacity = await dusk.get.previewBorrowCapacity({
  market,
  collateralAssetMint: baseMint,
  debtAssetMint: quoteMint,
  collateralAmount,
  // Optional: quote CF and health terms for this requested principal.
  projectedBorrowAmount,
});

capacity.maxDebtByHealth;
capacity.maxDebtByCash;
capacity.maxDebtByDailyLimit;
capacity.maxDebt;
capacity.maxBorrowAmount;
capacity.maxCfBps;
capacity.liquidationCfBps;
capacity.projectedGlobalHealthContribution;
capacity.projectedGlobalMarketHealthBps;
capacity.projectedEffectiveExistingDebtNad;
```

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
  type DuskIdl,
} from "@omnipair/dusk-sdk";

const program = createDuskProgram({ provider });
```

`DUSK_PROGRAM_ID` is exported for integrations that prefer an explicit program
name over the generic `PROGRAM_ID` constant.

## ESM Compatibility

This package ships strict ESM-compatible output. Relative module specifiers
include `.js` extensions in emitted files.

## License

MIT
