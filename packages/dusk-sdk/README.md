# @omnipair/dusk-sdk

TypeScript SDK for Omnipair Dusk market layout v3.

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

Market layout v3 has no market manager. A direct yLP holder burn-locks at least
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
  peakDepthNad: 20n * 1_000_000_000n,
  fadeScaleNad: 10_000_000n,
  // Optional. Omission uses 216,000 slots (approximately 24 hours).
  rampDurationSlots: 432_000,
});

const metadata = await uploadProposalMetadata({
  title: "Increase depth around the current center",
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

The other update constructors are `feeParameterUpdate(...)`,
`irmParameterUpdate(...)`, `emaHalfLivesParameterUpdate(...)`, and
`dailyBorrowLimitParameterUpdate(...)`. Only concentration ramps; its duration
must be 216,000–1,512,000 slots (approximately 24 hours–7 days).

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

LP token accounts should be owned by a wallet that can sign Dusk instructions,
or by a PDA whose controlling program invokes Dusk with `invoke_signed`. SPL
multisig-owned LP accounts are unsupported: Token-2022 transfers can checkpoint
yield to that owner, but the multisig account itself cannot sign Dusk's claim or
recipient-update instruction.

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
