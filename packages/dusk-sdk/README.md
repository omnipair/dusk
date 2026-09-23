# @omnipair/dusk-sdk

For volume, fee, and borrower-interest consumers, see the
[accounting event contract](../../programs/dusk/ACCOUNTING_EVENTS.md).
`SwapExecuted` covers spot and leverage AMM executions. `BorrowInterestAccrued`
separates credit, margin, and hLP accrual; `BorrowInterestPaid` reports actual
collections with the same source attribution. Typed events and their
`SwapOrigin`/`DebtSource` discriminants are exported from this package.

TypeScript SDK for Dusk, the Omnipair V2 protocol architecture. This package
targets Dusk market layout v1.

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

const connection = new Connection(process.env.SOLANA_RPC_URL!, "confirmed");
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
const ix = await dusk.write.swapInstruction(
  {
    exactAssetIn: amountIn,
    minAssetOut: minAmountOut,
  },
  {
    market,
    accounts: {
      market,
      futarchyAuthority,
      trader,
      assetInMint,
      assetOutMint,
      reserveInVault,
      reserveOutVault,
      traderAssetInAccount,
      traderAssetOutAccount,
      tokenProgram,
      token2022Program,
    },
    remainingAccounts: [
      // Token-2022 transfer-hook extras only. The SDK preserves this tail.
    ],
  }
);
```

`swapBuilder(...)`, `swapInstruction(...)`, `swapTransaction(...)`, and
`swapRpc(...)` fetch the market before building the swap. Whenever either hLP
side has nonzero supply or residual exposure, they prepend the canonical
five-account prefix exactly once: `[yLP mint, base hLP yLP vault, quote hLP yLP
vault, base interest vault, quote interest vault]`. Caller-provided Token-2022
transfer-hook extras remain after that prefix.

The write client supplies the canonical event-CPI authority and Dusk program
accounts for instructions that emit CPI events.

`write.builder(...)`, `write.transaction(...)`, and `write.rpc(...)` expose the
same generic path for every Dusk instruction in the IDL.

### Direct-yLP Parameter Governance

Market layout v1 has no market manager. The program exposes seven independent
parameter families: Fee, Concentration, IRM, EMA Half-Lives, Daily Borrow
Limit, Center Controller, and Insurance. A direct yLP holder burn-locks at least
1% of eligible direct yLP to create a typed proposal. Strictly more than 50%
support queues it for a 7-day timelock and a 7-day execution window. Execution
is permissionless and succeeds only while both lending sides are below 80%
utilization. These thresholds, the timelock, and the utilization guard are
immutable.

Use the typed update constructors so values are checked against the same hard
bounds before an instruction is built:

```typescript
import {
  concentrationParameterUpdate,
  uploadProposalMetadata,
} from "@omnipair/dusk-sdk";

const update = concentrationParameterUpdate({
  // 20x center depth versus the reserve-matched CPMM.
  peakAmplificationNad: 20n * 1_000_000_000n,
  // Full peak depth inside ±100 bps, then one 400-bps shoulder.
  coreHalfWidthBps: 100,
  fadeWidthBps: 400,
});

const metadata = await uploadProposalMetadata({
  title: "Update the concentration shape",
  markdown: markdownSource, // string or exact UTF-8 Uint8Array
  upload: async (exactBytes, { contentType }) => {
    // Pin to IPFS, upload to Arweave, or use durable HTTPS storage.
    // Do not transform `exactBytes` while uploading.
    return { uri: await uploadGovernanceDocument(exactBytes, contentType) };
  },
});

const { proposal, transaction } = await dusk.write.createParameterProposal({
  proposer: wallet.publicKey,
  market,
  nonce: 7,
  update,
  metadata,
  initialSupport,
  // holderYlpAccount is optional; the Token-2022 ATA is the default.
});
```

`uploadProposalMetadata(...)` uploads the exact Markdown bytes through the
provided storage adapter, retrieves the resulting URI, and verifies the exact
length and SHA-256 before returning `ProposalMetadataV1`. For content that is
already uploaded, use `createProposalMetadata(...)`; it performs the same
retrieval check. Accepted on-chain URI schemes are `ipfs://`, `ar://`, and
`https://`. IPFS content must still be pinned; a CID alone is not a persistence
guarantee.

Governance websites should call `tryFetchProposalDescription(...)`. Render the
Markdown only when `verified` is true, using sanitized GitHub-flavored Markdown
with raw HTML and external embeds disabled. On failure, display the immutable
on-chain title and typed parameter diff plus the returned warning—never an
unverified replacement document. Rationale availability never controls
execution. `verifyDecodedParameterProposalDigest(...)` additionally reproduces
the program's canonical Borsh/SHA-256 digest for a fetched proposal account.

