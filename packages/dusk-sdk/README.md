# @omnipair/dusk-sdk

For volume, fee, and borrower-interest consumers, see the
[accounting event contract](../../programs/dusk/ACCOUNTING_EVENTS.md).
`SwapExecuted` covers spot and leverage AMM executions and the market state they
leave behind. `BorrowInterestAccrued`
separates credit, margin, and hLP accrual; `BorrowInterestPaid` reports actual
collections with the same source attribution. Typed events and their
`SwapOrigin`/`DebtSource` discriminants are exported from this package.

TypeScript SDK for Dusk, the Omnipair V2 protocol architecture. This package
targets Dusk market layout v2, including aggregate leverage exposure and stored
position margin terms. Regenerate clients and use matching account layouts.

## Stored leverage margins

```typescript
import { getStoredLeverageMargins } from "@omnipair/dusk-sdk";

const position = await dusk.get.leveragePosition(positionAddress);
const { initialMarginBps, maintenanceMarginBps } = getStoredLeverageMargins(position);
// Or fetch and decode in one call:
const margins = await dusk.get.leverageMargins(positionAddress);
```

The helper reads the saved terms and applies the same conservative rate rounding
as Rust. It does not recalculate an existing position from live market crowding.
`admissionEquityCollateralNad` is collateral atoms times 1e9, not dollars or
debt-token atoms. These are required margin rates, not actual equity or an
executable leverage quote. Price and interest still change health. Simulate the
complete proposed transaction for openings, increases and withdrawals; executable
equity, cash and Token-2022 fees can impose tighter constraints.

`LeveragePositionOpened` and `LeveragePositionUpdated` include `marginTerms`.
Indexers should retain those fields per position and replace them on subsequent
position updates, rather than deriving old-position terms from current liquidity.

A `Dusk` instance is an enriched Anchor program facade. It exposes the raw
Anchor program through `dusk.program`, alongside typed on-chain reads and
previews through `dusk.get`, transaction builders through `dusk.write`, and
indexed historical data through `dusk.fetch`.

The package exports the generated Anchor IDL/types, PDA helpers, typed preview
decoders, a small write/read facade over the Dusk program, and an indexer client
for historical API data.

## Flash and emergency liquidations

The auction start/fill/backstop and old full leverage-liquidation instructions
have been replaced. Use `dusk.liquidations` for either borrowing or leverage:

```typescript
const position = { market, position: positionAddress, kind: "leverage" as const,
  debtAsset: "quote" as const };
// Send this separately to commit the incentive clock even if execution fails.
const observe = await dusk.liquidations.observeInstruction(position);
const quote = await dusk.liquidations.preview({ ...position, maxRepayment: 1_000_000n });
const plan = await dusk.liquidations.buildFlash({
  ...position, buyer, maxRepayment: 1_000_000n,
  route: async (ctx) => {
    // Return instructions that sell collateral or pay from buyer inventory.
    // Deliver ctx.requiredPaymentCredit NET into ctx.repaymentVault.
    // grossLiquidationPayment() handles the incoming token transfer fee.
    return buildPaymentRoute(ctx);
  },
});
// Compile plan.instructions unchanged, using flashLiquidationV0Transaction()
// with the necessary address lookup tables, then simulate, sign and submit.
```

`settlement: "duskAmm"` instead sells the flashed collateral in the paired settle,
with debt principal netted internally and no escrow-payment instruction. The
buyer must retain the flashed collateral until settle. Its return transfer can
incur another collateral token fee. Settlement rejects an AMM quote that cannot
fund the fixed obligation; it never silently debits the owner's wallet.
If the route changes active hLP accounts, pass the complete final-state account
list through `settleRemainingAccounts`; the default resolves the current market.