The other typed update constructors currently implemented by the handwritten
SDK are `feeParameterUpdate(...)`, `irmParameterUpdate(...)`,
`emaHalfLivesParameterUpdate(...)`, `dailyBorrowLimitParameterUpdate(...)`, and
`centerControllerParameterUpdate(...)`. Only concentration ramps; its duration
must be 216,000–1,512,000 slots (approximately 24 hours–7 days).

The generated IDL includes the seventh `insuranceDrawCaps` variant, but the
handwritten `ParameterUpdate` union and constructor layer do not yet expose it.
Treat Insurance proposal construction as an SDK release blocker; do not encode
that variant by copying a discriminator or handwritten byte layout into an
application.

Support and lifecycle builders derive the proposal/support PDAs and all market
governance accounts:

```typescript
await dusk.write.supportParameterProposal({
  supporter: wallet.publicKey,
  market,
  proposal,
  amount: additionalSupport,
});

await dusk.write.queueParameterProposal({ market, proposal });
await dusk.write.executeParameterProposal({ market, proposal });
await dusk.write.withdrawParameterSupport({
  supporter: wallet.publicKey,
  market,
  proposal,
});
```

Each lifecycle method returns `{ proposal, proposalSupport?, instruction,
transaction }`. Fetch state with `dusk.get.parameterProposal(proposal)`,
`dusk.get.proposalSupportFor(proposal, supporter)`, or the matching helpers in
`dusk.get.pda`.

Support is burned from the holder's external yLP account and represented by a
proposal-specific virtual claim, so it cannot back multiple proposals. The
claim continues earning yLP yield. Withdrawal destroys that claim, merges its
virtual-yield ledgers, and mints back exactly the locked yLP. Collecting support
can be withdrawn; queued support stays frozen until the proposal executes,
expires, or becomes stale.

hLP deposits and withdrawals use async composite builders because both
asset-denominated `YieldAccount` PDAs must exist before the liquidity
instruction runs. The SDK validates their owner, exact layout size, and Anchor
discriminator. If either account is missing or is only a prefunded System PDA,
it prepends the permissionless, idempotent initializer in the same transaction:

```typescript
const { transaction, setupInstructions, baseYieldAccount, quoteYieldAccount } =
  await dusk.write.depositSingleSided(
    { depositAmount, minHlpAmount },
    {
      payer: owner,
      owner,
      market,
      targetHlpMint,
      baseMint,
      quoteMint,
      accounts: depositAccounts,
    }
  );

// setupInstructions is empty when both canonical accounts are already valid.
await provider.sendAndConfirm(transaction);
```

`withdrawSingleSided(...)` has the same return shape and setup behavior. The
initializer is safe to compose unconditionally, including when a third party
has transferred lamports to the PDA address before initialization.

`harvest` accepts the LP owner, designated yield recipient, or independent
harvest authority for yLP and both hLP mints. Its `owner` account identifies
the LP holder; `caller` signs and must match that owner,
`YieldAccount.recipient`, or the optional `YieldAccount.harvestAuthority`.
An unrelated caller is rejected with `InvalidSigner`.

The LP owner sets or rotates a keeper with `setHarvestAuthority`, passing
`harvestAuthority: keeperPublicKey`, and revokes it with `harvestAuthority: null`.
New yield accounts start with no harvest authority. This changes who may
trigger payment without changing who receives it. Only the owner can update
either setting; changing the recipient does not clear the harvest authority.
Each yield account has its own configuration, so configure each underlying
asset and LP mint that the keeper should service.

```typescript
// The LP owner signs this configuration transaction.
const transaction = await dusk.write.transaction(
  "setHarvestAuthority",
  { tokenKind: { ylp: {} }, harvestAuthority: keeperPublicKey },
  { accounts: { market, owner, assetMint, lpMint, yieldAccount } }
);
await provider.sendAndConfirm(transaction);
// Pass harvestAuthority: null through the same instruction to revoke.
```

For harvesting, set `caller` to the keeper and sign with its key. Neither the
owner nor recipient needs to sign. Derive `recipientAssetAccount` as the
current recipient's ATA using the underlying mint's token program; arbitrary
destinations are rejected. Create that ATA before harvesting if necessary.
The claim event records the LP owner, recipient, and caller separately, with
the caller in `metadata.signer`. `HarvestAuthorityUpdated` records delegation,
rotation, and revocation.

LP token accounts should be owned by a wallet that can sign withdrawals and
permission updates, or by a PDA whose controlling program invokes those Dusk
instructions with `invoke_signed`. SPL multisig accounts cannot sign those
instructions directly. PDA custody needs a controlling-program path to set
an independent keeper; the existing hLP order flow can still harvest using
the configured recipient.

### Referral Interest Sharing

Futarchy first lists a referrer and configures its share of realized protocol
interest revenue:

```typescript
const configureTx = await dusk.write.configureReferralPartnerTransaction({
  authoritySigner: futarchySigner.publicKey,
  referrer,
  interestShareBps: 2_500,
  active: true,
});
```

The listed referrer may then designate the wallet that receives claims:

```typescript
const partnerTx = await dusk.write.setReferralRecipientTransaction({
  authority: referrer.publicKey,
  recipient,
});
```

The referred-action builders derive the partner and its per-market, per-mint
accrual account, initialize the accrual idempotently, and compose setup with the
debt-opening instruction:

```typescript
const { transaction, referralPartner, referralAccrual } =
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
Existing borrow debt sides and leverage positions retain their bound partner on
later debt increases. The program snapshots the partner share, capped by the
current runtime maximum, when the binding is created. Deactivation or later
rate/cap updates affect new bindings only. Referral does not change requested
principal, position debt, interest, health, or liquidation terms.

When interest is realized, the partner accrues a governed share of the DAO's
interest revenue. Claims always pay a token account owned by the partner's
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
- `previewBorrowPositionCapacity({ capacityKind, market, borrowPosition, collateralAssetMint, debtAssetMint, collateralChange, projectedBorrowAmount })`.

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

For an existing borrow account, use `previewBorrowPositionCapacity` instead of
adding its debt to `previewBorrowCapacity`. The account-aware preview accrues
interest and replaces the account's own market-health contribution, using the
same calculations as the borrow and withdrawal instructions.

```typescript
const capacity = await dusk.get.previewBorrowPositionCapacity({
  capacityKind: "borrow",
  market,
  borrowPosition,
  collateralAssetMint: baseMint,
  debtAssetMint: quoteMint,
  collateralChange: new BN(0),
  projectedBorrowAmount: null, // Maximum additional draw; BN(0) means no draw.
});