`previewEmergency()` and `buildEmergency()` accept `collateralDebit` and `full`.
They enforce the emergency EMA-health gate. A full sale also needs nonpositive
EMA equity or the program's conservative proof that no useful partial exists.
Failure to certify, including proof-budget exhaustion, rejects full closure.
Smaller useful fills can succeed without reaching the full MM +2-point target.
Preview and simulate each proposed slice; a failed slice is not a full-close
permission. Configured residual minima are debt-token atoms, separately per side.

Builders return unsigned instructions and owner-owned payout accounts. Prefix
instructions belong in the builder request: inserting instructions afterwards
invalidates the bound settle index. Helpers include ATA setup, a 256 KiB heap,
a 1.4M compute-unit limit and required hLP accounts. A compute-unit limit supplied
in the prefix overrides the default; set the priority price there as needed.
The session closes at settle. Its payment token account persists for reuse,
including withheld Token-2022 fees; preexisting token donations do not count as
payment and are not refunded to the next buyer.

Partial `maxRepayment` requests are capped for the configured minimum residual
debt. A request beyond the first MM+2 recovery crossing rejects; request a
smaller amount. Emergency collateral slices must also respect the residual
minimum and a conservative recovery cap. Its optimistic bound can reject a
target-crossing slice even if the exact smaller sale would pay less after costs;
useful below-target slices remain permitted. Preview acceptance does not reserve
liquidity or guarantee keeper profitability. Simulate the complete transaction.

`liquidationHealth`, `liquidationIncentives`, `liquidationFeeAllocation`,
`liquidationInsuranceTarget`, and `liquidationLossAllocation` use integer atoms
and mirror the policy arithmetic. They do not replace the on-chain execution
preview. Time raises incentives over 120 seconds but never grants emergency
access. A verified healthy position ends its distress episode.