capacity.existingDebtAmount;      // Accrued debt already owed.
capacity.maxBorrowAmount;        // Additional vault debit in borrow mode.
capacity.projectedDebtAmount;    // Existing debt plus the draw, with share rounding.
capacity.maxWithdrawAmount;      // Collateral vault debit in withdraw mode.
capacity.borrowAllowed;          // The requested draw is inside capacity limits.
```

Choose `capacityKind: "borrow"` for borrowing or `"withdraw"` for collateral
withdrawal. Only that limit is calculated; the other field is `null`. This keeps
the preview within the transaction compute budget at ordinary token scales.
Withdrawal mode permits only a zero or omitted projected draw.

`collateralChange` is a signed raw-unit delta: a positive value is the **net**
collateral credit after inbound transfer fees; a negative value is a vault
withdrawal debit. Invalid withdrawals fail the preview. A zero draw preserves
the position's issued liquidation factor; a draw quotes the terms of that draw.
Withdrawal capacity is measured after the collateral delta and before any
additional borrowing. Outbound transfer fees may reduce wallet receipts.

The instruction reads market and position accounts without persisting its
hypothetical changes, including when submitted with writable account metas.
Capacity does not replace transaction-time permission, reduce-only, token
account, referral, slippage, or fresh deployment checks.

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

### TypeScript and the IDL size

The Dusk IDL carries over 150 types. Anchor's `IdlTypes`, `IdlAccounts`, and
`IdlEvents` decode every type eagerly, several passes deep, which exceeds
TypeScript's fixed instantiation budget and fails with "Type instantiation is
excessively deep and possibly infinite" wherever `Program<Dusk>` or the
exported account and event types are used. This repository rewrites the
installed Anchor declarations with a lazily recursive decoder from
`scripts/patch_anchor_idl_types.js` during `postinstall`. Consumers that type
against `Program<Dusk>` need the same rewrite of
`@coral-xyz/anchor/dist/*/program/namespace/types.d.ts` until Anchor changes
the decoder upstream; the rewrite applies unchanged to Anchor 0.31 and 0.32.

## LP Mint Bootstrap

`initialize_market` takes three existing Token-2022 LP mints. Production builds
require their addresses to end in `yLP` (the yLP mint) and `hLP` (both hLP
mints). The SDK produces them as `create_with_seed` accounts of the creator,
so the creator is the only signer:

```typescript
import {
  createHookedLpMintWithSeedInstructions,
  grindMarketLpMintSeeds,
  lpMintRent,
  marketLpTokenNaming,
} from "@omnipair/dusk-sdk";

const seeds = await grindMarketLpMintSeeds({ base: creator }); // vanity.omnipair.fi
const rent = await lpMintRent(connection);
const ylp = await createHookedLpMintWithSeedInstructions({
  payer: creator, base: creator, seed: seeds.ylp.seed, mint: seeds.ylp.mint,
  decimals, mintAuthority: market, transferHookProgramId: PROGRAM_ID, lamports: rent,
});

const naming = marketLpTokenNaming({ baseSymbol: "META", quoteSymbol: "USDC" });
// naming.ylp      -> { name: "yMETA/USDC LP", symbol: "yMETA.USDC", description }
// naming.baseHlp  -> { name: "hMETA",         symbol: "hMETA.USDC", description }
// naming.quoteHlp -> { name: "hUSDC",         symbol: "hUSDC.META", description }
```

`grindLpMintSeed` asks the vanity server for a seed with `owner` set to
Token-2022 and re-derives the address locally before returning it; a server
that ground the wrong suffix or owner is rejected. Names are written once by
`initialize_lp_metadata` and cannot be changed afterwards, so use
`lpTokenNaming` rather than ad-hoc strings. The metadata JSON and images that
the URIs point to are produced by `scripts/lp-metadata/` in the dusk
repository.

## ESM Compatibility

This package ships strict ESM-compatible output. Relative module specifiers
include `.js` extensions in emitted files.

## License

MIT


## Token amounts and UI extensions

Dusk instruction amounts, token balances, and amount previews are raw integer
atoms (`BN`/`bigint`). The program supports Token-2022 group metadata,
`InterestBearingConfig`, and `ScaledUiAmount` on underlying assets. Group
metadata does not change settlement. Interest and UI multipliers change only
the displayed denomination; the SDK does not automatically apply them.

For these mints, dividing raw amounts by `10 ** decimals` alone is not a correct
wallet display. Use the Token-2022 program's `AmountToUiAmount` and
`UiAmountToAmount` instructions through RPC simulation with the current mint
state and clock. Refresh conversions after multiplier/rate updates or scheduled
changes take effect. Keep raw amounts as integers throughout transaction
construction; token-program UI conversion uses floating-point arithmetic and
must not be used as a lossless accounting round trip.

Dusk prices and price limits use fixed mint-decimal units, with nine-decimal NAD
precision, independently of UI multipliers. Convert UI prices at the client
boundary as well: if raw-decimal price is quote per base, displayed price is
`price * quote_display_multiplier / base_display_multiplier`. Interest-bearing
mints have a time-dependent display factor. Existing orders keep their original
raw price limits when the issuer changes the display. A display increase alone
does not create collateral or Dusk yield.

The token extension semantics are described in Solana's
[scaled UI amount](https://solana.com/docs/tokens/extensions/scaled-ui-amount) and
[interest-bearing token](https://solana.com/docs/tokens/extensions/interest-bearing-tokens)
documentation.


## Shared virtual-book previews and simulation context

Use `dusk` for a `Dusk` client instance. The SDK owns native preview construction,
account decoding, concentration sampling, cumulative quote batching, and
fee-inclusive depth projection:

```ts
const snapshot = await dusk.get.previewVirtualBook(market, {
  minContextSlot: verifiedSlot,
  groupingBps: 10,
  signal,
});

// snapshot.book contains bid/ask prices, cumulative sizes, fees, and surcharge.
// snapshot.slot / firstQuoteSlot identify the actual simulation banks.
// snapshot.observedAt is the first request's start time, not delivery time.
// snapshot.account is the accrued market returned by simulation.
```

These display previews use confirmed banks. Supported groupings are 5, 10, 25,
50, and 100 bps. A capture uses one market preview and at most six batches of
four swap previews. Batches must agree on market, mint and authority state,
epoch, and start price, with a maximum eight-slot spread. Output amounts include
Token-2022 transfer fees at the simulated epoch and the program's size-dependent
fees. Depth is display data; use a fresh preview for a user's exact transaction.

Applications that bracket each stage with their own deployment checks can call
`dusk.get.previewVirtualBookSnapshot(market, options)` and
`dusk.get.previewVirtualBookQuotes(snapshot, options)` separately, then
`projectDuskVirtualBook(quotes)`. `previewVirtualBookBatch(snapshot, requests,
options)` is available for one to four explicit cumulative inputs.

For other preview workflows, retain full RPC provenance instead of only return
data:

```ts
const result = await dusk.get.simulateWithContext([previewInstruction], {
  minContextSlot: verifiedSlot,
  accounts: [market, baseMint, quoteMint],
  signal,
  timeoutMs: 10_000,
});

result.context.slot;   // Actual RPC simulation slot.
result.value.accounts; // Ordered post-simulation accounts from that bank.
result.value.returnData;
result.value.logs;
result.observedAt;
```

The existing `get.previewSwap` and other decoded preview APIs remain compatible.
The SDK does not select deployments, attest program binaries, schedule refreshes,
set cache expiry, coordinate database locks, or stream to subscribers. Those
remain application responsibilities; validate deployment identity before and
after a capture. Cancellation and timeout bound the caller's wait; they do not
necessarily cancel an RPC request already sent over the transport.