Indexers should consume `FlashLiquidationBegun`, `FlashLiquidationSettled`,
`EmergencyLiquidationSettled` and `LiquidationObserved`. AMM settlement also emits
`SwapExecuted`; external purchases do not generate Dusk swap volume. Collected
interest retains `BorrowInterestPaid`/referral events. A terminal settlement
clears the debt leg but leaves position rent to the existing owner cleanup path.
`previewBorrowPosition` reports `liquidationHealth`, `liquidationRates` and
`liquidationDistress`; obsolete auction penalty and repayment-cap fields are gone.

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
  }
);
```

`swapBuilder(...)`, `swapInstruction(...)`, `swapTransaction(...)`, and
`swapRpc(...)` fetch the market before building the swap. Whenever either hLP
side has nonzero supply or residual exposure, they prepend the canonical
five-account prefix exactly once: `[yLP mint, base hLP yLP vault, quote hLP yLP
vault, base interest vault, quote interest vault]`. Caller-provided remaining
accounts remain after that prefix.

The write client supplies the canonical event-CPI authority and Dusk program
accounts for instructions that emit CPI events.

`write.builder(...)`, `write.transaction(...)`, and `write.rpc(...)` expose the
same generic path for every Dusk instruction in the IDL.

### Leverage funding

Leverage positions accept margin only in the debt token: USDC for a META long,
META for a META short. `buildOpenLeverageInstruction` swaps the credited margin
plus borrowing into the opposite token held as collateral. `closeLeverageInstruction`
sells exposure, repays the debt and returns the residual in the debt token.
An application can swap a different wallet token into the required funding token
before opening; the protocol has no optional collateral-funded mode.

### Swap and LP Transfer Events

`SwapExecuted` records the market immediately after a completed trade: the
internal yLP supply and, per side, a `MarketSideSnapshot` with the spot price
and price EMA exactly as `preview_market` reports them for the same state and
slot, plus the swap-fee and interest growth indexes. `start_price_nad` is the
Base price actually used to quote that trade, after any pending controller
step. Swap events provide historical execution and post-swap observations;
they are not a continuous live-price feed. For a prospective quote, use
`preview_swap` with the intended input and current state rather than treating
the last event's post-swap spot as the next trade's starting price.

`LpTransferred` reports each yLP or hLP transfer through the Dusk transfer
hook as an ordinary CPI event. `buildLpTransferHookAccountMetas` includes the
event authority the hook needs; Token-2022 ignores it for mints initialized
with the earlier seven-entry list, which transfer without a receipt.
`buildLpTransferHookValidationAccountData({ ..., legacyLayout: true })`
encodes that earlier list.

### Optional insurance funding

Markets can start with zero insurance. A voluntary top-up uses the existing
`fortify_market` instruction; it is a donation and does not mint LP shares or
give the donor a withdrawal claim.

```typescript
const topUp = await dusk.write.fortifyMarketInstruction({
  donor: wallet.publicKey,
  market,
  asset: "quote",
  assetMint: quoteMint,
  amount: 1_000_000n, // Gross raw token atoms, not a USD amount.
  // Optional: assetTokenProgram avoids the mint-owner RPC read.
  // Optional: donorAssetAccount overrides the donor's default ATA.
});
```

The builder does not fetch the market, so it can be appended after market
initialization, subject to the transaction's account, size and compute limits.
The donor's source token account must already exist. SPL Token and Token-2022
are supported; insurance accounting records the net amount actually received
after any transfer fee. `fortifyMarketTransaction(...)` wraps the same
instruction in a transaction without signing or sending it.

### Direct-yLP Parameter Governance

Market layout v2 has no market manager. The program exposes seven independent
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
  // One to seven updates, at most one per family, in any order. They are
  // sent in family order and pass or fail together.
  updates: [update],
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

The other typed update constructors are `feeParameterUpdate(...)`,
`irmParameterUpdate(...)`, `emaHalfLivesParameterUpdate(...)`,
`dailyBorrowLimitParameterUpdate(...)`, `centerControllerParameterUpdate(...)`
and `insuranceDrawCapsParameterUpdate(...)`. Only concentration ramps; its
duration must be 216,000–1,512,000 slots (approximately 24 hours–7 days).
`canonicalParameterUpdates(...)` returns a set in the order the program
accepts and rejects a repeated family, and `updatedFamilyRevisions(...)` builds
the revision list a new proposal binds.

Support and lifecycle builders derive the proposal/support PDAs and all market
governance accounts:

```typescript
await dusk.write.supportParameterProposal({
  supporter: wallet.publicKey,
  market,
  proposal,
  amount: additionalSupport,
  // The digest of the proposal the supporter reviewed, from its creation
  // event or account; support fails unless it matches the proposal.
  digest: proposalDigest,
});

await dusk.write.queueParameterProposal({ market, proposal });
await dusk.write.executeParameterProposal({ market, proposal });
await dusk.write.withdrawParameterSupport({
  supporter: wallet.publicKey,
  market,
  proposal,
  // Receives the proposal's rent, less its tombstone's, when the last
  // supporter withdraws.
  proposer,
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
expires, or becomes stale. When the last supporter withdraws, the proposal
becomes a tombstone: the account keeps only its discriminator, which stops the
address from ever holding another proposal, and the rest of its rent returns
to the proposer.

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
current recipient:

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
  Its `amountOut` is the output vault debit after Dusk trading fees.
  `outputTransferFee` is the fee withheld by the output mint at the simulated
  epoch, and `netAmountOut` is the recipient credit used by the swap's
  `minAssetOut` check. `grossAmountOut` is before Dusk trading fees.
- `previewBorrowCapacity({ market, collateralAssetMint, debtAssetMint, collateralAmount, projectedBorrowAmount })`.
- `previewBorrowPosition({ market, borrowPosition, baseMint, quoteMint })`.
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
`initialize_market` and cannot be changed afterwards, so use `lpTokenNaming`
rather than ad-hoc strings. Pass the three names, symbols, and URIs through
`initializeMarketInstruction`; `marketCreationLookupTablePlan` and
`marketCreationV0Transaction` build the v0 transaction once the lookup table is
active. The metadata JSON and images that
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
